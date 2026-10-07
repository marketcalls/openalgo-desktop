//! Quotes, multiquotes and depth (web `api/data.py`).
//!
//! `/fetch-ltp` is InvestRight's only REST market-data endpoint (LTP and
//! previous close, batches of 10). Everything else (OHLC, volume, OI, the
//! five-level book) comes from a short-lived feed snapshot: one bounded
//! connection, the instruments subscribed as `ALL`, packets collected until
//! every instrument is complete or the window closes, then the socket is
//! closed on every path. A slow feed degrades to zeros, never to an error,
//! as on the web.

use super::mapping::{s, ws_scrip_id};
use super::streaming::{decode_frame, feed_request, feed_url, pad5, subscribe_frames, Packet};
use super::HdfcSecuritiesBroker;
use crate::brokers::common::streaming::{round2, Message};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::{timeout, Instant};

/// Instruments per `/fetch-ltp` call (the gateway refuses more than 10).
pub const LTP_BATCH: usize = 10;
/// Pause between `/fetch-ltp` batches (web `_MULTIQUOTE_RATE_DELAY`).
pub const LTP_BATCH_DELAY: Duration = Duration::from_millis(150);
const LTP_ATTEMPTS: u32 = 4;
const LTP_BACKOFF: Duration = Duration::from_millis(500);
/// Feed snapshot budgets (web `_SNAPSHOT_CONNECT_TIMEOUT` / `_COLLECT_WINDOW`).
pub const SNAPSHOT_CONNECT: Duration = Duration::from_secs(8);
pub const SNAPSHOT_WINDOW: Duration = Duration::from_secs(3);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Exchanges whose instruments carry open interest.
fn has_oi(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO" | "CDS" | "MCX")
}

/// `(exchange, token)` -> `(ltp, prev_close)`.
pub type LtpMap = HashMap<(String, String), (f64, f64)>;

/// Parse a `/fetch-ltp` answer. Rows with an empty `exchange` (BFO, CDS)
/// take the exchange the token was requested under, when unambiguous.
pub fn parse_ltp(v: &Value, requested: &[(String, String)]) -> LtpMap {
    let mut asked: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (ex, tok) in requested {
        asked.entry(tok.as_str()).or_default().insert(ex.as_str());
    }
    let mut out = LtpMap::new();
    let rows = v
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in rows {
        let token = s(&row, "token");
        let mut ex = s(&row, "exchange").to_ascii_uppercase();
        if ex.is_empty() {
            match asked.get(token.as_str()) {
                Some(set) if set.len() == 1 => {
                    ex = set.iter().next().map(|e| e.to_string()).unwrap_or_default()
                }
                _ => continue,
            }
        }
        out.insert(
            (ex, token),
            (
                super::mapping::f(&row, "ltp"),
                super::mapping::f(&row, "prev_close"),
            ),
        );
    }
    out
}

/// One `/fetch-ltp` batch. A refused session is an error; anything else
/// (rate limit after retries, outage) is logged and yields no quotes, so
/// callers report per-leg errors instead of failing the whole request.
pub async fn fetch_ltp_batch(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    instruments: &[(String, String)],
) -> Result<LtpMap> {
    if instruments.is_empty() {
        return Ok(LtpMap::new());
    }
    let body = json!({
        "data": instruments
            .iter()
            .map(|(ex, tok)| json!({"exchange": ex, "token": tok}))
            .collect::<Vec<_>>()
    });
    const PATH: &str = "/oapi/v1/fetch-ltp";
    for attempt in 0..LTP_ATTEMPTS {
        let resp = match b.send(Method::PUT, PATH, auth, Some(&body)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("HDFC Securities LTP request failed: {}", e.code());
                return Ok(LtpMap::new());
            }
        };
        if resp.status() == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < LTP_ATTEMPTS {
            let delay = LTP_BACKOFF * 2u32.pow(attempt);
            tracing::warn!(
                "HDFC Securities LTP rate-limited for {} instruments, retrying",
                instruments.len()
            );
            tokio::time::sleep(delay).await;
            continue;
        }
        let (status, v) = match HdfcSecuritiesBroker::decode(PATH, resp).await {
            Ok(x) => x,
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("HDFC Securities LTP answer unreadable: {}", e.code());
                return Ok(LtpMap::new());
            }
        };
        if status != StatusCode::OK {
            tracing::warn!(
                status = status.as_u16(),
                "HDFC Securities LTP request failed for {} instruments: {}",
                instruments.len(),
                super::error_message(&v)
            );
            return Ok(LtpMap::new());
        }
        return Ok(parse_ltp(&v, instruments));
    }
    Ok(LtpMap::new())
}

