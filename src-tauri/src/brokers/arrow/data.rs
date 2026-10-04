//! Quotes, multiquotes, depth and history (web `api/data.py`).
//!
//! * Prices from the quote and candle APIs are paise: divide by 100
//!   (`PRICE_SCALE`, `data.py:22-32`); volume and OI are raw.
//! * Quote exchanges: NSE/BSE/NFO/BFO pass through, MCX is `MCXFO`, every
//!   index is `INDEX`; CDS/BCD/NCO are not served at all.
//! * Arrow's INDEX vocabulary differs from the master names: the five
//!   derivative indices answer to the OpenAlgo symbol, the rest to the
//!   uppercased display name. Candidates are tried in order and the accepted
//!   one is cached per token (`data.py:39-49,106-142`).
//! * `/info/quotes/full` takes at most 100 instruments (101 is an HTTP 500),
//!   so multiquotes go in batches of 100, 0.15 s apart.

use super::mapping::{history_exchange, is_index, quote_exchange, quote_unsupported};
use super::{envelope, ArrowBroker, Category, TIMEFRAME_MAP};
use crate::brokers::common::history::{chunks, parse_iso_epoch, sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Paise -> rupees.
pub const PRICE_SCALE: f64 = 100.0;
/// Hard server cap of `/info/quotes/full` (`data.py:241-251`).
pub const QUOTE_BATCH: usize = 100;

fn f(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn i(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => s.trim().parse::<f64>().map(|x| x as i64).unwrap_or(0),
        _ => 0,
    }
}

fn scaled(q: &Value, k: &str) -> f64 {
    f(q.get(k)) / PRICE_SCALE
}

fn first_level(q: &Value, side: &str) -> (f64, i64) {
    q.get(side)
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|l| (f(l.get("price")) / PRICE_SCALE, i(l.get("quantity"))))
        .unwrap_or((0.0, 0))
}

/// One Arrow FULL quote -> OpenAlgo quote (`_format_quote`, `data.py:257-271`).
pub fn format_quote(key: &QuoteKey, q: &Value) -> Quote {
    let (bid, bid_qty) = first_level(q, "bids");
    let (ask, ask_qty) = first_level(q, "asks");
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: scaled(q, "ltp"),
        open: scaled(q, "open"),
        high: scaled(q, "high"),
        low: scaled(q, "low"),
        close: scaled(q, "close"),
        volume: i(q.get("volume")),
        bid,
        ask,
        bid_qty,
        ask_qty,
        oi: i(q.get("oi")),
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    };
    if out.close > 0.0 {
        out.change = round2(out.ltp - out.close);
        out.change_percent = round2((out.ltp - out.close) / out.close * 100.0);
    }
    out
}

