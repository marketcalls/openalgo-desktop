//! Side-effect-free contracts shared by Flow node validation and execution:
//! the web's `services/flow_node_contracts.py`, plus the price monitor's
//! condition vocabulary (`FlowPriceMonitor.normalize_condition`), which the
//! validator also reads.
//!
//! One copy of each rule, so the validator, the executor, the monitors and
//! the editor's routes cannot drift: a leg the validator accepts must not be
//! refused mid-basket by the run, after earlier legs have filled. Nothing
//! here reads the clock, the database or the network; the day an expiry is
//! chosen against is an argument.

use crate::agent::tools::values::Pattern;
use chrono::{Datelike, NaiveDate};

/// Order-update status values a watch can match on, sorted as the web lists
/// them (`sorted(VALID_STATUSES)`); `any` matches every update.
pub const VALID_STATUSES: [&str; 6] = [
    "any",
    "cancelled",
    "complete",
    "open",
    "rejected",
    "trigger pending",
];

/// How a manually built leg picks its strike. OFFSET re-resolves against the
/// live underlying on every run; STRIKE names one contract and is used as
/// given.
pub const VALID_LEG_STRIKE_MODES: [&str; 2] = ["OFFSET", "STRIKE"];

/// Relative expiries the options nodes understand, sorted as the web's
/// messages list them (`', '.join(sorted(VALID_EXPIRY_TYPES))`).
pub const VALID_EXPIRY_TYPES: [&str; 4] =
    ["current_month", "current_week", "next_month", "next_week"];

/// Segments that trade contracts carried on margin rather than cash-settled
/// holdings. A position in one of these is normally taken NRML, so that is
/// what a node defaults to when its author never picked a product; MIS is
/// the default everywhere else. Index pseudo-exchanges are absent because no
/// order is ever placed on them, and so is CRYPTO, as on the web.
pub const DERIVATIVE_EXCHANGES: [&str; 7] = ["BCD", "BFO", "CDS", "MCX", "NCDEX", "NCO", "NFO"];

/// The price monitor's own conditions, and the editor's spellings it accepts
/// for them (`FlowPriceMonitor._CONDITION_ALIASES`).
const PRICE_CONDITION_ALIASES: [(&str, &str); 9] = [
    ("above", "greater_than"),
    ("price_above", "greater_than"),
    ("below", "less_than"),
    ("price_below", "less_than"),
    ("crosses_above", "crossing_up"),
    ("cross_above", "crossing_up"),
    ("crosses_below", "crossing_down"),
    ("cross_below", "crossing_down"),
    ("crosses", "crossing"),
];

/// An expiry named on a leg, in DDMMMYY, e.g. 28OCT25.
static EXPIRY_DATE_PATTERN: Pattern = Pattern::new(r"^\d{2}[A-Z]{3}\d{2}$");

/// A strike named relative to the money: ATM, ITM1-ITM50, OTM1-OTM50.
static OPTION_OFFSET_PATTERN: Pattern = Pattern::new(r"^(?:ATM|(?:ITM|OTM)(?:[1-9]|[1-4]\d|50))$");

/// An underlying with an expiry inside it: NIFTY28AUG26, CRUDEOIL19NOV25FUT.
/// Python's `$` also matches before one trailing newline.
static UNDERLYING_WITH_EXPIRY: Pattern = Pattern::new(r"^([A-Z]+)(\d{2}[A-Z]{3}\d{2})(?:FUT)?\n?$");

/// Master-contract expiry shapes, in the web's order (`%d-%b-%y`, `%d%b%y`,
/// `%d-%B-%Y`, `%d%B%Y`), as Python's `strptime` reads them: a one- or
/// two-digit day (a leading space allowed), the English month name, and a
/// two-digit year 69-99 in the 1900s.
static EXPIRY_INPUTS: [Pattern; 4] = [
    Pattern::new(
        r"^(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])-(JAN|FEB|MAR|APR|MAY|JUN|JUL|AUG|SEP|OCT|NOV|DEC)-([0-9]{2})$",
    ),
    Pattern::new(
        r"^(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])(JAN|FEB|MAR|APR|MAY|JUN|JUL|AUG|SEP|OCT|NOV|DEC)([0-9]{2})$",
    ),
    Pattern::new(
        r"^(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])-(SEPTEMBER|FEBRUARY|NOVEMBER|DECEMBER|JANUARY|OCTOBER|AUGUST|MARCH|APRIL|JUNE|JULY|MAY)-([0-9]{4})$",
    ),
    Pattern::new(
        r"^(3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])(SEPTEMBER|FEBRUARY|NOVEMBER|DECEMBER|JANUARY|OCTOBER|AUGUST|MARCH|APRIL|JUNE|JULY|MAY)([0-9]{4})$",
    ),
];

/// Whether `text` is a DDMMMYY expiry (`EXPIRY_DATE_PATTERN.fullmatch`).
/// Callers upper-case first, as the web does.
pub fn is_expiry_date(text: &str) -> bool {
    EXPIRY_DATE_PATTERN.is_match(text)
}

