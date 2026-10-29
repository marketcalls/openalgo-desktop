//! Delay, Wait Until, Time Window and Time Condition (the web's
//! `execute_delay`, `execute_wait_until`, `execute_time_window`,
//! `execute_time_condition`, `flow_executor_service.py` 2860-2933,
//! 3587-3670), and the schedule trigger's market-hours gate
//! (`flow_scheduler_service.py` 353-433, 522-539).
//!
//! Every time here is market time, Asia/Kolkata, on any machine (lead
//! decision 1; the web reads the host's clock), from the run's injected
//! clock. A wait wakes at once when the run is stopped (sign-out, quit),
//! where the web slept on inside its lock.

use super::math::bigint::BigInt;
use crate::db::sqlite::market_calendar::{self, SessionWindow};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::time::{
    ist_now, parse_time_string, DELAY_MAX_SECONDS, WAIT_UNTIL_MAX_SECONDS,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Asia::Kolkata;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// What a wait cut short by the run stopping answers.
pub const STOPPED_WHILE_WAITING: &str = "The workflow was stopped while it was waiting.";

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// Python's `int(value)` of a decoded JSON value, or what it raises.
pub fn py_int(v: &Value) -> Result<BigInt, String> {
    match v {
        Value::Bool(b) => Ok(BigInt::from_i64(i64::from(*b))),
        Value::Number(n) => Ok(match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => BigInt::from_i64(i),
            (None, Some(u)) => BigInt::from_u64(u),
            _ => BigInt::from_f64_integral(n.as_f64().unwrap_or(0.0)),
        }),
        Value::String(s) => {
            let invalid = || {
                format!(
                    "invalid literal for int() with base 10: {}",
                    pyfmt::quote(s)
                )
            };
            let t = s.trim();
            let (neg, body) = match t.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let bytes = body.as_bytes();
            let well_formed = !body.is_empty()
                && bytes.iter().enumerate().all(|(i, c)| {
                    c.is_ascii_digit()
                        || (*c == b'_'
                            && i > 0
                            && i + 1 < bytes.len()
                            && bytes[i - 1].is_ascii_digit()
                            && bytes[i + 1].is_ascii_digit())
                });
            if !well_formed {
                return Err(invalid());
            }
            let n = BigInt::parse_radix(&body.replace('_', ""), 10).ok_or_else(invalid)?;
            Ok(if neg { n.neg() } else { n })
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            pyfmt::type_name(other)
        )),
    }
}

/// A Delay's length as the web computes it: whole seconds from
/// `delayValue`, a float from the legacy `delayMs`.
#[derive(Debug, Clone, PartialEq)]
enum Seconds {
    Whole(BigInt),
    Fraction(f64),
}

impl Seconds {
    fn text(&self) -> String {
        match self {
            Seconds::Whole(n) => n.to_decimal(),
            Seconds::Fraction(f) => pyfmt::float(*f),
        }
    }

    fn over(&self, limit: i64) -> bool {
        match self {
            Seconds::Whole(n) => *n > BigInt::from_i64(limit),
            Seconds::Fraction(f) => *f > limit as f64,
        }
    }

    fn negative(&self) -> bool {
        match self {
            Seconds::Whole(n) => n.is_negative(),
            Seconds::Fraction(f) => *f < 0.0,
        }
    }

    fn duration(&self) -> Duration {
        match self {
            Seconds::Whole(n) => Duration::from_secs(n.to_i64().unwrap_or(0).max(0) as u64),
            Seconds::Fraction(f) => Duration::from_secs_f64(f.max(0.0)),
        }
    }
}

pub struct Delay;

impl Delay {
    fn seconds(data: &Map<String, Value>) -> Result<Seconds, String> {
        match data.get("delayValue") {
            Some(v) if !v.is_null() => {
                let n = py_int(v)?;
                let unit = match data.get("delayUnit").and_then(Value::as_str) {
                    Some("minutes") => 60,
                    Some("hours") => 3600,
                    _ => 1,
                };
                Ok(Seconds::Whole(n.mul(&BigInt::from_i64(unit))?))
            }
            _ => {
                let ms = match data.get("delayMs") {
                    None => BigInt::from_i64(1000),
                    Some(v) => py_int(v)?,
                };
                Ok(Seconds::Fraction(ms.true_div(&BigInt::from_i64(1000))?))
            }
        }
    }
}

