//! Quotes, multiquotes, depth and history (web `api/data.py`, aligned
//! with Groww's API docs in #2194).

use super::mapping::{check_data_exchange, groww_exchange, groww_segment, SEGMENT_FNO};
use super::{error_message, groww_error, Category, GrowwCore, Reply, TIMEFRAME_MAP};
use crate::brokers::common::history::sort_dedupe;
use crate::brokers::common::redact::url_safe_error;
use crate::brokers::common::streaming::round2;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Duration as CDuration, NaiveDate, NaiveDateTime, TimeZone};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;

/// Instruments per `/v1/live-data/ohlc` call ("up to 50").
pub const OHLC_BATCH: usize = 50;
/// Times a batch is retried after dropping a symbol Groww calls invalid.
pub const INVALID_SYMBOL_RETRIES: usize = 5;
/// Consecutive rate-limit refusals that end the F&O quote overlay.
pub const MAX_CONSECUTIVE_429: usize = 4;

/// Number from a JSON value (number or numeric string), 0 otherwise.
pub fn to_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Python `a or b or c` over numeric keys: the first non-zero value.
fn first_num(v: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .map(|k| to_f64(v.get(*k)))
        .find(|x| *x != 0.0)
        .unwrap_or(0.0)
}

/// Groww `ohlc`: a dict, or a non-JSON string such as
/// `"{open: 149.50,high: 150.50,low: 148.50,close: 149.50}"` (quirk 9.14).
pub fn parse_ohlc(v: Option<&Value>) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    match v {
        Some(Value::Object(m)) => {
            for (k, x) in m {
                out.insert(k.clone(), to_f64(Some(x)));
            }
        }
        Some(Value::String(s)) => {
            for part in s.trim().trim_matches(|c| c == '{' || c == '}').split(',') {
                let kv: Vec<&str> = part.split(':').collect();
                if kv.len() == 2 {
                    if let Ok(x) = kv[1].trim().parse::<f64>() {
                        out.insert(kv[0].trim().trim_matches('"').to_string(), x);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn depth_side(p: &Value, side: &str) -> Vec<DepthLevel> {
    p.pointer(&format!("/depth/{}", side))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|l| DepthLevel {
                    price: to_f64(l.get("price")),
                    quantity: to_f64(l.get("quantity")) as i64,
                    orders: to_f64(l.get("orders")) as i64,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_derivative(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO")
}

/// `/v1/live-data/quote` payload -> web quote (`prev_close` = `ohlc.close`;
/// OI only for derivatives; bid/ask fall back to the top of the book).
pub fn to_quote(key: &QuoteKey, p: &Value) -> Quote {
    let ohlc = parse_ohlc(p.get("ohlc"));
    let o = |k: &str| ohlc.get(k).copied().unwrap_or(0.0);
    let buy = depth_side(p, "buy");
    let sell = depth_side(p, "sell");
    let mut bid = first_num(p, &["bid_price", "bid", "best_bid_price"]);
    let mut ask = first_num(
        p,
        &["offer_price", "ask", "best_offer_price", "best_ask_price"],
    );
    let mut bid_qty = first_num(p, &["bid_quantity", "bid_size", "best_bid_quantity"]);
    let mut ask_qty = first_num(
        p,
        &[
            "offer_quantity",
            "ask_quantity",
            "ask_size",
            "offer_size",
            "best_offer_quantity",
        ],
    );
    if bid == 0.0 {
        if let Some(l) = buy.first() {
            bid = l.price;
            if bid_qty == 0.0 {
                bid_qty = l.quantity as f64;
            }
        }
    }
    if ask == 0.0 {
        if let Some(l) = sell.first() {
            ask = l.price;
            if ask_qty == 0.0 {
                ask_qty = l.quantity as f64;
            }
        }
    }
    let ltp = to_f64(p.get("last_price"));
    let close = o("close");
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: o("open"),
        high: o("high"),
        low: o("low"),
        close,
        volume: first_num(p, &["volume", "total_volume", "traded_volume"]) as i64,
        bid,
        ask,
        bid_qty: bid_qty as i64,
        ask_qty: ask_qty as i64,
        oi: if is_derivative(&key.exchange) {
            first_num(p, &["open_interest", "oi"]) as i64
        } else {
            0
        },
        change: to_f64(p.get("day_change")),
        change_percent: to_f64(p.get("day_change_perc")),
        timestamp: match p.get("last_trade_time") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
    };
    if q.change == 0.0 && close > 0.0 && ltp > 0.0 {
        q.change = round2(ltp - close);
        q.change_percent = round2((ltp - close) / close * 100.0);
    }
    q
}

/// Web `get_depth`: five levels per side, padded with zero levels.
pub fn to_depth(key: &QuoteKey, p: &Value) -> MarketDepth {
    let ohlc = parse_ohlc(p.get("ohlc"));
    let o = |k: &str| ohlc.get(k).copied().unwrap_or(0.0);
    let pad = |mut v: Vec<DepthLevel>| {
        v.truncate(5);
        v.resize(5, DepthLevel::default());
        v.into_iter()
            .map(|l| DepthLevel { orders: 0, ..l })
            .collect::<Vec<_>>()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(depth_side(p, "buy")),
        asks: pad(depth_side(p, "sell")),
        ltp: to_f64(p.get("last_price")),
        ltq: to_f64(p.get("last_trade_quantity")) as i64,
        open: o("open"),
        high: o("high"),
        low: o("low"),
        prev_close: o("close"),
        volume: first_num(p, &["volume", "total_volume", "traded_volume"]) as i64,
        oi: if is_derivative(&key.exchange) {
            first_num(p, &["open_interest", "oi"]) as i64
        } else {
            0
        },
        total_buy_qty: to_f64(p.get("total_buy_quantity")) as i64,
        total_sell_qty: to_f64(p.get("total_sell_quantity")) as i64,
    }
}

/// Web `_convert_openalgo_to_groww_derivative_symbol` (last resort when the
/// master has no row): `SBIN30SEP25FUT` -> `SBIN25SEPFUT`,
/// `SBIN30SEP25800CE` -> `SBIN25SEP800CE`.
pub fn derivative_symbol_fallback(symbol: &str) -> String {
    let alpha_end = symbol
        .char_indices()
        .find(|(_, c)| !c.is_ascii_uppercase())
        .map(|(i, _)| i)
        .unwrap_or(symbol.len());
    let (base, rest) = symbol.split_at(alpha_end);
    if base.is_empty() || rest.len() < 7 || !rest.is_ascii() {
        return symbol.to_string();
    }
    let (dd, mon, yy, tail) = (&rest[0..2], &rest[2..5], &rest[5..7], &rest[7..]);
    let ok = dd.bytes().all(|b| b.is_ascii_digit())
        && mon.bytes().all(|b| b.is_ascii_uppercase())
        && yy.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return symbol.to_string();
    }
    if tail == "FUT" {
        return format!("{}{}{}FUT", base, yy, mon);
    }
    for opt in ["CE", "PE"] {
        if let Some(strike) = tail.strip_suffix(opt) {
            if !strike.is_empty() && strike.bytes().all(|b| b.is_ascii_digit()) {
                return format!("{}{}{}{}{}", base, yy, mon, strike, opt);
            }
        }
    }
    symbol.to_string()
}

/// Groww trading symbol for an OpenAlgo instrument.
pub fn trading_symbol(core: &GrowwCore, key: &QuoteKey) -> String {
    match core.symbols.br_symbol(&key.symbol, &key.exchange) {
        Some(s) => s,
        None if is_derivative(&key.exchange) => derivative_symbol_fallback(&key.symbol),
        None => key.symbol.clone(),
    }
}

/// `exchange`, `segment` and `trading_symbol` query for one instrument.
/// The web sent `BSE_INDEX` to NSE (default branch); it goes to BSE here.
fn quote_query(core: &GrowwCore, key: &QuoteKey) -> String {
    format!(
        "exchange={}&segment={}&trading_symbol={}",
        groww_exchange(&key.exchange),
        groww_segment(&key.exchange),
        urlencoding::encode(&trading_symbol(core, key))
    )
}

async fn fetch_quote(core: &GrowwCore, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
    check_data_exchange(&key.exchange)?;
    core.call(
        Method::GET,
        &format!("/v1/live-data/quote?{}", quote_query(core, key)),
        auth,
        None,
        Category::Live,
    )
    .await
}

pub async fn get_quote(core: &GrowwCore, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let p = fetch_quote(core, auth, key).await?;
    Ok(to_quote(key, &p))
}

pub async fn get_market_depth(
    core: &GrowwCore,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let p = fetch_quote(core, auth, key).await?;
    Ok(to_depth(key, &p))
}

/// One multiquote row (web `_fetch_ohlc_batch`): `ltp` is the live price
/// from `/v1/live-data/ltp`; the `/v1/live-data/ohlc` entry (a dict, an
/// OHLC string) gives open/high/low and its `close`, which is the previous
/// session's close. An entry that is missing or a bare number leaves the
/// OHLC fields at 0; the live price still stands.
pub fn quote_from_ohlc(key: &QuoteKey, ohlc: Option<&Value>, ltp: f64) -> Quote {
    let (open, high, low, close) = match ohlc {
        Some(v @ (Value::Object(_) | Value::String(_))) => {
            let o = parse_ohlc(Some(v));
            let g = |k: &str| o.get(k).copied().unwrap_or(0.0);
            (g("open"), g("high"), g("low"), g("close"))
        }
        _ => (0.0, 0.0, 0.0, 0.0),
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open,
        high,
        low,
        close,
        ..Default::default()
    }
}

/// A multiquote entry's LTP from `/v1/live-data/ltp`: a number, or `None`
/// when Groww gave none.
fn ltp_of(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// `Invalid trading symbol: XYZ` in a Groww error -> `XYZ`.
pub fn invalid_symbol(text: &str) -> Option<String> {
    const MARK: &str = "Invalid trading symbol: ";
    let i = text.find(MARK)? + MARK.len();
    let s: String = text[i..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '&' | '-'))
        .collect();
    (!s.is_empty()).then_some(s)
}

/// `NSE_SBIN` / `BSE_RELIANCE` key used by the OHLC endpoint.
pub fn exchange_symbol(core: &GrowwCore, key: &QuoteKey) -> String {
    format!(
        "{}_{}",
        groww_exchange(&key.exchange),
        trading_symbol(core, key)
    )
}

/// One multiquote entry: the OHLC value and the live price, or why there
/// is none.
type OhlcEntry = std::result::Result<(Option<Value>, f64), String>;

/// A live-data call for a multiquote batch. A transport failure is that
/// batch's error (web: an error per symbol), not the whole request's; a
/// refused session or OpenAlgo's own pacing refusal still ends it.
async fn live_call(
    core: &GrowwCore,
    auth: &AuthToken,
    path: &str,
) -> Result<std::result::Result<Reply, String>> {
    match core
        .send(Method::GET, path, auth, None, Category::Live)
        .await
    {
        Ok(r) => Ok(Ok(r)),
        Err(AppError::Http(e)) => {
            tracing::warn!("Groww live data could not be read: {}", url_safe_error(&e));
            Ok(Err(
                "Could not reach Groww for a quote. Try again shortly.".to_string()
            ))
        }
        Err(e) => Err(e),
    }
}

/// OHLC for one segment's instruments, dropping symbols Groww reports as
/// invalid and retrying (at most `INVALID_SYMBOL_RETRIES` times).
async fn ohlc_batch(
    core: &GrowwCore,
    auth: &AuthToken,
    segment: &str,
    mut wanted: Vec<String>,
    out: &mut HashMap<String, OhlcEntry>,
) -> Result<()> {
    for _ in 0..=INVALID_SYMBOL_RETRIES {
        if wanted.is_empty() {
            return Ok(());
        }
        let path = format!(
            "/v1/live-data/ohlc?segment={}&exchange_symbols={}",
            segment,
            urlencoding::encode(&wanted.join(","))
        );
        let r = match live_call(core, auth, &path).await? {
            Ok(r) => r,
            Err(why) => {
                for es in &wanted {
                    out.insert(es.clone(), Err(why.clone()));
                }
                return Ok(());
            }
        };
        if r.is_success() {
            // The OHLC close is the previous session's close, so the live
            // price comes from the LTP endpoint (08-live-data "Get LTP").
            let ltp_path = format!(
                "/v1/live-data/ltp?segment={}&exchange_symbols={}",
                segment,
                urlencoding::encode(&wanted.join(","))
            );
            let (ltps, no_price) = match live_call(core, auth, &ltp_path).await? {
                Ok(lr) if lr.is_success() && lr.payload().is_object() => (
                    lr.payload().clone(),
                    "Groww returned no live price for this symbol".to_string(),
                ),
                Ok(lr) => {
                    let why = error_message(&lr.body);
                    tracing::warn!("Groww LTP batch failed for {}: {}", segment, why);
                    (
                        Value::Null,
                        if why.is_empty() {
                            "Groww did not return live prices for this symbol.".to_string()
                        } else {
                            format!("Groww did not return live prices: {}", why)
                        },
                    )
                }
                Err(why) => (Value::Null, why),
            };
            for es in &wanted {
                let ohlc = r.payload().get(es).filter(|v| !v.is_null());
                match ltp_of(ltps.get(es)) {
                    Some(ltp) => {
                        if !matches!(ohlc, Some(Value::Object(_) | Value::String(_))) {
                            tracing::warn!("No OHLC breakdown for {}", es);
                        }
                        out.insert(es.clone(), Ok((ohlc.cloned(), ltp)));
                    }
                    None => {
                        out.insert(es.clone(), Err(no_price.clone()));
                    }
                }
            }
            return Ok(());
        }
        let text = r.body.to_string();
        match invalid_symbol(&text) {
            Some(bad) => {
                let before = wanted.len();
                wanted.retain(|es| {
                    let drop = es.split_once('_').map(|(_, s)| s) == Some(bad.as_str());
                    if drop {
                        out.insert(es.clone(), Err("Invalid trading symbol in Groww".into()));
                    }
                    !drop
                });
                if wanted.len() == before {
                    break;
                }
            }
            None => {
                let msg = match groww_error(&r) {
                    AppError::Broker(m) => m,
                    e => e.client_message(),
                };
                for es in &wanted {
                    out.insert(es.clone(), Err(msg.clone()));
                }
                return Ok(());
            }
        }
    }
    for es in wanted {
        out.entry(es)
            .or_insert_with(|| Err("Groww refused the quote request.".into()));
    }
    Ok(())
}

/// Web `get_multiquotes`: OHLC batches of 50 per segment, then for F&O a
/// paced per-symbol quote overlay for bid/ask/volume/OI that stops after
/// four consecutive rate-limit refusals.
pub async fn get_multiquotes(
    core: &GrowwCore,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let es: Vec<String> = keys.iter().map(|k| exchange_symbol(core, k)).collect();
    let mut data: HashMap<String, OhlcEntry> = HashMap::new();
    // Exchanges Groww has no data for are refused per symbol, not sent.
    let refused: Vec<Option<String>> = keys
        .iter()
        .map(|k| {
            check_data_exchange(&k.exchange)
                .err()
                .map(|e| e.client_message())
        })
        .collect();
    for batch in (0..keys.len()).collect::<Vec<_>>().chunks(OHLC_BATCH) {
        for segment in ["CASH", SEGMENT_FNO] {
            let mut wanted: Vec<String> = batch
                .iter()
                .filter(|i| refused[**i].is_none())
                .filter(|i| groww_segment(&keys[**i].exchange) == segment)
                .map(|i| es[*i].clone())
                .filter(|e| !data.contains_key(e))
                .collect();
            wanted.dedup();
            ohlc_batch(core, auth, segment, wanted, &mut data).await?;
        }
    }
    let mut out: Vec<QuoteResult> = keys
        .iter()
        .zip(&es)
        .zip(&refused)
        .map(|((k, e), refused)| match data.get(e) {
            _ if refused.is_some() => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: refused.clone(),
            },
            Some(Ok((ohlc, ltp))) => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: Some(quote_from_ohlc(k, ohlc.as_ref(), *ltp)),
                error: None,
            },
            Some(Err(m)) => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some(m.clone()),
            },
            None => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some("No quote data available".into()),
            },
        })
        .collect();
    // F&O overlay.
    let mut consecutive = 0;
    for r in out.iter_mut().filter(|r| is_derivative(&r.exchange)) {
        let Some(base) = r.data.as_mut() else {
            continue;
        };
        if consecutive >= MAX_CONSECUTIVE_429 {
            break;
        }
        let key = QuoteKey::new(r.exchange.clone(), r.symbol.clone());
        match fetch_quote(core, auth, &key).await {
            Ok(p) => {
                consecutive = 0;
                let full = to_quote(&key, &p);
                base.bid = full.bid;
                base.ask = full.ask;
                base.bid_qty = full.bid_qty;
                base.ask_qty = full.ask_qty;
                base.volume = full.volume;
                base.oi = full.oi;
            }
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                let m = e.client_message();
                if m == super::rate_limiter::BUSY_MESSAGE {
                    // OpenAlgo's own pacing refused it: the rest would be
                    // refused too, so the overlay stops here.
                    break;
                }
                if m.contains("limiting requests") || m.contains("429") || m.contains("Rate limit")
                {
                    consecutive += 1;
                } else {
                    consecutive = 0;
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Groww `candle_interval` for an OpenAlgo interval.
pub fn candle_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Groww does not provide {} candles. Supported intervals: {}.",
                interval,
                list.join(", ")
            ))
        })
}

