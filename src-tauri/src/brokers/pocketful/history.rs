//! History over REST (web `api/data.py` `get_history`, rebuilt in
//! marketcalls/openalgo#2467; the desktop had none for Pocketful). Two
//! endpoints:
//!
//! * `GET /api/v1/charts` (chart-data.md): any exchange, by token, with the
//!   `clientid` header the page lists. It documents one `candletype`, `1`
//!   for one-minute candles, so every interval is built from those: minute
//!   bins counted from the exchange's regular session open (09:15; MCX and
//!   CDS 09:00), daily bars from each IST date.
//! * `POST /api/data/v1/historical` (getting-started.md, "Historical Data
//!   NFO"): NFO futures and options by underlying, expiry, strike and type,
//!   in Pocketful's own intervals ([`TIMEFRAME_MAP`]), with open interest as
//!   a seventh column. An error answer or no candles falls back to the
//!   charts, without OI; a session that ended does not fall back. A daily
//!   series that stops before today gets today's bar from a quote traded
//!   today.
//!
//! Candles are `[time, open, high, low, close, volume(, oi)]` with ISO times
//! at `+0530`. Timestamps come back in epoch seconds: an intraday bar's
//! start, a daily bar's IST date at 00:00 UTC. A chart candle carries no
//! OI, which reads 0 as on the web (history has no way to leave it out).
//! Requests go in chunks (7 days of minute candles, a year of daily ones);
//! a chunk that fails fails the history instead of leaving a gap, and the
//! whole request has one deadline ([`deadline_for`]).
//!
//! Bars are not handed to the session grid (`Broker::history_on_session_grid`
//! stays off): built bars already start at the regular session open, and
//! the grid would fetch the whole minute series again for every 10m, 30m
//! and 1h request (09:15 is not a multiple of those) and drop the NFO bars'
//! open interest. A special session (Muhurat) keeps the regular grid, as on
//! the web.

use super::client::{Answer, Request, HISTORY_TIMEOUT};
use super::data::{self, ends_the_batch, refusal};
use super::market::{self, number};
use super::PocketfulBroker;
use crate::brokers::common::history::{chunks, ist_day, parse_iso_epoch};
use crate::brokers::common::ratelimit::today_ist;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::common::underlying::extract_underlying;
use crate::brokers::types::*;
use crate::clock::IST_OFFSET_SECS;
use crate::error::{AppError, Result};
use crate::services::options_service::parse_compact_expiry;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// OpenAlgo interval -> the interval enum of `/api/data/v1/historical`
/// (getting-started.md, "Supported Intervals"; web `_TIMEFRAMES`). On the
/// charts endpoint every interval is built from one-minute candles.
pub(super) const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1minute"),
    ("3m", "3minute"),
    ("5m", "5minute"),
    ("10m", "10minute"),
    ("15m", "15minute"),
    ("30m", "30minute"),
    ("1h", "60minute"),
    ("D", "1day"),
];

const CHARTS: &str = "/api/v1/charts";
const HISTORICAL: &str = "/api/data/v1/historical";
/// The `candletype` chart-data.md documents: "1 for 1-minute candles".
const CHART_ONE_MINUTE: &str = "1";
const CHART_CHUNK_DAYS: i64 = 7;
const HISTORICAL_INTRADAY_CHUNK_DAYS: i64 = 7;
const HISTORICAL_DAILY_CHUNK_DAYS: i64 = 365;

/// One deadline per history: a minute, plus a second for each request the
/// range needs (`deadline_for`).
const DEADLINE_BASE: Duration = Duration::from_secs(60);
const DEADLINE_PER_REQUEST: Duration = Duration::from_secs(1);
/// The most chart requests one history may need: ten years of daily bars
/// (the 3,650 days a daily ticker range is clamped to) in 7-day chunks.
/// A longer range is refused before anything is sent rather than spending
/// Pocketful's 10,000 requests a day on it.
const MAX_REQUESTS: u64 = 530;

