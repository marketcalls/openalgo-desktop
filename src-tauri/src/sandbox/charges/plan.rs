//! The trader's brokerage plan: its config keys, defaults, validation and
//! wording (web `sandbox/charges.py`, configuration part). Still no I/O; the
//! caller reads and writes `sandbox_config`.

use super::rates::{segment, SEGMENTS};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;
use std::str::FromStr;

/// The one switch that turns every charge off, stored as `true` or `false`.
pub const CHARGES_ENABLED_KEY: &str = "charges_enabled";

/// The most a trader may enter: guards against a slip of the keyboard (20
/// typed into the percentage box would take a fifth of every trade), not
/// limits a broker observes.
pub const MAX_BROKERAGE_FLAT: Decimal = dec!(1000);
pub const MAX_BROKERAGE_PERCENT: Decimal = dec!(5);

/// The switch and one segment's brokerage, read together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub enabled: bool,
    pub flat: Decimal,
    pub percent: Decimal,
}

pub fn brokerage_flat_key(segment_key: &str) -> String {
    format!("brokerage_{segment_key}_flat")
}

pub fn brokerage_percent_key(segment_key: &str) -> String {
    format!("brokerage_{segment_key}_percent")
}

/// A decimal as a trader would type it: no exponent, no trailing zeros.
pub fn plain(value: Decimal) -> String {
    let text = value.normalize().to_string();
    if text == "-0" || text.is_empty() {
        "0".to_string()
    } else {
        text
    }
}

/// The plan in one sentence, as a broker's pricing page would word it.
pub fn describe_brokerage(flat: Decimal, percent: Decimal) -> String {
    match (flat > Decimal::ZERO, percent > Decimal::ZERO) {
        (true, true) => format!(
            "{} per executed order or {}% of the trade value, whichever is lower",
            plain(flat),
            plain(percent)
        ),
        (true, false) => format!("{} per executed order", plain(flat)),
        (false, true) => format!("{}% of the trade value", plain(percent)),
        (false, false) => "No brokerage".to_string(),
    }
}

/// Every charge setting with its default and description, as
/// `sandbox_config` holds them: one source for the seed, the migration and
/// both resets.
pub fn default_config_rows() -> Vec<(String, String, String)> {
    let mut rows = vec![(
        CHARGES_ENABLED_KEY.to_string(),
        "true".to_string(),
        "Charge brokerage and statutory charges on sandbox trades (true or false)".to_string(),
    )];
    for s in SEGMENTS.iter() {
        rows.push((
            brokerage_flat_key(s.key),
            plain(s.default_flat),
            format!(
                "{} brokerage per executed order (0 = not used). Range: 0-{}",
                s.label,
                plain(MAX_BROKERAGE_FLAT)
            ),
        ));
        rows.push((
            brokerage_percent_key(s.key),
            plain(s.default_percent),
            format!(
                "{} brokerage as a percentage of trade value (0 = not used). Range: 0-{}",
                s.label,
                plain(MAX_BROKERAGE_PERCENT)
            ),
        ));
    }
    rows
}

/// [`default_config_rows`] as `(key, value)`.
pub fn default_config_values() -> Vec<(String, String)> {
    default_config_rows()
        .into_iter()
        .map(|(k, v, _)| (k, v))
        .collect()
}

/// Every `sandbox_config` key the plan owns.
pub fn config_keys() -> Vec<String> {
    default_config_rows()
        .into_iter()
        .map(|(k, _, _)| k)
        .collect()
}

/// Python `Decimal(str(value).strip())`: a finite number, or `None`. Takes
/// plain and exponent forms ("1e3").
pub fn parse_amount(text: &str) -> Option<Decimal> {
    let t = text.trim();
    Decimal::from_str(t)
        .or_else(|_| Decimal::from_scientific(t))
        .ok()
}

/// A stored or submitted number, falling back when it is not one.
pub fn to_decimal(value: Option<&str>, default: Decimal) -> Decimal {
    value.and_then(parse_amount).unwrap_or(default)
}

/// Whether a stored `charges_enabled` means on. Anything unclear is on, so a
/// damaged setting never silently makes every trade free.
pub fn is_enabled(value: Option<&str>) -> bool {
    let v = value.unwrap_or("None").trim().to_ascii_lowercase();
    !matches!(v.as_str(), "false" | "0" | "no" | "off")
}

/// A submitted value as Python's `str(value)` reads it: text as is, a number
/// in its JSON form, anything else unreadable.
fn submitted(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Check one segment's brokerage as a trader submitted it: `Ok((flat,
/// percent))`, or a sentence for the trader naming the segment.
pub fn validate_brokerage(
    segment_key: &str,
    flat: &Value,
    percent: &Value,
) -> Result<(Decimal, Decimal), String> {
    let Some(s) = segment(segment_key) else {
        return Err("That segment is not on the rate card.".to_string());
    };
    let parse = |v: &Value, what: &str| {
        submitted(v)
            .as_deref()
            .and_then(parse_amount)
            .ok_or_else(|| format!("{}: enter a number for the {}.", s.label, what))
    };
    let flat = parse(flat, "amount per order")?;
    let percent = parse(percent, "percentage")?;
    if flat < Decimal::ZERO || percent < Decimal::ZERO {
        return Err(format!("{}: brokerage cannot be negative.", s.label));
    }
    if flat > MAX_BROKERAGE_FLAT {
        return Err(format!(
            "{}: the amount per order cannot be more than {}.",
            s.label,
            plain(MAX_BROKERAGE_FLAT)
        ));
    }
    if percent > MAX_BROKERAGE_PERCENT {
        return Err(format!(
            "{}: the percentage cannot be more than {}. Enter 0.03 for 0.03%, not 3.",
            s.label,
            plain(MAX_BROKERAGE_PERCENT)
        ));
    }
    Ok((flat, percent))
}

/// Check a value written to one of the plan's keys on its own, through the
/// sandbox's one-key update, so that route cannot store a value the
/// calculation would have to guess at. `None` when acceptable or not a plan
/// key.
pub fn validate_config_value(key: &str, value: &str) -> Option<String> {
    if key == CHARGES_ENABLED_KEY {
        let v = value.trim().to_ascii_lowercase();
        return (v != "true" && v != "false")
            .then(|| "Charges can only be switched on (true) or off (false).".to_string());
    }
    let given = Value::String(value.to_string());
    let zero = Value::String("0".to_string());
    for s in SEGMENTS.iter() {
        if key == brokerage_flat_key(s.key) {
            return validate_brokerage(s.key, &given, &zero).err();
        }
        if key == brokerage_percent_key(s.key) {
            return validate_brokerage(s.key, &zero, &given).err();
        }
    }
    None
}