/// Length in minutes of an intraday candle interval.
pub fn interval_minutes(candle_interval: &str) -> Option<i64> {
    Some(match candle_interval {
        "1minute" => 1,
        "2minute" => 2,
        "3minute" => 3,
        "5minute" => 5,
        "10minute" => 10,
        "15minute" => 15,
        "30minute" => 30,
        "1hour" => 60,
        "4hour" => 240,
        _ => return None,
    })
}

/// Longest range one request may span, in days (web `_MAX_DAYS`: candles
/// 1-5 min 30 days, 10-30 min 90, 1 hour+ 180; candle/range daily 1080,
/// weekly unlimited).
pub fn max_days(candle_interval: &str) -> i64 {
    match candle_interval {
        "1minute" | "2minute" | "3minute" | "5minute" => 30,
        "10minute" | "15minute" | "30minute" => 90,
        "1hour" | "4hour" => 180,
        "1day" => 1080,
        _ => 3650,
    }
}

/// Day ranges of at most `max_days` covering `start..=end`.
pub fn date_chunks(start: NaiveDate, end: NaiveDate, max_days: i64) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = Vec::new();
    let mut from = start;
    while from <= end {
        let to = (from + CDuration::days(max_days.max(1) - 1)).min(end);
        out.push((from, to));
        from = to + CDuration::days(1);
    }
    out
}

