//! The option vocabulary the live cards and the option charts share (the
//! web's `services/agent/tools/option_viz.py`, "Small pure helpers" and the
//! expiry choices; lead decision 5: owned here, imported by the viz lane).
//!
//! Legs: [`leg_entries`] reads the `legs` argument however it was sent,
//! [`leg_symbol`] the contract one entry names (a symbol, `NFO:SYMBOL`, or
//! the `24500CE` shorthand), [`leg_side_and_lots`] its side and size
//! whatever keys the model used, [`resolve_leg_exchange`] and
//! [`resolve_contract`] settle it against the instrument master, and
//! [`signed_multiplier`] and [`leg_label`] give its contribution and name.
//!
//! Expiries: [`EXPIRY_CHOICES`] is the one vocabulary (`current_week`,
//! `next_week`, `current_month`, `next_month` and their spellings) and
//! [`choose_expiry`] / [`resolve_expiry`] always come out as a date the
//! master lists, never the word they were given.
//!
//! Every refusal is a [`ToolOutcome::Retry`] worded as the web words it.

use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::tools::market::data_of;
use crate::agent::tools::market::normalise::Listed;
use crate::agent::tools::numbers::as_number;
use crate::agent::tools::options::{normalise_expiry, normalise_int};
use crate::agent::tools::symbols::{symbol_expiry, DERIVATIVE_EXCHANGES};
use crate::agent::tools::values::{quote, text, truthy_text, type_name, Pattern};
use crate::services::options_service;
use chrono::{Datelike, Duration, NaiveDate};
use serde_json::{json, Map, Value};
use std::future::Future;

/// Most legs one card or chart carries.
pub const MAX_LEGS: usize = 8;

/// How a model may name an expiry without knowing the dates; each resolves
/// against the listed expiries.
pub const EXPIRY_CHOICES: [&str; 4] = ["current_week", "next_week", "current_month", "next_month"];

/// What an unnamed expiry means: the nearest listed one.
pub const DEFAULT_EXPIRY_CHOICE: &str = "current_week";

/// A bought leg.
pub const BUY: &str = "BUY";
/// A sold leg.
pub const SELL: &str = "SELL";
/// An option contract's segment.
pub const SEGMENT_OPTION: &str = "OPTION";
/// A futures contract's segment.
pub const SEGMENT_FUTURE: &str = "FUTURE";

/// Exchanges searched, in order, for a contract named without one.
const LEG_SEARCH_ORDER: [&str; 8] = ["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"];

/// Exchanges searched, in order, for an underlying named without one.
const UNDERLYING_SEARCH_ORDER: [&str; 6] = ["NSE_INDEX", "BSE_INDEX", "NSE", "BSE", "MCX", "CDS"];

static OPTION_SUFFIX: Pattern = Pattern::new(r"(CE|PE)\z");
static FUTURE_SUFFIX: Pattern = Pattern::new(r"FUT\z");
static EMBEDDED_EXPIRY: Pattern = Pattern::new(r"\d{2}[A-Z]{3}\d{2}");
static SHORTHAND_LEG: Pattern = Pattern::new(r"\A(\d+(?:\.\d+)?)(CE|PE)\z");
static QUALIFIED_LEG: Pattern = Pattern::new(r"\A([A-Z_]{2,12}):(.+)\z");
static LEG_SEPARATORS: Pattern = Pattern::new(r"[,\n]");

const LIST_FIX: &str =
    "Pass a list, for example [\"NIFTY08SEP2623850CE\", \"NIFTY08SEP2623850PE\"].";

fn refuse(field: &str, reason: &str, fix: &str) -> ToolOutcome {
    ToolOutcome::invalid_argument(field, reason, Some(fix))
}

