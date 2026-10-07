//! Quotes, multiquotes, depth and history (web `api/data.py`).
//!
//! HDFC Sky's REST market data is only the LTP snapshot (`PUT
//! /oapi/v1/fetch-ltp`, at most 10 instruments per request) and the chart
//! candles. Quotes add the session OHLCV from the latest DAY candle; depth,
//! OI, last-traded quantity and total buy/sell quantity exist only on the
//! feed, so the calls that need them open a short-lived feed connection,
//! subscribe `ALL`, keep the most complete packet per token and close it.

use super::mapping::{self, is_index_exchange, s, series_type, to_ltp_exchange, to_rest_exchange};
use super::streaming::{decode_frame, feed_request, subscribe_messages, RawTick};
use super::{broker_error, message_of, HdfcSkyBroker, TIMEFRAME_MAP};
use crate::brokers::common::history::{chunks, sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// fetch-ltp refuses more than 10 instruments ("maximum 10 items allowed").
pub const LTP_BATCH: usize = 10;
/// 429 attempts per request (0.5 s, 1 s, 2 s between them).
pub const MAX_ATTEMPTS: u32 = 4;
/// Chunk-level attempts so a transient failure never leaves a silent gap.
pub const CHUNK_ATTEMPTS: u32 = 4;
/// Feed snapshot budgets.
pub const SNAPSHOT_CONNECT: Duration = Duration::from_secs(8);
pub const SNAPSHOT_COLLECT: Duration = Duration::from_secs(3);
const SNAPSHOT_CLOSE: Duration = Duration::from_secs(2);

/// `(ltp_exchange, token)` -> `(ltp, prev_close)`.
pub type LtpMap = HashMap<(String, String), (f64, f64)>;

fn lookup(b: &HdfcSkyBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// The answer rows of fetch-ltp.
pub fn parse_ltp(v: &Value) -> LtpMap {
    v.get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    (
                        (s(r, "exchange").to_ascii_uppercase(), s(r, "token")),
                        (mapping::f(r, "ltp"), mapping::f(r, "prev_close")),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `PUT /oapi/v1/fetch-ltp` for at most `LTP_BATCH` `(exchange, token)`
/// pairs. A rate-limited batch is retried; any other failure yields an empty
/// map so callers report per-instrument gaps (web never raises here).
pub(crate) async fn fetch_ltp(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    instruments: &[(String, String)],
) -> Result<LtpMap> {
    if instruments.is_empty() {
        return Ok(HashMap::new());
    }
    let body = json!({
        "data": instruments
            .iter()
            .map(|(e, t)| json!({"exchange": e, "token": t}))
            .collect::<Vec<_>>()
    });
    let mut attempt = 0;
    loop {
        let (status, v) = b
            .send(
                Method::PUT,
                "/oapi/v1/fetch-ltp",
                auth,
                &[],
                false,
                Some(&body),
            )
            .await?;
        if status == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < MAX_ATTEMPTS {
            tokio::time::sleep(b.delays.backoff * 2u32.pow(attempt)).await;
            attempt += 1;
            continue;
        }
        if status != StatusCode::OK {
            tracing::warn!(
                status = status.as_u16(),
                "HDFC Sky LTP request failed for {} instruments: {}",
                instruments.len(),
                message_of(&v)
            );
            return Ok(HashMap::new());
        }
        return Ok(parse_ltp(&v));
    }
}

/// `(ltp, prev_close)` of one master row (zeros when the broker omits it).
pub(crate) async fn ltp_of_row(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    row: &SymToken,
) -> Result<(f64, f64)> {
    let key = (
        to_ltp_exchange(&row.exchange).to_string(),
        row.token.clone(),
    );
    let m = fetch_ltp(b, auth, std::slice::from_ref(&key)).await?;
    Ok(m.get(&key).copied().unwrap_or((0.0, 0.0)))
}

// ---------------------------------------------------------------------------
// Candles
// ---------------------------------------------------------------------------

/// Chart symbol candidates (web `_chart_symbols`): cash uses the
/// series-free OpenAlgo symbol, derivatives the broker symbol, indices
/// either the OpenAlgo symbol or the uppercased broker name.
pub fn chart_symbols(row: &SymToken) -> Vec<String> {
    if is_index_exchange(&row.exchange) {
        let broker_form = row.brsymbol.to_uppercase();
        let mut v = vec![row.symbol.clone()];
        if !broker_form.is_empty() && broker_form != row.symbol {
            v.push(broker_form);
        }
        v
    } else if matches!(row.exchange.as_str(), "NSE" | "BSE") {
        vec![row.symbol.clone()]
    } else {
        vec![row.brsymbol.clone()]
    }
}

/// `GET /oapi/charts-api/charts/v1/fetch-candle` rows for one range.
pub(crate) async fn fetch_candles(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    row: &SymToken,
    chart_type: &str,
    start: NaiveDate,
    end: NaiveDate,
    symbol: &str,
) -> Result<Vec<Vec<Value>>> {
    let query = [
        ("symbol", symbol.to_string()),
        ("exchange", to_rest_exchange(&row.exchange).to_string()),
        ("chartType", chart_type.to_string()),
        ("seriesType", series_type(row)),
        ("start", start.format("%Y-%m-%d").to_string()),
        ("end", end.format("%Y-%m-%d").to_string()),
    ];
    let mut attempt = 0;
    let (status, v) = loop {
        let (status, v) = b
            .send(
                Method::GET,
                "/oapi/charts-api/charts/v1/fetch-candle",
                auth,
                &query,
                false,
                None,
            )
            .await?;
        if status == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < MAX_ATTEMPTS {
            tracing::warn!("HDFC Sky chart data rate-limited, retrying");
            tokio::time::sleep(b.delays.backoff * 2u32.pow(attempt)).await;
            attempt += 1;
            continue;
        }
        break (status, v);
    };
    if status != StatusCode::OK {
        let detail = v
            .get("meta")
            .and_then(|m| m.get("displayMessage"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| message_of(&v));
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Sky chart data failed for {}:{}: {}",
            row.exchange,
            row.symbol,
            detail
        );
        return Err(if status == StatusCode::TOO_MANY_REQUESTS {
            AppError::Broker(
                "HDFC Sky is limiting chart requests right now. Wait a moment and try again."
                    .into(),
            )
        } else {
            broker_error(&detail)
        });
    }
    let meta = v.get("meta").cloned().unwrap_or(Value::Null);
    let err = s(&meta, "err_code").to_ascii_lowercase();
    if !err.is_empty() && !matches!(err.as_str(), "success" | "ok" | "0") {
        let msg = s(&meta, "displayMessage");
        return Err(broker_error(&msg));
    }
    Ok(v.get("data")
        .and_then(|d| d.get("results"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(|r| r.as_array().cloned()).collect())
        .unwrap_or_default())
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Day-first IST candle time (`22-10-2024` or `22-10-2024 09:15[:00]`).
fn parse_ist(text: &str) -> Option<NaiveDateTime> {
    let t = text.trim();
    for fmt in ["%d-%m-%Y %H:%M:%S", "%d-%m-%Y %H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(d);
        }
    }
    NaiveDate::parse_from_str(t, "%d-%m-%Y")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// Chart rows `[open, high, low, close, volume, oi, time, cum_vol]` ->
/// candles. Daily rows land on IST midnight (UTC midnight + 5:30); intraday
/// rows are the true epoch of the IST time. Malformed rows are dropped.
pub fn parse_candles(rows: &[Vec<Value>], daily: bool) -> Vec<Candle> {
    let mut out: Vec<Candle> = rows
        .iter()
        .filter(|r| r.len() >= 7)
        .filter_map(|r| {
            let dt = parse_ist(r[6].as_str()?)?;
            let timestamp = if daily {
                dt.date().and_hms_opt(0, 0, 0)?.and_utc().timestamp() + IST_OFFSET_SECS
            } else {
                dt.and_utc().timestamp() - IST_OFFSET_SECS
            };
            Some(Candle {
                timestamp,
                open: num(r.first())?,
                high: num(r.get(1))?,
                low: num(r.get(2))?,
                close: num(r.get(3))?,
                volume: num(r.get(4)).unwrap_or(0.0) as i64,
                oi: num(r.get(5)).unwrap_or(0.0) as i64,
            })
        })
        .collect();
    out = sort_dedupe(out);
    out
}

/// How an interval is built from the two native chart resolutions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resample {
    None,
    /// Fixed bins of this many seconds, aligned to the epoch.
    Secs(i64),
    /// pandas `W-MON`, labelled on the Monday.
    WeekMon,
    /// pandas `MS`, labelled on the first of the month.
    MonthStart,
}

/// web `_interval_spec`: `(chartType, resample)`.
pub fn interval_spec(interval: &str) -> Option<(&'static str, Resample)> {
    Some(match interval {
        "1m" => ("MINUTE", Resample::None),
        "3m" => ("MINUTE", Resample::Secs(180)),
        "5m" => ("MINUTE", Resample::Secs(300)),
        "10m" => ("MINUTE", Resample::Secs(600)),
        "15m" => ("MINUTE", Resample::Secs(900)),
        "30m" => ("MINUTE", Resample::Secs(1800)),
        "1h" => ("MINUTE", Resample::Secs(3600)),
        "D" => ("DAY", Resample::None),
        "W" => ("DAY", Resample::WeekMon),
        "M" => ("DAY", Resample::MonthStart),
        _ => return None,
    })
}

fn bucket(ts: i64, rule: Resample) -> i64 {
    match rule {
        Resample::None => ts,
        Resample::Secs(n) => ts - ts.rem_euclid(n),
        Resample::WeekMon | Resample::MonthStart => {
            let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) else {
                return ts;
            };
            let d = dt.date_naive();
            let start = if rule == Resample::WeekMon {
                d - chrono::Duration::days(i64::from(d.weekday().num_days_from_monday()))
            } else {
                d.with_day(1).unwrap_or(d)
            };
            start
                .and_hms_opt(0, 0, 0)
                .map(|x| x.and_utc().timestamp())
                .unwrap_or(ts)
        }
    }
}

/// pandas `resample(rule, label="left", closed="left")` with first / max /
/// min / last / sum / last aggregation; empty bins are dropped. Input must
/// be sorted.
pub fn resample(candles: &[Candle], rule: Resample) -> Vec<Candle> {
    if rule == Resample::None {
        return candles.to_vec();
    }
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let b = bucket(c.timestamp, rule);
        match out.last_mut() {
            Some(last) if last.timestamp == b => {
                last.high = last.high.max(c.high);
                last.low = last.low.min(c.low);
                last.close = c.close;
                last.volume += c.volume;
                last.oi = c.oi;
            }
            _ => out.push(Candle { timestamp: b, ..*c }),
        }
    }
    out
}

fn ist_today() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::seconds(IST_OFFSET_SECS)).date_naive()
}

/// Session open/high/low/volume from the latest DAY candle of the last
/// week; zeros when unavailable so a quote never fails on this leg.
async fn session_ohlcv(b: &HdfcSkyBroker, auth: &AuthToken, row: &SymToken) -> Candle {
    let today = ist_today();
    let start = today - chrono::Duration::days(7);
    let sym = b
        .cached_chart_symbol(&row.exchange, &row.symbol)
        .unwrap_or_else(|| chart_symbols(row).remove(0));
    match fetch_candles(b, auth, row, "DAY", start, today, &sym).await {
        Ok(rows) => parse_candles(&rows, true).pop().unwrap_or_default(),
        Err(e) => {
            tracing::debug!(
                "Session OHLC unavailable for {}:{}: {}",
                row.exchange,
                row.symbol,
                e.code()
            );
            Candle::default()
        }
    }
}

fn with_change(mut q: Quote) -> Quote {
    if q.close > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

pub async fn get_quote(b: &HdfcSkyBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let row = lookup(b, key)?;
    let (ltp, prev_close) = ltp_of_row(b, auth, &row).await?;
    let c = session_ohlcv(b, auth, &row).await;
    Ok(with_change(Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: c.open,
        high: c.high,
        low: c.low,
        close: prev_close,
        volume: c.volume,
        ..Default::default()
    }))
}

pub async fn get_multiquotes(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let rows: Vec<Option<SymToken>> = keys
        .iter()
        .map(|k| b.resolver().by_symbol(&k.exchange, &k.symbol))
        .collect();
    let mut wanted: Vec<(String, String)> = rows
        .iter()
        .flatten()
        .map(|r| (to_ltp_exchange(&r.exchange).to_string(), r.token.clone()))
        .collect();
    wanted.sort();
    wanted.dedup();
    let mut ltp = HashMap::new();
    for (n, batch) in wanted.chunks(LTP_BATCH).enumerate() {
        if n > 0 {
            tokio::time::sleep(b.delays.pace).await;
        }
        ltp.extend(fetch_ltp(b, auth, batch).await?);
    }
    // Open interest exists only on the feed: one snapshot for the
    // derivative legs.
    let oi_targets: Vec<(String, String)> = rows
        .iter()
        .flatten()
        .filter(|r| mapping::has_oi(&r.exchange))
        .map(|r| (r.exchange.clone(), r.token.clone()))
        .collect();
    let oi: HashMap<String, i64> = if oi_targets.is_empty() {
        HashMap::new()
    } else {
        feed_snapshot(b, auth, &oi_targets, |t| t.oi != 0)
            .await
            .into_iter()
            .filter(|(_, t)| t.oi != 0)
            .map(|(k, t)| (k, t.oi))
            .collect()
    };
    Ok(keys
        .iter()
        .zip(rows)
        .map(|(k, row)| {
            let Some(row) = row else {
                return QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some(format!(
                        "Could not find instrument for {}:{}",
                        k.exchange, k.symbol
                    )),
                };
            };
            let lk = (
                to_ltp_exchange(&row.exchange).to_string(),
                row.token.clone(),
            );
            match ltp.get(&lk) {
                None => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("No quote data available".into()),
                },
                Some((l, pc)) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: Some(with_change(Quote {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        ltp: *l,
                        close: *pc,
                        oi: oi.get(&row.token).copied().unwrap_or(0),
                        ..Default::default()
                    })),
                    error: None,
                },
            }
        })
        .collect())
}

