//! Quotes, depth, multiquotes and history (web `api/data.py`).
//!
//! Quotes: `GET {base}/script-details/1.0/quotes/neosymbol/<seg>|<pSymbol>/all`
//! (comma-joined for several), `Authorization: <access token>`. The answer
//! is a JSON list; every field is a string. An index is queried by name
//! (`nse_cm|Nifty 50`), trying each known spelling until one answers.
//!
//! History: `GET {base}/market-data/1.0/historical/details?neosymbol=..&
//! fromdate=..&todate=..&interval=..`, NSE/BSE cash and F&O only, paced at
//! one request a second, chunked per interval, clamped to five years, with
//! "no data" faults read as empty and broken bars repaired.

use super::mapping::{n, s};
use super::{kotak_error, py_quote, KotakBroker, KotakSession, TIMEFRAME_MAP};
use crate::brokers::common::history::{parse_iso_epoch, sort_dedupe};
use crate::brokers::common::streaming::round2;
use crate::brokers::types::*;
use crate::clock::IST_OFFSET_SECS;
use crate::error::{AppError, Result};
use chrono::{Duration as Days, NaiveDate};
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;

/// Symbols per multiquote request (web `BATCH_SIZE`; Neo refuses 50).
pub const MULTIQUOTE_BATCH: usize = 25;
/// Pause between multiquote batches (web `RATE_LIMIT_DELAY`).
pub const MULTIQUOTE_DELAY: Duration = Duration::from_millis(200);
/// Quote 429 retries (web `QUOTES_MAX_RETRIES`).
pub const QUOTES_MAX_RETRIES: u32 = 3;
/// History 429 retries (web `HISTORY_MAX_RETRIES`).
pub const HISTORY_MAX_RETRIES: u32 = 4;
/// Segments the historical endpoint serves.
pub const HISTORY_SEGMENTS: &[&str] = &["nse_cm", "nse_fo", "bse_cm", "bse_fo"];
/// Neo serves five years of history.
pub const HISTORY_MAX_LOOKBACK_YEARS: i32 = 5;

/// OpenAlgo exchange -> Neo segment for quotes and history (web
/// `_get_kotak_exchange`, indices on the cash segments).
pub fn kotak_segment(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" | "NSE_INDEX" => "nse_cm",
        "BSE" | "BSE_INDEX" => "bse_cm",
        "NFO" => "nse_fo",
        "BFO" => "bse_fo",
        "CDS" => "cde_fo",
        "MCX" => "mcx_fo",
        _ => return None,
    })
}

/// Neo's names for an OpenAlgo index, in the order to try them (web
/// `_get_index_symbol_candidates`; the stream keeps the same map).
pub fn index_candidates(symbol: &str) -> Vec<String> {
    let list: &[&str] = match symbol.to_ascii_uppercase().as_str() {
        "NIFTY" | "NIFTY50" => &["Nifty 50"],
        "BANKNIFTY" => &["Nifty Bank"],
        "FINNIFTY" => &["Nifty Fin Service"],
        "MIDCPNIFTY" => &[
            "Nifty Mid Select",
            "Nifty Midcap Sel",
            "Nifty Midcap Select",
            "NIFTY MID SELECT",
        ],
        "NIFTYNXT50" => &["Nifty Next 50"],
        "INDIAVIX" => &["India VIX"],
        "SENSEX" => &["SENSEX"],
        "BANKEX" => &["BANKEX"],
        _ => return vec![symbol.to_string()],
    };
    list.iter().map(|s| s.to_string()).collect()
}

pub(super) fn is_index(exchange: &str) -> bool {
    exchange.to_ascii_uppercase().contains("INDEX")
}

/// The `<seg>|<pSymbol>` key of a tradable instrument. Cash rows store the
/// OpenAlgo code (`NSE`) as brexchange, F&O rows Neo's segment (`nse_fo`).
fn neo_key(b: &KotakBroker, key: &QuoteKey) -> Result<String> {
    let row = b
        .resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })?;
    let br = row.br_exchange().to_string();
    let seg = if matches!(br.as_str(), "NSE" | "BSE" | "NFO" | "BFO" | "CDS" | "MCX") {
        kotak_segment(&br).unwrap_or("").to_string()
    } else {
        br
    };
    Ok(format!("{}|{}", seg, row.token.trim()))
}

