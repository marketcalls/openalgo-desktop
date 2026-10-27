//! The trading and risk settings as the order guard reads them (the web's
//! `RiskLimits` and `get_risk_limits` in `services/agent/settings.py`).
//!
//! The rows live in `ag_setting` (the config part stores and shows them);
//! this module owns what they mean to the guard: the fifteen trading and
//! risk keys, their defaults, and the lenient read the web gives them. The
//! limits are read fresh for every order, so a change the trader makes
//! applies to the next order, and a store that cannot be read gives the
//! defaults, which refuse to trade (trading is off by default).
//!
//! Desktop difference: the kill switch file defaults to
//! `<app data folder>/agent_kill_switch` (lead decision 2, 2026-10-27), and
//! a relative path resolves against the data folder, where the web resolves
//! it against its repository root.

use super::kill_switch;
use crate::state::AppState;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const KEY_TRADING_ENABLED: &str = "trading_enabled";
pub const KEY_REQUIRE_ANALYZER_MODE: &str = "require_analyzer_mode";
pub const KEY_KILL_SWITCH: &str = "kill_switch";
pub const KEY_KILL_SWITCH_FILE: &str = "kill_switch_file";
pub const KEY_MAX_ORDERS_PER_SESSION: &str = "max_orders_per_session";
pub const KEY_MAX_ORDER_QUANTITY: &str = "max_order_quantity";
pub const KEY_MAX_ORDER_VALUE: &str = "max_order_value";
pub const KEY_MAX_PRICE_DEVIATION_PCT: &str = "max_price_deviation_pct";
pub const KEY_DUPLICATE_ORDER_WINDOW_SECONDS: &str = "duplicate_order_window_seconds";
pub const KEY_ALLOWED_EXCHANGES: &str = "allowed_exchanges";
pub const KEY_ALLOWED_PRODUCTS: &str = "allowed_products";
pub const KEY_SYMBOL_ALLOWLIST: &str = "symbol_allowlist";
pub const KEY_SYMBOL_BLOCKLIST: &str = "symbol_blocklist";
pub const KEY_MAX_FUNDS_UTILIZATION_PCT: &str = "max_funds_utilization_pct";
pub const KEY_ALLOW_BULK_DESTRUCTIVE: &str = "allow_bulk_destructive";

/// Every trading and risk key the guard reads, in the web's `_SPEC` order.
pub const RISK_KEYS: [&str; 15] = [
    KEY_TRADING_ENABLED,
    KEY_REQUIRE_ANALYZER_MODE,
    KEY_MAX_ORDERS_PER_SESSION,
    KEY_MAX_ORDER_QUANTITY,
    KEY_MAX_ORDER_VALUE,
    KEY_MAX_PRICE_DEVIATION_PCT,
    KEY_DUPLICATE_ORDER_WINDOW_SECONDS,
    KEY_ALLOWED_EXCHANGES,
    KEY_ALLOWED_PRODUCTS,
    KEY_SYMBOL_ALLOWLIST,
    KEY_SYMBOL_BLOCKLIST,
    KEY_MAX_FUNDS_UTILIZATION_PCT,
    KEY_ALLOW_BULK_DESTRUCTIVE,
    KEY_KILL_SWITCH,
    KEY_KILL_SWITCH_FILE,
];

/// The web's `TRADABLE_EXCHANGES`: every exchange but the quote-only index
/// codes.
pub fn tradable_exchanges() -> BTreeSet<String> {
    crate::services::schemas::VALID_EXCHANGES
        .iter()
        .filter(|e| !e.ends_with("_INDEX"))
        .map(|e| e.to_string())
        .collect()
}

/// An immutable snapshot of every limit the guard enforces.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskLimits {
    pub trading_enabled: bool,
    pub require_analyzer_mode: bool,
    pub max_orders_per_session: i64,
    pub max_order_quantity: i64,
    pub max_order_value: Decimal,
    pub max_price_deviation_pct: Decimal,
    pub duplicate_order_window_seconds: i64,
    pub allowed_exchanges: BTreeSet<String>,
    pub allowed_products: BTreeSet<String>,
    pub symbol_allowlist: BTreeSet<String>,
    pub symbol_blocklist: BTreeSet<String>,
    pub max_funds_utilization_pct: Decimal,
    pub allow_bulk_destructive: bool,
    /// The stored half of the kill switch.
    pub kill_switch_engaged: bool,
    /// The `kill_switch_file` setting as stored.
    pub kill_switch_file: String,
    /// That setting resolved: `~` expanded, relative to the data folder.
    pub kill_switch_path: PathBuf,
}

