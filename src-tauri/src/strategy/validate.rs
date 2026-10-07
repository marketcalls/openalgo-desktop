//! Strategy configuration validation (web `blueprints/strategy_module.py`,
//! `validate_strategy_config`). The store has no opinion on a leg's shape
//! (legs are JSON), so this is the only place a bad payload is refused.
//!
//! * Unknown fields are refused, never dropped: a misspelt `overall_sl_mtm`
//!   must not save as a strategy with no stop.
//! * Enums are matched case-insensitively and returned canonically, so the
//!   validator is idempotent (a PATCH re-validates the merged config).
//! * A loss threshold is entered positive and applied negative.
//! * A strike keeps its fractional part.

use super::resolver::{is_derivative_exchange, quantity_is_whole_lots};
use super::store::{DIRECTIONS, RUN_MODES, STRATEGY_KINDS, STRATEGY_TYPES, UPDATABLE_FIELDS};
use crate::brokers::common::symbols::SymbolGeneration;
use serde_json::{json, Map, Value};

pub const PRODUCTS: &[&str] = &["CNC", "NRML", "MIS"];
/// MARKET only: no leg carries a price, and exits are MARKET regardless.
pub const PRICETYPES: &[&str] = &["MARKET"];
pub const UNDERLYING_EXCHANGES: &[&str] = &[
    "NSE",
    "BSE",
    "NFO",
    "BFO",
    "CDS",
    "BCD",
    "MCX",
    "NCDEX",
    "NCO",
    "NSE_INDEX",
    "BSE_INDEX",
];
pub const UNIVERSE_TABS: &[&str] = &["weekly_monthly", "monthly_only", "stocks_fno", "mcx"];
pub const LEG_SEGMENTS: &[&str] = &["options", "futures", "cash"];
pub const LEG_POSITIONS: &[&str] = &["B", "S"];
pub const LEG_OPTION_TYPES: &[&str] = &["CE", "PE"];
pub const LEG_STRIKE_MODES: &[&str] = &["atm", "strike"];
pub const LEG_EXPIRIES: &[&str] = &[
    "weekly",
    "next_week",
    "monthly",
    "next_month",
    "current",
    "next",
];
pub const LOCK_PROFIT_MODES: &[&str] = &["lock", "lock_and_trail"];
pub const RISK_UNITS: &[&str] = &["points", "percent"];
pub const SCHEDULER_DAYS: &[&str] = &["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];
pub const SIGNAL_LEG_EXCHANGES: &[&str] = &[
    "NSE", "BSE", "NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO",
];
pub const LEG_SIDES: &[&str] = &["long", "short", "both"];
pub const QTY_MODES: &[&str] = &["lots", "units"];

const LEG_FIELDS: &[&str] = &[
    "id",
    "segment",
    "position",
    "lots",
    "option_type",
    "strike_mode",
    "atm_offset",
    "strike",
    "expiry",
    "sl_pts",
    "target_pts",
    "trail",
    "risk_unit",
];
const SIGNAL_LEG_FIELDS: &[&str] = &[
    "id",
    "symbol",
    "exchange",
    "side",
    "qty",
    "qty_mode",
    "segment",
    "expiry",
    "sl_pts",
    "target_pts",
    "trail",
    "risk_unit",
];
const TRAIL_FIELDS: &[&str] = &["x", "y"];
const LOCK_PROFIT_FIELDS: &[&str] = &["mode", "if_profit_reaches", "lock_profit", "trail_step"];
const SCHEDULER_FIELDS: &[&str] = &[
    "enabled",
    "days",
    "start_time",
    "auto_stop_time",
    "default_mode",
];

pub const MIN_LEGS: usize = 1;
pub const MAX_LEGS: usize = 10;
pub const MAX_LOTS: i64 = 50;
pub const MAX_CASH_QUANTITY: i64 = 1_000_000;
pub const MAX_SIGNAL_QTY: i64 = 1_000_000;
pub const MAX_SIGNAL_LOTS: i64 = 10_000;
pub const MAX_NAME_LENGTH: usize = 200;
pub const MAX_UNDERLYING_LENGTH: usize = 50;
pub const MAX_IP_ALLOWLIST: usize = 20;

/// `CONFIG_FIELDS`: the store's PATCH allowlist plus `strategy_kind`.
pub fn config_fields() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = UPDATABLE_FIELDS.to_vec();
    v.push("strategy_kind");
    v.sort();
    v
}

