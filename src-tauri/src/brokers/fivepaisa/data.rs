//! Quotes, depth and history (web `api/data.py`).
//!
//! * quotes: `POST /VendorsAPI/Service1.svc/MarketSnapshot` with
//!   `{ClientCode, Data:[{Exchange, ExchangeType, ScripCode, ScripData}]}`;
//!   bid/ask from `V2/MarketDepth` (best of `BbBuySellFlag` 66 / 83).
//! * multiquotes: the same snapshot, 50 scrips per call, 0.5 s apart; no
//!   bid/ask.
//! * history: `GET /V2/historical/{Exch}/{ExchType}/{token}/{interval}
//!   ?from=YYYY-MM-DD&end=YYYY-MM-DD`, 30-day chunks intraday, 100-day
//!   chunks daily; candles `[ts, o, h, l, c, v]` in IST wall clock.

use super::mapping::{self, body_rows, num, text};
use super::{session, FivepaisaBroker, Session};
use crate::brokers::common::history::{chunks, sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{json, Value};

pub const SNAPSHOT: &str = "/VendorsAPI/Service1.svc/MarketSnapshot";
pub const DEPTH: &str = "/VendorsAPI/Service1.svc/V2/MarketDepth";
/// web: the snapshot returns nothing for 100+ scrips; 50 is reliable.
pub const BATCH_SIZE: usize = 50;

/// An instrument resolved for a market-data call.
#[derive(Debug, Clone, PartialEq)]
pub struct Scrip {
    /// Exchange as requested (`Exchange` code is taken from this).
    pub exchange: String,
    /// Exchange the token was looked up under (`ExchangeType` from this).
    pub lookup_exchange: String,
    pub token: String,
    pub brsymbol: String,
}

impl Scrip {
    /// One entry of a snapshot / depth request.
    pub fn entry(&self) -> Value {
        json!({
            "Exchange": mapping::exch_code(&self.exchange),
            "ExchangeType": mapping::exch_type(&self.lookup_exchange),
            "ScripCode": self.token,
            "ScripData": if self.token == "0" { self.brsymbol.as_str() } else { "" },
        })
    }

    /// Key used to match a snapshot row back to this scrip.
    fn match_key(&self) -> String {
        if self.token == "0" {
            format!("scripdata:{}", self.brsymbol)
        } else {
            self.token.clone()
        }
    }
}

pub fn scrip(b: &FivepaisaBroker, key: &QuoteKey) -> Result<Scrip> {
    let lookup = mapping::query_exchange(&key.symbol, &key.exchange);
    let row = b
        .resolver()
        .by_symbol(&lookup, &key.symbol)
        .ok_or_else(|| mapping::unknown_symbol(&key.symbol, &key.exchange))?;
    Ok(Scrip {
        exchange: key.exchange.clone(),
        lookup_exchange: lookup,
        token: row.token.clone(),
        brsymbol: row.br_symbol().to_string(),
    })
}

/// `PClose`, else `PreviousClose`, else `Close`.
fn prev_close(q: &Value) -> f64 {
    ["PClose", "PreviousClose", "Close"]
        .iter()
        .map(|k| num(q, k))
        .find(|v| *v != 0.0)
        .unwrap_or(0.0)
}

/// One snapshot row -> quote (bid/ask filled by the caller).
pub fn to_quote(key: &QuoteKey, q: &Value) -> Quote {
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: num(q, "LastTradedPrice"),
        open: num(q, "Open"),
        high: num(q, "High"),
        low: num(q, "Low"),
        close: prev_close(q),
        volume: num(q, "Volume") as i64,
        oi: num(q, "OpenInterest") as i64,
        ..Default::default()
    };
    if out.close > 0.0 && out.ltp > 0.0 {
        out.change = crate::brokers::common::streaming::round2(out.ltp - out.close);
        out.change_percent =
            crate::brokers::common::streaming::round2((out.ltp - out.close) / out.close * 100.0);
    }
    out
}

fn depth_side(rows: &[Value], flag: i64) -> Vec<&Value> {
    rows.iter()
        .filter(|r| num(r, "BbBuySellFlag") as i64 == flag)
        .collect()
}

/// `MarketDepthData` -> (bids high-to-low, asks low-to-high, total buy,
/// total sell). Levels are padded to five.
pub fn to_levels(rows: &[Value]) -> (Vec<DepthLevel>, Vec<DepthLevel>, i64, i64) {
    let level = |r: &&Value| DepthLevel {
        price: num(r, "Price"),
        quantity: num(r, "Quantity") as i64,
        orders: num(r, "NumberOfOrders") as i64,
    };
    let mut bids: Vec<DepthLevel> = depth_side(rows, 66).iter().map(level).collect();
    let mut asks: Vec<DepthLevel> = depth_side(rows, 83).iter().map(level).collect();
    let tb = bids.iter().map(|l| l.quantity).sum();
    let ts = asks.iter().map(|l| l.quantity).sum();
    bids.sort_by(|a, b| b.price.total_cmp(&a.price));
    asks.sort_by(|a, b| a.price.total_cmp(&b.price));
    bids.truncate(5);
    asks.truncate(5);
    bids.resize(5, DepthLevel::default());
    asks.resize(5, DepthLevel::default());
    (bids, asks, tb, ts)
}