/// The `legs` argument as entries, however the model sent it: a list, one
/// object, JSON text of either, or a comma or newline separated string.
/// Empty when nothing was passed (a meaningful choice, not an error); more
/// than [`MAX_LEGS`] is refused.
pub fn leg_entries(value: Option<&Value>, field: &str) -> Result<Vec<Value>, ToolOutcome> {
    let mut raw = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => return Ok(Vec::new()),
        Some(v) => v.clone(),
    };
    if let Value::String(s) = &raw {
        let t = s.trim();
        raw = if t.starts_with('[') || t.starts_with('{') {
            serde_json::from_str(t)
                .map_err(|_| refuse(field, "it is a string that is not valid JSON", LIST_FIX))?
        } else {
            Value::Array(
                LEG_SEPARATORS
                    .split(t)
                    .into_iter()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(Value::from)
                    .collect(),
            )
        };
    }
    if raw.is_object() {
        raw = Value::Array(vec![raw]);
    }
    let Value::Array(items) = raw else {
        return Err(refuse(
            field,
            &format!("it is {}, not a list", type_name(&raw)),
            LIST_FIX,
        ));
    };
    if items.len() > MAX_LEGS {
        return Err(refuse(
            field,
            &format!(
                "it carries {} legs, more than the {} one chart shows",
                items.len(),
                MAX_LEGS
            ),
            "Chart the legs that answer the question; a wider structure is unreadable here.",
        ));
    }
    Ok(items)
}

/// A leg's side: `BUY` (also B, LONG, +, or nothing at all) or `SELL` (S,
/// SHORT, -).
pub fn leg_side(value: Option<&Value>, position: usize) -> Result<&'static str, ToolOutcome> {
    let t = text(value).to_uppercase();
    match t.as_str() {
        "" | "BUY" | "B" | "LONG" | "+" => Ok(BUY),
        "SELL" | "S" | "SHORT" | "-" => Ok(SELL),
        _ => Err(refuse(
            "legs",
            &format!(
                "leg {} names side {}, which is not a side",
                position,
                quote(&t)
            ),
            "Use 'BUY' for a long leg or 'SELL' for a short one.",
        )),
    }
}

/// A leg's lot count, 1 to 100, one by default.
pub fn leg_lots(value: Option<&Value>, position: usize) -> Result<i64, ToolOutcome> {
    match value {
        None | Some(Value::Null) => Ok(1),
        Some(Value::String(s)) if s.is_empty() => Ok(1),
        given => normalise_int(
            given,
            &format!("legs (leg {} lots)", position),
            1,
            100,
            None,
        ),
    }
}

fn lowered(map: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in map {
        out.insert(k.trim().to_lowercase(), v.clone());
    }
    out
}

