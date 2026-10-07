//! Quotes, depth and history (web `api/data.py`).
//!
//! * Scrip codes are `<SEGMENT>_<token>` with `NIDX` / `BIDX` for indices.
//! * `/market/quotes/full` 400-rejects a whole batch when one code cannot be
//!   quoted ("Invalid scrip codes or mode"); the batch is bisected to find
//!   the culprit, which is then skipped for five minutes.
//! * History: `GET /market/historical/<interval>?scrip-codes=&start_time=&
//!   end_time=` with IST day bounds in epoch milliseconds, chunked 7 days
//!   (minute bars), 14 days (hour bars) or 365 days (D/W/M). Candles are
//!   `{ts,o,h,l,c,v}` with `ts` in seconds, or `[ts_ms,o,h,l,c,v]`.

use super::mapping::{num, num_value, scrip_segment};
use super::{session_expired, token, IndmoneyBroker, BAD_SCRIP_MAX, BAD_SCRIP_TTL, TIMEFRAME_MAP};
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, TimeZone};
use chrono_tz::Asia::Kolkata;
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;

/// Quotes per `/market/quotes/full` request (web `BATCH_SIZE`).
pub const BATCH_SIZE: usize = 500;

/// A failed market-data call; `invalid_scrip` marks the batch-poisoning 400.
#[derive(Debug)]
pub(crate) struct MarketError {
    pub invalid_scrip: bool,
    pub error: AppError,
}

impl MarketError {
    fn broker(msg: &str) -> Self {
        Self {
            invalid_scrip: false,
            error: AppError::Broker(msg.into()),
        }
    }

    pub fn message(&self) -> String {
        self.error.client_message()
    }
}

/// Market-data call with the web `data.get_api_response` rules: any non-200
/// fails; a body with data (or `success: true`) passes even without
/// `status`; otherwise `status` must be `success`.
pub(crate) async fn market_get(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    path: &str,
    query: &[(&str, String)],
) -> std::result::Result<Value, MarketError> {
    let tok = token(auth).map_err(|error| MarketError {
        invalid_scrip: false,
        error,
    })?;
    let r = b
        .send(Method::GET, path, query, None, tok)
        .await
        .map_err(|error| MarketError {
            invalid_scrip: false,
            error,
        })?;
    if r.status == 401 || r.status == 403 {
        return Err(MarketError {
            invalid_scrip: false,
            error: session_expired(),
        });
    }
    if r.status != 200 {
        let invalid = r.text.contains("Invalid scrip");
        tracing::warn!(
            status = r.status,
            invalid,
            "INDmoney market data refused on {}",
            path
        );
        return Err(MarketError {
            invalid_scrip: invalid,
            error: AppError::Broker(if r.status == 429 {
                "INDmoney is limiting market data requests right now. Try again shortly.".into()
            } else {
                "INDmoney could not return market data for this instrument.".into()
            }),
        });
    }
    let v = r.json;
    if v.is_null() {
        return Err(MarketError::broker(
            "INDmoney sent market data OpenAlgo could not read.",
        ));
    }
    if v.get("success") == Some(&Value::Bool(true)) || has_valid_data(&v) {
        return Ok(v);
    }
    if v.get("status").and_then(Value::as_str) != Some("success") {
        let msg = super::error_message(&v)
            .unwrap_or_else(|| "INDmoney could not return market data.".into());
        return Err(MarketError {
            invalid_scrip: msg.contains("Invalid scrip"),
            error: AppError::Broker(msg),
        });
    }
    Ok(v)
}

fn has_valid_data(v: &Value) -> bool {
    let Some(data) = v.get("data") else {
        return false;
    };
    let candles = |x: &Value| {
        x.get("candles")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    };
    match data {
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => candles(data) || o.values().any(|x| x.is_object() && candles(x)),
        _ => false,
    }
}

/// `data` of a quotes call for comma-joined scrip codes.
pub(crate) async fn market_call(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    path: &str,
    codes: &str,
) -> std::result::Result<Value, MarketError> {
    let v = market_get(b, auth, path, &[("scrip-codes", codes.to_string())]).await?;
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

/// `<SEGMENT>_<token>` for an OpenAlgo instrument.
pub fn scrip_code(b: &IndmoneyBroker, key: &QuoteKey) -> Result<String> {
    let seg = scrip_segment(&key.exchange).ok_or_else(|| {
        AppError::Validation(format!("INDmoney has no market data for {}.", key.exchange))
    })?;
    let tok = b
        .resolver()
        .token(&key.symbol, &key.exchange)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })?;
    Ok(format!("{}_{}", seg, tok))
}