/// LTPs for any number of instruments: batches of 10, 150 ms apart.
pub async fn fetch_ltp(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    instruments: &[(String, String)],
) -> Result<LtpMap> {
    let mut seen = HashSet::new();
    let unique: Vec<(String, String)> = instruments
        .iter()
        .filter(|k| seen.insert((*k).clone()))
        .cloned()
        .collect();
    let mut out = LtpMap::new();
    for (n, batch) in unique.chunks(LTP_BATCH).enumerate() {
        if n > 0 {
            tokio::time::sleep(LTP_BATCH_DELAY).await;
        }
        out.extend(fetch_ltp_batch(b, auth, batch).await?);
    }
    Ok(out)
}

/// Collect one merged packet per instrument from a short-lived feed
/// connection. Never fails: whatever arrived before the window closed is
/// returned. `instruments` are `(OpenAlgo exchange, token)`.
pub async fn feed_snapshot(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    instruments: &[(String, String)],
    is_complete: impl Fn(&str, &Packet) -> bool,
) -> HashMap<(String, String), Packet> {
    let mut snap: HashMap<(String, String), Packet> = HashMap::new();
    if instruments.is_empty() {
        return snap;
    }
    let Ok(sess) = HdfcSecuritiesBroker::session(auth) else {
        return snap;
    };
    let url = feed_url(&b.urls.ws, sess.api_key, sess.token);
    let Ok(req) = feed_request(&url, sess.token) else {
        return snap;
    };
    let mut ws = match timeout(SNAPSHOT_CONNECT, tokio_tungstenite::connect_async(req)).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            tracing::warn!("HDFC Securities feed snapshot did not connect: {}", e);
            return snap;
        }
        Err(_) => {
            tracing::warn!("HDFC Securities feed snapshot did not connect in time");
            return snap;
        }
    };
    let wanted: HashSet<(String, String)> = instruments.iter().cloned().collect();
    let items: Vec<(String, &'static str)> = instruments
        .iter()
        .map(|(ex, tok)| (ws_scrip_id(ex, tok), "ALL"))
        .collect();
    let mut send_ok = true;
    for frame in subscribe_frames(&items, "subscribe") {
        if !matches!(timeout(CLOSE_TIMEOUT, ws.send(frame)).await, Ok(Ok(()))) {
            send_ok = false;
            break;
        }
    }
    let deadline = Instant::now() + SNAPSHOT_WINDOW;
    while send_ok {
        let done = wanted
            .iter()
            .all(|k| snap.get(k).is_some_and(|p| is_complete(&k.0, p)));
        if done {
            break;
        }
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => {
                tracing::warn!("HDFC Securities feed snapshot read failed: {}", e);
                break;
            }
            Ok(None) | Err(_) => break,
        };
        let Message::Binary(data) = msg else {
            continue;
        };
        for p in decode_frame(&data) {
            let token = p.token.to_string();
            let key = match p.exchange {
                Some(ex) => (ex.to_string(), token),
                None => {
                    let mut m = wanted.iter().filter(|(_, t)| *t == token);
                    match (m.next(), m.next()) {
                        (Some(k), None) => k.clone(),
                        _ => continue,
                    }
                }
            };
            if !wanted.contains(&key) {
                continue;
            }
            match snap.get_mut(&key) {
                Some(acc) => acc.accumulate(&p),
                None => {
                    snap.insert(key, p);
                }
            }
        }
    }
    // Close on every path (bounded), then drop the socket.
    let _ = timeout(CLOSE_TIMEOUT, ws.close(None)).await;
    snap
}

