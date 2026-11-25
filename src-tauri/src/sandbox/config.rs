//! Sandbox configuration: the web's 22 `sandbox_config` keys with the web's
//! defaults, descriptions and validation (`database/sandbox_db.py`,
//! `blueprints/sandbox.py:888-972`), read into a typed struct.

use super::charges;
use super::clock::parse_hhmm;
use super::types::dec_from_db;
use chrono::{NaiveTime, Weekday};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

/// `(key, default value, description)`, in the web's seeding order.
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    (
        "starting_capital",
        "10000000.00",
        "Starting sandbox capital in INR (\u{20b9}1 Crore) - Min: \u{20b9}1000",
    ),
    (
        "reset_day",
        "Never",
        "Day of week for automatic fund reset (Never = disabled)",
    ),
    ("reset_time", "00:00", "Time for automatic fund reset (IST)"),
    (
        "order_check_interval",
        "5",
        "Interval in seconds to check pending orders - Range: 1-30 seconds",
    ),
    (
        "mtm_update_interval",
        "5",
        "Interval in seconds to update MTM - Range: 0-60 seconds (0 = manual only)",
    ),
    (
        "nse_bse_square_off_time",
        "15:15",
        "Square-off time for NSE/BSE MIS positions (IST)",
    ),
    (
        "cds_bcd_square_off_time",
        "16:45",
        "Square-off time for CDS/BCD MIS positions (IST)",
    ),
    (
        "mcx_square_off_time",
        "23:30",
        "Square-off time for MCX MIS positions (IST)",
    ),
    (
        "ncdex_square_off_time",
        "17:00",
        "Square-off time for NCDEX MIS positions (IST)",
    ),
    (
        "equity_mis_leverage",
        "5",
        "Leverage multiplier for equity MIS (NSE/BSE) - Range: 1-50x",
    ),
    (
        "equity_cnc_leverage",
        "1",
        "Leverage multiplier for equity CNC (NSE/BSE) - Range: 1-50x",
    ),
    (
        "futures_leverage",
        "10",
        "Leverage multiplier for all futures (NFO/BFO/CDS/BCD/MCX/NCDEX) - Range: 1-50x",
    ),
    (
        "option_buy_leverage",
        "1",
        "Leverage for buying options (full premium) - Range: 1-50x",
    ),
    (
        "option_sell_leverage",
        "1",
        "Leverage for selling options (same as buying - full premium) - Range: 1-50x",
    ),
    (
        "order_rate_limit",
        "10",
        "Maximum orders per second - Range: 1-100 orders/sec (for future use)",
    ),
    (
        "api_rate_limit",
        "50",
        "Maximum API calls per second - Range: 1-1000 calls/sec (for future use)",
    ),
    (
        "smart_order_rate_limit",
        "2",
        "Maximum smart orders per second - Range: 1-50 orders/sec (for future use)",
    ),
    (
        "smart_order_delay",
        "0.5",
        "Delay between multi-leg smart orders - Range: 0.1-10 seconds (for future use)",
    ),
    (
        "expiry_settlement_timing",
        "expiry_day_close",
        "When expired F&O settles: 'expiry_day_close' (at exchange close on expiry day) or 'next_day' (from midnight after expiry)",
    ),
    (
        "option_expiry_settlement",
        "ltp",
        "Expired option settlement price: 'ltp' (last traded price, keeps ITM value) or 'zero' (all options expire worthless)",
    ),
    (
        "gtt_oco_margin_mode",
        "max",
        "OCO GTT margin mode: 'max' (block only the larger leg) or 'sum'",
    ),
    (
        "gtt_claim_timeout_sec",
        "60",
        "Seconds after which a GTT leg stuck in 'triggering' is reclaimed to 'pending'",
    ),
];

