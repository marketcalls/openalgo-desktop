//! Argument handling every toolkit shares (the web's `market.py`, "Shared
//! argument handling"): exchange spellings, the index-exchange correction,
//! `EXCH:SYM` pairs and symbol lists, dates and history ranges, and the
//! broker's intervals with their aliases and the weekly and monthly
//! roll-up.
//!
//! Module level, as on the web, because the chart, instrument, indicator and
//! live tools reach the same candles through the same corrections; a second
//! copy would drift. Every refusal is a [`ToolOutcome::Retry`] worded by
//! `invalid_argument`.

use super::{
    INDEX_EXCHANGES, IST, MAX_HISTORY_CALENDAR_DAYS, MAX_MULTIQUOTE_SYMBOLS, VALID_EXCHANGES,
};
use crate::agent::contract::tool::ToolOutcome;
use crate::agent::tools::values::{py_str, quote, text, truthy_text, type_name, Pattern};
use chrono::{Duration, NaiveDate};
use serde_json::{Map, Value};

/// Whether the instrument master lists a symbol on an exchange (the web's
/// `is_listed`, a `get_token` lookup). Built once per call from one master
/// snapshot ([`super::master`]); a test passes a fixed set.
pub type Listed<'a> = &'a (dyn Fn(&str, &str) -> bool + Sync);

/// One instrument: its symbol and exchange.
pub type Pair = (String, String);

/// A normalised symbol list and the notices about what was corrected.
pub type PairList = (Vec<Pair>, Vec<String>);

/// A refusal worded as every refusal is.
pub fn refuse(field: &str, reason: &str, fix: &str) -> ToolOutcome {
    ToolOutcome::invalid_argument(field, reason, Some(fix))
}

/// The exchange codes as one sentence fragment, in the web's order.
fn exchange_list() -> String {
    VALID_EXCHANGES.join(", ")
}

/// The index exchange to try first for the exchange asked for.
fn index_first_choice(exchange: &str) -> Option<&'static str> {
    match exchange {
        "NSE" | "NFO" => Some("NSE_INDEX"),
        "BSE" | "BFO" => Some("BSE_INDEX"),
        "MCX" => Some("MCX_INDEX"),
        _ => None,
    }
}

/// Every index exchange, the one most likely meant first.
pub fn index_candidates(exchange: &str) -> Vec<&'static str> {
    match index_first_choice(exchange) {
        None => INDEX_EXCHANGES.to_vec(),
        Some(first) => std::iter::once(first)
            .chain(INDEX_EXCHANGES.iter().copied().filter(|e| *e != first))
            .collect(),
    }
}

/// Move a symbol onto an index exchange when that is where the master lists
/// it, and only then: an unknown symbol stays unknown, and a tradable
/// instrument is never moved onto a quote-only exchange.
pub fn resolve_exchange(
    listed: Listed<'_>,
    symbol: &str,
    exchange: &str,
) -> (String, Option<String>) {
    if listed(symbol, exchange) {
        return (exchange.to_string(), None);
    }
    for candidate in index_candidates(exchange) {
        if candidate == exchange || !listed(symbol, candidate) {
            continue;
        }
        return (
            candidate.to_string(),
            Some(format!(
                "{symbol} is not listed on {exchange}, but it is listed on {candidate}, so {candidate} was used. Index values are quoted on the index exchanges and cannot be traded there; the tradable instrument is the index future or option."
            )),
        );
    }
    (exchange.to_string(), None)
}

static SPACES_OR_HYPHENS: Pattern = Pattern::new(r"[\s-]+");

/// An exchange spelled as OpenAlgo stores it: capitals, spaces and hyphens
/// as underscores (`nse index`, `NSE-INDEX` -> `NSE_INDEX`). Only spells;
/// whether the code is accepted is the caller's check.
pub fn clean_exchange(value: Option<&Value>) -> String {
    clean_exchange_text(&text(value))
}

/// [`clean_exchange`] of text.
pub fn clean_exchange_text(value: &str) -> String {
    SPACES_OR_HYPHENS
        .replace_all(&value.trim().to_uppercase(), "_")
        .into_owned()
}