/// The `{aggregate, depth}` object of a `market_depth` value: flat, under an
/// extra scrip-code key, or under a single unknown key.
pub fn extract_market_depth<'a>(container: Option<&'a Value>, scrip: &str) -> Option<&'a Value> {
    let c = container?.as_object()?;
    let has = |x: &Value| x.get("depth").is_some() || x.get("aggregate").is_some();
    if let Some(n) = c.get(scrip).filter(|n| n.is_object() && has(n)) {
        return Some(n);
    }
    let me = container?;
    if has(me) {
        return Some(me);
    }
    if c.len() == 1 {
        return c.values().next().filter(|x| x.is_object() && has(x));
    }
    None
}

fn levels(md: Option<&Value>) -> Vec<Value> {
    md.and_then(|m| m.get("depth"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn side(level: &Value, s: &str) -> DepthLevel {
    let x = level.get(s);
    DepthLevel {
        price: num_value(x.and_then(|v| v.get("price"))),
        quantity: num_value(x.and_then(|v| v.get("quantity"))) as i64,
        orders: num_value(x.and_then(|v| v.get("orders"))) as i64,
    }
}

fn has_quote_fields(q: &Value, keys: &[&str]) -> bool {
    q.is_object() && keys.iter().any(|k| q.get(*k).is_some())
}

fn live_price(q: &Value) -> f64 {
    match q.get("live_price") {
        Some(v) if !v.is_null() => num_value(Some(v)),
        _ => num(q, "ltp"),
    }
}

fn prev_close(q: &Value) -> f64 {
    match q.get("prev_close") {
        Some(v) if !v.is_null() => num_value(Some(v)),
        _ => num(q, "close"),
    }
}

fn open_interest(q: &Value) -> i64 {
    match q.get("oi") {
        Some(v) if !v.is_null() => num_value(Some(v)) as i64,
        _ => num(q, "open_interest") as i64,
    }
}

/// A `/market/quotes/full` entry as an OpenAlgo quote.
pub fn quote_from_full(key: &QuoteKey, full: &Value, scrip: &str) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: live_price(full),
        open: num(full, "day_open"),
        high: num(full, "day_high"),
        low: num(full, "day_low"),
        close: prev_close(full),
        volume: num(full, "volume") as i64,
        oi: open_interest(full),
        ..Default::default()
    };
    let md = extract_market_depth(full.get("market_depth"), scrip);
    if let Some(first) = levels(md).first() {
        let (bid, ask) = (side(first, "buy"), side(first, "sell"));
        q.bid = bid.price;
        q.bid_qty = bid.quantity;
        q.ask = ask.price;
        q.ask_qty = ask.quantity;
    }
    q
}

pub async fn get_quote(b: &IndmoneyBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let scrip = scrip_code(b, key)?;
    match market_call(b, auth, "/market/quotes/full", &scrip).await {
        Ok(d) => {
            let full = d.get(&scrip).cloned().unwrap_or(Value::Null);
            if has_quote_fields(&full, &["ltp", "live_price", "open", "high", "low"]) {
                return Ok(quote_from_full(key, &full, &scrip));
            }
        }
        Err(e) if matches!(e.error, AppError::Auth(_)) => return Err(e.error),
        Err(e) => tracing::warn!("INDmoney full quote failed, falling back: {}", e.message()),
    }
    // Fallback: LTP plus best bid/ask from the depth endpoint.
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ..Default::default()
    };
    let mut any = false;
    match market_call(b, auth, "/market/quotes/ltp", &scrip).await {
        Ok(d) => {
            any = true;
            q.ltp = d.get(&scrip).map(|x| num(x, "live_price")).unwrap_or(0.0);
        }
        Err(e) => tracing::warn!("INDmoney LTP failed: {}", e.message()),
    }
    match market_call(b, auth, "/market/quotes/mkt", &scrip).await {
        Ok(d) => {
            any = true;
            let raw = d.get(&scrip).cloned().unwrap_or(Value::Null);
            if let Some(first) =
                levels(extract_market_depth(raw.get("market_depth"), &scrip)).first()
            {
                q.bid = side(first, "buy").price;
                q.ask = side(first, "sell").price;
            }
        }
        Err(e) => tracing::warn!("INDmoney depth for quote failed: {}", e.message()),
    }
    if !any {
        return Err(AppError::Broker(
            "INDmoney could not return a quote for this instrument right now.".into(),
        ));
    }
    Ok(q)
}

impl IndmoneyBroker {
    fn is_known_bad(&self, code: &str) -> bool {
        let mut m = self.bad_scrips.lock();
        match m.get(code) {
            Some(t) if t.elapsed() <= BAD_SCRIP_TTL => true,
            Some(_) => {
                m.remove(code);
                false
            }
            None => false,
        }
    }