type R<T> = Result<T, String>;

fn mapping<'a>(v: &'a Value, label: &str) -> R<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| format!("{} must be a JSON object", label))
}

fn reject_unknown(m: &Map<String, Value>, allowed: &[&str], label: &str) -> R<()> {
    let mut extra: Vec<&String> = m
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    extra.sort();
    let mut allowed_sorted = allowed.to_vec();
    allowed_sorted.sort();
    Err(format!(
        "{} does not accept {}. Accepted fields: {}",
        label,
        extra
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        allowed_sorted.join(", ")
    ))
}

fn get<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    m.get(k).filter(|v| !v.is_null())
}

fn required<'a>(m: &'a Map<String, Value>, k: &str, label: &str) -> R<&'a Value> {
    get(m, k).ok_or_else(|| {
        if label.is_empty() {
            format!("{} is required", k)
        } else {
            format!("{}.{} is required", label, k)
        }
    })
}

fn text(v: &Value, label: &str, max: usize) -> R<String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("{} must be text", label))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(format!("{} is required", label));
    }
    let n = s.chars().count();
    if n > max {
        return Err(format!(
            "{} must be at most {} characters, got {}",
            label, max, n
        ));
    }
    Ok(s)
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s),
        Value::Null => "None".into(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        other => other.to_string(),
    }
}

fn choice(v: &Value, allowed: &[&'static str], label: &str) -> R<&'static str> {
    let Some(s) = v.as_str() else {
        return Err(format!("{} must be one of: {}", label, allowed.join(", ")));
    };
    let t = s.trim();
    allowed
        .iter()
        .find(|o| o.eq_ignore_ascii_case(t))
        .copied()
        .ok_or_else(|| {
            format!(
                "{} must be one of: {}. Got {}",
                label,
                allowed.join(", "),
                py_repr(v)
            )
        })
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// A finite number, type preserved (int stays int, float stays float).
fn number(v: &Value, label: &str, min: Option<f64>, max: Option<f64>, gt: Option<f64>) -> R<Value> {
    let out = match v {
        Value::Bool(_) => return Err(format!("{} must be a number", label)),
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => json!(f),
            _ => return Err(format!("{} must be a number, got {}", label, py_repr(v))),
        },
        Value::Number(n) => {
            if n.as_f64().map(f64::is_finite) != Some(true) {
                return Err(format!("{} must be a number", label));
            }
            v.clone()
        }
        _ => return Err(format!("{} must be a number", label)),
    };
    let f = out.as_f64().unwrap_or(0.0);
    if let Some(m) = min {
        if f < m {
            return Err(format!(
                "{} must be {} or more, got {}",
                label,
                fmt_num(m),
                fmt_num(f)
            ));
        }
    }
    if let Some(g) = gt {
        if f <= g {
            return Err(format!(
                "{} must be greater than {}, got {}",
                label,
                fmt_num(g),
                fmt_num(f)
            ));
        }
    }
    if let Some(m) = max {
        if f > m {
            return Err(format!(
                "{} must be {} or less, got {}",
                label,
                fmt_num(m),
                fmt_num(f)
            ));
        }
    }
    Ok(out)
}

fn integer(v: &Value, label: &str, min: i64, max: i64) -> R<i64> {
    let n = match v {
        Value::Bool(_) => return Err(format!("{} must be a whole number", label)),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("{} must be a whole number, got {}", label, py_repr(v)))?,
        Value::Number(n) => match n.as_i64() {
            Some(i) => i,
            None => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_finite() && f.fract() == 0.0 {
                    f as i64
                } else {
                    return Err(format!("{} must be a whole number, got {}", label, n));
                }
            }
        },
        _ => return Err(format!("{} must be a whole number", label)),
    };
    if n < min || n > max {
        return Err(format!(
            "{} must be between {} and {}, got {}",
            label, min, max, n
        ));
    }
    Ok(n)
}