/// One symbol and exchange, normalised, with the index correction. An
/// `EXCH:SYM` prefix that is an exchange code wins over the exchange
/// argument, with a notice when the two differ.
pub fn normalise_pair(
    listed: Listed<'_>,
    symbol: Option<&Value>,
    exchange: Option<&Value>,
) -> Result<(String, String, Vec<String>), ToolOutcome> {
    let mut notices = Vec::new();
    let mut cleaned_symbol = truthy_text(symbol).to_uppercase();
    let mut cleaned_exchange = clean_exchange(exchange);
    if let Some((prefix, rest)) = cleaned_symbol.clone().split_once(':') {
        let prefixed = clean_exchange_text(prefix);
        if VALID_EXCHANGES.contains(&prefixed.as_str()) {
            if !cleaned_exchange.is_empty() && cleaned_exchange != prefixed {
                notices.push(format!(
                    "The symbol was written as {}, so {} was used rather than the exchange {} passed beside it.",
                    quote(&cleaned_symbol),
                    prefixed,
                    cleaned_exchange
                ));
            }
            cleaned_symbol = rest.trim().to_string();
            cleaned_exchange = prefixed;
        }
    }
    if cleaned_symbol.is_empty() {
        return Err(refuse(
            "symbol",
            "it is empty",
            "Pass the OpenAlgo symbol in capitals, for example 'INFY' or 'NIFTY'.",
        ));
    }
    if cleaned_exchange.is_empty() {
        return Err(refuse(
            "exchange",
            "it is empty",
            &format!("Pass one of: {}.", exchange_list()),
        ));
    }
    if !VALID_EXCHANGES.contains(&cleaned_exchange.as_str()) {
        return Err(refuse(
            "exchange",
            &format!(
                "{} is not an OpenAlgo exchange code",
                quote(&cleaned_exchange)
            ),
            &format!("Pass one of: {}.", exchange_list()),
        ));
    }
    let (resolved, notice) = resolve_exchange(listed, &cleaned_symbol, &cleaned_exchange);
    notices.extend(notice);
    Ok((cleaned_symbol, resolved, notices))
}

/// The history source: `api` (the default) or `db` (the local Historify
/// store).
pub fn normalise_source(source: Option<&Value>) -> Result<String, ToolOutcome> {
    let mut cleaned = truthy_text(source).to_lowercase();
    if cleaned.is_empty() {
        cleaned = "api".into();
    }
    if cleaned != "api" && cleaned != "db" {
        return Err(refuse(
            "source",
            &format!("{} is not a data source", quote(&cleaned)),
            "Use 'api' for live broker history, or 'db' for candles already downloaded into the local Historify store.",
        ));
    }
    Ok(cleaned)
}

static ISO_DATETIME: Pattern = Pattern::new(r"\A(\d{4}-\d{2}-\d{2})[T ]");
static DAY_MONTH_YEAR: Pattern = Pattern::new(r"\A(\d{1,2})-(\d{1,2})-(\d{4})\z");
static YEAR_SLASH: Pattern = Pattern::new(r"\A(\d{4})/(\d{1,2})/(\d{1,2})\z");
static DAY_MONTHNAME_YEAR: Pattern = Pattern::new(r"\A(\d{1,2})-([A-Za-z]{3})-(\d{4})\z");
static YEAR_MONTH_DAY: Pattern = Pattern::new(r"\A(\d{4})-(\d{1,2})-(\d{1,2})\z");

fn ymd(y: &str, m: &str, d: &str) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
}