/// Where a history request goes (web `_groww_symbol`): Groww exchange,
/// segment, `groww_symbol` (`EXCHANGE-SYMBOL` for stocks and indices,
/// `EXCHANGE-UNDERLYING-DDMonYY-FUT` / `-STRIKE-CE|PE` for F&O, built from
/// the master contract) and the trading symbol.
pub fn history_target(
    core: &GrowwCore,
    key: &QuoteKey,
) -> Result<(String, &'static str, String, String)> {
    check_data_exchange(&key.exchange)?;
    let info = core.symbols.by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "{} is not in the {} master contract. Check the symbol, or download the master contract again.",
            key.symbol, key.exchange
        ))
    })?;
    let gex = if info.brexchange.is_empty() {
        groww_exchange(&key.exchange).to_string()
    } else {
        info.brexchange.clone()
    };
    let br = info.br_symbol().to_string();
    if !matches!(key.exchange.as_str(), "NFO" | "BFO") {
        return Ok((gex.clone(), "CASH", format!("{}-{}", gex, br), br));
    }
    let not_fno = || {
        AppError::Validation(format!(
            "{} is not in OpenAlgo F&O format; download the master contract again.",
            key.symbol
        ))
    };
    let expiry =
        NaiveDate::parse_from_str(&title_month(&info.expiry), "%d-%b-%y").map_err(|_| not_fno())?;
    let underlying = fno_underlying(&info.symbol).ok_or_else(not_fno)?;
    let exp = expiry.format("%d%b%y").to_string();
    let groww_symbol = if info.instrument_type == "FUT" {
        format!("{}-{}-{}-FUT", gex, underlying, exp)
    } else {
        let strike = if info.strike.fract() == 0.0 {
            format!("{}", info.strike as i64)
        } else {
            format!("{}", info.strike)
        };
        format!(
            "{}-{}-{}-{}-{}",
            gex, underlying, exp, strike, info.instrument_type
        )
    };
    Ok((gex, SEGMENT_FNO, groww_symbol, br))
}