/// The chart requests `start..=end` needs, counted without building them.
/// The NFO endpoint needs no more, and falls back to these.
fn chart_requests(start: NaiveDate, end: NaiveDate) -> u64 {
    let days = u64::try_from((end - start).num_days().saturating_add(1)).unwrap_or(0);
    days.div_ceil(CHART_CHUNK_DAYS.unsigned_abs())
}

/// The time one history over `start..=end` may take, waits and retries
/// included: a long daily range still finishes, while a broker that
/// stopped answering fails the request instead of holding it for hours.
pub(super) fn deadline_for(start: NaiveDate, end: NaiveDate) -> Duration {
    let requests = u32::try_from(chart_requests(start, end).min(MAX_REQUESTS)).unwrap_or(0);
    DEADLINE_BASE.saturating_add(DEADLINE_PER_REQUEST.saturating_mul(requests))
}

/// Minutes after IST midnight that bins of built bars are counted from: the
/// exchange's regular session open (web `_SESSION_OPEN_MINUTES`), so a 5
/// minute or hourly bar starts at 09:15 on NSE and at 09:00 on MCX.
fn session_offset_minutes(exchange: &str) -> i64 {
    match exchange {
        "MCX" | "CDS" => 0,
        _ => 15,
    }
}

/// The bar length of an intraday interval in minutes, read from its
/// Pocketful name (`5minute`); `None` for daily.
fn bar_minutes(pocketful_interval: &str) -> Option<i64> {
    pocketful_interval
        .strip_suffix("minute")
        .and_then(|n| n.parse().ok())
}

pub async fn get_history(
    b: &PocketfulBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    history_on(b, auth, req, today_ist(), deadline_for(req.start, req.end)).await
}

/// [`get_history`] on a given IST date and with a given deadline (tests
/// fix both). Whatever cannot be served is refused before any request.
pub(super) async fn history_on(
    b: &PocketfulBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
    today: NaiveDate,
    deadline: Duration,
) -> Result<Vec<Candle>> {
    let interval = TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let offered: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Pocketful history does not offer the {} interval. Choose one of {}.",
                req.interval,
                offered.join(", ")
            ))
        })?;
    let row = data::instrument(b, &req.key)?;
    if req.end < req.start {
        return Ok(Vec::new());
    }
    if chart_requests(req.start, req.end) > MAX_REQUESTS {
        return Err(AppError::Validation(format!(
            "That date range is too long for Pocketful history of {} on {}. Ask for at most ten years at a time.",
            req.key.symbol, req.key.exchange
        )));
    }
    match tokio::time::timeout(deadline, fetch(b, auth, req, &row, interval, today)).await {
        Ok(bars) => bars,
        Err(_) => {
            tracing::warn!(
                "Pocketful history for {} {} ran past its {:?} deadline",
                req.key.exchange,
                req.key.symbol,
                deadline
            );
            Err(AppError::Broker(format!(
                "Pocketful took too long to send the history for {} on {}. Try again, or ask for a shorter date range.",
                req.key.symbol, req.key.exchange
            )))
        }
    }
}

async fn fetch(
    b: &PocketfulBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
    row: &SymToken,
    interval: &str,
    today: NaiveDate,
) -> Result<Vec<Candle>> {
    let contract = (req.key.exchange == "NFO")
        .then(|| fno_contract(&req.key.symbol))
        .flatten();
    if let Some(contract) = contract {
        match historical_bars(b, auth, &contract, interval, req.start, req.end).await? {
            Some(mut bars) => {
                if interval == "1day" {
                    bars = with_todays_bar(b, auth, row, req, bars, today).await?;
                }
                return Ok(finish(bars, interval, &req.key.exchange, false));
            }
            None => tracing::warn!(
                "Pocketful historical data gave nothing for {} {}; using chart candles, without open interest",
                req.key.exchange,
                req.key.symbol
            ),
        }
    }
    let minutes = chart_minutes(b, auth, row, req.start, req.end).await?;
    Ok(finish(minutes, interval, &req.key.exchange, true))
}

