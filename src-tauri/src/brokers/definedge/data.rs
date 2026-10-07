//! Market data (web `api/data.py`): quotes, multiquotes, depth, security
//! info, open-interest backfill and history.
//!
//! * quotes `GET <trade>/quotes/{exchange}/{token}` (indices on their cash
//!   exchange). The quote has no previous close (the web reports
//!   `day_open` as `prev_close`) and no open interest: for derivatives OI
//!   is backfilled from the last 1-minute candle, cached 60 s per token.
//! * history `GET <data>/history/{segment}/{token}/{minute|day}/{from}/{to}`
//!   (`ddMMyyyyHHmm`), headerless CSV `datetime,open,high,low,close,volume[,oi]`.
//!   Intervals above 1m are resampled from 1-minute candles, bins aligned to
//!   the segment's session open; the API ignores `to`, so results are
//!   clipped client side.

use super::{int, num, text, DefinedgeBroker, DefinedgeSession, OI_CACHE_MAX, OI_CACHE_TTL};
use crate::brokers::common::history::IST_OFFSET_SECS;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Duration as CDuration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;

/// web `BrokerData.timeframe_map` (Definedge serves only minute and day).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "minute"),
    ("5m", "minute"),
    ("15m", "minute"),
    ("30m", "minute"),
    ("1h", "minute"),
    ("D", "day"),
];

/// Quotes fetched concurrently per batch (web `BATCH_SIZE`).
pub const MULTIQUOTE_BATCH: usize = 20;

const DERIVATIVE_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "BCD"];

/// Index exchanges are quoted on their cash exchange.
pub fn api_exchange(exchange: &str) -> &str {
    match exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => other,
    }
}

fn instrument(b: &DefinedgeBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .filter(|r| !r.token.is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
}

/// One `/quotes` body -> OpenAlgo quote (web `BrokerData.get_quotes`).
pub fn quote_from(v: &Value, key: &QuoteKey, oi: i64) -> Quote {
    let ltp = num(v, "ltp");
    let prev_close = if v.get("day_open").is_some() {
        num(v, "day_open")
    } else {
        ltp
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: num(v, "day_open"),
        high: num(v, "day_high"),
        low: num(v, "day_low"),
        close: prev_close,
        volume: int(v, "volume"),
        bid: num(v, "best_bid_price1"),
        ask: num(v, "best_ask_price1"),
        bid_qty: int(v, "best_bid_qty1"),
        ask_qty: int(v, "best_ask_qty1"),
        oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    }
}

/// One `/quotes` body -> five-level depth (web `BrokerData.get_depth`).
pub fn depth_from(v: &Value, key: &QuoteKey, oi: i64) -> MarketDepth {
    let level = |p: &str, q: &str, n: usize| DepthLevel {
        price: num(v, &format!("{}{}", p, n)),
        quantity: int(v, &format!("{}{}", q, n)),
        orders: 0,
    };
    let bids: Vec<DepthLevel> = (1..=5)
        .map(|n| level("best_bid_price", "best_bid_qty", n))
        .collect();
    let asks: Vec<DepthLevel> = (1..=5)
        .map(|n| level("best_ask_price", "best_ask_qty", n))
        .collect();
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: bids.iter().map(|l| l.quantity).sum(),
        total_sell_qty: asks.iter().map(|l| l.quantity).sum(),
        bids,
        asks,
        ltp: num(v, "ltp"),
        ltq: int(v, "last_traded_qty"),
        open: num(v, "day_open"),
        high: num(v, "day_high"),
        low: num(v, "day_low"),
        prev_close: num(v, "day_open"),
        volume: int(v, "volume"),
        oi,
    }
}

/// Raw `/quotes/{exchange}/{token}` body; anything but `status: SUCCESS`
/// is an error.
async fn fetch_quote(
    b: &DefinedgeBroker,
    s: &DefinedgeSession,
    exchange: &str,
    token: &str,
) -> Result<Value> {
    let path = format!(
        "/quotes/{}/{}",
        api_exchange(exchange),
        urlencoding::encode(token)
    );
    let v = b.trade_json(s, Method::GET, &path, None).await?;
    if text(&v, "status") != "SUCCESS" {
        return Err(super::broker_error(
            &v,
            "Definedge did not return a quote for this instrument right now.",
        ));
    }
    Ok(v)
}

