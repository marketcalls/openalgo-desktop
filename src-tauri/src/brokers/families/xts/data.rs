//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::mapping::{self, f, i};
use super::XtsBroker;
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::clock::IST_OFFSET_SECS;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, Utc};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// XTS caps one quotes call at 50 instruments (`data.py:291`).
pub const QUOTE_BATCH: usize = 50;
/// Gap between multiquote batches (`data.py:292`).
pub const QUOTE_BATCH_GAP: Duration = Duration::from_millis(100);
/// The ohlc call's `startTime`/`endTime` format, IST wall clock.
pub const OHLC_TIME_FORMAT: &str = "%b %d %Y";

fn resolve(b: &XtsBroker, key: &QuoteKey) -> Result<(Exchange, SymToken, i64)> {
    let ex: Exchange =
        key.exchange
            .parse()
            .map_err(|e: crate::brokers::common::mapping::InvalidConstant| {
                AppError::Validation(e.to_string())
            })?;
    let code = mapping::segment_code(ex).ok_or_else(|| {
        AppError::Validation(format!("Unknown exchange segment: {}", key.exchange))
    })?;
    let row = b.resolver().by_symbol(ex.as_str(), &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })?;
    Ok((ex, row, code))
}

fn instrument(code: i64, token: &str) -> Value {
    json!({"exchangeSegment": code, "exchangeInstrumentID": mapping::instrument_id(token)})
}