/// The 14 keys a sandbox reset puts back to their defaults
/// (`blueprints/sandbox.py` `_reset_config_locked`).
pub const RESET_KEYS: &[&str] = &[
    "starting_capital",
    "reset_day",
    "reset_time",
    "order_check_interval",
    "mtm_update_interval",
    "nse_bse_square_off_time",
    "cds_bcd_square_off_time",
    "mcx_square_off_time",
    "ncdex_square_off_time",
    "equity_mis_leverage",
    "equity_cnc_leverage",
    "futures_leverage",
    "option_buy_leverage",
    "option_sell_leverage",
];

/// Default value of a known key.
pub fn default_of(key: &str) -> Option<&'static str> {
    DEFAULTS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, v, _)| *v)
}

/// When an expired F&O position settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryTiming {
    /// From the exchange close on expiry day (default).
    ExpiryDayClose,
    /// From the day after expiry (legacy).
    NextDay,
}

/// Settlement price of an expired option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionSettlement {
    Ltp,
    Zero,
}

/// How an OCO GTT reserves margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcoMarginMode {
    Max,
    Sum,
}

/// Typed view of `sandbox_config`. Unparseable values fall back to the
/// default, as the web's `get_config(key, default)` callers do.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxConfig {
    pub starting_capital: Decimal,
    /// `None` = Never.
    pub reset_day: Option<Weekday>,
    pub reset_time: NaiveTime,
    pub order_check_interval: u64,
    pub mtm_update_interval: u64,
    pub nse_bse_square_off_time: NaiveTime,
    pub cds_bcd_square_off_time: NaiveTime,
    pub mcx_square_off_time: NaiveTime,
    pub ncdex_square_off_time: NaiveTime,
    pub equity_mis_leverage: Decimal,
    pub equity_cnc_leverage: Decimal,
    pub futures_leverage: Decimal,
    pub option_buy_leverage: Decimal,
    pub option_sell_leverage: Decimal,
    pub expiry_settlement_timing: ExpiryTiming,
    pub option_expiry_settlement: OptionSettlement,
    pub gtt_oco_margin_mode: OcoMarginMode,
    pub gtt_claim_timeout_sec: i64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self::from_lookup(|_| None)
    }
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN)
}