fn pad5(levels: &[DepthLevel]) -> Vec<DepthLevel> {
    (0..5)
        .map(|i| levels.get(i).copied().unwrap_or_default())
        .collect()
}

pub async fn get_market_depth(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let row = lookup(b, key)?;
    let q = get_quote(b, auth, key).await?;
    let need_oi = mapping::has_oi(&row.exchange);
    let snap = feed_snapshot(b, auth, &[(row.exchange.clone(), row.token.clone())], |t| {
        t.has_depth() && (!need_oi || t.oi != 0)
    })
    .await;
    let t = snap.get(&row.token).cloned().unwrap_or_default();
    Ok(MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad5(&t.buy),
        asks: pad5(&t.sell),
        ltp: q.ltp,
        ltq: t.ltq,
        open: q.open,
        high: q.high,
        low: q.low,
        prev_close: q.close,
        volume: q.volume,
        oi: t.oi,
        total_buy_qty: t.total_buy_quantity,
        total_sell_qty: t.total_sell_quantity,
    })
}

/// One packet per instrument from a short-lived feed connection: subscribe
/// `ALL`, merge packets per token until `complete` holds for every token or
/// the collection window ends, then close. Never fails: whatever arrived
/// (possibly nothing) is returned, keyed by token.
pub(crate) async fn feed_snapshot(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    instruments: &[(String, String)],
    complete: impl Fn(&RawTick) -> bool,
) -> HashMap<String, RawTick> {
    let mut snap: HashMap<String, RawTick> = HashMap::new();
    let Ok(sess) = HdfcSkyBroker::session(auth) else {
        return snap;
    };
    let Ok(req) = feed_request(&b.urls.ws, &sess.api_key, &sess.token) else {
        return snap;
    };
    let wanted: HashSet<String> = instruments.iter().map(|(_, t)| t.clone()).collect();
    let mut ws =
        match tokio::time::timeout(SNAPSHOT_CONNECT, tokio_tungstenite::connect_async(req)).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                // The handshake error can echo the URL; log only its kind.
                tracing::warn!(
                    "HDFC Sky feed snapshot could not connect ({})",
                    match e {
                        tokio_tungstenite::tungstenite::Error::Http(r) => r.status().to_string(),
                        _ => "network".into(),
                    }
                );
                return snap;
            }
            Err(_) => {
                tracing::warn!("HDFC Sky feed snapshot timed out connecting");
                return snap;
            }
        };
    let scrips: Vec<(String, &'static str)> = instruments
        .iter()
        .map(|(e, t)| (mapping::ws_scrip_id(e, t), "ALL"))
        .collect();
    let mut sent = true;
    for frame in subscribe_messages(&scrips) {
        if ws.send(Message::Text(frame)).await.is_err() {
            sent = false;
            break;
        }
    }
    if sent {
        let deadline = tokio::time::Instant::now() + SNAPSHOT_COLLECT;
        loop {
            if wanted
                .iter()
                .all(|t| snap.get(t).map(&complete).unwrap_or(false))
            {
                break;
            }
            let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
                Ok(Some(Ok(m))) => m,
                _ => break,
            };
            if let Message::Binary(bytes) = msg {
                for t in decode_frame(&bytes) {
                    let tok = t.token.to_string();
                    if wanted.contains(&tok) {
                        snap.entry(tok).or_default().merge(&t);
                    }
                }
            }
        }
    }
    let _ = tokio::time::timeout(SNAPSHOT_CLOSE, ws.close(None)).await;
    if snap.len() < wanted.len() {
        tracing::info!(
            "HDFC Sky feed snapshot: data for {}/{} instruments",
            snap.len(),
            wanted.len()
        );
    }
    snap
}