fn retry_after(resp_headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    resp_headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(Duration::from_secs_f64)
}

/// One quotes request. `Ok(None)` is "no answer" (Not_Ok, non-200 after
/// retries); the caller decides whether that is an error.
async fn quotes_request(
    b: &KotakBroker,
    sess: &KotakSession,
    query: &str,
) -> Result<std::result::Result<Vec<Value>, String>> {
    let url = format!(
        "{}/script-details/1.0/quotes/neosymbol/{}/all",
        sess.base_url,
        py_quote(query, "|,")
    );
    let mut attempt = 0u32;
    loop {
        let resp = {
            let _permit = b
                .quotes_gate
                .acquire()
                .await
                .map_err(|_| AppError::Internal("Kotak quote limiter closed".into()))?;
            b.http
                .get(&url)
                .header("Authorization", &sess.access_token)
                .header("Content-Type", "application/json")
                .timeout(Duration::from_secs(15))
                .send()
                .await?
        };
        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS && attempt < QUOTES_MAX_RETRIES {
            let wait =
                retry_after(resp.headers()).unwrap_or_else(|| b.retry_base / 2 * (1 << attempt));
            drop(resp);
            tokio::time::sleep(wait).await;
            attempt += 1;
            continue;
        }
        if status != StatusCode::OK {
            tracing::warn!(status = status.as_u16(), "Kotak quotes call failed");
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(super::session_expired());
            }
            return Ok(Err(format!("status {}", status.as_u16())));
        }
        let (_, v): (StatusCode, Value) =
            crate::brokers::common::http::read_json("kotak", resp).await?;
        // 200 with {"stat":"Not_Ok","emsg":..,"stCode":1009}: invalid symbol.
        if s(&v, "stat") == "Not_Ok" {
            tracing::warn!("Kotak quotes refused: {}", s(&v, "emsg"));
            return Ok(Err(s(&v, "emsg")));
        }
        return Ok(Ok(v.as_array().cloned().unwrap_or_default()));
    }
}