/// A leg's side and whole lots, whatever keys the model used: the side as
/// `side`, `action` or `transaction_type`; the size as `lots`, or as
/// `quantity` / `qty` converted with the contract's lot size, which
/// `lot_size` reads only when a quantity was given. A bare string entry is
/// one bought lot; a null lot count is one lot, with a notice.
pub async fn leg_side_and_lots<F, Fut>(
    entry: &Value,
    position: usize,
    lot_size: F,
    notices: &mut Vec<String>,
) -> Result<(&'static str, i64), ToolOutcome>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Option<i64>, ToolOutcome>>,
{
    let Value::Object(map) = entry else {
        return Ok((BUY, 1));
    };
    let keys = lowered(map);
    let side_value = ["side", "action", "transaction_type"]
        .iter()
        .filter_map(|k| keys.get(*k))
        .find(|v| !v.is_null() && *v != "");
    let side = leg_side(side_value, position)?;
    if let Some(lots) = keys.get("lots").filter(|v| !v.is_null()) {
        return Ok((side, leg_lots(Some(lots), position)?));
    }
    let Some(quantity_key) = ["quantity", "qty"]
        .into_iter()
        .find(|k| keys.get(*k).is_some_and(|v| !v.is_null()))
    else {
        if keys.contains_key("lots") {
            notices.push(format!(
                "Leg {} gave no lot count, so one lot was used.",
                position
            ));
        }
        return Ok((side, 1));
    };
    let field = format!("legs (leg {} {})", position, quantity_key);
    let quantity = normalise_int(keys.get(quantity_key), &field, 1, 10_000_000, None)?;
    let size = lot_size().await?.filter(|s| *s > 0).ok_or_else(|| {
        refuse(
            &field,
            "a quantity was given, but the instrument master has no lot size for this contract, so it cannot be turned into lots",
            "Pass 'lots' instead, for example {\"lots\": 1}.",
        )
    })?;
    if quantity % size != 0 {
        return Err(refuse(
            &field,
            &format!(
                "{} is not a whole number of lots; one lot of this contract is {}",
                quantity, size
            ),
            &format!("Pass a multiple of {}, or pass 'lots' instead.", size),
        ));
    }
    Ok((side, leg_lots(Some(&json!(quantity / size)), position)?))
}

/// One contract's lot size from the instrument master, or `None` when it
/// holds no positive one.
pub async fn master_lot_size(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
) -> Result<Option<i64>, ToolOutcome> {
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    let payload = ctx
        .service_read("symbol_service.get_symbol_info", "symbol", request)
        .await?;
    let size = data_of(payload).get("lotsize").and_then(as_number);
    Ok(size.filter(|s| *s > 0.0).map(|s| s as i64))
}

/// A strike as Python's `format(x, 'f')` with the trailing zeros and point
/// removed (`23850.5`, `23850`).
fn strike_text(strike: f64) -> String {
    let t = format!("{:.6}", strike);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The symbol (upper-cased) and exchange (empty when not named) one leg
/// entry names: the OpenAlgo symbol, `NFO:SYMBOL`, or the strike-and-right
/// shorthand (`23850CE`, also as `{"strike": 23850, "option_type": "CE"}`),
/// which needs the call's underlying and `DDMMMYY` expiry.
pub fn leg_symbol(
    entry: &Value,
    position: usize,
    underlying: &str,
    expiry: &str,
) -> Result<(String, String), ToolOutcome> {
    let (mut symbol, mut exchange) = match entry {
        Value::Object(map) => {
            let keys = lowered(map);
            let pick = |a: &str, b: &str| {
                let first = truthy_text(keys.get(a));
                if first.is_empty() {
                    truthy_text(keys.get(b))
                } else {
                    first
                }
            };
            let mut symbol = pick("symbol", "tradingsymbol").to_uppercase();
            let exchange = truthy_text(keys.get("exchange")).to_uppercase();
            let right = pick("option_type", "right").to_uppercase();
            let strike = keys.get("strike").and_then(as_number);
            if let Some(s) = strike.filter(|_| symbol.is_empty() && (right == "CE" || right == "PE")) {
                symbol = format!("{}{}", strike_text(s), right);
            }
            (symbol, exchange)
        }
        Value::String(s) => (s.trim().to_uppercase(), String::new()),
        other => {
            return Err(refuse(
                "legs",
                &format!("leg {} is {}, not a contract", position, type_name(other)),
                "Pass strings such as \"NIFTY08SEP2623850CE\", or objects such as {\"symbol\": \"NIFTY08SEP2623850CE\", \"exchange\": \"NFO\", \"side\": \"SELL\", \"lots\": 1}.",
            ))
        }
    };
    if let Some(c) = QUALIFIED_LEG.captures(&symbol.clone()) {
        if exchange.is_empty() {
            exchange = c[1].to_string();
        }
        symbol = c[2].trim().to_string();
    }
    if let Some(c) = SHORTHAND_LEG.captures(&symbol.clone()) {
        if underlying.is_empty() || expiry.is_empty() {
            return Err(refuse(
                "legs",
                &format!(
                    "leg {} is {}, which is a strike and a right with no contract in front of it",
                    position,
                    quote(&symbol)
                ),
                "Either pass the whole OpenAlgo symbol, such as 'NIFTY08SEP2623850CE', or set underlying and expiry_date on this call so the shorthand can be expanded.",
            ));
        }
        let mut strike = c[1].to_string();
        if strike.contains('.') {
            strike = strike
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
        }
        symbol = format!("{}{}{}{}", underlying, expiry, strike, &c[2]);
    }
    if symbol.is_empty() {
        return Err(refuse(
            "legs",
            &format!("leg {} names no symbol", position),
            "Pass the OpenAlgo symbol of each contract, for example 'NIFTY08SEP2623850CE'.",
        ));
    }
    Ok((symbol, exchange))
}

/// Every expiry spelling a model produces as `DDMMMYY` (the options
/// toolkit's reading; keywords are resolved before this).
pub fn normalise_leg_expiry(value: Option<&Value>, field: &str) -> Result<String, ToolOutcome> {
    normalise_expiry(value, field)
}

/// The exchange the master lists a leg on: the one named (checked), or the
/// first derivatives exchange that lists it. A contract listed nowhere is
/// refused: a chart of a symbol that does not exist is worse than none.
pub fn resolve_leg_exchange(
    listed: Listed<'_>,
    symbol: &str,
    exchange: &str,
    where_: &str,
) -> Result<String, ToolOutcome> {
    if !exchange.is_empty() {
        if !DERIVATIVE_EXCHANGES.contains(&exchange) {
            return Err(refuse(
                "legs",
                &format!("{} names exchange {}, which lists no futures or options", where_, quote(exchange)),
                &format!(
                    "Use one of: {}. An index code such as NSE_INDEX is a quote-only feed and carries no contract.",
                    DERIVATIVE_EXCHANGES.join(", ")
                ),
            ));
        }
        if !listed(symbol, exchange) {
            return Err(refuse(
                "legs",
                &format!("{}, {} on {}, is not in the instrument master", where_, symbol, exchange),
                "Resolve the contract first with get_option_symbol or search_symbols, and pass the symbol it returns. Do not adjust a strike or an expiry to make one fit.",
            ));
        }
        return Ok(exchange.to_string());
    }
    LEG_SEARCH_ORDER
        .into_iter()
        .find(|c| listed(symbol, c))
        .map(str::to_string)
        .ok_or_else(|| {
            refuse(
                "legs",
                &format!("{}, {}, is not listed on any derivatives exchange", where_, symbol),
                "Resolve the contract first with get_option_symbol or search_symbols, and pass the symbol it returns rather than assembling one.",
            )
        })
}

/// `OPTION` or `FUTURE` from the OpenAlgo symbol format. Anything else is
/// refused when the trader named it (`where_`), or answered with an empty
/// segment when it came out of the position book (`None`), for the caller
/// to report.
pub fn segment_of(symbol: &str, where_: Option<&str>) -> Result<&'static str, ToolOutcome> {
    let dated = EMBEDDED_EXPIRY.is_match(symbol);
    if OPTION_SUFFIX.is_match(symbol) && dated {
        return Ok(SEGMENT_OPTION);
    }
    if FUTURE_SUFFIX.is_match(symbol) && dated {
        return Ok(SEGMENT_FUTURE);
    }
    match where_ {
        None => Ok(""),
        Some(w) => Err(refuse(
            "legs",
            &format!("{}, {}, is neither an option nor a future", w, symbol),
            "This chart is built from derivative contracts. An option is [base][DDMMMYY][strike][CE or PE] and a future is [base][DDMMMYY]FUT.",
        )),
    }
}

/// One leg's multiplier in a combined premium: +lots bought, -lots sold
/// (per unit; the lot size is deliberately not in it).
pub fn signed_multiplier(side: &str, lots: i64) -> i64 {
    if side == BUY {
        lots
    } else {
        -lots
    }
}

/// A leg's name for a legend or a confirmation: `SELL 2x NIFTY...CE`, the
/// multiplier left out when it is one.
pub fn leg_label(leg: &Map<String, Value>) -> String {
    let lots = leg
        .get("lots")
        .filter(|v| crate::agent::tools::values::truthy(v))
        .and_then(as_number)
        .map_or(1, |l| l as i64);
    let count = if lots == 1 {
        String::new()
    } else {
        format!(" {}x", lots)
    };
    let side = leg.get("side").and_then(Value::as_str).unwrap_or(BUY);
    let symbol = leg
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{}{} {}", side, count, symbol)
}

/// The exchange an underlying is quoted on: the one named, or the first the
/// master lists it on (index feeds first), with a notice.
pub fn resolve_underlying_exchange(
    listed: Listed<'_>,
    base: &str,
    value: Option<&Value>,
    notices: &mut Vec<String>,
) -> Result<String, ToolOutcome> {
    let t = text(value).to_uppercase();
    if !t.is_empty() {
        return Ok(t);
    }
    if let Some(candidate) = UNDERLYING_SEARCH_ORDER
        .into_iter()
        .find(|c| listed(base, c))
    {
        notices.push(format!(
            "{} was resolved to {} from the instrument master.",
            base, candidate
        ));
        return Ok(candidate.to_string());
    }
    Err(refuse(
        "underlying",
        &format!("{} is not listed on any exchange this tool searched", base),
        &format!(
            "Pass the exchange explicitly, one of {}, or look the symbol up with search_symbols first.",
            UNDERLYING_SEARCH_ORDER.join(", ")
        ),
    ))
}

/// Every option expiry the master still lists for an underlying, as
/// `DDMMMYY`, nearest first; empty when it lists no options.
pub async fn listed_expiries(
    ctx: &ToolCtx,
    base: &str,
    venue: &str,
) -> Result<Vec<String>, ToolOutcome> {
    let mut request = Map::new();
    request.insert("symbol".into(), json!(base));
    request.insert(
        "exchange".into(),
        json!(options_service::option_exchange(venue)),
    );
    request.insert("instrumenttype".into(), json!("options"));
    let payload = ctx
        .service_read("expiry_service.get_expiry_dates", "expiry", request)
        .await?;
    Ok(match data_of(payload) {
        Value::Array(dates) => dates.iter().filter_map(symbol_expiry).collect(),
        _ => Vec::new(),
    })
}

/// Which choice a model's wording means, or `None` for an exact date.
pub fn expiry_keyword(value: Option<&Value>) -> Option<&'static str> {
    let t = truthy_text(value).to_lowercase().replace([' ', '-'], "_");
    match t.as_str() {
        "" | "current_week" | "nearest" | "weekly" | "this_week" | "front" | "front_week" => {
            Some("current_week")
        }
        "next_week" | "next" | "following_week" => Some("next_week"),
        "current_month" | "monthly" | "this_month" | "month" | "front_month" => {
            Some("current_month")
        }
        "next_month" | "following_month" => Some("next_month"),
        _ => None,
    }
}