/// HDFC Sky interval for an OpenAlgo key.
fn spec_for(interval: &str) -> Result<(&'static str, Resample)> {
    interval_spec(interval).ok_or_else(|| {
        let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
        AppError::Validation(format!(
            "Interval {} is not supported by HDFC Sky. Use one of: {}.",
            interval,
            list.join(", ")
        ))
    })
}

pub async fn get_history(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let (chart_type, rule) = spec_for(&req.interval)?;
    let row = lookup(b, &req.key)?;
    let daily = chart_type == "DAY";
    // Measured caps: DAY 2000 days, MINUTE 31 days per request.
    let max_days = if daily { 2000 } else { 31 };
    let mut chart = b.cached_chart_symbol(&row.exchange, &row.symbol);
    let ranges = chunks(req.start, req.end, max_days);
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut failed = 0usize;
    let mut last_err: Option<AppError> = None;
    for (n, (from, to)) in ranges.iter().enumerate() {
        if n > 0 {
            tokio::time::sleep(b.delays.pace).await;
        }
        let candidates = match &chart {
            Some(c) => vec![c.clone()],
            None => chart_symbols(&row),
        };
        let mut chunk: Option<Vec<Vec<Value>>> = None;
        for attempt in 0..CHUNK_ATTEMPTS {
            let mut outcome: Result<Vec<Vec<Value>>> = Ok(Vec::new());
            for (ci, cand) in candidates.iter().enumerate() {
                if ci > 0 {
                    tokio::time::sleep(b.delays.pace).await;
                }
                match fetch_candles(b, auth, &row, chart_type, *from, *to, cand).await {
                    Ok(r) if !r.is_empty() => {
                        if chart.as_deref() != Some(cand.as_str()) {
                            b.remember_chart_symbol(&row.exchange, &row.symbol, cand);
                            chart = Some(cand.clone());
                        }
                        outcome = Ok(r);
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        outcome = Err(e);
                        break;
                    }
                }
            }
            match outcome {
                Ok(r) => {
                    chunk = Some(r);
                    break;
                }
                Err(AppError::Auth(m)) => return Err(AppError::Auth(m)),
                Err(e) => {
                    tracing::warn!(
                        "History chunk {}..{} for {}:{} failed (attempt {}): {}",
                        from,
                        to,
                        req.key.exchange,
                        req.key.symbol,
                        attempt + 1,
                        e.code()
                    );
                    last_err = Some(e);
                    if attempt + 1 < CHUNK_ATTEMPTS {
                        tokio::time::sleep(b.delays.backoff * 2u32.pow(attempt)).await;
                    }
                }
            }
        }
        match chunk {
            Some(r) => rows.extend(r),
            None => {
                failed += 1;
                tracing::warn!(
                    "Possible gap: history chunk {}..{} for {}:{} could not be fetched",
                    from,
                    to,
                    req.key.exchange,
                    req.key.symbol
                );
            }
        }
    }
    if rows.is_empty() && !ranges.is_empty() && failed == ranges.len() {
        return Err(last_err.unwrap_or_else(|| {
            AppError::Broker("HDFC Sky returned no history for this request.".into())
        }));
    }
    let candles = parse_candles(&rows, daily);
    Ok(resample(&candles, rule))
}
