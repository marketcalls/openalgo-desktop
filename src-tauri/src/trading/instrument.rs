//! The facts an OpenScript run needs about its instrument, from the platform
//! (web `services/openscript_instrument_service.py`).
//!
//! The engine reads one record about the instrument a script runs on: tick
//! size, lot size, what kind of instrument it is, whether it reports volume and
//! open interest, the IANA zone, and its trading session. Every fact is read
//! from the platform's own data: the contract from the symbol master, the zone
//! and the session from the market calendar (admin-editable timings and its
//! holiday table, special sessions included). A fact the platform does not hold
//! is left out, which the engine reads as absent.
//!
//! The record states the exchange's regular window, which every historical bar
//! is read against; today's effective window (a special session, an evening
//! session on a holiday) travels beside it under `today`.

use crate::brokers::common::symbols::SymToken;
use crate::db::sqlite::market_calendar as cal;
use crate::state::AppState;
use chrono::{Datelike, NaiveDate};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const INDEX_EXCHANGES: &[&str] = &["NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "GLOBAL_INDEX"];
const FNO_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"];
const CRYPTO_EXCHANGES: &[&str] = &["CRYPTO"];
const MINUTE_MS: i64 = 60_000;
const DAY_MINUTES: i64 = 24 * 60;

/// How long an answer is kept, and how many are kept.
pub const CACHE_TTL: Duration = Duration::from_secs(60);
pub const CACHE_MAX: usize = 256;

/// The exchange whose calendar an instrument trades on: an index is computed
/// while its cash market trades.
pub fn calendar_exchange(exchange: &str) -> &str {
    match exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        other => other,
    }
}

/// A calendar offset from IST midnight as `"HH:MM"`. A close is exclusive and
/// rounded up to the next minute, so a day ending 23:59:59 ends at `"24:00"`.
pub fn clock(offset_ms: i64, closing: bool) -> String {
    let mut minutes = if closing {
        (offset_ms + MINUTE_MS - 1).div_euclid(MINUTE_MS)
    } else {
        offset_ms.div_euclid(MINUTE_MS)
    };
    if closing && minutes == DAY_MINUTES {
        return "24:00".into();
    }
    minutes = minutes.rem_euclid(DAY_MINUTES);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The record's word for an instrument, from its exchange and contract code.
pub fn instrument_type(exchange: &str, code: Option<&str>) -> Option<&'static str> {
    if INDEX_EXCHANGES.contains(&exchange) {
        return Some("index");
    }
    let kind = code?.trim().to_ascii_uppercase();
    match kind.as_str() {
        "INDEX" | "IDX" | "AMXIDX" => Some("index"),
        "CE" | "PE" => Some("option"),
        k if k.starts_with("OPT") => Some("option"),
        "PERPFUT" => Some("future"),
        k if k.starts_with("FUT") => Some("future"),
        "EQ" | "BE" => Some("equity"),
        "" if exchange == "NSE" || exchange == "BSE" => Some("equity"),
        "CUR" => Some("currency"),
        "COM" => Some("commodity"),
        _ => None,
    }
}

fn has_open_interest(exchange: &str, code: Option<&str>, kind: Option<&str>) -> bool {
    match kind {
        Some("future") | Some("option") => return true,
        Some("equity") | Some("index") => return false,
        _ => {}
    }
    if code.is_some_and(|c| c.trim().eq_ignore_ascii_case("SPOT")) {
        return false;
    }
    FNO_EXCHANGES.contains(&exchange)
}

/// A tick or lot size, or `None`: zero is not a size.
fn positive(value: f64) -> Option<Value> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    if value.fract() == 0.0 && value < 1e15 {
        Some(json!(value as i64))
    } else {
        Some(json!(value))
    }
}

/// Today's effective window on the calendar.
#[derive(Debug, Clone, PartialEq)]
pub struct Today {
    pub date: NaiveDate,
    /// `(start_ms, end_ms)` epoch milliseconds, or `None` when closed.
    pub window: Option<(i64, i64)>,
    pub is_special: bool,
}

