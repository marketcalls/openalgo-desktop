//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::mapping::{f, i, noren_exchange, text};
use super::transport::{
    emsg, is_no_data, is_session_error, noren_error, session, stat_ok, Category, Session,
};
use super::NorenBroker;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};

fn lookup(b: &NorenBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// Does a GetQuotes answer describe the instrument asked for? (web
/// `quote_matches_request`: Shoonya intermittently answers with another
/// instrument's snapshot.)
pub fn quote_matches(v: &Value, exch: &str, token: &str) -> bool {
    let got = text(v, "token");
    if got.is_empty() {
        return true;
    }
    if got != token {
        return false;
    }
    let e = text(v, "exch");
    e.is_empty() || e == exch
}

/// `GetQuotes` with the identity guard; `stat == Ok` or an error.
pub(crate) async fn quote_response(
    b: &NorenBroker,
    s: &Session,
    exch: &str,
    token: &str,
) -> Result<Value> {
    let attempts = b.cfg.quote_identity_retries.max(1);
    let mut last = Value::Null;
    for _ in 0..attempts {
        let v = b
            .post_raw(
                "/GetQuotes",
                json!({"exch": exch, "token": token}),
                s,
                Category::Quote,
            )
            .await?;
        if !stat_ok(&v) {
            return Err(noren_error(b.cfg.name, &emsg(&v)));
        }
        if b.cfg.quote_identity_retries == 0 || quote_matches(&v, exch, token) {
            return Ok(v);
        }
        last = v;
    }
    tracing::warn!(
        broker = b.cfg.id,
        "Quote for {}|{} answered for {}|{} on every attempt",
        exch,
        token,
        text(&last, "exch"),
        text(&last, "token")
    );
    Err(AppError::Broker(format!(
        "{} returned a price for a different instrument. Try again in a moment.",
        b.cfg.name
    )))
}

/// Web quote fields from a GetQuotes answer.
pub fn to_quote(key: &QuoteKey, v: &Value) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: f(v, "lp"),
        open: f(v, "o"),
        high: f(v, "h"),
        low: f(v, "l"),
        close: f(v, "c"),
        volume: i(v, "v"),
        bid: f(v, "bp1"),
        ask: f(v, "sp1"),
        bid_qty: i(v, "bq1"),
        ask_qty: i(v, "sq1"),
        oi: i(v, "oi"),
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    };
    if q.close > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

/// Web depth: five levels a side, totals summed over them.
pub fn to_depth(key: &QuoteKey, v: &Value, with_oi: bool) -> MarketDepth {
    let level = |p: &str, q: &str, o: &str, n: usize| DepthLevel {
        price: f(v, &format!("{}{}", p, n)),
        quantity: i(v, &format!("{}{}", q, n)),
        orders: i(v, &format!("{}{}", o, n)),
    };
    let bids: Vec<DepthLevel> = (1..=5).map(|n| level("bp", "bq", "bo", n)).collect();
    let asks: Vec<DepthLevel> = (1..=5).map(|n| level("sp", "sq", "so", n)).collect();
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: bids.iter().map(|l| l.quantity).sum(),
        total_sell_qty: asks.iter().map(|l| l.quantity).sum(),
        bids,
        asks,
        ltp: f(v, "lp"),
        ltq: i(v, "ltq"),
        open: f(v, "o"),
        high: f(v, "h"),
        low: f(v, "l"),
        prev_close: f(v, "c"),
        volume: i(v, "v"),
        oi: if with_oi { i(v, "oi") } else { 0 },
    }
}

pub async fn get_quote(b: &NorenBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = session(b.cfg, auth)?;
    let row = lookup(b, key)?;
    let v = quote_response(b, &s, noren_exchange(&key.exchange), &row.token).await?;
    Ok(to_quote(key, &v))
}

pub async fn get_market_depth(
    b: &NorenBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let s = session(b.cfg, auth)?;
    let row = lookup(b, key)?;
    let v = quote_response(b, &s, noren_exchange(&key.exchange), &row.token).await?;
    Ok(to_depth(key, &v, b.cfg.depth_oi))
}