async fn snapshot(b: &FivepaisaBroker, s: &Session, entries: Vec<Value>) -> Result<Vec<Value>> {
    let v = b
        .post(
            SNAPSHOT,
            json!({"ClientCode": s.client_code, "Data": entries}),
            s,
        )
        .await?;
    if !super::head_success(&v) {
        tracing::warn!(
            broker = "fivepaisa",
            "Market snapshot refused: {}",
            super::message(&v)
        );
        return Err(AppError::Broker(
            "5paisa did not return quotes for this request. Try again shortly.".into(),
        ));
    }
    Ok(body_rows(&v, "Data"))
}

async fn depth_rows(b: &FivepaisaBroker, s: &Session, sc: &Scrip) -> Result<Vec<Value>> {
    let mut body = sc.entry();
    if let Some(o) = body.as_object_mut() {
        o.insert("ClientCode".into(), json!(s.client_code));
    }
    let v = b.post(DEPTH, body, s).await?;
    if !super::head_success(&v) {
        tracing::warn!(
            broker = "fivepaisa",
            "Market depth refused: {}",
            super::message(&v)
        );
        return Err(AppError::Broker(
            "5paisa did not return market depth for this instrument. Try again shortly.".into(),
        ));
    }
    Ok(body_rows(&v, "MarketDepthData"))
}

fn no_data(key: &QuoteKey) -> AppError {
    AppError::Broker(format!(
        "5paisa returned no data for {} on {}.",
        key.symbol, key.exchange
    ))
}

pub async fn get_quote(b: &FivepaisaBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = session(auth)?;
    let sc = scrip(b, key)?;
    let rows = snapshot(b, &s, vec![sc.entry()]).await?;
    let first = rows.first().ok_or_else(|| no_data(key))?;
    let mut q = to_quote(key, first);
    // Bid/ask from the depth call; a failure leaves them at zero (web).
    match depth_rows(b, &s, &sc).await {
        Ok(d) => {
            q.bid = depth_side(&d, 66)
                .iter()
                .map(|r| num(r, "Price"))
                .fold(0.0, f64::max);
            q.ask = depth_side(&d, 83)
                .iter()
                .map(|r| num(r, "Price"))
                .reduce(f64::min)
                .unwrap_or(0.0);
        }
        Err(e) => tracing::debug!(
            broker = "fivepaisa",
            "Depth for bid/ask failed: {}",
            e.code()
        ),
    }
    Ok(q)
}

pub async fn get_multiquotes(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let s = session(auth)?;
    let mut out: Vec<Option<QuoteResult>> = vec![None; keys.len()];
    let mut pending: Vec<(usize, Scrip)> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        match scrip(b, k) {
            Ok(sc) => pending.push((i, sc)),
            Err(_) => {
                out[i] = Some(QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("Could not resolve token".into()),
                })
            }
        }
    }
    for (n, batch) in pending.chunks(BATCH_SIZE).enumerate() {
        if n > 0 {
            tokio::time::sleep(b.batch_pause).await;
        }
        let rows = snapshot(b, &s, batch.iter().map(|(_, sc)| sc.entry()).collect()).await?;
        for row in &rows {
            let code = text(row, "ScripCode");
            let data = {
                let d = text(row, "ScripData");
                if d.is_empty() {
                    text(row, "Symbol")
                } else {
                    d
                }
            };
            let hit = batch.iter().find(|(i, sc)| {
                out[*i].is_none()
                    && (sc.match_key() == code
                        || (code == "0" && sc.match_key() == format!("scripdata:{}", data))
                        || (!data.is_empty() && sc.brsymbol == data))
            });
            if let Some((i, _)) = hit {
                out[*i] = Some(QuoteResult {
                    symbol: keys[*i].symbol.clone(),
                    exchange: keys[*i].exchange.clone(),
                    data: Some(to_quote(&keys[*i], row)),
                    error: None,
                });
            }
        }
    }
    Ok(out
        .into_iter()
        .zip(keys)
        .map(|(r, k)| {
            r.unwrap_or_else(|| QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some("No data returned".into()),
            })
        })
        .collect())
}