fn boolean(v: &Value, label: &str) -> R<bool> {
    v.as_bool()
        .ok_or_else(|| format!("{} must be true or false", label))
}

/// `HH:MM` 24-hour, returned canonically (`09:20`).
pub fn hhmm(v: &Value, label: &str) -> R<String> {
    let bad = || format!("{} must be a HH:MM 24-hour time, for example 09:20", label);
    let s = v.as_str().ok_or_else(bad)?.trim();
    let (h, m) = s
        .split_once(':')
        .filter(|(h, m)| (1..=2).contains(&h.len()) && m.len() == 2)
        .ok_or_else(|| format!("{}. Got {}", bad(), py_repr(v)))?;
    let (h, m): (u32, u32) = match (h.parse(), m.parse()) {
        (Ok(h), Ok(m)) => (h, m),
        _ => return Err(format!("{}. Got {}", bad(), py_repr(v))),
    };
    if h > 23 || m > 59 {
        return Err(format!(
            "{} is not a valid time of day: {}",
            label,
            py_repr(v)
        ));
    }
    Ok(format!("{:02}:{:02}", h, m))
}

fn loss_amount(v: Option<&Value>, label: &str) -> R<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    let n = number(v, label, None, None, None)?;
    if n.as_f64().unwrap_or(0.0) < 0.0 {
        return Err(format!(
            "{} is entered as a positive amount and applied as a negative threshold, so it cannot be negative. Enter 5000 to stop at a loss of 5000.",
            label
        ));
    }
    Ok(n)
}

fn risk_max(unit: &str) -> Option<f64> {
    (unit == "percent").then_some(100.0)
}

fn trail(v: Option<&Value>, label: &str, unit: &str) -> R<Option<Value>> {
    let Some(v) = v else {
        return Ok(None);
    };
    let m = mapping(v, label)?;
    reject_unknown(m, TRAIL_FIELDS, label)?;
    Ok(Some(json!({
        "x": number(required(m, "x", label)?, &format!("{}.x", label), Some(0.0), risk_max(unit), None)?,
        "y": number(required(m, "y", label)?, &format!("{}.y", label), Some(0.0), risk_max(unit), None)?,
    })))
}

fn risk_fields(
    leg: &Map<String, Value>,
    clean: &mut Map<String, Value>,
    label: &str,
    trail_label: &str,
) -> R<()> {
    let unit = choice(
        get(leg, "risk_unit").unwrap_or(&json!("points")),
        RISK_UNITS,
        &format!("{}.risk_unit", label),
    )?;
    for k in ["sl_pts", "target_pts"] {
        if let Some(v) = get(leg, k) {
            clean.insert(
                k.into(),
                number(
                    v,
                    &format!("{}.{}", label, k),
                    Some(0.0),
                    risk_max(unit),
                    None,
                )?,
            );
        }
    }
    if let Some(t) = trail(get(leg, "trail"), trail_label, unit)? {
        clean.insert("trail".into(), t);
    }
    clean.insert("risk_unit".into(), json!(unit));
    Ok(())
}

fn leg_id(leg: &Map<String, Value>, label: &str, index: usize) -> R<i64> {
    match get(leg, "id") {
        Some(v) => integer(v, &format!("{}.id", label), 1, MAX_LEGS as i64),
        None => Ok(index as i64 + 1),
    }
}