pub fn parse_weekday(s: &str) -> Option<Weekday> {
    match s {
        "Monday" => Some(Weekday::Mon),
        "Tuesday" => Some(Weekday::Tue),
        "Wednesday" => Some(Weekday::Wed),
        "Thursday" => Some(Weekday::Thu),
        "Friday" => Some(Weekday::Fri),
        "Saturday" => Some(Weekday::Sat),
        "Sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

impl SandboxConfig {
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let text = |k: &str| get(k).unwrap_or_else(|| default_of(k).unwrap_or("").to_string());
        let dec = |k: &str, d: i64| {
            let v = dec_from_db(&text(k));
            if v > Decimal::ZERO {
                v
            } else {
                Decimal::from(d)
            }
        };
        let time = |k: &str, h: u32, m: u32| parse_hhmm(&text(k)).unwrap_or_else(|| hm(h, m));
        let int = |k: &str, d: i64| text(k).trim().parse::<f64>().map(|v| v as i64).unwrap_or(d);
        let starting = dec_from_db(&text("starting_capital"));
        Self {
            starting_capital: if starting > Decimal::ZERO {
                starting
            } else {
                Decimal::from(10_000_000)
            },
            reset_day: parse_weekday(text("reset_day").trim()),
            reset_time: time("reset_time", 0, 0),
            order_check_interval: int("order_check_interval", 5).clamp(1, 30) as u64,
            mtm_update_interval: int("mtm_update_interval", 5).clamp(0, 60) as u64,
            nse_bse_square_off_time: time("nse_bse_square_off_time", 15, 15),
            cds_bcd_square_off_time: time("cds_bcd_square_off_time", 16, 45),
            mcx_square_off_time: time("mcx_square_off_time", 23, 30),
            ncdex_square_off_time: time("ncdex_square_off_time", 17, 0),
            equity_mis_leverage: dec("equity_mis_leverage", 5),
            equity_cnc_leverage: dec("equity_cnc_leverage", 1),
            futures_leverage: dec("futures_leverage", 10),
            option_buy_leverage: dec("option_buy_leverage", 1),
            option_sell_leverage: dec("option_sell_leverage", 1),
            expiry_settlement_timing: if text("expiry_settlement_timing").trim() == "next_day" {
                ExpiryTiming::NextDay
            } else {
                ExpiryTiming::ExpiryDayClose
            },
            option_expiry_settlement: if text("option_expiry_settlement").trim() == "zero" {
                OptionSettlement::Zero
            } else {
                OptionSettlement::Ltp
            },
            gtt_oco_margin_mode: if text("gtt_oco_margin_mode")
                .trim()
                .eq_ignore_ascii_case("sum")
            {
                OcoMarginMode::Sum
            } else {
                OcoMarginMode::Max
            },
            gtt_claim_timeout_sec: int("gtt_claim_timeout_sec", 60).max(0),
        }
    }

    /// Read every key from `sandbox_config`.
    pub fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let mut stmt =
            conn.prepare_cached("SELECT config_key, config_value FROM sandbox_config")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self::from_lookup(|k| {
            rows.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }))
    }

    /// MIS square-off time for an exchange; `None` for exchanges the web has
    /// no square-off for (CRYPTO, indices, NCO).
    pub fn square_off_time(&self, exchange: &str) -> Option<NaiveTime> {
        match exchange {
            "NSE" | "BSE" | "NFO" | "BFO" => Some(self.nse_bse_square_off_time),
            "CDS" | "BCD" => Some(self.cds_bcd_square_off_time),
            "MCX" => Some(self.mcx_square_off_time),
            "NCDEX" => Some(self.ncdex_square_off_time),
            _ => None,
        }
    }
}

/// Raw value of one key (default when missing).
pub fn get_raw(conn: &Connection, key: &str) -> rusqlite::Result<String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT config_value FROM sandbox_config WHERE config_key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or_else(|| default_of(key).unwrap_or("").to_string()))
}

/// Write one key (insert when missing, description from the defaults).
pub fn set_raw(conn: &Connection, key: &str, value: &str, now: &str) -> rusqlite::Result<()> {
    let description = DEFAULTS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, _, d)| *d);
    conn.execute(
        "INSERT INTO sandbox_config (config_key, config_value, description, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(config_key) DO UPDATE SET config_value = excluded.config_value,
           updated_at = excluded.updated_at",
        params![key, value, description, now],
    )?;
    Ok(())
}

/// Whether `key` is a sandbox setting: one of [`DEFAULTS`] or the brokerage
/// plan's (`/sandbox/commissions`, web openalgo#2496).
pub fn is_setting(key: &str) -> bool {
    default_of(key).is_some() || charges::config_keys().iter().any(|k| k == key)
}

/// Web `validate_config`, the brokerage plan's keys included: the plan's own
/// check first, so the one-key update cannot store a value the charge
/// calculation would have to guess at.
pub fn validate_setting(key: &str, value: &str) -> Option<String> {
    charges::validate_config_value(key, value).or_else(|| validate(key, value))
}

/// Put [`RESET_KEYS`] and the brokerage plan back to their defaults (web
/// `_reset_config_locked`: the dialog promises every setting goes back to
/// its default, and the plan is one of them).
pub(crate) fn reset_to_defaults(conn: &Connection, now: &str) -> rusqlite::Result<()> {
    for k in RESET_KEYS {
        set_raw(conn, k, default_of(k).unwrap_or(""), now)?;
    }
    for (k, v) in charges::default_config_values() {
        set_raw(conn, &k, &v, now)?;
    }
    Ok(())
}