/// `28-OCT-25` -> `28-Oct-25` so `%b` parses it.
fn title_month(expiry: &str) -> String {
    let mut out = String::with_capacity(expiry.len());
    let mut prev_alpha = false;
    for c in expiry.chars() {
        if c.is_ascii_alphabetic() && prev_alpha {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
        prev_alpha = c.is_ascii_alphabetic();
    }
    out
}

/// The underlying of an OpenAlgo F&O symbol: the part before its
/// `DDMMMYY` expiry, followed by `FUT` or `STRIKE` + `CE|PE` (web regex
/// `^(.+?)(\d{2}[A-Z]{3}\d{2})(FUT|[\d.]+(CE|PE))$`).
pub fn fno_underlying(symbol: &str) -> Option<&str> {
    let b = symbol.as_bytes();
    for i in 1..b.len() {
        if i + 7 > b.len() {
            break;
        }
        let e = &b[i..i + 7];
        let is_expiry = e[0].is_ascii_digit()
            && e[1].is_ascii_digit()
            && e[2..5].iter().all(u8::is_ascii_uppercase)
            && e[5].is_ascii_digit()
            && e[6].is_ascii_digit();
        if !is_expiry {
            continue;
        }
        let tail = &symbol[i + 7..];
        let ok = tail == "FUT"
            || ["CE", "PE"].iter().any(|t| {
                tail.strip_suffix(t).is_some_and(|k| {
                    !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit() || c == b'.')
                })
            });
        if ok {
            return Some(&symbol[..i]);
        }
    }
    None
}