/// The first non-empty answer over a key's candidates.
async fn quote_rows(b: &KotakBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Vec<Value>> {
    let sess = KotakSession::parse(auth)?;
    let queries: Vec<String> = if is_index(&key.exchange) {
        let seg = kotak_segment(&key.exchange).unwrap_or("nse_cm");
        index_candidates(&key.symbol)
            .into_iter()
            .map(|c| format!("{}|{}", seg, c))
            .collect()
    } else {
        vec![neo_key(b, key)?]
    };
    let mut last = String::new();
    for q in &queries {
        match quotes_request(b, &sess, q).await? {
            Ok(rows) if !rows.is_empty() => return Ok(rows),
            Ok(_) => last.clear(),
            Err(m) => last = m,
        }
    }
    if last.is_empty() {
        Ok(Vec::new())
    } else {
        Err(AppError::Broker(format!(
            "Kotak did not return a quote for {} {}.",
            key.exchange, key.symbol
        )))
    }
}

fn levels(q: &Value, side: &str) -> Vec<Value> {
    q.get("depth")
        .and_then(|d| d.get(side))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Web quote fields from one Neo quote row; bid/ask fall back to the LTP.
pub fn quote_from_row(key: &QuoteKey, q: &Value) -> Quote {
    let ohlc = q.get("ohlc").cloned().unwrap_or(Value::Null);
    let ltp = n(q, "ltp");
    let buy = levels(q, "buy");
    let sell = levels(q, "sell");
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: n(&ohlc, "open"),
        high: n(&ohlc, "high"),
        low: n(&ohlc, "low"),
        close: n(&ohlc, "close"),
        volume: n(q, "last_volume") as i64,
        bid: buy.first().map(|l| n(l, "price")).unwrap_or(ltp),
        ask: sell.first().map(|l| n(l, "price")).unwrap_or(ltp),
        bid_qty: buy.first().map(|l| n(l, "quantity") as i64).unwrap_or(0),
        ask_qty: sell.first().map(|l| n(l, "quantity") as i64).unwrap_or(0),
        oi: n(q, "open_int") as i64,
        ..Default::default()
    };
    if quote.close > 0.0 {
        quote.change = round2(quote.ltp - quote.close);
        quote.change_percent = round2((quote.ltp - quote.close) / quote.close * 100.0);
    }
    quote
}

/// Web `get_depth` from one row: five levels padded; totals are Neo's when
/// set (cash) or the level sums (F&O leaves them 0).
pub fn depth_from_quote(key: &QuoteKey, q: &Value) -> MarketDepth {
    let pad = |side: Vec<Value>| -> Vec<DepthLevel> {
        let mut v: Vec<DepthLevel> = side
            .iter()
            .take(5)
            .map(|l| DepthLevel {
                price: n(l, "price"),
                quantity: n(l, "quantity") as i64,
                orders: n(l, "orders") as i64,
            })
            .collect();
        v.resize(5, DepthLevel::default());
        v
    };
    let bids = pad(levels(q, "buy"));
    let asks = pad(levels(q, "sell"));
    let level_sum = |side: &[DepthLevel]| {
        side.iter()
            .map(|l| l.quantity)
            .filter(|q| *q > 0)
            .sum::<i64>()
    };
    let tb = n(q, "total_buy") as i64;
    let ts = n(q, "total_sell") as i64;
    let ohlc = q.get("ohlc").cloned().unwrap_or(Value::Null);
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: if tb != 0 { tb } else { level_sum(&bids) },
        total_sell_qty: if ts != 0 { ts } else { level_sum(&asks) },
        bids,
        asks,
        ltp: n(q, "ltp"),
        ltq: n(q, "last_traded_quantity") as i64,
        open: n(&ohlc, "open"),
        high: n(&ohlc, "high"),
        low: n(&ohlc, "low"),
        prev_close: n(&ohlc, "close"),
        volume: n(q, "last_volume") as i64,
        oi: n(q, "open_int") as i64,
    }
}

pub async fn get_quote(b: &KotakBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let rows = quote_rows(b, auth, key).await?;
    Ok(match rows.first() {
        Some(q) => quote_from_row(key, q),
        None => Quote {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            ..Default::default()
        },
    })
}

pub async fn get_market_depth(
    b: &KotakBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let rows = quote_rows(b, auth, key).await?;
    Ok(match rows.first() {
        Some(q) => depth_from_quote(key, q),
        None => MarketDepth {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            bids: vec![DepthLevel::default(); 5],
            asks: vec![DepthLevel::default(); 5],
            ..Default::default()
        },
    })
}

/// Match answer rows back to the queries (web `_process_quotes_batch`):
/// `<exchange>|<exchange_token>`, then `<exchange>|<display_symbol without
/// -EQ/-IN>`, then the same keys case-insensitively.
pub fn match_multiquotes<'a>(queries: &[String], rows: &'a [Value]) -> Vec<Option<&'a Value>> {
    let mut lookup: Vec<(String, &Value)> = Vec::new();
    for q in rows {
        let ex = s(q, "exchange");
        lookup.push((format!("{}|{}", ex, s(q, "exchange_token")), q));
        let display = s(q, "display_symbol");
        if !display.is_empty() {
            lookup.push((
                format!("{}|{}", ex, display.replace("-EQ", "").replace("-IN", "")),
                q,
            ));
        }
    }
    queries
        .iter()
        .map(|query| {
            lookup
                .iter()
                .find(|(k, _)| k == query)
                .or_else(|| lookup.iter().find(|(k, _)| k.eq_ignore_ascii_case(query)))
                .map(|(_, v)| *v)
        })
        .collect()
}