fn validate_signal_leg(raw: &Value, index: usize, g: &SymbolGeneration) -> R<Value> {
    let label = format!("legs[{}]", index);
    let leg = mapping(raw, &label)?;
    reject_unknown(leg, SIGNAL_LEG_FIELDS, &label)?;
    let segment = choice(
        get(leg, "segment").unwrap_or(&json!("cash")),
        &["cash", "futures"],
        &format!("{}.segment", label),
    )?;
    let mut c = Map::new();
    c.insert("id".into(), json!(leg_id(leg, &label, index)?));
    let symbol = text(
        required(leg, "symbol", &label)?,
        &format!("{}.symbol", label),
        100,
    )?
    .to_ascii_uppercase();
    let exch_text = text(
        required(leg, "exchange", &label)?,
        &format!("{}.exchange", label),
        20,
    )?
    .to_ascii_uppercase();
    let exchange = choice(
        &json!(exch_text),
        SIGNAL_LEG_EXCHANGES,
        &format!("{}.exchange", label),
    )?;
    c.insert("symbol".into(), json!(symbol));
    c.insert("exchange".into(), json!(exchange));
    c.insert(
        "side".into(),
        json!(choice(
            get(leg, "side").unwrap_or(&json!("both")),
            LEG_SIDES,
            &format!("{}.side", label)
        )?),
    );
    c.insert("segment".into(), json!(segment));
    let derivative = is_derivative_exchange(exchange);
    let qty_mode = choice(
        get(leg, "qty_mode").unwrap_or(&json!(if derivative { "lots" } else { "units" })),
        QTY_MODES,
        &format!("{}.qty_mode", label),
    )?;
    if qty_mode == "lots" && !derivative {
        return Err(format!(
            "{}.qty_mode is 'lots', but {} has no lot size. Cash instruments are counted in units.",
            label, exchange
        ));
    }
    c.insert("qty_mode".into(), json!(qty_mode));
    let qty = integer(
        required(leg, "qty", &label)?,
        &format!("{}.qty", label),
        1,
        if qty_mode == "lots" {
            MAX_SIGNAL_LOTS
        } else {
            MAX_SIGNAL_QTY
        },
    )?;
    c.insert("qty".into(), json!(qty));
    if segment == "futures" {
        c.insert(
            "expiry".into(),
            json!(choice(
                get(leg, "expiry").unwrap_or(&json!("current")),
                LEG_EXPIRIES,
                &format!("{}.expiry", label)
            )?),
        );
    } else if get(leg, "expiry").is_some() {
        return Err(format!("{}.expiry does not apply to a cash leg", label));
    }
    risk_fields(leg, &mut c, &label, &label)?;
    if qty_mode == "units" {
        let (whole, lot) = quantity_is_whole_lots(g, qty, &symbol, exchange);
        if !whole {
            return Err(format!(
                "{}.qty is {}, which is not a whole number of lots. {} on {} trades in lots of {}.",
                label,
                qty,
                symbol,
                exchange,
                lot.unwrap_or(0)
            ));
        }
    }
    Ok(Value::Object(c))
}