pub async fn get_market_depth(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let s = session(auth)?;
    let sc = scrip(b, key)?;
    let rows = snapshot(b, &s, vec![sc.entry()]).await?;
    let q = rows.first().ok_or_else(|| no_data(key))?.clone();
    let d = depth_rows(b, &s, &sc).await?;
    let (bids, asks, tb, ts) = to_levels(&d);
    Ok(MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids,
        asks,
        ltp: num(&q, "LastTradedPrice"),
        ltq: num(&q, "LastTradedQty") as i64,
        open: num(&q, "Open"),
        high: num(&q, "High"),
        low: num(&q, "Low"),
        prev_close: num(&q, "PClose"),
        volume: num(&q, "Volume") as i64,
        oi: num(&q, "OpenInterest") as i64,
        total_buy_qty: tb,
        total_sell_qty: ts,
    })
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// OpenAlgo interval -> 5paisa interval code (web `map_interval`).
pub fn interval_code(interval: &str) -> Option<&'static str> {
    Some(match interval {
        "1m" => "1m",
        "5m" => "5m",
        "10m" => "10m",
        "15m" => "15m",
        "30m" => "30m",
        "1h" => "60m",
        "D" | "d" | "1d" => "1d",
        _ => return None,
    })
}

/// web chunking: 100 days daily, 30 days intraday.
pub fn chunk_days(daily: bool) -> i64 {
    if daily {
        100
    } else {
        30
    }
}

fn value_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One `[ts, o, h, l, c, v]` candle with the web's filters: indices drop
/// only all-zero OHLC; daily non-index also drops zero volume and
/// high == low; intraday non-index drops only all-zero OHLC. Daily
/// candles are stamped at the date's midnight UTC; intraday at the IST
/// wall-clock time.
pub fn parse_candle(c: &Value, daily: bool, is_index: bool) -> Option<Candle> {
    let a = c.as_array()?;
    if a.len() < 6 {
        return None;
    }
    let ts = NaiveDateTime::parse_from_str(a[0].as_str()?, "%Y-%m-%dT%H:%M:%S").ok()?;
    let (o, h, l, cl) = (
        value_f64(&a[1])?,
        value_f64(&a[2])?,
        value_f64(&a[3])?,
        value_f64(&a[4])?,
    );
    let vol = value_f64(&a[5])? as i64;
    let all_zero = o == 0.0 && h == 0.0 && l == 0.0 && cl == 0.0;
    if all_zero || (!is_index && daily && (vol == 0 || h == l)) {
        return None;
    }
    let timestamp = if daily {
        ts.date().and_hms_opt(0, 0, 0)?.and_utc().timestamp()
    } else {
        ts.and_utc().timestamp() - IST_OFFSET_SECS
    };
    Some(Candle {
        timestamp,
        open: o,
        high: h,
        low: l,
        close: cl,
        volume: vol,
        oi: 0,
    })
}

/// Whether a candle's date lies inside the requested chunk. 5paisa answers
/// a range with no sessions in it (a future-dated one) with the latest
/// candle instead of an empty list, so anything outside is dropped
/// (web #2195).
pub fn candle_in_range(c: &Value, from: NaiveDate, to: NaiveDate) -> bool {
    c.as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .and_then(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").ok())
        .is_some_and(|ts| (from..=to).contains(&ts.date()))
}

/// The history path for one chunk.
pub fn history_path(
    exchange: &str,
    token: &str,
    code: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> String {
    format!(
        "/V2/historical/{}/{}/{}/{}?from={}&end={}",
        mapping::exch_code(exchange),
        mapping::exch_type(exchange),
        token,
        code,
        from.format("%Y-%m-%d"),
        to.format("%Y-%m-%d")
    )
}

pub async fn get_history(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let s = session(auth)?;
    let code = interval_code(&req.interval).ok_or_else(|| {
        AppError::Validation(format!(
            "Interval '{}' is not supported by 5paisa. Use one of 1m, 5m, 10m, 15m, 30m, 1h, D.",
            req.interval
        ))
    })?;
    let daily = code == "1d";
    let lookup = mapping::query_exchange(&req.key.symbol, &req.key.exchange);
    let is_index = lookup.ends_with("_INDEX");
    let token = b
        .resolver()
        .token(&req.key.symbol, &lookup)
        .ok_or_else(|| mapping::unknown_symbol(&req.key.symbol, &req.key.exchange))?;
    let mut candles = Vec::new();
    for (from, to) in chunks(req.start, req.end, chunk_days(daily)) {
        let path = history_path(&req.key.exchange, &token, code, from, to);
        let v = match b.get(&path, &s).await {
            Ok(v) => v,
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!(
                    broker = "fivepaisa",
                    "History chunk {} to {} failed: {}",
                    from,
                    to,
                    e.code()
                );
                continue;
            }
        };
        if v.get("status").and_then(Value::as_str) != Some("success") {
            tracing::warn!(
                broker = "fivepaisa",
                "History chunk {} to {} refused: {}",
                from,
                to,
                text(&v, "message")
            );
            continue;
        }
        if let Some(rows) = v
            .get("data")
            .and_then(|d| d.get("candles"))
            .and_then(Value::as_array)
        {
            candles.extend(
                rows.iter()
                    .filter(|c| candle_in_range(c, from, to))
                    .filter_map(|c| parse_candle(c, daily, is_index)),
            );
        }
    }
    Ok(sort_dedupe(candles))
}