pub async fn get_multiquotes(
    b: &KotakBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let sess = KotakSession::parse(auth)?;
    // One query per key (an index uses its first name only).
    let queries: Vec<std::result::Result<String, String>> = keys
        .iter()
        .map(|k| {
            if is_index(&k.exchange) {
                let seg = kotak_segment(&k.exchange).unwrap_or("nse_cm");
                Ok(format!("{}|{}", seg, index_candidates(&k.symbol)[0]))
            } else {
                neo_key(b, k).map_err(|e| e.client_message())
            }
        })
        .collect();
    let mut results: Vec<Option<QuoteResult>> = vec![None; keys.len()];
    let valid: Vec<usize> = (0..keys.len()).filter(|i| queries[*i].is_ok()).collect();
    let mut batches_ok = 0usize;
    let batches: Vec<&[usize]> = valid.chunks(MULTIQUOTE_BATCH).collect();
    for (bi, batch) in batches.iter().enumerate() {
        if bi > 0 {
            tokio::time::sleep(MULTIQUOTE_DELAY).await;
        }
        let qs: Vec<String> = batch
            .iter()
            .filter_map(|i| queries[*i].as_ref().ok().cloned())
            .collect();
        match quotes_request(b, &sess, &qs.join(",")).await? {
            Ok(rows) => {
                batches_ok += 1;
                for (pos, matched) in match_multiquotes(&qs, &rows).into_iter().enumerate() {
                    let i = batch[pos];
                    let k = &keys[i];
                    results[i] = Some(match matched {
                        Some(q) => QuoteResult {
                            symbol: k.symbol.clone(),
                            exchange: k.exchange.clone(),
                            data: Some(quote_from_row(k, q)),
                            error: None,
                        },
                        None => QuoteResult {
                            symbol: k.symbol.clone(),
                            exchange: k.exchange.clone(),
                            data: None,
                            error: Some("No quote data available".into()),
                        },
                    });
                }
            }
            Err(m) => {
                tracing::warn!("Kotak multiquote batch failed: {}", m);
                for i in batch.iter() {
                    let k = &keys[*i];
                    results[*i] = Some(QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: None,
                        error: Some("Kotak did not return quotes for this batch".into()),
                    });
                }
            }
        }
    }
    if !batches.is_empty() && batches_ok == 0 {
        return Err(AppError::Broker(
            "Kotak did not return quotes. Try again shortly.".into(),
        ));
    }
    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| QuoteResult {
                symbol: keys[i].symbol.clone(),
                exchange: keys[i].exchange.clone(),
                data: None,
                error: Some(
                    queries[i]
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "Could not resolve broker symbol".into()),
                ),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Days per history request (web `HISTORY_CHUNK_DAYS`).
pub fn history_chunk_days(resolution: &str) -> i64 {
    match resolution {
        "1min" | "3min" | "5min" => 30,
        "10min" | "15min" => 60,
        "30min" | "60min" => 90,
        _ => 180,
    }
}

fn neo_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let mut list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            list.sort();
            AppError::Validation(format!(
                "Unsupported timeframe: {}. Supported: {}",
                interval,
                list.join(", ")
            ))
        })
}

/// web `_is_no_data_fault`.
pub fn is_no_data_fault(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "no data found",
        "data not available",
        "no data is available",
        "market has not yet opened",
    ]
    .iter()
    .any(|p| m.contains(p))
}

/// Pad positional rows to seven columns; rows shorter than five are
/// dropped (web `_normalize_candles`).
pub fn normalize_candles(rows: &[Value]) -> Vec<Vec<Value>> {
    rows.iter()
        .filter_map(|r| {
            let a = r.as_array()?;
            if a.len() < 5 {
                tracing::warn!("Kotak history: skipping a malformed candle row");
                return None;
            }
            let mut v: Vec<Value> = a.iter().take(7).cloned().collect();
            while v.len() < 7 {
                v.push(Value::from(0));
            }
            Some(v)
        })
        .collect()
}