/// `DDMMMYY` as a date (month any case).
fn expiry_date(expiry: &str) -> Option<NaiveDate> {
    options_service::parse_compact_expiry(expiry)
}

/// Whether a `DDMMMYY` expiry falls on or before the coming Sunday; an
/// unreadable one is not this week.
pub fn within_this_week(expiry: &str, today: NaiveDate) -> bool {
    let Some(moment) = expiry_date(expiry) else {
        return false;
    };
    let sunday = today + Duration::days(6 - i64::from(today.weekday().num_days_from_monday()));
    (today..=sunday).contains(&moment)
}

/// The last expiry listed in the nearest contract month (or the month
/// after, `later`): the monthly contract is the last weekly of its month.
pub fn monthly_expiry(listed: &[String], later: bool) -> String {
    let month = |e: &String| e.get(2..).unwrap_or_default().to_string();
    let mut months: Vec<String> = Vec::new();
    for entry in listed {
        let m = month(entry);
        if !months.contains(&m) {
            months.push(m);
        }
    }
    let wanted = match (later, months.as_slice()) {
        (true, [_, second, ..]) => second.clone(),
        (_, [first, ..]) => first.clone(),
        _ => return String::new(),
    };
    listed
        .iter()
        .rfind(|e| month(e) == wanted)
        .or(listed.last())
        .cloned()
        .unwrap_or_default()
}