#[async_trait]
impl FlowNode for Delay {
    fn node_type(&self) -> &'static str {
        "delay"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        // A value int() refuses ends the run, as the web's raise does
        // (parity N-delay-02, N-logic-05).
        let mut seconds = match Self::seconds(data) {
            Ok(s) => s,
            Err(raised) => return NodeOutcome::abort(raised),
        };
        if seconds.over(DELAY_MAX_SECONDS) {
            ctx.log(
                format!(
                    "Delay of {}s exceeds the {}s maximum; waiting the maximum instead.",
                    seconds.text(),
                    DELAY_MAX_SECONDS
                ),
                WARNING,
            );
            seconds = Seconds::Whole(BigInt::from_i64(DELAY_MAX_SECONDS));
        }
        ctx.info(format!("Waiting for {} seconds", seconds.text()));
        if seconds.negative() {
            // The web's time.sleep raised here and ended the whole run;
            // only this branch stops (parity N-delay-05).
            return NodeOutcome::error("sleep length must be non-negative");
        }
        if !ctx.sleep(seconds.duration()).await {
            ctx.error(STOPPED_WHILE_WAITING);
            return NodeOutcome::error(STOPPED_WHILE_WAITING);
        }
        NodeOutcome::result(
            json!({"status": "success", "message": format!("Waited {}s", seconds.text())}),
        )
    }
}

/// Seconds since midnight, IST, whole seconds.
fn seconds_of_day(t: NaiveTime) -> i64 {
    (t.hour() * 3600 + t.minute() * 60 + t.second()) as i64
}

/// The current market time of day, to the microsecond as Python keeps it.
fn now_time(ctx: &NodeCtx) -> NaiveTime {
    let t = ist_now(ctx.clock.as_ref()).time();
    t.with_nanosecond(t.nanosecond() / 1000 * 1000).unwrap_or(t)
}

pub struct WaitUntil;

#[async_trait]
impl FlowNode for WaitUntil {
    fn node_type(&self) -> &'static str {
        "waitUntil"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let raw = data.get("targetTime").cloned().unwrap_or(json!("09:30"));
        let shown = pyfmt::str(&raw);
        let (h, m, s) = parse_time_string(raw.as_str(), 9, 30);
        let target = (h * 3600 + m * 60 + s) as i64;
        let now = seconds_of_day(now_time(ctx));
        if now >= target {
            ctx.info(format!("Target time {} already passed", shown));
            return NodeOutcome::result(json!({"status": "success", "waited": false}));
        }
        let wait = target - now;
        if wait > WAIT_UNTIL_MAX_SECONDS {
            let message = format!(
                "Wait Until {shown} is {wait}s away, over the {WAIT_UNTIL_MAX_SECONDS}s limit. The wait holds this workflow's lock and the request that triggered it, so every trigger in between is answered 'already running'. Use a schedule trigger at {shown} instead, or split the square-off into its own workflow."
            );
            ctx.error(format!("Wait Until aborted: {}", message));
            return NodeOutcome::error(message);
        }
        ctx.info(format!("Waiting until {} (~{}s)", shown, wait));
        if !ctx.sleep(Duration::from_secs(wait as u64)).await {
            ctx.error(STOPPED_WHILE_WAITING);
            return NodeOutcome::error(STOPPED_WHILE_WAITING);
        }
        NodeOutcome::result(json!({"status": "success", "waited": true}))
    }
}

pub struct TimeWindow;

/// `invertCondition` as a switch: JSON `true`, or the text `true` / `1`.
/// The web took `bool(value)`, so an imported `"false"` inverted the
/// window (parity N-timeWindow-04).
fn switched_on(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1"),
        _ => false,
    }
}

#[async_trait]
impl FlowNode for TimeWindow {
    fn node_type(&self) -> &'static str {
        "timeWindow"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let start_raw = data.get("startTime").cloned().unwrap_or(json!("09:15"));
        let end_raw = data.get("endTime").cloned().unwrap_or(json!("15:30"));
        let invert = switched_on(data.get("invertCondition"));
        let now = now_time(ctx);
        let (sh, sm, _) = parse_time_string(start_raw.as_str(), 9, 15);
        let (eh, em, _) = parse_time_string(end_raw.as_str(), 15, 30);
        let start = NaiveTime::from_hms_opt(sh, sm, 0).unwrap_or_default();
        let end = NaiveTime::from_hms_opt(eh, em, 0).unwrap_or_default();
        // A window whose end is before its start crosses midnight.
        let in_window = if start <= end {
            start <= now && now <= end
        } else {
            now >= start || now <= end
        };
        let met = if invert { !in_window } else { in_window };
        ctx.info(format!(
            "Time window: {}-{}, in_window={}, invert={}, result={}",
            pyfmt::str(&start_raw),
            pyfmt::str(&end_raw),
            py_bool(in_window),
            py_bool(invert),
            py_bool(met)
        ));
        NodeOutcome::condition(met)
    }
}

pub struct TimeCondition;