/// A candle timestamp: ISO 8601 with `+0530` as the true epoch; `D`/`W`
/// shifted 5:30 and floored to the day (web `get_history`).
pub fn history_timestamp(raw: &Value, daily: bool) -> Option<i64> {
    let ts = match raw {
        Value::String(s) => parse_iso_epoch(s.trim()).or_else(|| s.trim().parse::<i64>().ok())?,
        Value::Number(n) => n.as_i64()?,
        _ => return None,
    };
    Some(if daily {
        (ts + IST_OFFSET_SECS).div_euclid(86_400) * 86_400
    } else {
        ts
    })
}

fn cell(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Rows -> candles: OHLC rows with a missing value are dropped, high/low
/// widened to cover open/close, negative volume zeroed (web
/// `_repair_candles`).
pub fn repair_candles(rows: &[Vec<Value>], daily: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let ts = history_timestamp(&r[0], daily)?;
            let (o, h, l, c) = (cell(&r[1])?, cell(&r[2])?, cell(&r[3])?, cell(&r[4])?);
            let hi = o.max(h).max(l).max(c);
            let lo = o.min(h).min(l).min(c);
            let vol = cell(&r[5]).unwrap_or(0.0) as i64;
            Some(Candle {
                timestamp: ts,
                open: o,
                high: hi,
                low: lo,
                close: c,
                volume: vol.max(0),
                oi: cell(&r[6]).unwrap_or(0.0) as i64,
            })
        })
        .collect()
}

fn history_segment(b: &KotakBroker, key: &QuoteKey) -> Result<String> {
    let br = b.resolver().brexchange(&key.symbol, &key.exchange);
    let seg = br
        .and_then(|br| kotak_segment(&br).map(str::to_string).or(Some(br)))
        .filter(|s| !s.is_empty())
        .or_else(|| kotak_segment(&key.exchange).map(str::to_string))
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Kotak has no historical data for the {} exchange.",
                key.exchange
            ))
        })?;
    // Cash rows of an index store the OpenAlgo exchange as brexchange.
    let seg = match seg.as_str() {
        "NSE_INDEX" => "nse_cm".to_string(),
        "BSE_INDEX" => "bse_cm".to_string(),
        _ => seg,
    };
    if !HISTORY_SEGMENTS.contains(&seg.as_str()) {
        return Err(AppError::Validation(format!(
            "Kotak Neo serves historical data for NSE, BSE, NFO, BFO, NSE_INDEX and BSE_INDEX only. {} is quote-only.",
            key.exchange
        )));
    }
    Ok(seg)
}