/// The date spellings a model uses, read in one fixed order (09-10-2026 is
/// always the ninth of October), or `None`.
fn parse_date(raw: &str, today: NaiveDate) -> Option<NaiveDate> {
    match raw.to_lowercase().as_str() {
        "today" => return Some(today),
        "yesterday" => return today.pred_opt(),
        _ => {}
    }
    let text = match ISO_DATETIME.captures(raw) {
        Some(c) => c.get(1).map_or(raw, |m| m.as_str()),
        None => raw,
    };
    if let Some(c) = DAY_MONTH_YEAR.captures(text) {
        return ymd(&c[3], &c[2], &c[1]);
    }
    if let Some(c) = YEAR_SLASH.captures(text) {
        return ymd(&c[1], &c[2], &c[3]);
    }
    if let Some(c) = DAY_MONTHNAME_YEAR.captures(text) {
        let month = super::MONTHS
            .iter()
            .position(|m| *m == c[2].to_uppercase())?;
        return ymd(&c[3], &(month + 1).to_string(), &c[1]);
    }
    let c = YEAR_MONTH_DAY.captures(text)?;
    ymd(&c[1], &c[2], &c[3])
}

/// One date argument as `YYYY-MM-DD`.
pub fn normalise_date(
    field: &str,
    value: Option<&Value>,
    today: NaiveDate,
) -> Result<String, ToolOutcome> {
    let raw = truthy_text(value);
    match parse_date(&raw, today) {
        Some(d) => Ok(d.format("%Y-%m-%d").to_string()),
        None => Err(refuse(
            field,
            &format!("{} is not a date", quote(&raw)),
            "Use YYYY-MM-DD, for example 2026-01-15.",
        )),
    }
}

/// Bars per trading day for an interval (the web's `_BARS_PER_DAY`,
/// looked up lower-cased; an unknown interval counts as 5 minute bars).
fn bars_per_day(interval: &str) -> f64 {
    match interval.to_lowercase().as_str() {
        "1m" => 375.0,
        "3m" => 125.0,
        "5m" => 75.0,
        "10m" => 38.0,
        "15m" => 25.0,
        "30m" => 13.0,
        "1h" | "60m" => 7.0,
        "2h" => 4.0,
        "3h" => 3.0,
        "4h" => 2.0,
        "d" | "1d" | "day" => 1.0,
        "w" | "week" => 0.2,
        "m" | "month" => 0.05,
        "q" | "quarter" => 0.016,
        "y" | "year" => 0.004,
        _ => 75.0,
    }
}

/// Calendar days that should cover `bars` candles (the web's
/// `indicator_service.calendar_days_for_bars`): 1.6 for weekends and
/// holidays, plus five.
pub fn calendar_days_for_bars(bars: u32, interval: &str) -> i64 {
    ((f64::from(bars) / bars_per_day(interval)) * 1.6) as i64 + 5
}

/// The `YYYY-MM-DD` range covering about `bars` candles, ending on `end`.
pub fn lookback_range(interval: &str, bars: u32, end: NaiveDate) -> (String, String) {
    let days = calendar_days_for_bars(bars.max(1), interval).min(MAX_HISTORY_CALENDAR_DAYS);
    let start = end - Duration::days(days);
    (
        start.format("%Y-%m-%d").to_string(),
        end.format("%Y-%m-%d").to_string(),
    )
}

fn given(value: Option<&Value>) -> bool {
    !text(value).is_empty()
}

/// A history range whose dates the model may have left out: a missing end
/// is today (IST), a missing start the lookback covering `bars` candles at
/// `interval` ending on that end.
pub fn history_range(
    interval: &str,
    start_date: Option<&Value>,
    end_date: Option<&Value>,
    bars: u32,
    today: NaiveDate,
) -> Result<(String, String), ToolOutcome> {
    let mut end = match given(end_date) {
        true => Some(normalise_date("end_date", end_date, today)?),
        false => None,
    };
    let start = if given(start_date) {
        truthy_text(start_date)
    } else {
        let anchor = end
            .as_deref()
            .and_then(|e| NaiveDate::parse_from_str(e, "%Y-%m-%d").ok())
            .unwrap_or(today);
        let (start, default_end) = lookback_range(interval, bars, anchor);
        end = end.or(Some(default_end));
        start
    };
    let end = end.unwrap_or_else(|| today.format("%Y-%m-%d").to_string());
    normalise_range(&start, &end, today)
}