/// One EOD candle from `/v1/historical/candle/range` `[ts, o, h, l, c, v]`
/// (seconds or milliseconds), stamped at midnight UTC of its IST date. A
/// candle with a missing price is left out rather than drawn at 0; a
/// missing volume reads as 0.
pub fn eod_candle(v: &Value) -> Option<Candle> {
    let a = v.as_array()?;
    let mut ts = to_f64(a.first());
    if ts <= 0.0 {
        return None;
    }
    if ts > 4_102_444_800.0 {
        ts /= 1000.0;
    }
    let day = chrono_tz::Asia::Kolkata
        .timestamp_opt(ts as i64, 0)
        .single()?
        .date_naive();
    Some(Candle {
        timestamp: day.and_hms_opt(0, 0, 0)?.and_utc().timestamp(),
        open: opt_f64(a.get(1))?,
        high: opt_f64(a.get(2))?,
        low: opt_f64(a.get(3))?,
        close: opt_f64(a.get(4))?,
        volume: opt_f64(a.get(5)).map(|v| v as i64).unwrap_or(0),
        oi: 0,
    })
}

/// A session candle from `/v1/historical/candles`: IST wall-clock start,
/// prices (`None` where Groww sent null) and volume.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionCandle {
    pub start: NaiveDateTime,
    pub open: f64,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<i64>,
}