/// One Arrow FULL quote -> five-level depth (`get_depth`, `data.py:174-216`).
pub fn to_depth(key: &QuoteKey, q: &Value) -> MarketDepth {
    let side = |k: &str| -> Vec<DepthLevel> {
        let levels = q
            .get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        (0..5)
            .map(|n| {
                levels
                    .get(n)
                    .map(|l| DepthLevel {
                        price: f(l.get("price")) / PRICE_SCALE,
                        quantity: i(l.get("quantity")),
                        orders: i(l.get("orders")),
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: side("bids"),
        asks: side("asks"),
        ltp: scaled(q, "ltp"),
        ltq: i(q.get("ltq")),
        open: scaled(q, "open"),
        high: scaled(q, "high"),
        low: scaled(q, "low"),
        prev_close: scaled(q, "close"),
        volume: i(q.get("volume")),
        oi: i(q.get("oi")),
        total_buy_qty: i(q.get("totalBuyQty")),
        total_sell_qty: i(q.get("totalSellQty")),
    }
}

fn lookup(b: &ArrowBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

fn unsupported_exchange(exchange: &str) -> AppError {
    AppError::Broker(format!(
        "Arrow does not provide quotes for {} instruments. Live prices for them are available through streaming.",
        exchange
    ))
}

/// `POST /info/quote/{mode}` for one instrument. A 400 is reported as
/// `Ok(None)` so index candidates can move on.
async fn quote_once(
    b: &ArrowBroker,
    auth: &AuthToken,
    mode: &str,
    exchange: &str,
    symbol: &str,
) -> Result<Option<Value>> {
    let url = format!("{}/info/quote/{}", b.urls().rest, mode);
    let body = json!({"exchange": exchange, "symbol": symbol});
    let (status, v) = b
        .call_raw(Method::POST, &url, auth, Some(&body), Category::Quote)
        .await?;
    if status == StatusCode::BAD_REQUEST {
        return Ok(None);
    }
    envelope(status, v, "/info/quote").map(Some)
}

/// Quote an index through Arrow's INDEX vocabulary: OpenAlgo symbol, then
/// uppercased display name, then the raw display name; the accepted name is
/// cached by token.
async fn quote_index(
    b: &ArrowBroker,
    auth: &AuthToken,
    mode: &str,
    row: &SymToken,
) -> Result<Value> {
    if let Some(name) = b.cached_index_name(&row.token) {
        return quote_once(b, auth, mode, "INDEX", &name)
            .await?
            .ok_or_else(|| index_refused(&row.symbol));
    }
    if b.index_refused(&row.token) {
        return Err(index_refused(&row.symbol));
    }
    for cand in index_candidates(&row.symbol, row.br_symbol()) {
        if let Some(data) = quote_once(b, auth, mode, "INDEX", &cand).await? {
            b.remember_index_name(&row.token, &cand);
            return Ok(data);
        }
    }
    b.mark_index_refused(&row.token);
    Err(index_refused(&row.symbol))
}

/// `[oa_symbol, brsymbol.upper(), brsymbol]`, deduplicated, in order.
pub fn index_candidates(symbol: &str, brsymbol: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in [
        symbol.to_string(),
        brsymbol.to_uppercase(),
        brsymbol.to_string(),
    ] {
        if !c.is_empty() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

fn index_refused(symbol: &str) -> AppError {
    AppError::Broker(format!(
        "Arrow does not provide a quote for the index {}. Live prices for it are available through streaming.",
        symbol
    ))
}

/// The verified INDEX name for an index row, probing with a cheap `ltp`
/// quote when it is not cached yet. `None` when Arrow does not serve it.
async fn resolve_index_name(b: &ArrowBroker, auth: &AuthToken, row: &SymToken) -> Option<String> {
    if let Some(n) = b.cached_index_name(&row.token) {
        return Some(n);
    }
    if b.index_refused(&row.token) {
        return None;
    }
    match quote_index(b, auth, "ltp", row).await {
        Ok(_) => b.cached_index_name(&row.token),
        Err(_) => None,
    }
}

async fn fetch_full(b: &ArrowBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
    if quote_unsupported(&key.exchange) {
        return Err(unsupported_exchange(&key.exchange));
    }
    let row = lookup(b, key)?;
    let ex = quote_exchange(&key.exchange);
    if ex == "INDEX" {
        return quote_index(b, auth, "full", &row).await;
    }
    quote_once(b, auth, "full", &ex, row.br_symbol())
        .await?
        .ok_or_else(|| {
            AppError::Broker(format!(
                "Arrow did not return a quote for {} {}.",
                key.exchange, key.symbol
            ))
        })
}

pub async fn get_quote(b: &ArrowBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let q = fetch_full(b, auth, key).await?;
    Ok(format_quote(key, &q))
}

pub async fn get_market_depth(
    b: &ArrowBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let q = fetch_full(b, auth, key).await?;
    Ok(to_depth(key, &q))
}

fn leg_error(k: &QuoteKey, msg: &str) -> QuoteResult {
    QuoteResult {
        symbol: k.symbol.clone(),
        exchange: k.exchange.clone(),
        data: None,
        error: Some(msg.to_string()),
    }
}

/// `POST /info/quotes/full` with one batch; indexed by token. A failed batch
/// yields an empty map so its legs become per-leg errors, never sinking the
/// other batches (`data.py:347-384`).
async fn fetch_batch(b: &ArrowBroker, auth: &AuthToken, body: &[Value]) -> HashMap<String, Value> {
    let url = format!("{}/info/quotes/full", b.urls().rest);
    let payload = Value::Array(body.to_vec());
    let res = b
        .call_raw(Method::POST, &url, auth, Some(&payload), Category::Quote)
        .await;
    match res {
        Ok((status, v)) if status.is_success() => v
            .get("data")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|q| {
                        let t = match q.get("token") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Null) | None => String::new(),
                            Some(o) => o.to_string(),
                        };
                        (t, q.clone())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Ok((status, _)) => {
            tracing::warn!(
                status = status.as_u16(),
                "Arrow quotes refused a batch of {} instruments",
                body.len()
            );
            HashMap::new()
        }
        Err(e) => {
            tracing::warn!(
                "Arrow quotes failed for a batch of {} instruments: {}",
                body.len(),
                e.code()
            );
            HashMap::new()
        }
    }
}

pub async fn get_multiquotes(
    b: &ArrowBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(QUOTE_BATCH) {
        let mut body = Vec::new();
        let mut legs: Vec<(String, QuoteKey)> = Vec::new();
        let mut skipped = Vec::new();
        for k in batch {
            if quote_unsupported(&k.exchange) {
                skipped.push(leg_error(k, "Exchange not supported by Arrow quotes"));
                continue;
            }
            let Some(row) = b.resolver().by_symbol(&k.exchange, &k.symbol) else {
                skipped.push(leg_error(
                    k,
                    &format!("Could not find instrument for {}:{}", k.exchange, k.symbol),
                ));
                continue;
            };
            if is_index(&k.exchange) {
                match resolve_index_name(b, auth, &row).await {
                    Some(name) => body.push(json!({"exchange": "INDEX", "symbol": name})),
                    None => {
                        skipped.push(leg_error(k, "Index not served by Arrow quote API"));
                        continue;
                    }
                }
            } else {
                body.push(json!({
                    "exchange": quote_exchange(&k.exchange),
                    "symbol": row.br_symbol(),
                }));
            }
            legs.push((row.token.clone(), k.clone()));
        }
        out.extend(skipped);
        if legs.is_empty() {
            continue;
        }
        let quotes = fetch_batch(b, auth, &body).await;
        for (token, k) in legs {
            match quotes.get(&token) {
                Some(q) => out.push(QuoteResult {
                    data: Some(format_quote(&k, q)),
                    symbol: k.symbol,
                    exchange: k.exchange,
                    error: None,
                }),
                None => out.push(leg_error(&k, "No quote data available")),
            }
        }
    }
    Ok(out)
}

/// Arrow interval for an OpenAlgo interval key.
pub fn arrow_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Arrow. Use one of: {}.",
                interval,
                list.join(", ")
            ))
        })
}

/// Candle rows `[ts, o, h, l, c, v(, oi)]` -> candles: ISO-8601 `+0530`
/// timestamps to epoch seconds (daily/weekly/monthly shifted +5:30 to stand
/// for IST midnight), OHLC divided by 100 (`data.py:452-474`).
pub fn parse_candles(rows: &[Value], daily: bool, with_oi: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let r = r.as_array()?;
            let ts = match r.first()? {
                Value::String(s) => parse_iso_epoch(s)?,
                Value::Number(n) => n.as_i64()?,
                _ => return None,
            };
            Some(Candle {
                timestamp: if daily { ts + IST_OFFSET_SECS } else { ts },
                open: f(r.get(1)) / PRICE_SCALE,
                high: f(r.get(2)) / PRICE_SCALE,
                low: f(r.get(3)) / PRICE_SCALE,
                close: f(r.get(4)) / PRICE_SCALE,
                volume: i(r.get(5)),
                oi: if with_oi { i(r.get(6)) } else { 0 },
            })
        })
        .collect()
}