fn validate_leg(raw: &Value, index: usize) -> R<Value> {
    let label = format!("legs[{}]", index);
    let leg = mapping(raw, &label)?;
    reject_unknown(leg, LEG_FIELDS, &label)?;
    let segment = choice(
        required(leg, "segment", &label)?,
        LEG_SEGMENTS,
        &format!("{}.segment", label),
    )?;
    let mut c = Map::new();
    c.insert("id".into(), json!(leg_id(leg, &label, index)?));
    c.insert("segment".into(), json!(segment));
    c.insert(
        "position".into(),
        json!(choice(
            required(leg, "position", &label)?,
            LEG_POSITIONS,
            &format!("{}.position", label)
        )?),
    );
    c.insert(
        "lots".into(),
        json!(integer(
            required(leg, "lots", &label)?,
            &format!("{}.lots", label),
            1,
            if segment == "cash" {
                MAX_CASH_QUANTITY
            } else {
                MAX_LOTS
            }
        )?),
    );
    if segment == "options" {
        c.insert(
            "option_type".into(),
            json!(choice(
                required(leg, "option_type", &label)?,
                LEG_OPTION_TYPES,
                &format!("{}.option_type", label)
            )?),
        );
        let mode = choice(
            get(leg, "strike_mode").unwrap_or(&json!("atm")),
            LEG_STRIKE_MODES,
            &format!("{}.strike_mode", label),
        )?;
        c.insert("strike_mode".into(), json!(mode));
        if mode == "atm" {
            if get(leg, "strike").is_some() {
                return Err(format!(
                    "{}.strike is only used when strike_mode is 'strike'. Set strike_mode to 'strike', or remove the strike.",
                    label
                ));
            }
            let offsets: Vec<String> = std::iter::once("ATM".to_string())
                .chain((1..=5).map(|n| format!("ITM{}", n)))
                .chain((1..=5).map(|n| format!("OTM{}", n)))
                .collect();
            let raw_off = get(leg, "atm_offset").cloned().unwrap_or(json!("ATM"));
            let t = raw_off.as_str().unwrap_or("").trim().to_ascii_uppercase();
            if !offsets.contains(&t) {
                return Err(format!(
                    "{}.atm_offset must be one of: {}. Got {}",
                    label,
                    offsets.join(", "),
                    py_repr(&raw_off)
                ));
            }
            c.insert("atm_offset".into(), json!(t));
        } else {
            if get(leg, "atm_offset").is_some() {
                return Err(format!(
                    "{}.atm_offset is only used when strike_mode is 'atm'. Set strike_mode to 'atm', or remove the offset.",
                    label
                ));
            }
            c.insert(
                "strike".into(),
                number(
                    required(leg, "strike", &label)?,
                    &format!("{}.strike", label),
                    None,
                    None,
                    Some(0.0),
                )?,
            );
        }
    } else {
        for f in ["option_type", "strike_mode", "atm_offset", "strike"] {
            if get(leg, f).is_some() {
                return Err(format!("{}.{} is only valid on an options leg", label, f));
            }
        }
    }
    if segment == "cash" {
        if get(leg, "expiry").is_some() {
            return Err(format!("{}.expiry is not valid on a cash leg", label));
        }
    } else {
        c.insert(
            "expiry".into(),
            json!(choice(
                required(leg, "expiry", &label)?,
                LEG_EXPIRIES,
                &format!("{}.expiry", label)
            )?),
        );
    }
    risk_fields(leg, &mut c, &label, &format!("{}.trail", label))?;
    Ok(Value::Object(c))
}

fn validate_legs(raw: &Value, kind: &str, g: &SymbolGeneration) -> R<Vec<Value>> {
    let list = raw.as_array().ok_or("legs must be a list")?;
    if list.len() < MIN_LEGS {
        return Err(format!("A strategy needs at least {} leg", MIN_LEGS));
    }
    if list.len() > MAX_LEGS {
        return Err(format!(
            "A strategy takes at most {} legs, got {}",
            MAX_LEGS,
            list.len()
        ));
    }
    let legs = list
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if kind == "signal" {
                validate_signal_leg(l, i, g)
            } else {
                validate_leg(l, i)
            }
        })
        .collect::<R<Vec<_>>>()?;
    let mut ids: Vec<i64> = legs.iter().filter_map(|l| l["id"].as_i64()).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != n {
        return Err("Every leg needs its own id".into());
    }
    Ok(legs)
}