/// A validated range: both dates read, the start not after the end.
pub fn normalise_range(
    start_date: &str,
    end_date: &str,
    today: NaiveDate,
) -> Result<(String, String), ToolOutcome> {
    let start = normalise_date("start_date", Some(&Value::from(start_date)), today)?;
    let end = normalise_date("end_date", Some(&Value::from(end_date)), today)?;
    if start > end {
        return Err(refuse(
            "start_date",
            &format!("it is {}, which is after end_date {}", start, end),
            "Pass the earlier day as start_date.",
        ));
    }
    Ok((start, end))
}

static DAILY_ALIAS: Pattern = Pattern::new(r"(?i)\A(?:1\s*d|1\s*day|day|daily)\z");
static WEEKLY_ALIAS: Pattern = Pattern::new(r"(?i)\A(?:1\s*w|1\s*week|week|weekly)\z");
static MONTHLY_ALIAS: Pattern = Pattern::new(r"(?i)\A(?:1\s*mo|1\s*month|month|monthly)\z");
static MINUTE_ALIAS: Pattern = Pattern::new(r"(?i)\A(\d+)\s*(?:min|mins|minute|minutes)\z");
static HOUR_ALIAS: Pattern = Pattern::new(r"(?i)\A(\d+)\s*(?:hr|hrs|hour|hours)\z");

/// A common interval spelling in OpenAlgo's own (`1d` -> `D`, `5min` ->
/// `5m`, `2hours` -> `2h`), or `None` when it is none of them or already
/// OpenAlgo's spelling. A bare `m` or `M` is never an alias: only the case
/// tells minutes from months.
pub fn interval_alias(text: &str) -> Option<String> {
    let number = |c: regex::Captures<'_>| c[1].parse::<u64>().ok();
    let alias = if DAILY_ALIAS.is_match(text) {
        "D".to_string()
    } else if WEEKLY_ALIAS.is_match(text) {
        "W".to_string()
    } else if MONTHLY_ALIAS.is_match(text) {
        "M".to_string()
    } else if let Some(n) = MINUTE_ALIAS.captures(text).and_then(number) {
        format!("{}m", n)
    } else {
        let n = HOUR_ALIAS.captures(text).and_then(number)?;
        format!("{}h", n)
    };
    (alias != text).then_some(alias)
}

/// The broker's spelling of daily (`D`, `1D`, `1d`, `day`, `1day`, in that
/// preference), or `D` when it lists none or the list is unknown.
pub fn daily_interval(accepted: Option<&[String]>) -> String {
    for spelling in ["D", "1D", "1d", "day", "1day"] {
        if accepted.is_some_and(|a| a.iter().any(|v| v == spelling)) {
            return spelling.to_string();
        }
    }
    "D".into()
}

fn listed_in(accepted: &[String], value: &str) -> bool {
    accepted.iter().any(|v| v == value)
}

