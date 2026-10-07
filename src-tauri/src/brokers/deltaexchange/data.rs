//! Quotes, depth and history (web `api/data.py`). All public, unsigned.

use super::{DeltaBroker, TIMEFRAME_MAP};
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveTime};
use serde_json::Value;

fn fv(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn iv(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| fv(v) as i64),
        _ => fv(v) as i64,
    }
}

/// The broker symbol of an OpenAlgo symbol; the symbol itself when the
/// master does not have it (web `_get_br_symbol`).
fn br_symbol(b: &DeltaBroker, key: &QuoteKey) -> String {
    b.resolver()
        .br_symbol(&key.symbol, &key.exchange)
        .unwrap_or_else(|| {
            tracing::debug!("{} not in the Delta master; using it as is", key.symbol);
            key.symbol.clone()
        })
}

/// `GET /v2/tickers/{symbol}` result (a single object).
async fn ticker(b: &DeltaBroker, br: &str) -> Result<Value> {
    let t: Value = b.public(&format!("/v2/tickers/{}", br), &[]).await?;
    match t {
        Value::Object(_) => Ok(t),
        Value::Null => Ok(Value::Object(Default::default())),
        _ => Err(AppError::Broker(
            "Delta Exchange sent a quote OpenAlgo could not read. Try again shortly.".into(),
        )),
    }
}

/// Ticker -> quote (web `get_quotes`): `ltp = mark_price`, `close` is the
/// previous close, bid/ask from `quotes.best_bid/best_ask`.
pub fn to_quote(key: &QuoteKey, t: &Value) -> Quote {
    let q = t.get("quotes").cloned().unwrap_or(Value::Null);
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: fv(t.get("mark_price")),
        open: fv(t.get("open")),
        high: fv(t.get("high")),
        low: fv(t.get("low")),
        close: fv(t.get("close")),
        volume: iv(t.get("volume")),
        bid: fv(q.get("best_bid")),
        ask: fv(q.get("best_ask")),
        bid_qty: 0,
        ask_qty: 0,
        oi: fv(t.get("oi")) as i64,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    }
}

pub async fn get_quote(b: &DeltaBroker, key: &QuoteKey) -> Result<Quote> {
    let t = ticker(b, &br_symbol(b, key)).await?;
    Ok(to_quote(key, &t))
}

fn levels(side: Option<&Value>) -> Vec<DepthLevel> {
    let mut out: Vec<DepthLevel> = side
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(5)
                .map(|l| DepthLevel {
                    price: fv(l.get("price")),
                    quantity: iv(l.get("size")),
                    orders: 0,
                })
                .collect()
        })
        .unwrap_or_default();
    out.resize(5, DepthLevel::default());
    out
}

/// Ticker + `l2orderbook` -> five-level depth (web `get_depth`).
pub fn to_depth(key: &QuoteKey, t: &Value, book: Option<&Value>) -> MarketDepth {
    let (bids, asks) = match book {
        Some(bk) => (levels(bk.get("buy")), levels(bk.get("sell"))),
        None => (levels(None), levels(None)),
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: bids.iter().map(|l| l.quantity).sum(),
        total_sell_qty: asks.iter().map(|l| l.quantity).sum(),
        bids,
        asks,
        ltp: fv(t.get("mark_price")),
        ltq: 0,
        open: fv(t.get("open")),
        high: fv(t.get("high")),
        low: fv(t.get("low")),
        prev_close: fv(t.get("close")),
        volume: iv(t.get("volume")),
        oi: fv(t.get("oi")) as i64,
    }
}

pub async fn get_market_depth(b: &DeltaBroker, key: &QuoteKey) -> Result<MarketDepth> {
    let t = ticker(b, &br_symbol(b, key)).await?;
    let product_id = iv(t.get("product_id"));
    if product_id == 0 {
        tracing::warn!("No product id in the Delta ticker for {}", key.symbol);
        return Ok(to_depth(key, &t, None));
    }
    // A failed book still answers with the ticker's prices (web).
    let book = match b
        .public::<Value>(&format!("/v2/l2orderbook/{}", product_id), &[])
        .await
    {
        Ok(v) if v.is_object() => Some(v),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Delta order book for {} failed: {}", key.symbol, e.code());
            None
        }
    };
    Ok(to_depth(key, &t, book.as_ref()))
}