/// An expiry choice or an exact date as one of the listed expiries (nearest
/// first), saying which date was picked; an exact date that is not listed
/// is refused with the listed ones.
pub fn choose_expiry(
    listed: &[String],
    value: Option<&Value>,
    notices: &mut Vec<String>,
    today: NaiveDate,
) -> Result<String, ToolOutcome> {
    let Some(first) = listed.first() else {
        return Err(refuse(
            "expiry",
            "no expiry is listed",
            "Name the expiry explicitly in DDMMMYY form.",
        ));
    };
    match expiry_keyword(value) {
        Some("current_week") => {
            if within_this_week(first, today) {
                notices.push(format!("{} is this week's expiry and was used.", first));
            } else {
                notices.push(format!(
                    "No expiry is listed inside the current week, so {}, the nearest listed one, was used.",
                    first
                ));
            }
            Ok(first.clone())
        }
        Some("next_week") => {
            let picked = listed.get(1).unwrap_or(first);
            if picked == first {
                notices.push(format!(
                    "{} is the only listed expiry, so the next one could not be used.",
                    picked
                ));
            } else {
                notices.push(format!(
                    "{} is the next listed expiry after {}.",
                    picked, first
                ));
            }
            Ok(picked.clone())
        }
        Some(month @ ("current_month" | "next_month")) => {
            let later = month == "next_month";
            let picked = monthly_expiry(listed, later);
            notices.push(format!(
                "{} is the last expiry listed in {} contract month.",
                picked,
                if later {
                    "the following"
                } else {
                    "the current"
                }
            ));
            Ok(picked)
        }
        _ => {
            let exact = normalise_leg_expiry(value, "expiry")?;
            if !listed.contains(&exact) {
                let shown: Vec<&str> = listed.iter().take(8).map(String::as_str).collect();
                return Err(refuse(
                    "expiry",
                    &format!("{} is not a listed expiry", exact),
                    &format!(
                        "The listed ones are {}. Pass one of those, or one of {}.",
                        shown.join(", "),
                        EXPIRY_CHOICES.join(", ")
                    ),
                ));
            }
            Ok(exact)
        }
    }
}

