//! Quotes, depth and history (web `api/data.py`).
//!
//! * Quotes: `GET /instruments/quote` with a JSON body
//!   `{"mode":"OHLC","exchangeTokens":{"NSE":["3045"]}}` -> `data.fetched[]`
//!   (`ltp, open, high, low, close, volume, symbolToken`); bid, ask and OI
//!   are not in OHLC mode and read 0 (`data.py:215-243`). Multiquotes send up
//!   to 500 tokens per call, paced at one call per second (`:262-281`).
//! * Depth: no REST endpoint; a one-shot market-data socket in snap mode
//!   (web `fetch_quote(token, exchange_type, mode=3)`, `:854-912`), bounded
//!   by a timeout and closed on every path.
//! * History: `GET /instruments/historical` (JSON body) in per-interval
//!   chunks paced at one per second, plus `POST /instruments/intraday` for
//!   today (`:452-830`).

use super::mapping::{f, s};
use super::streaming::{self, exchange_type, parse_frame, Packet};
use super::{is_success, message, refusal, MstockBroker, MstockSession};
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::common::streaming::Message;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub const QUOTE_PATH: &str = "/instruments/quote";
pub const HISTORICAL_PATH: &str = "/instruments/historical";
pub const INTRADAY_PATH: &str = "/instruments/intraday";
/// web multiquote `BATCH_SIZE`.
pub const QUOTE_BATCH: usize = 500;

/// Quote / historical exchange string (`data.py:129-139, 194-204`).
pub fn api_exchange(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" | "NSE_INDEX" => "NSE",
        "BSE" | "BSE_INDEX" => "BSE",
        "NFO" => "NFO",
        "BFO" => "BFO",
        "CDS" => "CDS",
        "MCX" | "MCX_INDEX" => "MCX",
        _ => return None,
    })
}

/// Intraday numeric exchange code (`data.py:142-153`).
pub fn intraday_exchange(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" | "NSE_INDEX" => "1",
        "NFO" => "2",
        "CDS" => "3",
        "BSE" | "BSE_INDEX" => "4",
        "BFO" => "5",
        "MCX" | "MCX_INDEX" => "6",
        _ => return None,
    })
}

/// Depth socket exchange type (`data.py:156-166`; no MCX_INDEX).
pub fn depth_exchange_type(exchange: &str) -> Option<u8> {
    match exchange {
        "NSE" | "NFO" | "BSE" | "BFO" | "CDS" | "MCX" | "NSE_INDEX" | "BSE_INDEX" => {
            Some(exchange_type(exchange))
        }
        _ => None,
    }
}

/// Days per historical chunk (`data.py:588-597`, about 1000 candles).
pub fn chunk_days(interval: &str) -> Option<i64> {
    Some(match interval {
        "1m" => 2,
        "3m" => 8,
        "5m" => 13,
        "10m" => 26,
        "15m" => 40,
        "30m" => 76,
        "1h" => 166,
        "D" => 1000,
        _ => return None,
    })
}

fn broker_interval(interval: &str) -> Result<&'static str> {
    super::TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let supported: Vec<&str> = super::TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Timeframe '{}' is not supported by mStock. Supported timeframes are: {}",
                interval,
                supported.join(", ")
            ))
        })
}

/// One `fetched[]` row as an OpenAlgo quote.
pub fn quote_from_row(r: &Value, key: &QuoteKey) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: f(r, "ltp"),
        open: f(r, "open"),
        high: f(r, "high"),
        low: f(r, "low"),
        close: f(r, "close"),
        volume: f(r, "volume") as i64,
        ..Default::default()
    };
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = crate::brokers::common::streaming::round2(q.ltp - q.close);
        q.change_percent =
            crate::brokers::common::streaming::round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