#[async_trait]
impl FlowNode for TimeCondition {
    fn node_type(&self) -> &'static str {
        "timeCondition"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let target_raw = data.get("targetTime").cloned().unwrap_or(json!("09:30"));
        let operator = data.get("operator").cloned().unwrap_or(json!(">="));
        let condition_type = data.get("conditionType").cloned().unwrap_or(json!("entry"));
        let now = now_time(ctx);
        let (h, m, s) = parse_time_string(target_raw.as_str(), 9, 30);
        let target = (h * 3600 + m * 60 + s) as i64;
        let now_s = seconds_of_day(now);
        let met = match operator.as_str() {
            Some(">=") => now_s >= target,
            Some("<=") => now_s <= target,
            Some(">") => now_s > target,
            Some("<") => now_s < target,
            // Minute precision on purpose: a second-exact match would almost
            // never be polled.
            Some("==") => now.hour() == h && now.minute() == m,
            _ => {
                let message = format!(
                    "operator {} is not one of >=, <=, >, <, ==",
                    pyfmt::repr(&operator)
                );
                ctx.error(format!("Time condition aborted: {}", message));
                return NodeOutcome::undecided(message);
            }
        };
        ctx.info(format!(
            "Time condition ({}): {} {} {} = {}",
            pyfmt::str(&condition_type),
            now.format("%H:%M"),
            pyfmt::str(&operator),
            pyfmt::str(&target_raw),
            py_bool(met)
        ));
        NodeOutcome::result(json!({
            "status": "success",
            "condition": met,
            "condition_type": condition_type,
            "current_time": now.format("%H:%M:%S").to_string(),
            "target_time": target_raw,
            "operator": operator,
        }))
    }
}

/// The calendar the gate reads when the trigger node names none.
pub const DEFAULT_MARKET_HOURS_EXCHANGE: &str = "NSE";

/// A workflow's market-hours settings (`get_market_hours_config`), read
/// from its first `start` node on every run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarketHours {
    pub enabled: bool,
    pub start: Option<Value>,
    pub end: Option<Value>,
    pub exchange: Option<Value>,
}

/// The settings of the first `start` node in `nodes`; a workflow without
/// one runs ungated.
pub fn market_hours_config(nodes: &Value) -> MarketHours {
    let start = nodes
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n.get("type").and_then(Value::as_str) == Some("start"));
    let Some(node) = start else {
        return MarketHours::default();
    };
    let empty = Map::new();
    let data = node
        .get("data")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let pick = |k: &str| data.get(k).filter(|v| pyfmt::truthy(v)).cloned();
    MarketHours {
        enabled: data.get("marketHoursOnly").is_some_and(pyfmt::truthy),
        start: pick("marketHoursStart"),
        end: pick("marketHoursEnd"),
        exchange: pick("marketHoursExchange"),
    }
}

/// `_parse_hhmm`: `HH:MM` as minutes past midnight, or `None` when it is
/// not a valid time, so the calendar's side applies instead (never a
/// widened window).
pub fn parse_hhmm(v: &Value) -> Option<i64> {
    let text = pyfmt::str(v);
    let (h, m) = text.trim().split_once(':')?;
    let h = pyfmt::to_int(&json!(h))?;
    let m = pyfmt::to_int(&json!(m))?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

/// Minutes past IST midnight of an epoch-ms instant.
fn ist_minutes(ms: i64) -> Option<i64> {
    let t = Kolkata.timestamp_millis_opt(ms).single()?;
    Some((t.hour() * 60 + t.minute()) as i64)
}

/// Whether `now` is inside the day's `window` with the trigger node's
/// overrides (`is_within_market_hours` once the calendar has answered):
/// a shut day is never inside; an override that parses replaces its side
/// only; the comparison is by minute and inclusive at both ends.
pub fn within_window(
    window: Option<SessionWindow>,
    now: DateTime<Utc>,
    start: Option<&Value>,
    end: Option<&Value>,
) -> bool {
    let Some(w) = window else {
        return false;
    };
    let start = start
        .and_then(parse_hhmm)
        .or_else(|| ist_minutes(w.start_ms));
    let end = end.and_then(parse_hhmm).or_else(|| ist_minutes(w.end_ms));
    let (Some(start), Some(end)) = (start, end) else {
        return false;
    };
    let now = now.with_timezone(&Kolkata);
    let minutes = (now.hour() * 60 + now.minute()) as i64;
    start <= minutes && minutes <= end
}

/// The schedule trigger's market-hours gate on the app's trading calendar
/// (`is_within_market_hours`).
pub fn is_within_market_hours(conn: &Connection, now: DateTime<Utc>, hours: &MarketHours) -> bool {
    let exchange = hours
        .exchange
        .as_ref()
        .map(pyfmt::str)
        .unwrap_or_else(|| DEFAULT_MARKET_HOURS_EXCHANGE.to_string())
        .to_uppercase();
    let date = now.with_timezone(&Kolkata).date_naive();
    let window = market_calendar::effective_session_window(conn, date, &exchange);
    within_window(window, now, hours.start.as_ref(), hours.end.as_ref())
}

#[cfg(test)]
mod tests;