fn lock_profit(v: Option<&Value>) -> R<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    let label = "lock_profit";
    let m = mapping(v, label)?;
    reject_unknown(m, LOCK_PROFIT_FIELDS, label)?;
    let mode = choice(
        required(m, "mode", label)?,
        LOCK_PROFIT_MODES,
        "lock_profit.mode",
    )?;
    let reaches = number(
        required(m, "if_profit_reaches", label)?,
        "lock_profit.if_profit_reaches",
        None,
        None,
        Some(0.0),
    )?;
    let locked = number(
        required(m, "lock_profit", label)?,
        "lock_profit.lock_profit",
        Some(0.0),
        None,
        None,
    )?;
    if locked.as_f64() > reaches.as_f64() {
        return Err(
            "lock_profit.lock_profit cannot be more than lock_profit.if_profit_reaches: the floor would be above the profit that arms it"
                .into(),
        );
    }
    let mut c = Map::new();
    c.insert("mode".into(), json!(mode));
    c.insert("if_profit_reaches".into(), reaches);
    c.insert("lock_profit".into(), locked);
    if mode == "lock_and_trail" {
        let step = get(m, "trail_step")
            .ok_or("lock_profit.trail_step is required when mode is 'lock_and_trail'")?;
        c.insert(
            "trail_step".into(),
            number(step, "lock_profit.trail_step", None, None, Some(0.0))?,
        );
    } else if let Some(step) = get(m, "trail_step") {
        c.insert(
            "trail_step".into(),
            number(step, "lock_profit.trail_step", None, None, Some(0.0))?,
        );
    }
    Ok(Value::Object(c))
}

fn scheduler(v: Option<&Value>) -> R<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    let label = "scheduler";
    let m = mapping(v, label)?;
    reject_unknown(m, SCHEDULER_FIELDS, label)?;
    let enabled = boolean(
        get(m, "enabled").unwrap_or(&json!(false)),
        "scheduler.enabled",
    )?;
    let raw_days = get(m, "days").cloned().unwrap_or(json!([]));
    let list = raw_days
        .as_array()
        .ok_or("scheduler.days must be a list of days, MON to SUN")?;
    let mut days: Vec<&'static str> = Vec::new();
    for (i, d) in list.iter().enumerate() {
        let c = choice(d, SCHEDULER_DAYS, &format!("scheduler.days[{}]", i))?;
        if days.contains(&c) {
            return Err(format!("scheduler.days lists {} more than once", c));
        }
        days.push(c);
    }
    if enabled && days.is_empty() {
        return Err("scheduler.days needs at least one day when the scheduler is enabled".into());
    }
    let (start_raw, stop_raw) = if enabled {
        (
            Some(required(m, "start_time", label)?),
            Some(required(m, "auto_stop_time", label)?),
        )
    } else {
        (get(m, "start_time"), get(m, "auto_stop_time"))
    };
    let start = start_raw
        .map(|v| hhmm(v, "scheduler.start_time"))
        .transpose()?;
    let stop = stop_raw
        .map(|v| hhmm(v, "scheduler.auto_stop_time"))
        .transpose()?;
    if let (Some(a), Some(b)) = (&start, &stop) {
        if a >= b {
            return Err(
                "scheduler.start_time must be earlier than scheduler.auto_stop_time".into(),
            );
        }
    }
    days.sort_by_key(|d| SCHEDULER_DAYS.iter().position(|x| x == d));
    Ok(json!({
        "enabled": enabled,
        "days": days,
        "start_time": start,
        "auto_stop_time": stop,
        "default_mode": choice(get(m, "default_mode").unwrap_or(&json!("sandbox")), RUN_MODES, "scheduler.default_mode")?,
    }))
}