fn fetched(v: &Value) -> Vec<Value> {
    match v.get("data").and_then(|d| d.get("fetched")) {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// `exchangeTokens` grouped by API exchange, in first-seen order.
fn quote_body(items: &[(String, String)]) -> Value {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (ex, tok) in items {
        match groups.iter_mut().find(|(e, _)| e == ex) {
            Some((_, v)) => v.push(tok.clone()),
            None => groups.push((ex.clone(), vec![tok.clone()])),
        }
    }
    let map: serde_json::Map<String, Value> =
        groups.into_iter().map(|(e, v)| (e, json!(v))).collect();
    json!({"mode": "OHLC", "exchangeTokens": map})
}

pub async fn get_quote(b: &MstockBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let row = b.lookup(key)?;
    let ex = api_exchange(&key.exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Quotes are not available from mStock for {}.",
            key.exchange
        ))
    })?;
    let body = quote_body(&[(ex.to_string(), row.token.clone())]);
    let v = b.call(Method::GET, QUOTE_PATH, auth, Some(&body)).await?;
    if !is_success(&v) {
        return Err(refusal(
            &v,
            "mStock did not return a quote. Try again shortly.",
        ));
    }
    let rows = fetched(&v);
    let r = rows
        .iter()
        .find(|r| s(r, "symbolToken") == row.token)
        .or_else(|| rows.first())
        .ok_or_else(|| {
            AppError::Broker(format!(
                "mStock returned no quote for {}. Try again shortly.",
                key.symbol
            ))
        })?;
    Ok(quote_from_row(r, key))
}

/// web `get_multiquotes`: one entry per key in request order; unresolved
/// keys and tokens the answer leaves out carry an error.
pub async fn get_multiquotes(
    b: &MstockBroker,
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
    // (index, api exchange, token) of every resolvable key.
    let mut wanted: Vec<(usize, String, String)> = Vec::new();
    for (idx, k) in keys.iter().enumerate() {
        let Some(row) = b.resolver().by_symbol(&k.exchange, &k.symbol) else {
            out[idx].error = Some("Could not resolve token".into());
            continue;
        };
        let Some(ex) = api_exchange(&k.exchange) else {
            out[idx].error = Some(format!("Exchange '{}' not supported", k.exchange));
            continue;
        };
        wanted.push((idx, ex.to_string(), row.token));
    }
    for batch in wanted.chunks(QUOTE_BATCH) {
        b.data_pacer.acquire().await;
        let items: Vec<(String, String)> = batch
            .iter()
            .map(|(_, e, t)| (e.clone(), t.clone()))
            .collect();
        let body = quote_body(&items);
        let answer = match b.call(Method::GET, QUOTE_PATH, auth, Some(&body)).await {
            Ok(v) if is_success(&v) => Ok(v),
            Ok(v) => Err(refusal(&v, "mStock did not return quotes.")),
            Err(e) => Err(e),
        };
        match answer {
            Ok(v) => {
                let by_token: HashMap<String, Value> = fetched(&v)
                    .into_iter()
                    .map(|r| (s(&r, "symbolToken"), r))
                    .collect();
                for (idx, _, token) in batch {
                    match by_token.get(token) {
                        Some(r) => out[*idx].data = Some(quote_from_row(r, &keys[*idx])),
                        None => out[*idx].error = Some("No data received".into()),
                    }
                }
            }
            Err(AppError::Auth(m)) => return Err(AppError::Auth(m)),
            Err(e) => {
                let msg = e.client_message();
                for (idx, _, _) in batch {
                    out[*idx].error = Some(msg.clone());
                }
            }
        }
    }
    Ok(out)
}

/// One-shot snap quote over the market-data socket (web `fetch_quote`):
/// connect, `LOGIN:<jwt>`, subscribe the token, return the first packet for
/// it (or the first packet at all). The whole exchange is bounded by
/// `timeout`; the socket is closed on every path (dropped on timeout).
pub async fn fetch_snap(
    url: &str,
    jwt: &str,
    exchange_type: u8,
    token: &str,
    mode: u8,
    timeout: Duration,
) -> Result<Option<Packet>> {
    let work = async {
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await?;
        let outcome: Result<Option<Packet>> = async {
            ws.send(Message::Text(format!("LOGIN:{}", jwt))).await?;
            let sub = streaming::sub_frames(&[(mode, exchange_type, token.to_string())], true);
            for m in sub {
                ws.send(m).await?;
            }
            while let Some(frame) = ws.next().await {
                match frame? {
                    Message::Binary(b) => {
                        let packets = parse_frame(&b);
                        if let Some(p) = packets
                            .iter()
                            .find(|p| p.token == token)
                            .or_else(|| packets.first())
                        {
                            return Ok(Some(p.clone()));
                        }
                    }
                    Message::Close(_) => return Ok(None),
                    _ => continue,
                }
            }
            Ok(None)
        }
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
        outcome
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(r) => r,
        Err(_) => Ok(None),
    }
}