/// Web `validate_config`: `None` when the value is acceptable, else the
/// message the web returns.
pub fn validate(key: &str, value: &str) -> Option<String> {
    const NUMERIC: &[&str] = &[
        "starting_capital",
        "equity_mis_leverage",
        "equity_cnc_leverage",
        "futures_leverage",
        "option_buy_leverage",
        "option_sell_leverage",
        "order_check_interval",
        "mtm_update_interval",
    ];
    if NUMERIC.contains(&key) {
        let v: f64 = match value.trim().parse() {
            Ok(v) => v,
            Err(_) => return Some(format!("{key} must be a valid number")),
        };
        if !v.is_finite() {
            return Some(format!("{key} must be a valid number"));
        }
        if v < 0.0 {
            return Some(format!("{key} must be a positive number"));
        }
        if key == "starting_capital"
            && ![
                100000.0, 500000.0, 1000000.0, 2500000.0, 5000000.0, 10000000.0,
            ]
            .contains(&v)
        {
            return Some(
                "Starting capital must be one of: \u{20b9}1L, \u{20b9}5L, \u{20b9}10L, \u{20b9}25L, \u{20b9}50L, or \u{20b9}1Cr"
                    .to_string(),
            );
        }
        if key.ends_with("_leverage") {
            if v < 1.0 {
                return Some("Leverage must be at least 1x".to_string());
            }
            if v > 50.0 {
                return Some("Leverage cannot exceed 50x".to_string());
            }
        }
        if key == "order_check_interval" && !(1.0..=30.0).contains(&v) {
            return Some("Order check interval must be between 1-30 seconds".to_string());
        }
        if key == "mtm_update_interval" && !(0.0..=60.0).contains(&v) {
            return Some(
                "MTM update interval must be between 0-60 seconds (0 = manual only)".to_string(),
            );
        }
    }
    if key.ends_with("_time") {
        if !value.contains(':') {
            return Some("Time must be in HH:MM format".to_string());
        }
        let mut parts = value.splitn(2, ':');
        let h = parts.next().unwrap_or("").trim().parse::<i64>();
        let m = parts.next().unwrap_or("").trim().parse::<i64>();
        match (h, m) {
            (Ok(h), Ok(m)) => {
                if !((0..=23).contains(&h) && (0..=59).contains(&m)) {
                    return Some("Invalid time format".to_string());
                }
            }
            _ => return Some("Time must be in HH:MM format".to_string()),
        }
    }
    if key == "reset_day" && value != "Never" && parse_weekday(value).is_none() {
        return Some(
            "Reset day must be one of: Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday, Never"
                .to_string(),
        );
    }
    if key == "expiry_settlement_timing" && !matches!(value, "expiry_day_close" | "next_day") {
        return Some(
            "Expiry settlement timing must be 'expiry_day_close' or 'next_day'".to_string(),
        );
    }
    if key == "option_expiry_settlement" && !matches!(value, "ltp" | "zero") {
        return Some("Option expiry settlement must be 'ltp' or 'zero'".to_string());
    }
    if key == "gtt_oco_margin_mode" && !matches!(value.to_ascii_lowercase().as_str(), "max" | "sum")
    {
        return Some("OCO margin mode must be 'max' or 'sum'".to_string());
    }
    if key == "gtt_claim_timeout_sec" && value.trim().parse::<u32>().is_err() {
        return Some("gtt_claim_timeout_sec must be a whole number of seconds".to_string());
    }
    None
}