fn ip_allowlist(v: Option<&Value>) -> R<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    let label = "webhook_ip_allowlist";
    let list = v
        .as_array()
        .ok_or_else(|| format!("{} must be a list of IP addresses or CIDR ranges", label))?;
    if list.len() > MAX_IP_ALLOWLIST {
        return Err(format!(
            "{} takes at most {} entries, got {}",
            label,
            MAX_IP_ALLOWLIST,
            list.len()
        ));
    }
    let mut out = Vec::new();
    for (i, e) in list.iter().enumerate() {
        let t = e
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{}[{}] must be an IP address or CIDR range", label, i))?;
        if super::webhook::parse_network(t).is_none() {
            return Err(format!(
                "{}[{}] is not a valid IP address or CIDR range: '{}'",
                label, i, t
            ));
        }
        out.push(json!(t));
    }
    Ok(Value::Array(out))
}

fn tab_for_legs(raw: &Map<String, Value>, legs: &[Value]) -> &'static str {
    if legs.iter().any(|l| l["segment"] == "cash") {
        return "stocks_fno";
    }
    let ux = raw
        .get("underlying_exchange")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    if ["MCX", "NCDEX", "NCO"].contains(&ux.as_str()) {
        return "mcx";
    }
    if legs.iter().any(|l| {
        matches!(
            l["expiry"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "weekly" | "next_week"
        )
    }) {
        "weekly_monthly"
    } else {
        "monthly_only"
    }
}

fn tab_segments(tab: &str) -> &'static [&'static str] {
    match tab {
        "stocks_fno" => &["cash", "futures", "options"],
        _ => &["futures", "options"],
    }
}