/// A snap packet as OpenAlgo depth, five levels each side.
pub fn depth_from_packet(p: &Packet, key: &QuoteKey) -> MarketDepth {
    let mut bids = p.bids.clone();
    let mut asks = p.asks.clone();
    bids.resize(5, DepthLevel::default());
    asks.resize(5, DepthLevel::default());
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids,
        asks,
        ltp: p.ltp,
        ltq: i64::try_from(p.last_traded_qty).unwrap_or(0),
        open: p.open,
        high: p.high,
        low: p.low,
        prev_close: p.close,
        volume: i64::try_from(p.volume).unwrap_or(0),
        oi: i64::try_from(p.oi).unwrap_or(0),
        total_buy_qty: p.total_buy_qty as i64,
        total_sell_qty: p.total_sell_qty as i64,
    }
}

pub async fn get_market_depth(
    b: &MstockBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let row = b.lookup(key)?;
    let et = depth_exchange_type(&key.exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Market depth is not available from mStock for {}.",
            key.exchange
        ))
    })?;
    let session = MstockSession::parse(auth)?;
    let url = streaming::feed_url(&b.ws_url, &session.private_key, &session.jwt);
    let packet = match fetch_snap(&url, &session.jwt, et, &row.token, 3, b.depth_timeout).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("mStock depth socket failed: {}", e.code());
            None
        }
    };
    let p = packet.ok_or_else(|| {
        AppError::Broker(format!(
            "mStock did not send market depth for {}. Try again in a moment.",
            key.symbol
        ))
    })?;
    Ok(depth_from_packet(&p, key))
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Candle time -> UTC epoch seconds. Timestamps with an offset
/// (`2024-01-01T09:15:00+05:30`, `+0530`, or pandas' reading of `+05`) are
/// converted; naive ones (`2025-04-04 15:27`) are IST (`data.py:650-690,
/// 790-800`).
pub fn parse_candle_time(ts: &str) -> Option<i64> {
    let t = ts.trim();
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S%:z",
    ] {
        if let Ok(d) = chrono::DateTime::parse_from_str(t, fmt) {
            return Some(d.timestamp());
        }
    }
    // `+05` / `-03` (hours only).
    if t.len() > 3 {
        let (head, tail) = t.split_at(t.len() - 3);
        if (tail.starts_with('+') || tail.starts_with('-'))
            && tail[1..].chars().all(|c| c.is_ascii_digit())
        {
            let full = format!("{}{}:00", head, tail);
            if let Ok(d) = chrono::DateTime::parse_from_str(&full, "%Y-%m-%dT%H:%M:%S%:z")
                .or_else(|_| chrono::DateTime::parse_from_str(&full, "%Y-%m-%d %H:%M:%S%:z"))
            {
                return Some(d.timestamp());
            }
        }
    }
    let naive = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(t, f).ok())
    .or_else(|| {
        NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    })?;
    chrono_tz::Asia::Kolkata
        .from_local_datetime(&naive)
        .single()
        .map(|d| d.timestamp())
}

/// `data.candles` rows `[ts, open, high, low, close, volume]`; daily candles
/// are normalised to midnight of their UTC date (web `dt.normalize()` after
/// the UTC conversion).
pub fn parse_candles(v: &Value, daily: bool) -> Vec<Candle> {
    let Some(Value::Array(list)) = v.get("data").and_then(|d| d.get("candles")) else {
        return Vec::new();
    };
    let num = |x: Option<&Value>| match x {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    };
    list.iter()
        .filter_map(|c| {
            let a = c.as_array()?;
            let ts = match a.first()? {
                Value::String(s) => parse_candle_time(s)?,
                Value::Number(n) => n.as_i64()?,
                _ => return None,
            };
            let ts = if daily {
                ts - ts.rem_euclid(86_400)
            } else {
                ts
            };
            Some(Candle {
                timestamp: ts,
                open: num(a.get(1)),
                high: num(a.get(2)),
                low: num(a.get(3)),
                close: num(a.get(4)),
                volume: num(a.get(5)) as i64,
                oi: 0,
            })
        })
        .collect()
}