/// `result.listQuotes`, each a JSON string (or an object), decoded.
pub fn list_quotes(v: &Value) -> Vec<Value> {
    v.get("result")
        .and_then(|r| r.get("listQuotes"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| match q {
                    Value::String(s) => serde_json::from_str(s).ok(),
                    o @ Value::Object(_) => Some(o.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn quotes_call(
    b: &XtsBroker,
    auth: &AuthToken,
    instruments: Vec<Value>,
    code: u16,
) -> Result<Vec<Value>> {
    let body = json!({
        "instruments": instruments,
        "xtsMessageCode": code,
        "publishFormat": "JSON",
    });
    let v = b
        .market(Method::POST, "/instruments/quotes", auth, Some(&body), None)
        .await?;
    Ok(list_quotes(&v))
}

/// Instrument key of a quote payload: `"{ExchangeSegment}_{ExchangeInstrumentID}"`.
pub fn quote_key(q: &Value) -> String {
    format!(
        "{}_{}",
        mapping::s(q, "ExchangeSegment"),
        mapping::s(q, "ExchangeInstrumentID")
    )
}

fn touchline(q: &Value) -> &Value {
    q.get("Touchline").filter(|t| t.is_object()).unwrap_or(q)
}

/// A 1502 payload as an OpenAlgo quote (`data.py:254-265`).
pub fn quote_from(q: &Value, oi: i64, key: &QuoteKey) -> Quote {
    let t = touchline(q);
    let ask = t.get("AskInfo").unwrap_or(&Value::Null);
    let bid = t.get("BidInfo").unwrap_or(&Value::Null);
    let ltp = f(t, "LastTradedPrice");
    let close = f(t, "Close");
    let (change, change_percent) = if close > 0.0 {
        (round2(ltp - close), round2((ltp - close) / close * 100.0))
    } else {
        (0.0, 0.0)
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: f(t, "Open"),
        high: f(t, "High"),
        low: f(t, "Low"),
        close,
        volume: i(t, "TotalTradedQuantity"),
        bid: f(bid, "Price"),
        ask: f(ask, "Price"),
        bid_qty: i(bid, "Size"),
        ask_qty: i(ask, "Size"),
        oi,
        change,
        change_percent,
        timestamp: String::new(),
    }
}

async fn open_interest(b: &XtsBroker, auth: &AuthToken, code: i64, token: &str) -> i64 {
    match quotes_call(b, auth, vec![instrument(code, token)], 1510).await {
        Ok(list) => list.first().map(|q| i(q, "OpenInterest")).unwrap_or(0),
        Err(e) => {
            tracing::debug!(broker = b.cfg.id, "Open interest unavailable: {}", e.code());
            0
        }
    }
}

async fn depth_payload(b: &XtsBroker, auth: &AuthToken, code: i64, token: &str) -> Result<Value> {
    quotes_call(b, auth, vec![instrument(code, token)], 1502)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            AppError::Broker(format!(
                "{} returned no market data for this instrument.",
                b.cfg.name
            ))
        })
}

/// The last traded price of one instrument, 0 when the touchline carries
/// none (web ibulls `get_ltp`): one 1502 read without the open-interest
/// call `get_quote` adds, since an order priced off it should not wait on
/// a second round trip.
pub(crate) async fn get_ltp(b: &XtsBroker, auth: &AuthToken, key: &QuoteKey) -> Result<f64> {
    let (_, row, code) = resolve(b, key)?;
    let quotes = quotes_call(b, auth, vec![instrument(code, &row.token)], 1502).await?;
    Ok(quotes
        .first()
        .map(|q| f(touchline(q), "LastTradedPrice"))
        .unwrap_or(0.0))
}

pub(crate) async fn get_quote(b: &XtsBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let (_, row, code) = resolve(b, key)?;
    let q = depth_payload(b, auth, code, &row.token).await?;
    let oi = open_interest(b, auth, code, &row.token).await;
    Ok(quote_from(&q, oi, key))
}

pub(crate) async fn get_multiquotes(
    b: &XtsBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut out: Vec<QuoteResult> = keys
        .iter()
        .map(|k| QuoteResult {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            data: None,
            error: None,
        })
        .collect();
    // (request index, instrument key, instrument json)
    let mut wanted: Vec<(usize, String, Value)> = Vec::new();
    for (idx, k) in keys.iter().enumerate() {
        match resolve(b, k) {
            Ok((_, row, code)) => wanted.push((
                idx,
                format!("{}_{}", code, row.token),
                instrument(code, &row.token),
            )),
            Err(e) => out[idx].error = Some(e.client_message()),
        }
    }
    for (n, batch) in wanted.chunks(QUOTE_BATCH).enumerate() {
        if n > 0 {
            tokio::time::sleep(QUOTE_BATCH_GAP).await;
        }
        let instruments: Vec<Value> = batch.iter().map(|(_, _, v)| v.clone()).collect();
        let quotes = quotes_call(b, auth, instruments.clone(), 1502).await?;
        let by_key: HashMap<String, Value> =
            quotes.into_iter().map(|q| (quote_key(&q), q)).collect();
        let oi: HashMap<String, i64> = if b.cfg.hooks.multiquote_oi {
            match quotes_call(b, auth, instruments, 1510).await {
                Ok(list) => list
                    .iter()
                    .map(|q| (quote_key(q), i(q, "OpenInterest")))
                    .collect(),
                Err(_) => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        for (idx, ik, _) in batch {
            match by_key.get(ik) {
                Some(q) => {
                    out[*idx].data =
                        Some(quote_from(q, oi.get(ik).copied().unwrap_or(0), &keys[*idx]))
                }
                None => {
                    out[*idx].error =
                        Some(format!("{} returned no quote for this symbol.", b.cfg.name))
                }
            }
        }
    }
    Ok(out)
}

fn levels(v: Option<&Value>) -> Vec<DepthLevel> {
    let mut out: Vec<DepthLevel> = v
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(5)
                .map(|l| DepthLevel {
                    price: f(l, "Price"),
                    quantity: i(l, "Size"),
                    orders: i(l, "TotalOrders"),
                })
                .collect()
        })
        .unwrap_or_default();
    out.resize(5, DepthLevel::default());
    out
}

/// A 1502 payload as five-level depth (`data.py:896-919`).
pub fn depth_from(q: &Value, oi: i64, key: &QuoteKey) -> MarketDepth {
    let t = touchline(q);
    // `LastTradedQunatity` is XTS's own spelling.
    let ltq = mapping::num(
        t.get("LastTradedQunatity")
            .or_else(|| t.get("LastTradedQuantity")),
    ) as i64;
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: levels(q.get("Bids")),
        asks: levels(q.get("Asks")),
        ltp: f(t, "LastTradedPrice"),
        ltq,
        open: f(t, "Open"),
        high: f(t, "High"),
        low: f(t, "Low"),
        prev_close: f(t, "Close"),
        volume: i(t, "TotalTradedQuantity"),
        oi,
        total_buy_qty: i(t, "TotalBuyQuantity"),
        total_sell_qty: i(t, "TotalSellQuantity"),
    }
}

pub(crate) async fn get_market_depth(
    b: &XtsBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let (_, row, code) = resolve(b, key)?;
    let q = depth_payload(b, auth, code, &row.token).await?;
    let oi = open_interest(b, auth, code, &row.token).await;
    Ok(depth_from(&q, oi, key))
}

/// `result.dataReponse` (sic): rows split on `,`, fields on `|`:
/// `epoch|open|high|low|close|volume` (`data.py:614-633`).
pub fn parse_ohlc(data: &str) -> Vec<Candle> {
    data.trim()
        .split(',')
        .filter_map(|row| {
            let p: Vec<&str> = row.split('|').collect();
            if p.len() < 6 {
                return None;
            }
            Some(Candle {
                timestamp: p[0].trim().parse().ok()?,
                open: p[1].trim().parse().ok()?,
                high: p[2].trim().parse().ok()?,
                low: p[3].trim().parse().ok()?,
                close: p[4].trim().parse().ok()?,
                volume: p[5].trim().parse::<f64>().ok()? as i64,
                oi: 0,
            })
        })
        .collect()
}

/// Post-processing (`data.py:717-741`): sort and dedupe; daily candles
/// snap to midnight; intraday candles lose the 5:30 h IST shift XTS bakes
/// into its epochs and are floored to the interval.
pub fn normalise_candles(candles: Vec<Candle>, compression: &str) -> Vec<Candle> {
    let mut out = sort_dedupe(candles);
    if compression == "D" {
        for c in &mut out {
            c.timestamp -= c.timestamp.rem_euclid(86_400);
        }
    } else {
        let step = compression.parse::<i64>().unwrap_or(0) / 60 * 60;
        for c in &mut out {
            c.timestamp -= IST_OFFSET_SECS;
            if step > 0 {
                c.timestamp -= c.timestamp.rem_euclid(step);
            }
        }
    }
    out
}

/// `startTime` / `endTime` of one chunk: 00:00:00 to 23:59:59 IST.
pub fn ohlc_window(start: NaiveDate, end: NaiveDate) -> (String, String) {
    (
        format!("{} 000000", start.format(OHLC_TIME_FORMAT)),
        format!("{} 235959", end.format(OHLC_TIME_FORMAT)),
    )
}

fn today_ist() -> NaiveDate {
    (Utc::now() + chrono::Duration::seconds(IST_OFFSET_SECS)).date_naive()
}

pub(crate) async fn get_history(
    b: &XtsBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let compression = super::timeframes(b.cfg)
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| AppError::Validation(format!("Unsupported timeframe: {}", req.interval)))?;
    let (ex, row, code) = resolve(b, &req.key)?;
    let segment = mapping::history_segment(ex)
        .ok_or_else(|| AppError::Validation(format!("Unsupported exchange: {}", ex)))?;
    let mut all = Vec::new();
    // Inclusive windows of the member's whole days, each 00:00:00 to
    // 23:59:59, the next starting one second later. The template web loop
    // ends a six-day window at 00:00:00 of its sixth day and starts the next
    // a day later, losing that day; full days avoid the gap. 5paisa XTS's
    // asks 365 days at a time to the second (web #2490), which these are.
    for (s, e) in chunks(req.start, req.end, b.cfg.hooks.history_window_days) {
        let (from, to) = ohlc_window(s, e);
        let query = [
            ("exchangeSegment", segment.to_string()),
            ("exchangeInstrumentID", row.token.clone()),
            ("startTime", from),
            ("endTime", to),
            ("compressionValue", compression.to_string()),
        ];
        let v = b
            .market(Method::GET, "/instruments/ohlc", auth, None, Some(&query))
            .await?;
        let data = v
            .get("result")
            .and_then(|r| r.get("dataReponse"))
            .and_then(Value::as_str)
            .unwrap_or("");
        all.extend(parse_ohlc(data));
    }
    if all.is_empty() && compression == "D" && req.end == today_ist() {
        // `data.py:642-711`: today's daily candle from a 1502 quote.
        let q = depth_payload(b, auth, code, &row.token).await?;
        let t = touchline(&q);
        let midnight = today_ist()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp())
            .unwrap_or(0);
        return Ok(vec![Candle {
            timestamp: midnight,
            open: f(t, "Open"),
            high: f(t, "High"),
            low: f(t, "Low"),
            close: f(t, "LastTradedPrice"),
            volume: i(t, "TotalTradedQuantity"),
            oi: 0,
        }]);
    }
    Ok(normalise_candles(all, compression))
}