    pub(crate) fn mark_bad(&self, code: &str) {
        let mut m = self.bad_scrips.lock();
        m.retain(|_, t| t.elapsed() <= BAD_SCRIP_TTL);
        if m.len() >= BAD_SCRIP_MAX {
            if let Some(oldest) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) {
                m.remove(&oldest);
            }
        }
        m.insert(code.to_string(), Instant::now());
    }

    pub fn bad_scrip_count(&self) -> usize {
        self.bad_scrips.lock().len()
    }
}

/// `/market/quotes/full` for many codes, bisecting a batch that one
/// unquotable code poisons (web `_fetch_full_quotes_map`).
pub(crate) async fn fetch_full_quotes(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    codes: &[String],
) -> Result<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    let mut work: Vec<Vec<String>> = vec![codes.to_vec()];
    while let Some(batch) = work.pop() {
        let batch: Vec<String> = batch.into_iter().filter(|c| !b.is_known_bad(c)).collect();
        if batch.is_empty() {
            continue;
        }
        match market_call(b, auth, "/market/quotes/full", &batch.join(",")).await {
            Ok(Value::Object(m)) => out.extend(m),
            Ok(_) => {}
            Err(e) if matches!(e.error, AppError::Auth(_)) => return Err(e.error),
            Err(e) if e.invalid_scrip && batch.len() == 1 => {
                tracing::warn!("Skipping unquotable INDmoney scrip {}", batch[0]);
                b.mark_bad(&batch[0]);
            }
            Err(e) if e.invalid_scrip => {
                let mid = batch.len() / 2;
                work.push(batch[mid..].to_vec());
                work.push(batch[..mid].to_vec());
            }
            Err(e) => {
                tracing::warn!(
                    "INDmoney quotes failed for {} codes: {}",
                    batch.len(),
                    e.message()
                );
            }
        }
    }
    Ok(out)
}

pub async fn get_multiquotes(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let codes: Vec<std::result::Result<String, String>> = keys
        .iter()
        .map(|k| scrip_code(b, k).map_err(|e| e.client_message()))
        .collect();
    let valid: Vec<String> = codes
        .iter()
        .filter_map(|c| c.as_ref().ok().cloned())
        .collect();
    let mut data = serde_json::Map::new();
    for batch in valid.chunks(BATCH_SIZE) {
        data.extend(fetch_full_quotes(b, auth, batch).await?);
    }
    Ok(keys
        .iter()
        .zip(codes)
        .map(|(k, c)| {
            let (data, error) = match c {
                Err(e) => (None, Some(e)),
                Ok(code) => match data.get(&code) {
                    Some(q)
                        if has_quote_fields(
                            q,
                            &["ltp", "live_price", "day_open", "day_high", "day_low"],
                        ) =>
                    {
                        // web: multiquotes carry no bid/ask.
                        let mut quote = quote_from_full(k, q, &code);
                        quote.bid = 0.0;
                        quote.ask = 0.0;
                        quote.bid_qty = 0;
                        quote.ask_qty = 0;
                        (Some(quote), None)
                    }
                    _ => (None, Some("No data received".to_string())),
                },
            };
            QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data,
                error,
            }
        })
        .collect())
}

/// Five-level depth from a `/market/quotes/mkt` entry plus the OHLC of a
/// `/market/quotes/full` entry (web `get_depth`).
pub fn depth_from(
    key: &QuoteKey,
    scrip: &str,
    full: &Value,
    mkt: &Value,
    ltp_only: Option<f64>,
) -> MarketDepth {
    let mut d = MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ..Default::default()
    };
    let md = extract_market_depth(mkt.get("market_depth"), scrip);
    let lv = levels(md);
    for i in 0..5 {
        match lv.get(i) {
            Some(l) => {
                let (mut bid, mut ask) = (side(l, "buy"), side(l, "sell"));
                bid.orders = 0;
                ask.orders = 0;
                d.bids.push(bid);
                d.asks.push(ask);
            }
            None => {
                d.bids.push(DepthLevel::default());
                d.asks.push(DepthLevel::default());
            }
        }
    }
    if mkt.is_object() && !mkt.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        let agg = md.and_then(|m| m.get("aggregate"));
        let total = |k: &str, levels: &[DepthLevel]| {
            let v = agg.and_then(|a| a.get(k));
            let n = num_value(v);
            if n != 0.0 {
                n as i64
            } else {
                levels.iter().map(|l| l.quantity).sum()
            }
        };
        d.total_buy_qty = total("total_buy", &d.bids);
        d.total_sell_qty = total("total_sell", &d.asks);
    }
    if full.is_object() && !full.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        d.ltp = live_price(full);
        d.open = num(full, "day_open");
        d.high = num(full, "day_high");
        d.low = num(full, "day_low");
        d.prev_close = prev_close(full);
        d.volume = num(full, "volume") as i64;
        d.oi = open_interest(full);
    } else if let Some(l) = ltp_only {
        d.ltp = l;
    } else if d.bids[0].price > 0.0 {
        d.ltp = d.bids[0].price;
    }
    d
}