impl RiskLimits {
    /// The shipped policy (the web's `default_risk_limits`): trading off.
    pub fn defaults(data_dir: &Path) -> RiskLimits {
        let file = kill_switch::default_file(data_dir)
            .to_string_lossy()
            .into_owned();
        RiskLimits {
            trading_enabled: false,
            require_analyzer_mode: false,
            max_orders_per_session: 20,
            max_order_quantity: 10_000,
            max_order_value: Decimal::new(50_000_000, 2),
            max_price_deviation_pct: Decimal::from(5),
            duplicate_order_window_seconds: 10,
            allowed_exchanges: tradable_exchanges(),
            allowed_products: ["CNC", "NRML", "MIS"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            symbol_allowlist: BTreeSet::new(),
            symbol_blocklist: BTreeSet::new(),
            max_funds_utilization_pct: Decimal::from(100),
            allow_bulk_destructive: false,
            kill_switch_engaged: false,
            kill_switch_path: kill_switch::resolve(&file, data_dir),
            kill_switch_file: file,
        }
    }

    /// The limits from stored rows (`key` to `value` text), each read as
    /// the web reads it (`_parse(strict=False)`): a corrupt row falls back
    /// to its default, a number outside its bounds is clamped to them.
    pub fn from_rows(rows: &HashMap<String, Option<String>>, data_dir: &Path) -> RiskLimits {
        let d = RiskLimits::defaults(data_dir);
        let raw = |key: &str| rows.get(key).and_then(|v| v.as_deref());
        let file = raw(KEY_KILL_SWITCH_FILE)
            .map(str::to_string)
            .unwrap_or_else(|| d.kill_switch_file.clone());
        RiskLimits {
            trading_enabled: read_bool(raw(KEY_TRADING_ENABLED), d.trading_enabled),
            require_analyzer_mode: read_bool(
                raw(KEY_REQUIRE_ANALYZER_MODE),
                d.require_analyzer_mode,
            ),
            max_orders_per_session: read_int(raw(KEY_MAX_ORDERS_PER_SESSION), 20, 0),
            max_order_quantity: read_int(raw(KEY_MAX_ORDER_QUANTITY), 10_000, 0),
            max_order_value: read_decimal(raw(KEY_MAX_ORDER_VALUE), d.max_order_value, None, true),
            max_price_deviation_pct: read_decimal(
                raw(KEY_MAX_PRICE_DEVIATION_PCT),
                d.max_price_deviation_pct,
                Some(Decimal::from(100)),
                false,
            ),
            duplicate_order_window_seconds: read_int(
                raw(KEY_DUPLICATE_ORDER_WINDOW_SECONDS),
                10,
                0,
            ),
            allowed_exchanges: read_set(raw(KEY_ALLOWED_EXCHANGES), &d.allowed_exchanges),
            allowed_products: read_set(raw(KEY_ALLOWED_PRODUCTS), &d.allowed_products),
            symbol_allowlist: read_set(raw(KEY_SYMBOL_ALLOWLIST), &d.symbol_allowlist),
            symbol_blocklist: read_set(raw(KEY_SYMBOL_BLOCKLIST), &d.symbol_blocklist),
            max_funds_utilization_pct: read_decimal(
                raw(KEY_MAX_FUNDS_UTILIZATION_PCT),
                d.max_funds_utilization_pct,
                Some(Decimal::from(100)),
                false,
            ),
            allow_bulk_destructive: read_bool(raw(KEY_ALLOW_BULK_DESTRUCTIVE), false),
            kill_switch_engaged: read_bool(raw(KEY_KILL_SWITCH), false),
            kill_switch_path: kill_switch::resolve(&file, data_dir),
            kill_switch_file: file,
        }
    }

    /// The limits as JSON-safe primitives (the web's `RiskLimits.as_dict`):
    /// sets as sorted lists, decimals as strings.
    pub fn as_dict(&self) -> Value {
        json!({
            KEY_TRADING_ENABLED: self.trading_enabled,
            KEY_REQUIRE_ANALYZER_MODE: self.require_analyzer_mode,
            KEY_MAX_ORDERS_PER_SESSION: self.max_orders_per_session,
            KEY_MAX_ORDER_QUANTITY: self.max_order_quantity,
            KEY_MAX_ORDER_VALUE: self.max_order_value.to_string(),
            KEY_MAX_PRICE_DEVIATION_PCT: self.max_price_deviation_pct.to_string(),
            KEY_DUPLICATE_ORDER_WINDOW_SECONDS: self.duplicate_order_window_seconds,
            KEY_ALLOWED_EXCHANGES: self.allowed_exchanges,
            KEY_ALLOWED_PRODUCTS: self.allowed_products,
            KEY_SYMBOL_ALLOWLIST: self.symbol_allowlist,
            KEY_SYMBOL_BLOCKLIST: self.symbol_blocklist,
            KEY_MAX_FUNDS_UTILIZATION_PCT: self.max_funds_utilization_pct.to_string(),
            KEY_ALLOW_BULK_DESTRUCTIVE: self.allow_bulk_destructive,
            KEY_KILL_SWITCH: self.kill_switch_engaged,
            KEY_KILL_SWITCH_FILE: self.kill_switch_file,
        })
    }
}

/// The limits as stored now, read past any cache. Synchronous: callers run
/// it on the blocking pool. Never fails: a store that cannot be read (or
/// has no settings table yet) gives the defaults, and trading stays off.
pub fn read(app: &AppState) -> RiskLimits {
    match stored_rows(app) {
        Ok(rows) => RiskLimits::from_rows(&rows, &app.data_dir),
        Err(e) => {
            tracing::error!(
                "Agent risk limits could not be read; using the defaults, which do not trade: {}",
                e
            );
            RiskLimits::defaults(&app.data_dir)
        }
    }
}

fn stored_rows(app: &AppState) -> crate::error::Result<HashMap<String, Option<String>>> {
    let conn = app.sqlite.conn()?;
    let mut st = conn.prepare("SELECT key, value FROM ag_setting")?;
    let rows = st
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(k, _)| RISK_KEYS.contains(&k.as_str()))
        .collect())
}