fn opt_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn market_open(t: NaiveDateTime) -> NaiveDateTime {
    t.date().and_hms_opt(9, 15, 0).unwrap_or(t)
}

/// Candles of the regular session (web `get_history`): a candle wholly
/// inside the 09:00-09:15 pre-open is left out, and so is a pre-open
/// fragment with volume but no opening price. Nothing is filled in.
pub fn session_candles(rows: &[Value], fetch_minutes: i64) -> Vec<SessionCandle> {
    rows.iter()
        .filter_map(|r| {
            let a = r.as_array()?;
            let raw = match a.first()? {
                Value::String(s) => s.replace(' ', "T"),
                _ => return None,
            };
            let start = NaiveDateTime::parse_from_str(&raw, "%Y-%m-%dT%H:%M:%S")
                .or_else(|_| NaiveDateTime::parse_from_str(&raw, "%Y-%m-%dT%H:%M"))
                .ok()?;
            if start + CDuration::minutes(fetch_minutes) <= market_open(start) {
                return None;
            }
            let open = opt_f64(a.get(1))?;
            Some(SessionCandle {
                start,
                open,
                high: opt_f64(a.get(2)),
                low: opt_f64(a.get(3)),
                close: opt_f64(a.get(4)),
                volume: opt_f64(a.get(5)).map(|v| v as i64),
            })
        })
        .collect()
}