pub async fn get_market_depth(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let scrip = scrip_code(b, key)?;
    let full = match market_call(b, auth, "/market/quotes/full", &scrip).await {
        Ok(d) => d.get(&scrip).cloned().unwrap_or(Value::Null),
        Err(e) if matches!(e.error, AppError::Auth(_)) => return Err(e.error),
        Err(e) => {
            tracing::warn!("INDmoney full quote for depth failed: {}", e.message());
            Value::Null
        }
    };
    let mkt = match market_call(b, auth, "/market/quotes/mkt", &scrip).await {
        Ok(d) => d.get(&scrip).cloned().unwrap_or(Value::Null),
        Err(e) => return Err(e.error),
    };
    let ltp_only = match market_call(b, auth, "/market/quotes/ltp", &scrip).await {
        Ok(d) => d
            .get(&scrip)
            .and_then(|x| x.get("live_price"))
            .map(|v| num_value(Some(v))),
        Err(_) => None,
    };
    Ok(depth_from(key, &scrip, &full, &mkt, ltp_only))
}

/// Days per history request for a broker interval (web `max_ranges`).
pub fn chunk_days(interval: &str) -> i64 {
    match interval {
        "60minute" | "120minute" | "180minute" | "240minute" => 14,
        "1day" | "1week" | "1month" => 365,
        _ => 7,
    }
}

/// IST day bound in epoch milliseconds (start 00:00:00, end 23:59:59).
pub fn day_ms(d: NaiveDate, end_of_day: bool) -> i64 {
    let t = if end_of_day {
        d.and_hms_opt(23, 59, 59)
    } else {
        d.and_hms_opt(0, 0, 0)
    };
    t.and_then(|t| Kolkata.from_local_datetime(&t).single())
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
}

/// Candles of one history answer for `scrip`.
pub fn parse_candles(v: &Value, scrip: &str) -> Vec<Candle> {
    let data = v.get("data").unwrap_or(&Value::Null);
    let list = if let Some(s) = data.get(scrip) {
        s.get("candles").cloned().unwrap_or(Value::Null)
    } else if data.get("candles").is_some() {
        data["candles"].clone()
    } else if data.is_array() {
        data.clone()
    } else {
        Value::Null
    };
    let mut out = Vec::new();
    for c in list.as_array().into_iter().flatten() {
        if c.get("ts").is_some() {
            out.push(Candle {
                timestamp: num(c, "ts") as i64,
                open: num(c, "o"),
                high: num(c, "h"),
                low: num(c, "l"),
                close: num(c, "c"),
                volume: num(c, "v") as i64,
                oi: 0,
            });
        } else if let Some(a) = c.as_array().filter(|a| a.len() >= 6) {
            let n = |i: usize| num_value(a.get(i));
            out.push(Candle {
                timestamp: (n(0) / 1000.0) as i64,
                open: n(1),
                high: n(2),
                low: n(3),
                close: n(4),
                volume: n(5) as i64,
                oi: 0,
            });
        }
    }
    out
}

pub async fn get_history(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Unsupported interval '{}'. Supported intervals are: {}",
                req.interval,
                TIMEFRAME_MAP
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let scrip = scrip_code(b, &req.key)?;
    let path = format!("/market/historical/{}", interval);
    let mut all = Vec::new();
    let mut last_err = None;
    for (from, to) in chunks(req.start, req.end, chunk_days(interval)) {
        let q = [
            ("scrip-codes", scrip.clone()),
            ("start_time", day_ms(from, false).to_string()),
            ("end_time", day_ms(to, true).to_string()),
        ];
        match market_get(b, auth, &path, &q).await {
            Ok(v) => all.extend(parse_candles(&v, &scrip)),
            Err(e) if matches!(e.error, AppError::Auth(_)) => return Err(e.error),
            Err(e) => {
                tracing::warn!(
                    "INDmoney history chunk {}..{} failed: {}",
                    from,
                    to,
                    e.message()
                );
                last_err = Some(e.error);
            }
        }
    }
    if all.is_empty() {
        if let Some(e) = last_err {
            return Err(e);
        }
    }
    Ok(sort_dedupe(all))
}