pub async fn get_history(
    b: &ArrowBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = arrow_interval(&req.interval)?;
    let row = lookup(b, &req.key)?;
    let ex = history_exchange(&req.key.exchange);
    // OI only on NFO/BFO (`data.py:405-406`).
    let want_oi = matches!(req.key.exchange.as_str(), "NFO" | "BFO");
    let daily = matches!(interval, "day" | "week" | "month");
    let max_days = if daily { 2000 } else { 60 };
    let mut out = Vec::new();
    for (from, to) in chunks(req.start, req.end, max_days) {
        let mut url = format!(
            "{}/candle/{}/{}/{}?from={}T00:00:00&to={}T23:59:59",
            b.urls().history,
            ex,
            urlencoding::encode(&row.token),
            interval,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );
        if want_oi {
            url.push_str("&oi=1");
        }
        let (status, v) = b
            .call_raw(Method::GET, &url, auth, None, Category::History)
            .await?;
        match v {
            Value::Array(rows) if status.is_success() => {
                out.extend(parse_candles(&rows, daily, want_oi));
            }
            other => {
                // Errors come back as a `{status, message}` dict.
                return Err(envelope(status, other, "/candle").err().unwrap_or_else(|| {
                    AppError::Broker(
                        "Arrow did not return candles for this request. Try again shortly.".into(),
                    )
                }));
            }
        }
    }
    Ok(sort_dedupe(out))
}