/// REST fan-out in batches with a pause between batches (there is no
/// bulk quote endpoint on Noren).
pub async fn get_multiquotes(
    b: &NorenBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let s = session(b.cfg, auth)?;
    let mut out = Vec::with_capacity(keys.len());
    let batch = b.cfg.multiquote_batch.max(1);
    for (n, chunk) in keys.chunks(batch).enumerate() {
        if n > 0 && b.cfg.multiquote_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(b.cfg.multiquote_delay_ms)).await;
        }
        let futs = chunk.iter().map(|k| {
            let s = s.clone();
            async move {
                let Some(row) = b.resolver().by_symbol(&k.exchange, &k.symbol) else {
                    return QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: None,
                        error: Some("Could not resolve broker symbol".into()),
                    };
                };
                match quote_response(b, &s, noren_exchange(&k.exchange), &row.token).await {
                    Ok(v) => QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: Some(to_quote(k, &v)),
                        error: None,
                    },
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

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn ist_epoch(d: NaiveDate, h: u32, m: u32, s: u32) -> i64 {
    let naive = d.and_hms_opt(h, m, s).unwrap_or_default();
    Kolkata
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| naive.and_utc().timestamp() - 19800)
}

/// A candle field as the hardened parser reads it (web flattrade
/// `_candle_number`, #2198): `None` when it is missing, null, empty, not a
/// number, NaN or infinite, so a missing price is never charted as 0.
pub fn candle_number(v: &Value, k: &str) -> Option<f64> {
    let n = match v.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    n.is_finite().then_some(n)
}

/// One TPSeries / EODChartData element -> candle. Elements may be JSON
/// strings; all-zero OHLC rows are skipped; `ssboe` wins over `time`
/// (`DD-MM-YYYY HH:MM:SS` IST, or `DD-Mon-YYYY` at UTC midnight). A
/// missing field reads as 0.
pub fn parse_candle(el: &Value) -> Option<Candle> {
    candle_from(el, false)
}

/// `parse_candle` for members with `strict_candles` (flattrade, web
/// #2198): a candle whose open, high, low or close is missing, null, empty,
/// NaN or infinite is skipped instead of charted at 0; a missing or
/// unreadable volume or OI reads as 0.
pub fn parse_candle_strict(el: &Value) -> Option<Candle> {
    candle_from(el, true)
}

fn candle_from(el: &Value, strict: bool) -> Option<Candle> {
    let owned;
    let c = match el {
        Value::String(s) => {
            owned = serde_json::from_str::<Value>(s).ok()?;
            &owned
        }
        Value::Object(_) => el,
        _ => return None,
    };
    let (o, h, l, cl) = if strict {
        (
            candle_number(c, "into")?,
            candle_number(c, "inth")?,
            candle_number(c, "intl")?,
            candle_number(c, "intc")?,
        )
    } else {
        (f(c, "into"), f(c, "inth"), f(c, "intl"), f(c, "intc"))
    };
    if o == 0.0 && h == 0.0 && l == 0.0 && cl == 0.0 {
        return None;
    }
    let whole = |k: &str| {
        if strict {
            candle_number(c, k).map_or(0, |v| v as i64)
        } else {
            i(c, k)
        }
    };
    let ssboe = if strict {
        candle_number(c, "ssboe").map(|v| v as i64)
    } else {
        match c.get("ssboe") {
            Some(v) if !v.is_null() && !text(c, "ssboe").is_empty() => Some(i(c, "ssboe")),
            _ => None,
        }
    };
    let ts = match ssboe {
        Some(ts) => ts,
        None => {
            let t = text(c, "time");
            if let Ok(dt) = NaiveDateTime::parse_from_str(&t, "%d-%m-%Y %H:%M:%S") {
                Kolkata.from_local_datetime(&dt).single()?.timestamp()
            } else {
                let d = NaiveDate::parse_from_str(&t, "%d-%b-%Y").ok()?;
                d.and_hms_opt(0, 0, 0)?.and_utc().timestamp()
            }
        }
    };
    let oi = if c.get("oi").is_some() {
        whole("oi")
    } else {
        whole("intoi")
    };
    Some(Candle {
        timestamp: ts,
        open: o,
        high: h,
        low: l,
        close: cl,
        volume: whole("intv"),
        oi,
    })
}

/// Exchanges whose TPSeries adds a pre-open bar before the 09:15 open
/// (web flattrade `get_history`, #2198; indices query on NSE / BSE).
const PRE_OPEN_EXCHANGES: &[&str] = &["NSE", "BSE", "NFO", "BFO"];

/// Is this bar stamped before 09:15 IST?
pub fn before_session_open(ts: i64) -> bool {
    use chrono::Timelike;
    Kolkata
        .timestamp_opt(ts, 0)
        .single()
        .is_some_and(|t| t.hour() * 60 + t.minute() < 9 * 60 + 15)
}

/// Intraday clean-up for members with `strict_candles` (web flattrade
/// `get_history`, #2198). On NSE/BSE/NFO/BFO (`exch` is the Noren
/// exchange) TPSeries adds a 09:14 bar holding the pre-open discovered
/// price, flat with no bar volume; the session opens at 09:15, so bars
/// before it are dropped (QA HS-07). Volume is floored at 0: during the
/// closing session the cumulative volume switches counters and back, so
/// the bar-to-bar difference goes negative and a chart refuses the whole
/// history on one such bar.
pub fn clean_intraday(candles: &mut Vec<Candle>, exch: &str) {
    if PRE_OPEN_EXCHANGES.contains(&exch) {
        candles.retain(|c| !before_session_open(c.timestamp));
    }
    for c in candles.iter_mut() {
        c.volume = c.volume.max(0);
    }
}

/// Are open, high, low and close all real numbers (no NaN or infinity)?
pub fn has_finite_prices(c: &Candle) -> bool {
    [c.open, c.high, c.low, c.close]
        .iter()
        .all(|v| v.is_finite())
}

/// Make every candle satisfy `low <= open, close <= high`, volume >= 0
/// (web shoonya `_repair_candles`).
pub fn repair(c: &mut Candle) {
    let hi = c.open.max(c.high).max(c.low).max(c.close);
    let lo = c.open.min(c.high).min(c.low).min(c.close);
    c.high = hi;
    c.low = lo;
    if c.volume < 0 {
        c.volume = 0;
    }
}

/// Widen an EOD candle's high/low to cover its open and close (web
/// flattrade `get_history`, #2196): `high = max(high, open, close)`,
/// `low = min(low, open, close)`; volume is left as sent.
pub fn widen_to_open_close(c: &mut Candle) {
    c.high = c.high.max(c.open).max(c.close);
    c.low = c.low.min(c.open).min(c.close);
}

/// Sort by timestamp, duplicates keep the last (web `keep="last"`).
pub fn sort_dedupe_last(mut v: Vec<Candle>) -> Vec<Candle> {
    v.sort_by_key(|c| c.timestamp);
    let mut out: Vec<Candle> = Vec::with_capacity(v.len());
    for c in v {
        match out.last_mut() {
            Some(last) if last.timestamp == c.timestamp => *last = c,
            _ => out.push(c),
        }
    }
    out
}

pub fn noren_interval(b: &NorenBroker, interval: &str) -> Result<&'static str> {
    b.cfg
        .timeframes
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = b.cfg.timeframes.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by {}. Use one of: {}.",
                interval,
                b.cfg.name,
                list.join(", ")
            ))
        })
}