/// Combine 15-minute session candles into `minutes`-long candles starting
/// at 09:15 each day (web `_rebucket`): first open, highest high, lowest
/// low, last close, summed volume.
pub fn rebucket(mut candles: Vec<SessionCandle>, minutes: i64) -> Vec<SessionCandle> {
    fn pick(a: Option<f64>, b: Option<f64>, f: fn(f64, f64) -> f64) -> Option<f64> {
        match (a, b) {
            (None, x) | (x, None) => x,
            (Some(x), Some(y)) => Some(f(x, y)),
        }
    }
    candles.sort_by_key(|c| c.start);
    let mut out: Vec<SessionCandle> = Vec::new();
    for c in candles {
        let open = market_open(c.start);
        let offset = (c.start - open).num_minutes().div_euclid(minutes) * minutes;
        let key = open + CDuration::minutes(offset);
        match out.last_mut() {
            Some(b) if b.start == key => {
                b.high = pick(b.high, c.high, f64::max);
                b.low = pick(b.low, c.low, f64::min);
                b.close = c.close;
                b.volume = match (b.volume, c.volume) {
                    (None, x) | (x, None) => x,
                    (Some(x), Some(y)) => Some(x + y),
                };
            }
            _ => out.push(SessionCandle { start: key, ..c }),
        }
    }
    out
}

/// Session candles -> OpenAlgo candles at the IST start in epoch seconds.
/// A missing volume reads as 0 (charts reject a null volume); a candle
/// with a missing price is left out rather than filled in.
pub fn to_candles(rows: Vec<SessionCandle>) -> Vec<Candle> {
    rows.into_iter()
        .filter_map(|c| {
            let ts = chrono_tz::Asia::Kolkata
                .from_local_datetime(&c.start)
                .single()?
                .timestamp();
            Some(Candle {
                timestamp: ts,
                open: c.open,
                high: c.high?,
                low: c.low?,
                close: c.close?,
                volume: c.volume.unwrap_or(0),
                oi: 0,
            })
        })
        .collect()
}