/// Validate a whole strategy configuration (create, and a merged PATCH).
pub fn validate_strategy_config(payload: &Value, g: &SymbolGeneration) -> R<Value> {
    let raw = mapping(payload, "The request body")?;
    let fields = config_fields();
    reject_unknown(raw, &fields, "The request")?;
    let kind = choice(
        get(raw, "strategy_kind").unwrap_or(&json!("batch")),
        STRATEGY_KINDS,
        "strategy_kind",
    )?;
    let legs = validate_legs(required(raw, "legs", "")?, kind, g)?;
    let mut c = Map::new();
    c.insert(
        "name".into(),
        json!(text(required(raw, "name", "")?, "name", MAX_NAME_LENGTH)?),
    );
    c.insert("strategy_kind".into(), json!(kind));
    c.insert(
        "direction".into(),
        json!(choice(
            get(raw, "direction").unwrap_or(&json!("both")),
            DIRECTIONS,
            "direction"
        )?),
    );
    let tab = match get(raw, "universe_tab") {
        Some(v) => choice(v, UNIVERSE_TABS, "universe_tab")?,
        None => tab_for_legs(raw, &legs),
    };
    c.insert("universe_tab".into(), json!(tab));
    c.insert(
        "underlying".into(),
        json!(text(
            required(raw, "underlying", "")?,
            "underlying",
            MAX_UNDERLYING_LENGTH
        )?
        .to_ascii_uppercase()),
    );
    c.insert(
        "underlying_exchange".into(),
        json!(choice(
            required(raw, "underlying_exchange", "")?,
            UNDERLYING_EXCHANGES,
            "underlying_exchange"
        )?),
    );
    let stype = choice(
        get(raw, "strategy_type").unwrap_or(&json!("intraday")),
        STRATEGY_TYPES,
        "strategy_type",
    )?;
    c.insert("strategy_type".into(), json!(stype));
    let product = choice(
        get(raw, "product").unwrap_or(&json!("NRML")),
        PRODUCTS,
        "product",
    )?;
    c.insert("product".into(), json!(product));
    c.insert(
        "pricetype".into(),
        json!(choice(
            get(raw, "pricetype").unwrap_or(&json!("MARKET")),
            PRICETYPES,
            "pricetype"
        )?),
    );
    c.insert("legs".into(), Value::Array(legs.clone()));
    c.insert(
        "overall_sl_mtm".into(),
        loss_amount(get(raw, "overall_sl_mtm"), "overall_sl_mtm")?,
    );
    c.insert(
        "overall_target_mtm".into(),
        match get(raw, "overall_target_mtm") {
            Some(v) => number(v, "overall_target_mtm", Some(0.0), None, None)?,
            None => Value::Null,
        },
    );
    c.insert("lock_profit".into(), lock_profit(get(raw, "lock_profit"))?);
    c.insert(
        "trail_sl_to_entry".into(),
        json!(boolean(
            get(raw, "trail_sl_to_entry").unwrap_or(&json!(false)),
            "trail_sl_to_entry"
        )?),
    );
    c.insert("scheduler".into(), scheduler(get(raw, "scheduler"))?);
    c.insert(
        "daily_loss_limit_inr".into(),
        loss_amount(get(raw, "daily_loss_limit_inr"), "daily_loss_limit_inr")?,
    );
    c.insert(
        "webhook_ip_allowlist".into(),
        ip_allowlist(get(raw, "webhook_ip_allowlist"))?,
    );

    let entry = get(raw, "entry_time");
    let exit = get(raw, "exit_time");
    if stype == "intraday" {
        if entry.is_none() {
            return Err("entry_time is required for an intraday strategy".into());
        }
        if exit.is_none() {
            return Err("exit_time is required for an intraday strategy".into());
        }
    }
    let entry = entry.map(|v| hhmm(v, "entry_time")).transpose()?;
    let exit = exit.map(|v| hhmm(v, "exit_time")).transpose()?;
    if let (Some(a), Some(b)) = (&entry, &exit) {
        if a >= b {
            return Err("entry_time must be earlier than exit_time".into());
        }
    }
    c.insert("entry_time".into(), json!(entry));
    c.insert("exit_time".into(), json!(exit));

    // Cross-field rules.
    if kind == "signal" {
        let direction = c["direction"].as_str().unwrap_or("both");
        let accepted: &[&str] = match direction {
            "long_only" => &["long", "both"],
            "short_only" => &["short", "both"],
            _ => &["long", "short", "both"],
        };
        for (i, leg) in legs.iter().enumerate() {
            let side = leg["side"].as_str().unwrap_or("both");
            if !accepted.contains(&side) {
                let mut a = accepted.to_vec();
                a.sort();
                return Err(format!(
                    "legs[{}].side is '{}', which a '{}' strategy never acts on. Use {}, or change the strategy direction.",
                    i,
                    side,
                    direction,
                    a.join(" or ")
                ));
            }
        }
    }
    let allowed = tab_segments(tab);
    for (i, leg) in legs.iter().enumerate() {
        if let Some(seg) = leg["segment"].as_str() {
            if !allowed.contains(&seg) {
                return Err(format!(
                    "legs[{}].segment is '{}', which the '{}' universe does not offer. That tab trades {}.",
                    i,
                    seg,
                    tab,
                    allowed.join(" and ")
                ));
            }
        }
    }
    if kind == "signal" {
        for (i, leg) in legs.iter().enumerate() {
            let seg = leg["segment"].as_str().unwrap_or("");
            let ex = leg["exchange"].as_str().unwrap_or("");
            if seg == "cash" && is_derivative_exchange(ex) {
                return Err(format!(
                    "legs[{}] is a cash leg on {}, which lists derivatives. Use NSE or BSE for cash, or set the segment to 'futures'.",
                    i, ex
                ));
            }
            if seg == "futures" && !is_derivative_exchange(ex) {
                return Err(format!(
                    "legs[{}] is a futures leg on {}, which lists cash. Use a derivative exchange, or set the segment to 'cash'.",
                    i, ex
                ));
            }
        }
    } else if product != "MIS" {
        for (i, leg) in legs.iter().enumerate() {
            if leg["segment"] == "cash" && leg["position"] == "S" {
                return Err(format!(
                    "legs[{}] sells cash short, but product '{}' carries the position. Cash cannot be held short overnight. Use MIS for an intraday short, or make the leg long.",
                    i, product
                ));
            }
        }
    }
    Ok(Value::Object(c))
}