fn lookup(b: &HdfcSecuritiesBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

fn quote_complete(exchange: &str, p: &Packet) -> bool {
    if has_oi(exchange) && p.oi == 0 {
        return false;
    }
    p.depth.is_some() || p.ltp != 0.0
}

async fn ltp_and_snapshot(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    row: &SymToken,
) -> Result<((f64, f64), Option<Packet>)> {
    let k = (row.exchange.clone(), row.token.clone());
    let ltp = fetch_ltp_batch(b, auth, std::slice::from_ref(&k))
        .await?
        .remove(&k)
        .unwrap_or((0.0, 0.0));
    let mut snap = feed_snapshot(b, auth, std::slice::from_ref(&k), quote_complete).await;
    Ok((ltp, snap.remove(&k)))
}

fn best(levels: &[DepthLevel]) -> (f64, i64) {
    levels
        .iter()
        .find(|l| l.price != 0.0)
        .map(|l| (l.price, l.quantity))
        .unwrap_or((0.0, 0))
}

/// Web `get_quotes`: REST LTP and previous close, the rest from the feed.
pub fn compose_quote(key: &QuoteKey, rest: (f64, f64), tick: Option<&Packet>) -> Quote {
    let empty = (Vec::new(), Vec::new());
    let (buy, sell) = tick.and_then(|t| t.depth.as_ref()).unwrap_or(&empty);
    let (bid, bid_qty) = best(buy);
    let (ask, ask_qty) = best(sell);
    let tf = |g: fn(&Packet) -> f64| tick.map(g).unwrap_or(0.0);
    let ltp = if rest.0 != 0.0 { rest.0 } else { tf(|t| t.ltp) };
    let close = if rest.1 != 0.0 {
        rest.1
    } else {
        tf(|t| t.close)
    };
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: tf(|t| t.open),
        high: tf(|t| t.high),
        low: tf(|t| t.low),
        close,
        volume: tick.map(|t| t.volume).unwrap_or(0),
        bid,
        ask,
        bid_qty,
        ask_qty,
        oi: tick.map(|t| t.oi).unwrap_or(0),
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    };
    if close > 0.0 && ltp > 0.0 {
        q.change = round2(ltp - close);
        q.change_percent = round2((ltp - close) / close * 100.0);
    }
    q
}

/// Web `get_depth`.
pub fn compose_depth(key: &QuoteKey, rest: (f64, f64), tick: Option<&Packet>) -> MarketDepth {
    let (buy, sell) = tick.and_then(|t| t.depth.clone()).unwrap_or_default();
    let tf = |g: fn(&Packet) -> f64| tick.map(g).unwrap_or(0.0);
    let ti = |g: fn(&Packet) -> i64| tick.map(g).unwrap_or(0);
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad5(buy),
        asks: pad5(sell),
        ltp: if rest.0 != 0.0 { rest.0 } else { tf(|t| t.ltp) },
        ltq: ti(|t| t.ltq),
        open: tf(|t| t.open),
        high: tf(|t| t.high),
        low: tf(|t| t.low),
        prev_close: if rest.1 != 0.0 {
            rest.1
        } else {
            tf(|t| t.close)
        },
        volume: ti(|t| t.volume),
        oi: ti(|t| t.oi),
        total_buy_qty: ti(|t| t.total_buy_quantity),
        total_sell_qty: ti(|t| t.total_sell_quantity),
    }
}

pub async fn get_quote(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<Quote> {
    let row = lookup(b, key)?;
    let (rest, tick) = ltp_and_snapshot(b, auth, &row).await?;
    Ok(compose_quote(key, rest, tick.as_ref()))
}

pub async fn get_market_depth(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let row = lookup(b, key)?;
    let (rest, tick) = ltp_and_snapshot(b, auth, &row).await?;
    Ok(compose_depth(key, rest, tick.as_ref()))
}

/// Web `get_multiquotes`: LTP and previous close from REST batches; OHLC
/// and volume stay zero; OI for derivative legs from one feed snapshot.
pub async fn get_multiquotes(
    b: &HdfcSecuritiesBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let rows: Vec<Option<SymToken>> = keys
        .iter()
        .map(|k| b.resolver().by_symbol(&k.exchange, &k.symbol))
        .collect();
    let wanted: Vec<(String, String)> = rows
        .iter()
        .flatten()
        .map(|r| (r.exchange.clone(), r.token.clone()))
        .collect();
    let ltp = fetch_ltp(b, auth, &wanted).await?;
    let mut oi_targets: Vec<(String, String)> = wanted
        .iter()
        .filter(|(ex, _)| has_oi(ex))
        .cloned()
        .collect();
    oi_targets.sort();
    oi_targets.dedup();
    let oi = feed_snapshot(b, auth, &oi_targets, |_, p| p.oi != 0).await;
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
            let key = (row.exchange.clone(), row.token.clone());
            match ltp.get(&key) {
                None => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("No quote data available".into()),
                },
                Some(&(l, pc)) => {
                    let mut q = compose_quote(k, (l, pc), None);
                    q.oi = oi.get(&key).map(|p| p.oi).unwrap_or(0);
                    QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: Some(q),
                        error: None,
                    }
                }
            }
        })
        .collect())
}