/// Days per history request by Delta resolution (web `CHUNK_DAYS`); 0 means
/// one request for the whole range.
pub fn chunk_days(resolution: &str) -> i64 {
    match resolution {
        "1m" => 1,
        "3m" => 7,
        "5m" => 12,
        "15m" => 30,
        "30m" => 60,
        "1h" | "2h" | "4h" | "6h" => 90,
        "1d" | "1w" => 0,
        _ => 30,
    }
}

/// An IST calendar date at 00:00:00 or 23:59:59 IST, as epoch seconds (web
/// `_to_epoch`).
pub fn ist_epoch(d: NaiveDate, end_of_day: bool) -> i64 {
    let t = if end_of_day {
        NaiveTime::from_hms_opt(23, 59, 59)
    } else {
        NaiveTime::from_hms_opt(0, 0, 0)
    }
    .unwrap_or(NaiveTime::MIN);
    d.and_time(t).and_utc().timestamp() - 19_800
}

/// One candle in either shape Delta sends: `[t, o, h, l, c, v(, oi)]` or a
/// named object (`time`/`timestamp`/`t`, `open`/`o`, ...).
pub fn parse_candle(c: &Value) -> Option<Candle> {
    if let Some(a) = c.as_array() {
        if a.len() < 6 {
            return None;
        }
        return Some(Candle {
            timestamp: iv(a.first()),
            open: fv(a.get(1)),
            high: fv(a.get(2)),
            low: fv(a.get(3)),
            close: fv(a.get(4)),
            volume: iv(a.get(5)),
            oi: iv(a.get(6)),
        });
    }
    let o = c.as_object()?;
    let pick = |a: &str, b: &str| o.get(a).or_else(|| o.get(b));
    Some(Candle {
        timestamp: iv(o
            .get("time")
            .or_else(|| o.get("timestamp"))
            .or_else(|| o.get("t"))),
        open: fv(pick("open", "o")),
        high: fv(pick("high", "h")),
        low: fv(pick("low", "l")),
        close: fv(pick("close", "c")),
        volume: iv(pick("volume", "v")),
        oi: iv(o.get("oi")),
    })
}

/// `GET /v2/history/candles` chunked over the range (web `get_history`).
/// Requests stop at the present moment and bars after it are dropped:
/// Delta pads the window it is asked for with flat zero-volume candles.
pub async fn get_history(b: &DeltaBroker, req: &HistoryRequest) -> Result<Vec<Candle>> {
    let resolution = TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let keys: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Unsupported interval '{}'. Supported: {}",
                req.interval,
                keys.join(", ")
            ))
        })?;
    let br = br_symbol(b, &req.key);
    let days = chunk_days(resolution);
    let ranges = if days == 0 {
        if req.start <= req.end {
            vec![(req.start, req.end)]
        } else {
            Vec::new()
        }
    } else {
        chunks(req.start, req.end, days)
    };
    let now = b.now();
    let mut all = Vec::new();
    for (start, end) in ranges {
        let start_ts = ist_epoch(start, false);
        let end_ts = ist_epoch(end, true).min(now);
        if end_ts < start_ts {
            continue;
        }
        let params = [
            ("symbol", br.clone()),
            ("resolution", resolution.to_string()),
            ("start", start_ts.to_string()),
            ("end", end_ts.to_string()),
        ];
        let rows: Value = b.public("/v2/history/candles", &params).await?;
        let Some(rows) = rows.as_array() else {
            return Err(AppError::Broker(
                "Delta Exchange sent history OpenAlgo could not read. Try again shortly.".into(),
            ));
        };
        all.extend(rows.iter().filter_map(parse_candle));
    }
    all.retain(|c| c.timestamp <= now);
    Ok(sort_dedupe(all))
}