fn now_ist() -> NaiveDateTime {
    (chrono::Utc::now() + CDuration::seconds(IST_OFFSET_SECS)).naive_utc()
}

/// Open interest of a derivative from its last 1-minute candle (web
/// `fetch_latest_oi`), cached `OI_CACHE_TTL` in a cache bounded by
/// `OI_CACHE_MAX`. 0 for non-derivatives and on any failure.
pub(crate) async fn latest_oi(
    b: &DefinedgeBroker,
    s: &DefinedgeSession,
    exchange: &str,
    token: &str,
) -> i64 {
    if !DERIVATIVE_EXCHANGES.contains(&exchange) {
        return 0;
    }
    let key = (exchange.to_string(), token.to_string());
    if let Some((oi, at)) = b.oi_cache.lock().get(&key) {
        if at.elapsed() < OI_CACHE_TTL {
            return *oi;
        }
    }
    let now = now_ist();
    let from = format!("{}0915", (now - CDuration::days(4)).format("%d%m%Y"));
    let to = now.format("%d%m%Y%H%M").to_string();
    let url = format!(
        "{}/history/{}/{}/minute/{}/{}",
        b.urls.data,
        exchange,
        urlencoding::encode(token),
        from,
        to
    );
    let oi = match b
        .send(Method::GET, &url, &s.api_session_key, None, false)
        .await
    {
        Ok((st, body)) if st.is_success() => body
            .trim()
            .rsplit('\n')
            .next()
            .map(|row| row.split(',').collect::<Vec<_>>())
            .filter(|cols| cols.len() >= 7)
            .and_then(|cols| cols[6].trim().parse::<f64>().ok())
            .map(|x| x as i64)
            .unwrap_or(0),
        _ => 0,
    };
    let mut cache = b.oi_cache.lock();
    if cache.len() >= OI_CACHE_MAX {
        cache.retain(|_, (_, at)| at.elapsed() < OI_CACHE_TTL);
        if cache.len() >= OI_CACHE_MAX {
            cache.clear();
        }
    }
    cache.insert(key, (oi, Instant::now()));
    oi
}

pub async fn get_quote(b: &DefinedgeBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = DefinedgeSession::parse(auth)?;
    let row = instrument(b, key)?;
    let v = fetch_quote(b, &s, &key.exchange, &row.token).await?;
    let oi = latest_oi(b, &s, &key.exchange, &row.token).await;
    Ok(quote_from(&v, key, oi))
}

pub async fn get_market_depth(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let s = DefinedgeSession::parse(auth)?;
    let row = instrument(b, key)?;
    let v = fetch_quote(b, &s, &key.exchange, &row.token).await?;
    let oi = latest_oi(b, &s, &key.exchange, &row.token).await;
    Ok(depth_from(&v, key, oi))
}

/// Batches of 20 quotes fetched concurrently (each paced by the shared
/// per-host clock), then OI backfilled for derivatives; one entry per key
/// in request order.
pub async fn get_multiquotes(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let s = DefinedgeSession::parse(auth)?;
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(MULTIQUOTE_BATCH) {
        let futs = batch.iter().map(|k| {
            let s = &s;
            async move {
                let row = match instrument(b, k) {
                    Ok(r) => r,
                    Err(_) => {
                        return QuoteResult {
                            symbol: k.symbol.clone(),
                            exchange: k.exchange.clone(),
                            data: None,
                            error: Some("Could not resolve token".into()),
                        }
                    }
                };
                match fetch_quote(b, s, &k.exchange, &row.token).await {
                    Ok(v) => {
                        let oi = latest_oi(b, s, &k.exchange, &row.token).await;
                        QuoteResult {
                            symbol: k.symbol.clone(),
                            exchange: k.exchange.clone(),
                            data: Some(quote_from(&v, k, oi)),
                            error: None,
                        }
                    }
                    Err(e) => QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: None,
                        error: Some(e.client_message()),
                    },
                }
            }
        });
        out.extend(futures_util::future::join_all(futs).await);
    }
    Ok(out)
}