pub async fn get_history(
    b: &NorenBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let intrv = noren_interval(b, &req.interval)?;
    let s = session(b.cfg, auth)?;
    let row = lookup(b, &req.key)?;
    let exch = noren_exchange(&req.key.exchange).to_string();
    let daily = intrv == "D";
    let start_ts = ist_epoch(req.start, 0, 0, 0);
    let end_ts = ist_epoch(req.end, 23, 59, 59);
    let eod_symbol = b
        .cfg
        .eod_index_names
        .iter()
        .find(|((e, sym), _)| *e == req.key.exchange && *sym == req.key.symbol)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| row.br_symbol().to_string());
    let window = b
        .cfg
        .history_window_secs
        .map(|w| w(&req.interval))
        .unwrap_or(i64::MAX / 4);
    let endpoint = if daily { "/EODChartData" } else { "/TPSeries" };
    let mut raw = Vec::new();
    let (mut attempted, mut failed, mut last_err) = (0usize, 0usize, String::new());
    let mut cur = start_ts;
    while cur <= end_ts {
        let chunk_end = cur.saturating_add(window).min(end_ts);
        attempted += 1;
        let body = if daily {
            json!({"sym": format!("{}:{}", exch, eod_symbol), "from": cur.to_string(), "to": chunk_end.to_string()})
        } else {
            json!({"exch": exch, "token": row.token, "st": cur.to_string(), "et": chunk_end.to_string(), "intrv": intrv})
        };
        match b.post_raw(endpoint, body, &s, Category::Data).await {
            Ok(Value::Array(a)) => raw.extend(a),
            Ok(other) => {
                let e = emsg(&other);
                if is_session_error(&e) {
                    return Err(noren_error(b.cfg.name, &e));
                }
                if !is_no_data(&e) && !other.is_null() {
                    failed += 1;
                    last_err = e;
                }
            }
            Err(e) => {
                if matches!(e, AppError::Auth(_)) {
                    return Err(e);
                }
                failed += 1;
                last_err = e.client_message();
            }
        }
        cur = chunk_end + 1;
    }
    if attempted > 0 && failed == attempted {
        tracing::warn!(broker = b.cfg.id, "History failed: {}", last_err);
        return Err(noren_error(b.cfg.name, &last_err));
    }
    let parse = if b.cfg.strict_candles {
        parse_candle_strict
    } else {
        parse_candle
    };
    let mut candles: Vec<Candle> = raw.iter().filter_map(parse).collect();
    if b.cfg.strict_candles {
        let skipped = raw.len() - candles.len();
        if skipped > 0 {
            tracing::warn!(
                broker = b.cfg.id,
                "Skipped {} history rows with a missing or zero price",
                skipped
            );
        }
        if !daily {
            clean_intraday(&mut candles, &exch);
        }
    }
    if daily && b.cfg.eod_widen {
        candles.iter_mut().for_each(widen_to_open_close);
    }
    if daily {
        let today = chrono::Utc::now().with_timezone(&Kolkata).date_naive();
        let today_ts = if b.cfg.today_bar_utc {
            today
                .and_hms_opt(0, 0, 0)
                .unwrap_or_default()
                .and_utc()
                .timestamp()
        } else {
            ist_epoch(today, 0, 0, 0)
        };
        let max = candles.iter().map(|c| c.timestamp).max();
        if today_ts >= start_ts && today_ts <= end_ts && max.is_none_or(|m| m < today_ts) {
            match quote_response(b, &s, &exch, &row.token).await {
                Ok(q) => candles.push(Candle {
                    timestamp: today_ts,
                    open: f(&q, "o"),
                    high: f(&q, "h"),
                    low: f(&q, "l"),
                    close: f(&q, "lp"),
                    volume: i(&q, "v"),
                    oi: i(&q, "oi"),
                }),
                Err(e) => tracing::info!("Today's daily bar from quotes failed: {}", e.code()),
            }
        }
    }
    let mut out = sort_dedupe_last(candles);
    if b.cfg.history_repair {
        // Web shoonya `_repair_candles` (#2161): a bar still missing a price
        // (NaN) has no honest repair and a chart refuses the whole series
        // over it, so it is dropped before the rest are repaired.
        let before = out.len();
        out.retain(has_finite_prices);
        if out.len() < before {
            tracing::warn!(
                broker = b.cfg.id,
                "Dropped {} history bars with a missing price",
                before - out.len()
            );
        }
        out.iter_mut().for_each(repair);
    }
    Ok(out)
}