/// The expiry to price: an exact date costs no lookup; a word (or nothing,
/// the nearest) is resolved against the listed expiries.
pub async fn resolve_expiry(
    ctx: &ToolCtx,
    base: &str,
    venue: &str,
    value: Option<&Value>,
    notices: &mut Vec<String>,
) -> Result<String, ToolOutcome> {
    let t = text(value);
    if !t.is_empty() && expiry_keyword(Some(&json!(t))).is_none() {
        return normalise_leg_expiry(Some(&json!(t)), "expiry_date");
    }
    let listed = listed_expiries(ctx, base, venue).await?;
    if listed.is_empty() {
        return Err(refuse(
            "expiry_date",
            &format!(
                "{} lists no option expiries on {}",
                base,
                options_service::option_exchange(venue)
            ),
            "Confirm the underlying has listed options, or name the expiry explicitly in DDMMMYY form.",
        ));
    }
    let today = crate::agent::tools::market::normalise::today(&ctx.app);
    choose_expiry(&listed, Some(&json!(t)), notices, today)
}

/// One contract's own details from the instrument master (the web's
/// `resolve_contract` dict).
#[derive(Debug, Clone, PartialEq)]
pub struct Contract {
    pub symbol: String,
    pub exchange: String,
    /// `OPTION` or `FUTURE`.
    pub segment: String,
    pub strike: Option<f64>,
    /// `CE`, `PE` or empty for a future.
    pub option_type: String,
    pub lot_size: i64,
    pub tick_size: Option<f64>,
    /// `DDMMMYY`.
    pub expiry: String,
    /// The expiry instant (exchange cut-off, IST) in epoch seconds.
    pub expiry_ts: Option<i64>,
    pub base: String,
    pub underlying_exchange: String,
}