/// The candle interval checked against what this broker accepts: an alias
/// translated, `60m` read as `1h` where the broker offers only that, a
/// case-only difference corrected, each with a notice. The check is skipped
/// for the local store (`db`) and when the broker's list could not be read.
pub fn normalise_interval(
    interval: Option<&Value>,
    source: &str,
    accepted: Option<&[String]>,
) -> Result<(String, Option<String>), ToolOutcome> {
    let cleaned = truthy_text(interval);
    if cleaned.is_empty() {
        return Err(refuse(
            "interval",
            "it is empty",
            "Pass a candle size such as '5m', '1h' or 'D'. Call get_intervals for the ones this broker accepts.",
        ));
    }
    let accepted = accepted.filter(|a| !a.is_empty());
    if source == "api" && accepted.is_some_and(|a| listed_in(a, &cleaned)) {
        return Ok((cleaned, None));
    }
    let alias = interval_alias(&cleaned);
    let alias_notice = alias.as_ref().map(|a| {
        format!(
            "The interval was read as {} rather than {}. OpenAlgo writes minutes as 'm', hours as 'h', and days, weeks and months as 'D', 'W' and 'M'.",
            quote(a),
            quote(&cleaned)
        )
    });
    let accepted = match accepted {
        Some(a) if source == "api" => a,
        _ => {
            return Ok(match alias {
                Some(a) => (a, alias_notice),
                None => (cleaned, None),
            })
        }
    };
    if let Some(a) = alias.as_ref().filter(|a| listed_in(accepted, a)) {
        return Ok((a.clone(), alias_notice));
    }
    if alias.as_deref().unwrap_or(&cleaned) == "60m"
        && listed_in(accepted, "1h")
        && !listed_in(accepted, "60m")
    {
        return Ok((
            "1h".into(),
            Some(format!(
                "The interval was read as '1h' rather than {}, because this broker offers hourly candles as '1h' and not as '60m'.",
                quote(&cleaned)
            )),
        ));
    }
    let matches: Vec<&String> = accepted
        .iter()
        .filter(|v| v.to_lowercase() == cleaned.to_lowercase())
        .collect();
    if let [only] = matches.as_slice() {
        return Ok((
            (*only).clone(),
            Some(format!(
                "The interval was read as {} rather than {}. Interval names are case sensitive: 'm' is minutes and 'M' is months.",
                quote(only),
                quote(&cleaned)
            )),
        ));
    }
    Err(refuse(
        "interval",
        &format!("{} is not one this broker accepts", quote(&cleaned)),
        &format!("Use one of: {}.", accepted.join(", ")),
    ))
}

/// The weekly and monthly targets a daily-only broker is rolled up to.
fn rollup_label(target: &str) -> Option<&'static str> {
    match target {
        "W" => Some("weekly"),
        "M" => Some("monthly"),
        _ => None,
    }
}

/// The interval to fetch, the notice when anything changed, and the roll-up
/// target (`W` or `M`) when a broker serving neither must have its daily
/// candles rolled up ([`super::candles::roll_up_candles`]).
pub fn plan_interval(
    interval: Option<&Value>,
    source: &str,
    accepted: Option<&[String]>,
) -> Result<(String, Option<String>, Option<String>), ToolOutcome> {
    let cleaned = truthy_text(interval);
    let target = interval_alias(&cleaned).unwrap_or(cleaned);
    if let (Some(label), Some(list)) = (rollup_label(&target), accepted) {
        if source == "api" && !list.is_empty() && !listed_in(list, &target) {
            let daily = daily_interval(Some(list));
            if listed_in(list, &daily) {
                return Ok((
                    daily,
                    Some(format!(
                        "This broker serves no {label} candles, so {label} candles were built from its daily ones: the open of the first day, the high and low across the period, the close of the last day and the volume summed."
                    )),
                    Some(target),
                ));
            }
        }
    }
    let (fetched, notice) = normalise_interval(interval, source, accepted)?;
    Ok((fetched, notice, None))
}

fn lowered_keys(map: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in map {
        out.insert(k.trim().to_lowercase(), v.clone());
    }
    out
}

/// The raw symbol and exchange of one symbol-list entry: an object (keys
/// any case) or an `EXCH:SYM` string.
pub fn pair_fields(
    item: &Value,
    index: usize,
    field: &str,
) -> Result<(Option<Value>, Option<Value>), ToolOutcome> {
    match item {
        Value::Object(map) => {
            let lowered = lowered_keys(map);
            let symbol = lowered.get("symbol").filter(|v| !v.is_null()).cloned();
            let exchange = lowered.get("exchange").filter(|v| !v.is_null()).cloned();
            let prefixed = symbol
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains(':'));
            if symbol.is_none() || (exchange.is_none() && !prefixed) {
                return Err(refuse(
                    field,
                    &format!("entry {} is missing 'symbol' or 'exchange'", index + 1),
                    "Every entry needs both, for example {\"symbol\": \"INFY\", \"exchange\": \"NSE\"}.",
                ));
            }
            Ok((symbol, exchange))
        }
        Value::String(s) if s.contains(':') => {
            let (exchange, symbol) = s.split_once(':').unwrap_or((s.as_str(), ""));
            Ok((Some(Value::from(symbol)), Some(Value::from(exchange))))
        }
        other => Err(refuse(
            field,
            &format!(
                "entry {} is {}, not an object carrying a symbol and an exchange",
                index + 1,
                type_name(other)
            ),
            "Pass objects, for example [{\"symbol\": \"INFY\", \"exchange\": \"NSE\"}].",
        )),
    }
}