/// The `/api/data/v1/historical` body fields of an OpenAlgo NFO symbol
/// (web `_fno_contract`): the underlying, `expiry_date` (YYYY-MM-DD),
/// `product_type`, and for an option `type` and `strike_price` (a whole
/// strike as an integer, as the documented body sends it). `None` for a
/// symbol that is not `<base><DDMMMYY>FUT` or `<base><DDMMMYY><strike>CE|PE`.
fn fno_contract(symbol: &str) -> Option<Map<String, Value>> {
    let base = extract_underlying(symbol, "NFO")?;
    let upper = symbol.to_uppercase();
    let rest = upper.get(base.len()..)?;
    let (expiry, tail) = (rest.get(..7)?, rest.get(7..)?);
    // Exactly `DDMMMYY` in the 2000s (web `strptime("%d%b%y")`).
    // `parse_broker_expiry` tries `%d%b%Y` first, which chrono reads with
    // a one-to-four digit year: `29JAN25` would be the year 25.
    let expiry = parse_compact_expiry(expiry)?;
    let mut body = Map::new();
    body.insert("symbol".into(), json!(base));
    body.insert(
        "expiry_date".into(),
        json!(expiry.format("%Y-%m-%d").to_string()),
    );
    if tail == "FUT" {
        body.insert("product_type".into(), json!("futures"));
        return Some(body);
    }
    // `extract_underlying` has checked the strike's digits; an empty one
    // (`..CE` alone) does not parse.
    let (strike, kind) = tail
        .strip_suffix("CE")
        .map(|s| (s, "CE"))
        .or_else(|| tail.strip_suffix("PE").map(|s| (s, "PE")))?;
    let strike_value: f64 = strike.parse().ok()?;
    body.insert("product_type".into(), json!("options"));
    body.insert("type".into(), json!(kind));
    let strike_json = if strike_value.fract() == 0.0 && strike_value.abs() < 1e15 {
        json!(strike_value as i64)
    } else {
        json!(strike_value)
    };
    body.insert("strike_price".into(), strike_json);
    Some(body)
}

/// The candles of one chunk's answer: `Some` for a success whose `candles`
/// is a list (or missing: no candles), `None` for anything else. Read as
/// each chunk arrives, so a long range holds candles, not its answers.
fn chunk_candles(answer: &Answer) -> Option<Vec<Candle>> {
    if !answer.ok() {
        return None;
    }
    match answer.payload().and_then(|p| p.get("candles")) {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(rows)) => Some(candles(rows)),
        Some(_) => None,
    }
}