const TRUE_TOKENS: [&str; 7] = ["1", "true", "t", "yes", "y", "on", "enabled"];
const FALSE_TOKENS: [&str; 7] = ["0", "false", "f", "no", "n", "off", "disabled"];

fn read_bool(raw: Option<&str>, default: bool) -> bool {
    let Some(raw) = raw else { return default };
    let token = raw.trim().to_lowercase();
    if TRUE_TOKENS.contains(&token.as_str()) {
        true
    } else if FALSE_TOKENS.contains(&token.as_str()) {
        false
    } else {
        tracing::warn!("An Agent risk setting holds an unreadable switch; using its default");
        default
    }
}

fn read_int(raw: Option<&str>, default: i64, minimum: i64) -> i64 {
    let Some(raw) = raw else { return default };
    match raw.trim().parse::<i64>() {
        Ok(v) => v.max(minimum),
        Err(_) => {
            tracing::warn!(
                "An Agent risk setting holds an unreadable whole number; using its default"
            );
            default
        }
    }
}

fn read_decimal(
    raw: Option<&str>,
    default: Decimal,
    maximum: Option<Decimal>,
    money: bool,
) -> Decimal {
    let Some(raw) = raw else { return default };
    let Some(mut value) = super::risk::num::parse_decimal(raw) else {
        tracing::warn!("An Agent risk setting holds an unreadable number; using its default");
        return default;
    };
    if money {
        value = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
        value.rescale(2);
    }
    if value < Decimal::ZERO {
        return Decimal::ZERO;
    }
    match maximum {
        Some(max) if value > max => max,
        _ => value,
    }
}