/// The record, from what the platform holds. Pure, so the shape is tested
/// without a database.
pub fn facts(
    symbol: &str,
    exchange: &str,
    row: Option<&SymToken>,
    regular: Option<(i64, i64)>,
    today: Option<&Today>,
) -> Value {
    let code = row.map(|r| r.instrument_type.as_str());
    let kind = instrument_type(exchange, code);
    let mut instrument = Map::new();
    instrument.insert("exchange".into(), json!(exchange));
    instrument.insert("timezone".into(), json!("Asia/Kolkata"));
    if let Some(r) = row {
        if let Some(t) = positive(r.tick_size) {
            instrument.insert("tickSize".into(), t);
        }
        if let Some(l) = positive(f64::from(r.lot_size)) {
            instrument.insert("lotSize".into(), l);
        }
    }
    if let Some(k) = kind {
        instrument.insert("instrumentType".into(), json!(k));
    }
    instrument.insert("hasVolume".into(), json!(kind != Some("index")));
    instrument.insert(
        "hasOpenInterest".into(),
        json!(has_open_interest(exchange, code, kind)),
    );
    let cal_ex = calendar_exchange(exchange);
    if let Some((start, end)) = regular {
        let days: Vec<u32> = if CRYPTO_EXCHANGES.contains(&cal_ex) {
            (1..=7).collect()
        } else {
            (1..=5).collect()
        };
        instrument.insert(
            "session".into(),
            json!({"start": clock(start, false), "end": clock(end, true), "days": days}),
        );
    }
    let today_json = match (regular, today) {
        (Some(_), Some(t)) => {
            let mut m = Map::new();
            m.insert("date".into(), json!(t.date.format("%Y-%m-%d").to_string()));
            m.insert("open".into(), json!(t.window.is_some()));
            m.insert(
                "isSpecial".into(),
                json!(t.is_special && t.window.is_some()),
            );
            if let Some((s, e)) = t.window {
                let midnight = cal::ist_midnight_ms(t.date);
                m.insert(
                    "session".into(),
                    json!({
                        "start": clock(s - midnight, false),
                        "end": clock(e - midnight, true),
                        "days": [t.date.weekday().number_from_monday()],
                    }),
                );
            }
            Value::Object(m)
        }
        _ => Value::Null,
    };
    json!({
        "symbol": symbol,
        "contractFound": row.is_some(),
        "instrument": Value::Object(instrument),
        "today": today_json,
    })
}

type FactsKey = (String, String, NaiveDate);

/// The facts with a short, bounded cache in front of the database reads.
#[derive(Default)]
pub struct FactsCache {
    held: Mutex<HashMap<FactsKey, (Instant, Value)>>,
}

impl FactsCache {
    pub fn len(&self) -> usize {
        self.held.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.held.lock().clear();
    }

    /// The record for one symbol on one date, read through the cache.
    pub fn get(&self, ctx: &AppState, symbol: &str, exchange: &str, date: NaiveDate) -> Value {
        let key = (symbol.to_string(), exchange.to_string(), date);
        let now = Instant::now();
        if let Some((at, v)) = self.held.lock().get(&key) {
            if now.duration_since(*at) < CACHE_TTL {
                return v.clone();
            }
        }
        let (value, trusted) = read(ctx, symbol, exchange, date);
        if trusted {
            let mut held = self.held.lock();
            if held.len() >= CACHE_MAX {
                held.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
                if held.len() >= CACHE_MAX {
                    held.clear();
                }
            }
            held.insert(key, (now, value.clone()));
        }
        value
    }
}