impl Contract {
    /// The web's dict, in its key order (`lotSize`, `tickSize`,
    /// `expiryTs` camel-cased as the cards read them).
    pub fn to_json(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "exchange": self.exchange,
            "segment": self.segment,
            "strike": self.strike,
            "option_type": self.option_type,
            "lotSize": self.lot_size,
            "tickSize": self.tick_size,
            "expiry": self.expiry,
            "expiryTs": self.expiry_ts,
            "base": self.base,
            "underlying_exchange": self.underlying_exchange,
        })
    }
}

/// A leg settled against the master: its exchange ([`resolve_leg_exchange`],
/// `exchange` empty to search), its segment, and the master's strike, lot
/// size, tick size, expiry and underlying. `where_` names the leg in a
/// refusal; `None` means it came from the position book.
pub async fn resolve_contract(
    ctx: &ToolCtx,
    listed: Listed<'_>,
    symbol: &str,
    exchange: &str,
    where_: Option<&str>,
) -> Result<Contract, ToolOutcome> {
    let label = where_
        .map(str::to_string)
        .unwrap_or_else(|| format!("the open position {}", symbol));
    let venue = resolve_leg_exchange(listed, symbol, exchange, &label)?;
    let segment = segment_of(symbol, Some(&label))?;
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(venue));
    let payload = ctx
        .service_read("symbol_service.get_symbol_info", "symbol", request)
        .await?;
    let info = match data_of(payload) {
        Value::Object(m) if !m.is_empty() => m,
        _ => {
            return Err(refuse(
                "legs",
                &format!("{} on {} has no row in the instrument master", symbol, venue),
                "Resolve the contract with get_option_symbol or search_symbols and pass the symbol it returns.",
            ))
        }
    };
    let expiry = EMBEDDED_EXPIRY
        .find(symbol)
        .map(str::to_string)
        .or_else(|| info.get("expiry").and_then(symbol_expiry))
        .unwrap_or_default();
    let (mut option_type, mut strike, mut expiry_ts) = (String::new(), None, None);
    if segment == SEGMENT_OPTION {
        option_type = symbol
            .get(symbol.len().saturating_sub(2)..)
            .unwrap_or_default()
            .to_string();
        strike = info.get("strike").and_then(as_number);
        match options_service::parse_option_symbol(symbol) {
            Ok(parsed) => {
                option_type = parsed.option_type.clone();
                strike = strike.or(Some(parsed.strike));
                expiry_ts = options_service::expiry_datetime(&parsed.expiry, &venue, None)
                    .map(|t| t.timestamp());
            }
            Err(e) => tracing::warn!(
                "Payoff leg {} could not be parsed for its expiry instant: {}",
                symbol,
                e
            ),
        }
    }
    let mut base = truthy_text(info.get("name")).to_uppercase();
    if base.is_empty() {
        base = EMBEDDED_EXPIRY
            .split(symbol)
            .first()
            .copied()
            .unwrap_or_default()
            .to_string();
    }
    let lot = info.get("lotsize").and_then(as_number).unwrap_or(0.0) as i64;
    Ok(Contract {
        symbol: symbol.to_string(),
        exchange: venue.clone(),
        segment: segment.to_string(),
        strike,
        option_type,
        lot_size: if lot == 0 { 1 } else { lot },
        tick_size: info.get("tick_size").and_then(as_number),
        expiry,
        expiry_ts,
        underlying_exchange: options_service::greeks_underlying_exchange(&base, &venue),
        base,
    })
}

#[cfg(test)]
mod tests;