/// NFO bars from `/api/data/v1/historical` for `start..=end`, chunked.
/// `None` when it answered with an error or with no candles at all, so the
/// charts are asked instead; a session refused is an error, not a fallback.
async fn historical_bars(
    b: &PocketfulBroker,
    auth: &AuthToken,
    contract: &Map<String, Value>,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Option<Vec<Candle>>> {
    let days = if interval == "1day" {
        HISTORICAL_DAILY_CHUNK_DAYS
    } else {
        HISTORICAL_INTRADAY_CHUNK_DAYS
    };
    let mut bars = Vec::new();
    for (first, last) in chunks(start, end, days) {
        let mut body = contract.clone();
        body.insert("interval".into(), json!(interval));
        body.insert("from_date".into(), json!(format!("{}T00:00:00", first)));
        body.insert("to_date".into(), json!(format!("{}T23:59:59", last)));
        let request = Request::new(Method::POST, HISTORICAL)
            .json(Value::Object(body))
            .timeout(HISTORY_TIMEOUT);
        let answer = b.send_data(auth, request).await?;
        if answer.session_refused() {
            return Err(refusal(&answer, "historical data"));
        }
        let Some(chunk) = chunk_candles(&answer) else {
            tracing::warn!(
                status = answer.status.as_u16(),
                "Pocketful historical data refused {} to {}: {}",
                first,
                last,
                answer.message()
            );
            return Ok(None);
        };
        bars.extend(chunk);
    }
    let bars = within(bars, start, end);
    Ok((!bars.is_empty()).then_some(bars))
}

/// One-minute candles from `/api/v1/charts` for `start..=end`, chunked; a
/// chunk Pocketful refuses fails the whole request.
async fn chart_minutes(
    b: &PocketfulBroker,
    auth: &AuthToken,
    row: &SymToken,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Candle>> {
    let (exchange, _segment) = market::route(&row.exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Pocketful market data does not cover the {} exchange.",
            row.exchange
        ))
    })?;
    // chart-data.md lists a clientid header beside the session's own.
    let client_id = b.client_id(auth)?;
    let mut minutes = Vec::new();
    for (first, last) in chunks(start, end, CHART_CHUNK_DAYS) {
        let request = Request::get(CHARTS)
            .query("exchange", exchange)
            .query("token", row.token.trim())
            .query("candletype", CHART_ONE_MINUTE)
            .query("from_date", format!("{}T00:00:00", first))
            .query("to_date", format!("{}T23:59:59", last))
            .header("clientid", client_id.clone())
            .timeout(HISTORY_TIMEOUT);
        let answer = b.send_data(auth, request).await?;
        let Some(chunk) = chunk_candles(&answer) else {
            return Err(refusal(
                &answer,
                &format!(
                    "history for {} on {} from {} to {}",
                    row.symbol, row.exchange, first, last
                ),
            ));
        };
        minutes.extend(chunk);
    }
    Ok(within(minutes, start, end))
}

/// Epoch seconds of a candle time: ISO with an offset (fractional seconds
/// too, as the web's ISO 8601 reading takes them), or without one read as
/// IST (the documented candles carry `+0530`).
fn candle_time(v: &Value) -> Option<i64> {
    let s = v.as_str()?.trim();
    if let Some(t) = parse_iso_epoch(s) {
        return Some(t);
    }
    if let Ok(t) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z") {
        return Some(t.timestamp());
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .map(|t| t.and_utc().timestamp() - IST_OFFSET_SECS)
}

/// Candles from Pocketful's rows (web `_candle_frame`): a row that is not a
/// list of at least six, or whose time cannot be read, is skipped; a
/// missing seventh member (OI) is 0, and a number that cannot be read is 0.
fn candles(rows: &[Value]) -> Vec<Candle> {
    let mut skipped = 0usize;
    let out: Vec<Candle> = rows
        .iter()
        .filter_map(|row| {
            let parsed = row
                .as_array()
                .filter(|r| r.len() >= 6)
                .and_then(|r| Some((candle_time(&r[0])?, r)));
            if parsed.is_none() {
                skipped += 1;
            }
            let (timestamp, r) = parsed?;
            Some(Candle {
                timestamp,
                open: number(&r[1]),
                high: number(&r[2]),
                low: number(&r[3]),
                close: number(&r[4]),
                volume: number(&r[5]) as i64,
                oi: r.get(6).map(number).unwrap_or(0.0) as i64,
            })
        })
        .collect();
    if skipped > 0 {
        tracing::warn!(
            "Skipped {} Pocketful candle(s) that are not candles",
            skipped
        );
    }
    out
}

/// The candles whose IST date is in `start..=end`, oldest first, the last
/// of a repeated time kept (web `_within`).
fn within(mut candles: Vec<Candle>, start: NaiveDate, end: NaiveDate) -> Vec<Candle> {
    candles.retain(|c| ist_day(c.timestamp).is_some_and(|d| (start..=end).contains(&d)));
    candles.sort_by_key(|c| c.timestamp);
    let mut out: Vec<Candle> = Vec::with_capacity(candles.len());
    for c in candles {
        match out.last_mut() {
            Some(last) if last.timestamp == c.timestamp => *last = c,
            _ => out.push(c),
        }
    }
    out
}