/// Read the record from the symbol master and the calendar. The second half
/// says whether every read ran (an answer from a failed read is not cached).
fn read(ctx: &AppState, symbol: &str, exchange: &str, date: NaiveDate) -> (Value, bool) {
    let row = ctx.symbols.by_symbol(exchange, symbol);
    let cal_ex = calendar_exchange(exchange).to_string();
    let conn = match ctx.sqlite.conn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Could not read the market calendar: {}", e);
            return (facts(symbol, exchange, row.as_ref(), None, None), false);
        }
    };
    let regular = cal::all_timings(&conn).ok().and_then(|list| {
        list.into_iter()
            .find(|t| t.exchange == cal_ex)
            .map(|t| (t.start_offset, t.end_offset))
    });
    let windows = cal::timings_for_date(&conn, date).unwrap_or_default();
    let special = cal::holidays_by_year(&conn, date.year())
        .unwrap_or_default()
        .into_iter()
        .any(|h| {
            h.date == date.format("%Y-%m-%d").to_string() && h.holiday_type == "SPECIAL_SESSION"
        });
    drop(conn);
    let today = Today {
        date,
        window: windows
            .iter()
            .find(|w| w.exchange == cal_ex)
            .map(|w| (w.start_time, w.end_time)),
        is_special: special,
    };
    (
        facts(symbol, exchange, row.as_ref(), regular, Some(&today)),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, tick: f64, lot: i32) -> SymToken {
        SymToken {
            symbol: "X".into(),
            brsymbol: String::new(),
            name: String::new(),
            exchange: "NFO".into(),
            brexchange: String::new(),
            token: "1".into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: lot,
            instrument_type: kind.into(),
            tick_size: tick,
        }
    }

    #[test]
    fn clock_rounds_a_close_up_and_spells_midnight() {
        assert_eq!(clock(9 * 3_600_000 + 15 * 60_000, false), "09:15");
        assert_eq!(clock(86_399_000, true), "24:00");
        assert_eq!(clock(15 * 3_600_000 + 30 * 60_000, true), "15:30");
        assert_eq!(clock(24 * 3_600_000 + 15 * 60_000, true), "00:15");
    }

    #[test]
    fn kinds_follow_the_contract_code() {
        assert_eq!(instrument_type("NSE_INDEX", None), Some("index"));
        assert_eq!(instrument_type("NFO", Some("OPTIDX")), Some("option"));
        assert_eq!(instrument_type("NFO", Some("CE")), Some("option"));
        assert_eq!(instrument_type("MCX", Some("FUTCOM")), Some("future"));
        assert_eq!(instrument_type("NSE", Some("")), Some("equity"));
        assert_eq!(instrument_type("NSE", None), None);
        assert_eq!(instrument_type("CRYPTO", Some("PERPFUT")), Some("future"));
    }

    #[test]
    fn record_shape() {
        let r = row("FUTIDX", 0.05, 75);
        let date = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let midnight = cal::ist_midnight_ms(date);
        let today = Today {
            date,
            window: Some((midnight + 33_300_000, midnight + 55_800_000)),
            is_special: false,
        };
        let v = facts(
            "NIFTYFUT",
            "NFO",
            Some(&r),
            Some((33_300_000, 55_800_000)),
            Some(&today),
        );
        assert_eq!(v["contractFound"], true);
        assert_eq!(v["instrument"]["tickSize"], 0.05);
        assert_eq!(v["instrument"]["lotSize"], 75);
        assert_eq!(v["instrument"]["instrumentType"], "future");
        assert_eq!(v["instrument"]["hasOpenInterest"], true);
        assert_eq!(v["instrument"]["hasVolume"], true);
        assert_eq!(
            v["instrument"]["session"],
            json!({"start": "09:15", "end": "15:30", "days": [1, 2, 3, 4, 5]})
        );
        assert_eq!(v["today"]["open"], true);
        assert_eq!(v["today"]["session"]["days"], json!([1]));

        let v = facts("NIFTY", "NSE_INDEX", None, None, None);
        assert_eq!(v["contractFound"], false);
        assert_eq!(v["instrument"]["hasVolume"], false);
        assert!(v["instrument"].get("session").is_none());
        assert_eq!(v["today"], Value::Null);
    }
}