static LIST_SEPARATORS: Pattern = Pattern::new(r"[,;\n]");

/// A model's list of instruments, normalised: the documented list of
/// objects, one object, a JSON string of either, `EXCH:SYM` strings or one
/// separated string of them. Every pair goes through [`normalise_pair`];
/// duplicates are asked for once. Over `limit` the call is refused, or with
/// `truncate` the extra entries are dropped with a notice (a live card).
pub fn symbol_pairs(
    listed: Listed<'_>,
    symbols: Option<&Value>,
    field: &str,
    limit: usize,
    truncate: bool,
) -> Result<PairList, ToolOutcome> {
    let empty_fix = "Pass a list of objects, for example [{\"symbol\": \"INFY\", \"exchange\": \"NSE\"}, {\"symbol\": \"SBIN\", \"exchange\": \"NSE\"}].";
    let mut raw = symbols.cloned().unwrap_or(Value::Null);
    if let Value::String(s) = &raw {
        raw = match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => {
                let parts: Vec<Value> = LIST_SEPARATORS
                    .split(s)
                    .into_iter()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(Value::from)
                    .collect();
                if parts.is_empty() || !parts.iter().all(|p| py_str(p).contains(':')) {
                    return Err(refuse(
                        field,
                        "it is a string that is neither valid JSON nor a list of EXCHANGE:SYMBOL entries",
                        "Pass a list of objects, for example [{\"symbol\": \"INFY\", \"exchange\": \"NSE\"}], or a comma separated string such as 'NSE:INFY, NSE:SBIN'.",
                    ));
                }
                Value::Array(parts)
            }
        };
    }
    if raw.is_object() {
        raw = Value::Array(vec![raw]);
    }
    let mut items = match raw {
        Value::Array(items) if !items.is_empty() => items,
        _ => return Err(refuse(field, "it is empty or is not a list", empty_fix)),
    };
    let mut notices = Vec::new();
    if items.len() > limit {
        if !truncate {
            return Err(refuse(
                field,
                &format!(
                    "it carries {} entries, more than the {} allowed in one call",
                    items.len(),
                    limit
                ),
                &format!(
                    "Split it into batches of at most {} and call the tool once per batch.",
                    limit
                ),
            ));
        }
        notices.push(format!(
            "{} of {} entries were dropped, because this call carries at most {}.",
            items.len() - limit,
            items.len(),
            limit
        ));
        items.truncate(limit);
    }
    let mut pairs: Vec<Pair> = Vec::new();
    let mut duplicates = 0usize;
    for (index, item) in items.iter().enumerate() {
        let (symbol, exchange) = pair_fields(item, index, field)?;
        let (symbol, exchange, item_notices) =
            normalise_pair(listed, symbol.as_ref(), exchange.as_ref())?;
        notices.extend(item_notices);
        if pairs.iter().any(|(s, e)| *s == symbol && *e == exchange) {
            duplicates += 1;
            continue;
        }
        pairs.push((symbol, exchange));
    }
    if duplicates > 0 {
        notices.push(format!(
            "{} duplicate entries were requested once each rather than repeatedly.",
            duplicates
        ));
    }
    Ok((pairs, notices))
}

/// [`symbol_pairs`] with the `get_quotes` limit.
pub fn quote_pairs(listed: Listed<'_>, symbols: Option<&Value>) -> Result<PairList, ToolOutcome> {
    symbol_pairs(listed, symbols, "symbols", MAX_MULTIQUOTE_SYMBOLS, false)
}

/// Today in IST from the app's clock.
pub fn today(app: &crate::state::AppState) -> NaiveDate {
    app.now().with_timezone(&IST).date_naive()
}