/// Epoch seconds of `day` at 00:00 UTC: a daily bar's time.
fn utc_midnight(day: NaiveDate) -> i64 {
    day.and_time(NaiveTime::MIN).and_utc().timestamp()
}

/// Merge time-ordered candles that share a key (web pandas `agg`: open
/// first, high max, low min, close last, volume summed, OI last), each
/// bar stamped with its key.
fn fold(candles: Vec<Candle>, key: impl Fn(i64) -> Option<i64>) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let Some(at) = key(c.timestamp) else { continue };
        match out.last_mut() {
            Some(bar) if bar.timestamp == at => {
                bar.high = bar.high.max(c.high);
                bar.low = bar.low.min(c.low);
                bar.close = c.close;
                bar.volume = bar.volume.saturating_add(c.volume);
                bar.oi = c.oi;
            }
            _ => out.push(Candle { timestamp: at, ..c }),
        }
    }
    out
}

/// OpenAlgo's bars from time-ordered candles inside the requested dates
/// (web `_finish`): daily bars per IST date; intraday bars as they are, or
/// with `build`, built from one-minute candles in bins counted from the
/// exchange's session open (pandas `resample(origin="start_day", offset=..,
/// closed="left", label="left")`; empty bins dropped).
fn finish(candles: Vec<Candle>, interval: &str, exchange: &str, build: bool) -> Vec<Candle> {
    let Some(minutes) = bar_minutes(interval) else {
        return fold(candles, |t| ist_day(t).map(utc_midnight));
    };
    if !build || minutes <= 1 {
        return candles;
    }
    let Some(first_day) = candles.first().and_then(|c| ist_day(c.timestamp)) else {
        return candles;
    };
    let origin = utc_midnight(first_day) - IST_OFFSET_SECS + session_offset_minutes(exchange) * 60;
    let span = minutes * 60;
    fold(candles, |t| {
        Some(origin + (t - origin).div_euclid(span) * span)
    })
}

/// Today's daily bar from a quote, when the daily series stops before
/// today (web `_with_todays_bar`): only when today is in the range, and
/// only from an answer whose last trade (`last_trade_time`, epoch seconds)
/// is today, since on a holiday the quote still shows the last session,
/// which is already a bar.
async fn with_todays_bar(
    b: &PocketfulBroker,
    auth: &AuthToken,
    row: &SymToken,
    req: &HistoryRequest,
    mut bars: Vec<Candle>,
    today: NaiveDate,
) -> Result<Vec<Candle>> {
    if !(req.start..=req.end).contains(&today) {
        return Ok(bars);
    }
    if bars
        .last()
        .and_then(|c| ist_day(c.timestamp))
        .is_some_and(|d| d >= today)
    {
        return Ok(bars);
    }
    let raw = match data::market_data(b, auth, row).await {
        Ok(raw) => raw,
        Err(e) if ends_the_batch(&e) => return Err(e),
        Err(_) => {
            tracing::warn!(
                "No quote for today's {} {} daily bar; the series ends earlier",
                req.key.exchange,
                req.key.symbol
            );
            return Ok(bars);
        }
    };
    let traded = raw.get("last_trade_time").map(number).unwrap_or(0.0);
    let q = data::quote_from(&req.key, &raw);
    if traded == 0.0 || q.open <= 0.0 || ist_day(traded as i64) != Some(today) {
        return Ok(bars);
    }
    bars.push(Candle {
        timestamp: utc_midnight(today) - IST_OFFSET_SECS,
        open: q.open,
        high: q.high,
        low: q.low,
        close: q.ltp,
        volume: q.volume,
        oi: q.oi,
    });
    Ok(bars)
}