/// web `get_security_info`: `GET /securityinfo/{exchange}/{token}`.
pub async fn security_info(b: &DefinedgeBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
    let s = DefinedgeSession::parse(auth)?;
    let row = instrument(b, key)?;
    let path = format!(
        "/securityinfo/{}/{}",
        key.exchange,
        urlencoding::encode(&row.token)
    );
    b.trade_json(&s, Method::GET, &path, None).await
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// One parsed CSV candle, timestamp naive IST.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub at: NaiveDateTime,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: i64,
}

/// Days per request (web `interval_limits`).
pub fn chunk_days(interval: &str) -> i64 {
    match interval {
        "1m" => 30,
        "5m" => 90,
        "15m" => 150,
        "30m" | "1h" => 180,
        "D" => 365,
        _ => 30,
    }
}

/// Minutes of a resampled interval.
pub fn interval_minutes(interval: &str) -> Option<i64> {
    match interval {
        "5m" => Some(5),
        "15m" => Some(15),
        "30m" => Some(30),
        "1h" => Some(60),
        _ => None,
    }
}

/// Minute past 09:00 the segment opens (web `_SESSION_OPEN_MINUTE`): MCX,
/// CDS, BCD and NCDEX open on the hour, everything else at 09:15.
pub fn session_open_minute(exchange: &str) -> i64 {
    match exchange.to_ascii_uppercase().as_str() {
        "MCX" | "MCX_INDEX" | "CDS" | "BCD" | "NCDEX" => 0,
        _ => 15,
    }
}

/// The requested window in naive IST (web `get_history` set-up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryWindow {
    pub from: NaiveDateTime,
    pub to: NaiveDateTime,
    pub daily: bool,
}

fn at(d: NaiveDate, h: u32, m: u32) -> NaiveDateTime {
    d.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN))
}

impl HistoryWindow {
    /// Intraday spans whole calendar days, stopping at the current minute
    /// when the range ends today; daily runs midnight to midnight.
    pub fn new(interval: &str, start: NaiveDate, end: NaiveDate, now_ist: NaiveDateTime) -> Self {
        if interval == "D" {
            return Self {
                from: at(start, 0, 0),
                to: at(end, 0, 0),
                daily: true,
            };
        }
        let to = if end == now_ist.date() {
            now_ist
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now_ist)
        } else {
            at(end, 23, 59)
        };
        Self {
            from: at(start, 0, 0),
            to,
            daily: false,
        }
    }
}

/// Request chunks (web chunk loop): intraday chunks end at 23:59 of their
/// last day so no trading day falls between two chunks.
pub fn history_chunks(w: &HistoryWindow, interval: &str) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    let days = chunk_days(interval);
    let mut out = Vec::new();
    let mut cur = w.from;
    while cur <= w.to {
        let mut end = (cur + CDuration::days(days - 1)).min(w.to);
        let next = if w.daily {
            end + CDuration::days(1)
        } else {
            end = at(end.date(), 23, 59).min(w.to);
            match end.date().succ_opt() {
                Some(d) => at(d, 0, 0),
                None => break,
            }
        };
        out.push((cur, end));
        cur = next;
    }
    out
}

/// Parse the headerless history CSV. The datetime column is left-padded to
/// 12 digits (a numeric read drops the leading zero of days 1-9) and parsed
/// as `%d%m%Y%H%M`; rows without OI get 0; unparsable rows are dropped.
pub fn parse_history_csv(body: &str) -> Vec<Bar> {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for line in body.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split(',').map(str::trim).collect();
        if c.len() < 6 {
            dropped += 1;
            continue;
        }
        let raw = c[0].split('.').next().unwrap_or(c[0]);
        let padded = format!("{:0>12}", raw);
        let Ok(at) = NaiveDateTime::parse_from_str(&padded, "%d%m%Y%H%M") else {
            dropped += 1;
            continue;
        };
        let f = |s: &str| s.parse::<f64>().unwrap_or(0.0);
        out.push(Bar {
            at,
            open: f(c[1]),
            high: f(c[2]),
            low: f(c[3]),
            close: f(c[4]),
            volume: f(c[5]) as i64,
            oi: c.get(6).map(|s| f(s) as i64).unwrap_or(0),
        });
    }
    if dropped > 0 {
        tracing::warn!(
            broker = "definedge",
            "History: dropped {} rows with unreadable timestamps",
            dropped
        );
    }
    out
}