/// web `_get_historical_data`: chunked, paced, a failed chunk skipped (an
/// expired session stops the run).
async fn historical(
    b: &MstockBroker,
    auth: &AuthToken,
    token: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Candle>> {
    let ex = api_exchange(exchange).unwrap_or(exchange).to_string();
    let br_interval = broker_interval(interval)?;
    let days = chunk_days(interval).unwrap_or(1);
    let mut out = Vec::new();
    for (from, to) in chunks(start, end, days) {
        b.data_pacer.acquire().await;
        // Each chunk spans whole days: 00:00 to 23:59 (the web leaves
        // intermediate chunks ending at 00:00, which drops their last day).
        let body = json!({
            "exchange": ex,
            "symboltoken": token,
            "interval": br_interval,
            "fromdate": format!("{} 00:00", from.format("%Y-%m-%d")),
            "todate": format!("{} 23:59", to.format("%Y-%m-%d")),
        });
        match b
            .call(Method::GET, HISTORICAL_PATH, auth, Some(&body))
            .await
        {
            Ok(v) if is_success(&v) => out.extend(parse_candles(&v, interval == "D")),
            Ok(v) => tracing::info!(
                "mStock history chunk {} to {} refused: {}",
                from,
                to,
                message(&v)
            ),
            Err(AppError::Auth(m)) => return Err(AppError::Auth(m)),
            Err(e) => tracing::warn!(
                "mStock history chunk {} to {} failed: {}",
                from,
                to,
                e.code()
            ),
        }
    }
    Ok(sort_dedupe(out))
}

/// web `_get_intraday_data`: today's candles, IST.
async fn intraday(
    b: &MstockBroker,
    auth: &AuthToken,
    token: &str,
    exchange: &str,
    interval: &str,
) -> Result<Vec<Candle>> {
    let code = intraday_exchange(exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Today's candles are not available from mStock for {}.",
            exchange
        ))
    })?;
    let body = json!({
        "exchange": code,
        "symboltoken": token,
        "interval": broker_interval(interval)?,
    });
    b.data_pacer.acquire().await;
    let v = b
        .call(Method::POST, INTRADAY_PATH, auth, Some(&body))
        .await?;
    if !is_success(&v) {
        return Err(refusal(&v, "mStock did not return today's candles."));
    }
    Ok(sort_dedupe(parse_candles(&v, interval == "D")))
}

/// web `get_history`: today only -> intraday; a range ending today ->
/// history to yesterday plus intraday (an intraday failure keeps the
/// history); otherwise history.
pub async fn get_history(
    b: &MstockBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    broker_interval(&req.interval)?;
    let row = b.lookup(&req.key)?;
    let token = row.token.trim().to_string();
    if token.is_empty() || token == "None" {
        return Err(AppError::Validation(format!(
            "Symbol {} on {} has no mStock token. Download the master contract again.",
            req.key.symbol, req.key.exchange
        )));
    }
    let ex = req.key.exchange.as_str();
    let today = b.today_ist();
    if req.start == today && req.end == today {
        return intraday(b, auth, &token, ex, &req.interval).await;
    }
    if req.end == today && req.start < today {
        let yesterday = today.pred_opt().unwrap_or(today);
        let mut candles =
            historical(b, auth, &token, ex, &req.interval, req.start, yesterday).await?;
        match intraday(b, auth, &token, ex, &req.interval).await {
            Ok(c) => candles.extend(c),
            Err(AppError::Auth(m)) => return Err(AppError::Auth(m)),
            Err(e) => tracing::warn!("mStock intraday candles failed: {}", e.code()),
        }
        return Ok(sort_dedupe(candles));
    }
    historical(b, auth, &token, ex, &req.interval, req.start, req.end).await
}