/// `GET /sandbox/api/configs` body: `{"status":"success","configs":{...}}`
/// grouped in the web's five categories.
pub fn grouped(conn: &Connection) -> rusqlite::Result<Value> {
    let mut stmt =
        conn.prepare_cached("SELECT config_key, config_value, description FROM sandbox_config")?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let entry = |key: &str| -> Value {
        match rows.iter().find(|(k, _, _)| k == key) {
            Some((_, v, d)) => json!({"value": v, "description": d}),
            None => json!({
                "value": default_of(key).unwrap_or(""),
                "description": DEFAULTS.iter().find(|(k, _, _)| *k == key).map(|(_, _, d)| *d).unwrap_or(""),
            }),
        }
    };
    let group = |title: &str, keys: &[&str]| -> Value {
        let mut configs = Map::new();
        for k in keys {
            configs.insert((*k).to_string(), entry(k));
        }
        json!({"title": title, "configs": Value::Object(configs)})
    };
    Ok(json!({
        "status": "success",
        "configs": {
            "capital": group("Capital Settings", &["starting_capital", "reset_day", "reset_time"]),
            "leverage": group("Leverage Settings", &[
                "equity_mis_leverage", "equity_cnc_leverage", "futures_leverage",
                "option_buy_leverage", "option_sell_leverage",
            ]),
            "square_off": group("Square-Off Times (IST)", &[
                "nse_bse_square_off_time", "cds_bcd_square_off_time",
                "mcx_square_off_time", "ncdex_square_off_time",
            ]),
            "intervals": group("Update Intervals (seconds)", &["order_check_interval", "mtm_update_interval"]),
            "expiry": group("F&O Expiry Settlement", &["expiry_settlement_timing", "option_expiry_settlement"]),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_two_keys_with_web_defaults() {
        assert_eq!(DEFAULTS.len(), 22);
        let c = SandboxConfig::default();
        assert_eq!(c.starting_capital, Decimal::from(10_000_000));
        assert_eq!(c.square_off_time("NFO"), Some(hm(15, 15)));
        assert_eq!(c.square_off_time("BFO"), Some(hm(15, 15)));
        assert_eq!(c.square_off_time("BCD"), Some(hm(16, 45)));
        assert_eq!(c.square_off_time("MCX"), Some(hm(23, 30)));
        assert_eq!(c.square_off_time("NCDEX"), Some(hm(17, 0)));
        assert_eq!(c.square_off_time("CRYPTO"), None);
        assert_eq!(c.reset_day, None);
        assert_eq!(c.gtt_oco_margin_mode, OcoMarginMode::Max);
    }

    /// Web test_charges.py::test_the_one_key_update_route_cannot_store_what_the_page_would_refuse,
    /// through the sandbox's own setting check.
    #[test]
    fn the_brokerage_plan_is_a_setting_with_its_own_check() {
        assert!(is_setting("charges_enabled"));
        assert!(is_setting("brokerage_fno_options_flat"));
        assert!(!is_setting("brokerage_penny_stocks_flat"));
        assert!(validate_setting("charges_enabled", "maybe").is_some());
        assert!(validate_setting("brokerage_fno_options_percent", "30").is_some());
        assert!(validate_setting("brokerage_fno_options_percent", "0.03").is_none());
        assert_eq!(
            validate_setting("futures_leverage", "99").as_deref(),
            Some("Leverage cannot exceed 50x")
        );
    }

    #[test]
    fn validation_messages_match_the_web() {
        assert_eq!(
            validate("equity_mis_leverage", "51").as_deref(),
            Some("Leverage cannot exceed 50x")
        );
        assert_eq!(
            validate("equity_mis_leverage", "0.5").as_deref(),
            Some("Leverage must be at least 1x")
        );
        assert!(validate("starting_capital", "1234").is_some());
        assert!(validate("starting_capital", "500000").is_none());
        assert_eq!(
            validate("mcx_square_off_time", "2330").as_deref(),
            Some("Time must be in HH:MM format")
        );
        assert_eq!(
            validate("mcx_square_off_time", "24:00").as_deref(),
            Some("Invalid time format")
        );
        assert!(validate("reset_day", "Funday").is_some());
        assert!(validate("reset_day", "Sunday").is_none());
        assert!(validate("order_check_interval", "31").is_some());
        assert!(validate("mtm_update_interval", "0").is_none());
    }
}