/// pandas `resample("<N>min", offset="<M>min")` with the default
/// `origin="start_day"`, left-closed and left-labelled: open first, high
/// max, low min, close last, volume sum, OI last; empty bins dropped.
pub fn resample(mut bars: Vec<Bar>, minutes: i64, offset_minutes: i64) -> Vec<Bar> {
    if bars.is_empty() || minutes <= 1 {
        return bars;
    }
    bars.sort_by_key(|b| b.at);
    let origin = at(bars[0].at.date(), 0, 0) + CDuration::minutes(offset_minutes);
    let mut out: Vec<Bar> = Vec::new();
    for b in bars {
        let delta = (b.at - origin).num_minutes();
        let label = origin + CDuration::minutes(delta.div_euclid(minutes) * minutes);
        match out.last_mut() {
            Some(cur) if cur.at == label => {
                cur.high = cur.high.max(b.high);
                cur.low = cur.low.min(b.low);
                cur.close = b.close;
                cur.volume += b.volume;
                cur.oi = b.oi;
            }
            _ => out.push(Bar { at: label, ..b }),
        }
    }
    out
}

/// Epoch seconds: daily candles at naive midnight read as UTC (the web's
/// `normalize()` then `astype(int64)`), intraday IST wall time -> UTC.
pub fn candle_epoch(at_ist: NaiveDateTime, daily: bool) -> i64 {
    if daily {
        at(at_ist.date(), 0, 0).and_utc().timestamp()
    } else {
        at_ist.and_utc().timestamp() - IST_OFFSET_SECS
    }
}

/// 408 and 425 are timing, not client errors (web `_TRANSIENT_4XX`).
fn terminal(status: reqwest::StatusCode) -> bool {
    status.is_client_error() && !matches!(status.as_u16(), 408 | 425)
}

pub async fn get_history(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let Some((_, timeframe)) = TIMEFRAME_MAP.iter().find(|(k, _)| *k == req.interval) else {
        tracing::warn!(
            broker = "definedge",
            "Interval {} is not offered by Definedge",
            req.interval
        );
        return Ok(Vec::new());
    };
    let s = DefinedgeSession::parse(auth)?;
    let row = instrument(b, &req.key)?;
    let w = HistoryWindow::new(&req.interval, req.start, req.end, now_ist());
    let segment = api_exchange(&req.key.exchange.to_ascii_uppercase()).to_string();
    let mut bars: Vec<Bar> = Vec::new();
    for (from, to) in history_chunks(&w, &req.interval) {
        let url = format!(
            "{}/history/{}/{}/{}/{}/{}",
            b.urls.data,
            segment,
            urlencoding::encode(&row.token),
            timeframe,
            from.format("%d%m%Y%H%M"),
            to.format("%d%m%Y%H%M")
        );
        let mut body = None;
        for attempt in 0..3u32 {
            match b
                .send(Method::GET, &url, &s.api_session_key, None, false)
                .await
            {
                Ok((st, txt)) if st.is_success() => {
                    body = Some(txt);
                    break;
                }
                Ok((st, _))
                    if st == reqwest::StatusCode::UNAUTHORIZED
                        || st == reqwest::StatusCode::FORBIDDEN =>
                {
                    return Err(super::session_expired());
                }
                Ok((st, _)) if terminal(st) => break,
                _ => {}
            }
            if attempt < 2 {
                tokio::time::sleep(b.chunk_retry_base.saturating_mul(attempt + 1)).await;
            }
        }
        let Some(txt) = body else {
            tracing::warn!(
                broker = "definedge",
                "History chunk {} to {} could not be fetched; those candles are missing",
                from,
                to
            );
            continue;
        };
        let mut chunk = parse_history_csv(&txt);
        if !w.daily {
            if let Some(n) = interval_minutes(&req.interval) {
                chunk = resample(chunk, n, session_open_minute(&req.key.exchange));
            }
        }
        bars.extend(chunk);
    }
    let mut candles: Vec<Candle> = bars
        .into_iter()
        .filter(|b| b.at >= w.from && b.at <= w.to)
        .map(|b| Candle {
            timestamp: candle_epoch(b.at, w.daily),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
            oi: b.oi,
        })
        .collect();
    candles = crate::brokers::common::history::sort_dedupe(candles);
    Ok(candles)
}