/// Neo symbol keys for history, best first (web `_history_neosymbols`).
fn history_candidates(b: &KotakBroker, key: &QuoteKey, seg: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    if let Some(tok) = b.resolver().token(&key.symbol, &key.exchange) {
        out.push(format!("{}|{}", seg, tok.trim()));
    }
    if is_index(&key.exchange) {
        for name in index_candidates(&key.symbol) {
            for variant in [name.clone(), name.to_ascii_uppercase()] {
                let k = format!("{}|{}", seg, variant);
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(AppError::Validation(format!(
            "Symbol {} was not found on {}. Download the master contract again.",
            key.symbol, key.exchange
        )));
    }
    Ok(out)
}

/// One history request; `Ok(rows)` (possibly empty for a no-data fault).
async fn history_chunk(
    b: &KotakBroker,
    sess: &KotakSession,
    neosymbol: &str,
    resolution: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Value>> {
    let url = format!(
        "{}/market-data/1.0/historical/details?neosymbol={}&fromdate={}&todate={}&interval={}",
        sess.base_url,
        py_quote(neosymbol, "|"),
        from.format("%Y-%m-%d"),
        to.format("%Y-%m-%d"),
        resolution
    );
    let mut attempt = 0u32;
    let (status, v) = loop {
        b.history_pacer.acquire().await?;
        let resp = b
            .http
            .get(&url)
            .header("Authorization", &sess.access_token)
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            if attempt >= HISTORY_MAX_RETRIES {
                return Err(AppError::Broker(
                    "Kotak is limiting history requests right now. Wait a moment and try again."
                        .into(),
                ));
            }
            let wait = retry_after(resp.headers()).unwrap_or_else(|| b.retry_base * (1 << attempt));
            drop(resp);
            tokio::time::sleep(wait).await;
            attempt += 1;
            continue;
        }
        let bytes = resp.bytes().await?;
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => break (status, v),
            Err(_) => {
                tracing::warn!(status = status.as_u16(), "Kotak history answer is not JSON");
                return Err(AppError::Broker(
                    "Kotak sent a history answer OpenAlgo could not read. Try again shortly."
                        .into(),
                ));
            }
        }
    };
    if !v.is_object() {
        return Err(AppError::Broker(
            "Kotak sent a history answer OpenAlgo could not read. Try again shortly.".into(),
        ));
    }
    let fault = v
        .get("fault")
        .map(|f| s(f, "message"))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| s(&v, "emsg"));
    if is_no_data_fault(&fault) {
        return Ok(Vec::new());
    }
    if status != StatusCode::OK || !s(&v, "status").eq_ignore_ascii_case("success") {
        tracing::warn!(status = status.as_u16(), "Kotak history refused: {}", fault);
        return Err(kotak_error(status, &v, "Kotak did not return history."));
    }
    Ok(v.get("data")
        .and_then(|d| d.get("candles"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Earliest date Neo serves: IST today minus five years plus one day.
pub fn earliest_start(today_ist: NaiveDate) -> NaiveDate {
    use chrono::Datelike;
    let y = today_ist.year() - HISTORY_MAX_LOOKBACK_YEARS;
    let base = NaiveDate::from_ymd_opt(y, today_ist.month(), today_ist.day())
        .or_else(|| NaiveDate::from_ymd_opt(y, today_ist.month(), 28))
        .unwrap_or(today_ist);
    base + Days::days(1)
}

pub async fn get_history(
    b: &KotakBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = neo_interval(&req.interval)?;
    let sess = KotakSession::parse(auth)?;
    let seg = history_segment(b, &req.key)?;
    let candidates = history_candidates(b, &req.key, &seg)?;
    if req.start > req.end {
        return Err(AppError::Validation(format!(
            "The start date {} is after the end date {}.",
            req.start, req.end
        )));
    }
    let today = (chrono::Utc::now() + chrono::Duration::seconds(IST_OFFSET_SECS)).date_naive();
    let earliest = earliest_start(today);
    if req.end < earliest {
        return Ok(Vec::new());
    }
    let start = req.start.max(earliest);
    let daily = matches!(resolution, "D" | "W");
    let chunk = history_chunk_days(resolution);
    let mut resolved: Option<String> = None;
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut cur = start;
    while cur <= req.end {
        let ce = (cur + Days::days(chunk - 1)).min(req.end);
        let attempts: Vec<String> = match &resolved {
            Some(r) => vec![r.clone()],
            None => candidates.clone(),
        };
        let mut got: Option<Vec<Value>> = None;
        let mut errors: Vec<String> = Vec::new();
        for ns in attempts {
            match history_chunk(b, &sess, &ns, resolution, cur, ce).await {
                Ok(r) => {
                    let found = !r.is_empty();
                    got = Some(r);
                    if found {
                        resolved = Some(ns);
                        break;
                    }
                }
                Err(e) => {
                    if matches!(e, AppError::Auth(_)) {
                        return Err(e);
                    }
                    errors.push(e.client_message());
                }
            }
        }
        match got {
            Some(r) => rows.extend(normalize_candles(&r)),
            None => {
                tracing::warn!(
                    "Kotak history failed for {}:{} {} to {}: {}",
                    req.key.exchange,
                    req.key.symbol,
                    cur,
                    ce,
                    errors.join("; ")
                );
                return Err(AppError::Broker(format!(
                    "Kotak did not return history for {} to {}. Try again shortly.",
                    cur, ce
                )));
            }
        }
        cur = ce + Days::days(1);
    }
    Ok(sort_dedupe(repair_candles(&rows, daily)))
}