fn read_set(raw: Option<&str>, default: &BTreeSet<String>) -> BTreeSet<String> {
    let Some(raw) = raw else {
        return default.clone();
    };
    let text = raw.trim();
    if text.is_empty() {
        return BTreeSet::new();
    }
    let items: Vec<String> = if text.starts_with('[') {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(a)) => a.iter().map(super::risk::num::py_str).collect(),
            _ => {
                tracing::warn!("An Agent risk setting holds an unreadable list; using its default");
                return default.clone();
            }
        }
    } else {
        text.split(',').map(str::to_string).collect()
    };
    items
        .iter()
        .map(|i| i.trim().to_uppercase())
        .filter(|i| !i.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(pairs: &[(&str, &str)]) -> HashMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Some(v.to_string())))
            .collect()
    }

    #[test]
    fn the_defaults_are_the_webs_and_do_not_trade() {
        let dir = Path::new("/data");
        let d = RiskLimits::defaults(dir);
        assert!(!d.trading_enabled && !d.kill_switch_engaged && !d.allow_bulk_destructive);
        assert_eq!(d.as_dict()[KEY_MAX_ORDER_VALUE], "500000.00");
        assert_eq!(d.as_dict()[KEY_MAX_PRICE_DEVIATION_PCT], "5");
        assert_eq!(d.as_dict()[KEY_MAX_FUNDS_UTILIZATION_PCT], "100");
        assert_eq!(
            d.as_dict()[KEY_ALLOWED_EXCHANGES],
            json!(["BCD", "BFO", "BSE", "CDS", "CRYPTO", "MCX", "NCDEX", "NCO", "NFO", "NSE"])
        );
        assert_eq!(
            d.as_dict()[KEY_ALLOWED_PRODUCTS],
            json!(["CNC", "MIS", "NRML"])
        );
        assert_eq!(d.kill_switch_path, dir.join("agent_kill_switch"));
        assert_eq!(RiskLimits::from_rows(&HashMap::new(), dir), d);
    }

    #[test]
    fn stored_rows_are_read_leniently_as_the_web_reads_them() {
        let dir = Path::new("/data");
        let l = RiskLimits::from_rows(
            &rows(&[
                (KEY_TRADING_ENABLED, " Enabled "),
                (KEY_REQUIRE_ANALYZER_MODE, "maybe"),
                (KEY_MAX_ORDERS_PER_SESSION, "-3"),
                (KEY_MAX_ORDER_QUANTITY, "abc"),
                (KEY_MAX_ORDER_VALUE, "1234.565"),
                (KEY_MAX_PRICE_DEVIATION_PCT, "250"),
                (KEY_ALLOWED_EXCHANGES, "nse, nfo ,"),
                (KEY_ALLOWED_PRODUCTS, "[\"cnc\", \"mis\"]"),
                (KEY_SYMBOL_BLOCKLIST, "[not json"),
                (KEY_SYMBOL_ALLOWLIST, ""),
                (KEY_KILL_SWITCH_FILE, "stop_here"),
            ]),
            dir,
        );
        assert!(l.trading_enabled);
        assert!(
            !l.require_analyzer_mode,
            "an unreadable switch reads its default"
        );
        assert_eq!(l.max_orders_per_session, 0, "clamped to the minimum");
        assert_eq!(l.max_order_quantity, 10_000);
        assert_eq!(
            l.max_order_value.to_string(),
            "1234.57",
            "money to paise, half up"
        );
        assert_eq!(l.max_price_deviation_pct, Decimal::from(100));
        assert_eq!(
            l.allowed_exchanges,
            ["NFO", "NSE"].iter().map(|s| s.to_string()).collect::<BTreeSet<String>>()
        );
        assert_eq!(
            l.allowed_products,
            ["CNC", "MIS"].iter().map(|s| s.to_string()).collect::<BTreeSet<String>>()
        );
        assert!(l.symbol_blocklist.is_empty() && l.symbol_allowlist.is_empty());
        assert_eq!(l.kill_switch_path, dir.join("stop_here"));
    }

    /// A store without the settings table (or any read failure) gives the
    /// defaults: an Agent whose settings cannot be read does not trade.
    #[test]
    fn an_unreadable_store_does_not_trade() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let l = read(&t.ctx);
        assert!(!l.trading_enabled);
        assert_eq!(l.kill_switch_path, t.ctx.data_dir.join("agent_kill_switch"));
    }
}