/// Whether `text` is ATM, ITM1-ITM50 or OTM1-OTM50
/// (`OPTION_OFFSET_PATTERN.fullmatch`).
pub fn is_option_offset(text: &str) -> bool {
    OPTION_OFFSET_PATTERN.is_match(text)
}

/// The product a node on `exchange` uses when its author picked none.
///
/// This is a default, never an override: a product the author actually
/// chose is stored on the node and wins, so an intraday NFO order stays MIS.
pub fn default_product_for_exchange(exchange: Option<&str>) -> &'static str {
    let segment = crate::flow::validator::py::strip(exchange.unwrap_or_default()).to_uppercase();
    if DERIVATIVE_EXCHANGES.contains(&segment.as_str()) {
        "NRML"
    } else {
        "MIS"
    }
}

/// One master-contract expiry string as a date, or None if unreadable.
pub fn parse_expiry_date(value: &str) -> Option<NaiveDate> {
    let upper = value.to_uppercase();
    EXPIRY_INPUTS.iter().find_map(|pattern| {
        let caps = pattern.captures(&upper)?;
        let day: u32 = caps.get(1)?.as_str().trim().parse().ok()?;
        let month = month_number(caps.get(2)?.as_str())?;
        let year_text = caps.get(3)?.as_str();
        let mut year: i32 = year_text.parse().ok()?;
        if year_text.len() == 2 {
            year += if year <= 68 { 2000 } else { 1900 };
        }
        NaiveDate::from_ymd_opt(year, month, day)
    })
}

fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let index = MONTHS.iter().position(|m| name.starts_with(m))?;
    u32::try_from(index + 1).ok()
}

/// DDMMMYY, the form every options API here expects.
pub fn format_expiry_for_api(value: &str) -> String {
    value.replace('-', "").to_uppercase()
}

/// The expiry a relative type resolves to, in DDMMMYY.
///
/// Shared by the executor and the editor's expiry picker, so the panel's
/// "Same as node - 28AUG26" is the date the run uses. The weekly types index
/// into the sorted list, so on a monthly-only product such as an MCX
/// commodity `current_week` is simply the nearest expiry. The monthly types
/// take the last expiry within the month, which is the monthly contract on a
/// weekly-listed underlying. `today` is the trading day the month is counted
/// from (the web reads the server's clock here).
///
/// Returns None when the type cannot be satisfied: no second expiry to be
/// `next_week`, no contract in that month, or a type that is not known.
pub fn select_expiry<S: AsRef<str>>(
    expiries: &[S],
    expiry_type: &str,
    today: NaiveDate,
) -> Option<String> {
    let mut parsed: Vec<(&str, NaiveDate)> = expiries
        .iter()
        .filter_map(|value| {
            let value = value.as_ref();
            parse_expiry_date(value).map(|when| (value, when))
        })
        .collect();
    // Stable, as Python's sort: equal dates keep the order they were listed.
    parsed.sort_by_key(|(_, when)| *when);
    let first = parsed.first()?;

    match expiry_type {
        "current_week" => return Some(format_expiry_for_api(first.0)),
        "next_week" => return parsed.get(1).map(|(value, _)| format_expiry_for_api(value)),
        "current_month" | "next_month" => {}
        _ => return None,
    }

    let (mut month, mut year) = (today.month(), today.year());
    if expiry_type == "next_month" {
        (month, year) = if month == 12 {
            (1, year + 1)
        } else {
            (month + 1, year)
        };
    }
    parsed
        .iter()
        .rev()
        .find(|(_, when)| when.month() == month && when.year() == year)
        .map(|(value, _)| format_expiry_for_api(value))
}

/// Canonicalize a broker order status for watch validation and matching.
pub fn normalize_status(value: &str) -> String {
    crate::flow::validator::py::strip(value)
        .to_lowercase()
        .replace('_', " ")
}

/// Split an optional DDMMMYY expiry from an underlying symbol:
/// `NIFTY28AUG26` is `("NIFTY", Some("28AUG26"))`, `NIFTY` is
/// `("NIFTY", None)`.
pub fn parse_underlying_symbol(underlying: &str) -> (String, Option<String>) {
    let upper = underlying.to_uppercase();
    let split = UNDERLYING_WITH_EXPIRY.as_ref().and_then(|re| {
        let caps = re.captures(&upper)?;
        Some((
            caps.get(1)?.as_str().to_string(),
            caps.get(2)?.as_str().to_string(),
        ))
    });
    match split {
        Some((base, expiry)) => (base, Some(expiry)),
        None => (upper, None),
    }
}

/// The condition name the price monitor compares on, accepting the editor's
/// spellings (`FlowPriceMonitor.normalize_condition`).
pub fn normalize_condition(value: &str) -> String {
    let raw = crate::flow::validator::py::strip(value)
        .to_lowercase()
        .replace('-', "_");
    PRICE_CONDITION_ALIASES
        .iter()
        .find(|(alias, _)| *alias == raw)
        .map_or(raw, |(_, canonical)| (*canonical).to_string())
}

#[cfg(test)]
mod tests;