/// Every candle row between `start` and `end`, one request per chunk. A
/// refused chunk fails the read with Groww's reason.
async fn fetch_candles(
    core: &GrowwCore,
    auth: &AuthToken,
    symbol: &str,
    path: &str,
    params: &[(&str, String)],
    (start, end): (NaiveDate, NaiveDate),
    max_days: i64,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (from, to) in date_chunks(start, end, max_days) {
        let mut q: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect();
        q.push(format!(
            "start_time={}",
            urlencoding::encode(&format!("{} 00:00:00", from.format("%Y-%m-%d")))
        ));
        q.push(format!(
            "end_time={}",
            urlencoding::encode(&format!("{} 23:59:59", to.format("%Y-%m-%d")))
        ));
        let r = core
            .send(
                Method::GET,
                &format!("{}?{}", path, q.join("&")),
                auth,
                None,
                Category::History,
            )
            .await?;
        if !r.is_success() {
            let why = r.error_message();
            tracing::warn!(
                status = r.status.as_u16(),
                "Groww history for {} refused: {}",
                symbol,
                why
            );
            return Err(if why.is_empty() {
                groww_error(&r)
            } else {
                AppError::Broker(format!(
                    "Groww did not return history for {}: {}",
                    symbol, why
                ))
            });
        }
        if let Some(c) = r.payload().get("candles").and_then(Value::as_array) {
            rows.extend(c.iter().cloned());
        }
    }
    Ok(rows)
}

/// Web `get_history`. Intraday (1m-4h) comes from `/v1/historical/candles`
/// (per-candle volume); 30m/1h/4h are built from 15m candles aligned to
/// 09:15, since Groww starts them on the clock hour and mixes the
/// pre-open in. Daily and weekly come from `/v1/historical/candle/range`,
/// whose EOD candles match NSE's bhavcopy, stamped at midnight UTC of
/// their IST date. Open interest is not taken from history.
pub async fn get_history(
    core: &GrowwCore,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = candle_interval(&req.interval)?;
    let (gex, segment, groww_symbol, trading_symbol) = history_target(core, &req.key)?;
    let symbol = &req.key.symbol;
    let eod = match req.interval.as_str() {
        "D" => Some(("1440", "1day")),
        "W" => Some(("10080", "1week")),
        _ => None,
    };
    if let Some((minutes, name)) = eod {
        let rows = fetch_candles(
            core,
            auth,
            symbol,
            "/v1/historical/candle/range",
            &[
                ("exchange", gex),
                ("segment", segment.to_string()),
                ("trading_symbol", trading_symbol),
                ("interval_in_minutes", minutes.to_string()),
            ],
            (req.start, req.end),
            max_days(name),
        )
        .await?;
        return Ok(sort_dedupe(rows.iter().filter_map(eod_candle).collect()));
    }
    let rebucketed = matches!(interval, "30minute" | "1hour" | "4hour");
    let fetch = if rebucketed { "15minute" } else { interval };
    let rows = fetch_candles(
        core,
        auth,
        symbol,
        "/v1/historical/candles",
        &[
            ("exchange", gex),
            ("segment", segment.to_string()),
            ("groww_symbol", groww_symbol),
            ("candle_interval", fetch.to_string()),
        ],
        (req.start, req.end),
        max_days(fetch),
    )
    .await?;
    let mut session = session_candles(&rows, interval_minutes(fetch).unwrap_or(1));
    if rebucketed {
        session = rebucket(session, interval_minutes(interval).unwrap_or(15));
    }
    Ok(sort_dedupe(to_candles(session)))
}
