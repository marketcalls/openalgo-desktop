//! Option tools: the chain around ATM, one option symbol from an offset,
//! one contract's Greeks and the synthetic future (the web's
//! `services/agent/tools/options.py`, toolkit `options`).
//!
//! Every tool reads; none needs approval. Every argument is checked before
//! a broker request is spent, and the module computes no Greeks of its own:
//! the services price Black-76 off the forward. The normalisers here are
//! shared with the viz and live tools, so every toolkit that takes an
//! expiry, an offset or a strike count reads the same spellings.

#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::tools::answer::{json_result, label, Label};
use crate::agent::tools::market::candles::rendered_length;
use crate::agent::tools::market::normalise::{clean_exchange, resolve_exchange, Listed};
use crate::agent::tools::market::{master, MONTHS, VALID_EXCHANGES};
use crate::agent::tools::specs;
use crate::agent::tools::symbols::symbol_expiry;
use crate::agent::tools::values::{py_repr, quote, text, Pattern};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Strikes each side of ATM when the model does not say.
pub const DEFAULT_STRIKE_COUNT: i64 = 5;
/// Most strikes each side.
pub const MAX_STRIKE_COUNT: i64 = 100;
/// The strike count that means the whole listed chain.
pub const ALL_STRIKES: i64 = 0;

/// The exchanges an option contract lists on (the web's sorted
/// `FNO_EXCHANGES`).
pub const OPTION_EXCHANGES: [&str; 8] =
    ["BCD", "BFO", "CDS", "CRYPTO", "MCX", "NCDEX", "NCO", "NFO"];

/// The exchanges an underlying may be named on: every exchange code.
pub const UNDERLYING_EXCHANGES: [&str; 14] = VALID_EXCHANGES;

static EXPIRY: Pattern = Pattern::new(r"\A(\d{2})([A-Z]{3})(\d{2})\z");
static EMBEDDED_EXPIRY: Pattern = Pattern::new(r"\d{2}[A-Z]{3}\d{2}");
static ISO_EXPIRY: Pattern = Pattern::new(r"\A(\d{4})-(\d{1,2})-(\d{1,2})\z");
static EMBEDDED_BASE: Pattern = Pattern::new(r"\A(.+?)\d{2}[A-Z]{3}\d{2}");
static OFFSET: Pattern = Pattern::new(r"\A(?:ATM|(?:ITM|OTM)(?:[1-9]|[1-4][0-9]|50))\z");
static ATM_STEP: Pattern = Pattern::new(r"\AATM([+-]?)(\d+)\z");
static SPACES: Pattern = Pattern::new(r"\s+");
static SPACES_OR_SLASHES: Pattern = Pattern::new(r"[\s/]+");

const CHAIN_TRIMMED_NOTE: &str = "The full ladder was larger than one tool result can carry, so the strikes farthest from ATM were dropped. What is returned is a contiguous window centred on the ATM strike. Call again with a smaller strike_count, or name the specific strike you need, rather than assuming the missing strikes do not exist.";

/// Whether a symbol carries its own `DDMMMYY` expiry (`NIFTY28OCT25FUT`).
pub fn has_embedded_expiry(symbol: &str) -> bool {
    EMBEDDED_EXPIRY.is_match(symbol)
}

/// A symbol argument upper-cased; empty is refused.
pub fn normalise_symbol(value: Option<&Value>, field: &str) -> Result<String, ToolOutcome> {
    let t = text(value).to_uppercase();
    if t.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            field,
            "it is empty.",
            Some("Pass the symbol, for example 'NIFTY' for an underlying or 'NIFTY28NOV2524000CE' for a contract."),
        ));
    }
    Ok(t)
}

/// An exchange code checked against what this argument permits, spelled
/// by `clean_exchange` (`nse index` is `NSE_INDEX`).
pub fn normalise_exchange(value: Option<&Value>, allowed: &[&str]) -> Result<String, ToolOutcome> {
    let t = clean_exchange(value);
    if !allowed.contains(&t.as_str()) {
        let shown = if t.is_empty() { "it".to_string() } else { t };
        return Err(ToolOutcome::invalid_argument(
            "exchange",
            &format!("{} is not an exchange this tool accepts.", shown),
            Some(&format!("Use one of: {}.", allowed.join(", "))),
        ));
    }
    Ok(t)
}

/// `DDMMMYY` for `28OCT26`, `28-OCT-26`, `28OCT2026`, `8OCT26`,
/// `28 OCT 26`, `28/OCT/26` and `2026-10-28`, naming a real calendar day;
/// `None` otherwise.
pub fn expiry_text(t: &str) -> Option<String> {
    if let Some(c) = ISO_EXPIRY.captures(t) {
        let day =
            NaiveDate::from_ymd_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)?;
        return Some(format!(
            "{:02}{}{:02}",
            day.day(),
            MONTHS[day.month0() as usize],
            day.year() % 100
        ));
    }
    let hyphened = SPACES_OR_SLASHES.replace_all(t, "-");
    let spelled = symbol_expiry(&Value::String(hyphened.into_owned()))?;
    let c = EXPIRY.captures(&spelled)?;
    let month = MONTHS.iter().position(|m| *m == &c[2])?;
    NaiveDate::from_ymd_opt(
        2000 + c[3].parse::<i32>().ok()?,
        month as u32 + 1,
        c[1].parse().ok()?,
    )?;
    Some(spelled)
}

/// An expiry argument as `DDMMMYY`; empty is allowed (and returned empty)
/// only when `allow_embedded` and the underlying carries its own expiry.
pub fn normalise_expiry_argument(
    value: Option<&Value>,
    underlying: &str,
    allow_embedded: bool,
    field: &str,
) -> Result<String, ToolOutcome> {
    let t = text(value).to_uppercase();
    if t.is_empty() {
        if allow_embedded && has_embedded_expiry(underlying) {
            return Ok(String::new());
        }
        let reason = if allow_embedded {
            "no expiry was given and the underlying does not carry one."
        } else {
            "no expiry was given, and this tool needs one named explicitly."
        };
        return Err(ToolOutcome::invalid_argument(
            field,
            reason,
            Some("Pass the expiry in DDMMMYY format, for example '28NOV25'. Look up the listed expiries first rather than guessing a date."),
        ));
    }
    expiry_text(&t).ok_or_else(|| {
        ToolOutcome::invalid_argument(
            field,
            &format!("{} is not an expiry date.", quote(&t)),
            Some("Use DDMMMYY: two digits for the day, a three-letter month and two digits for the year, for example '28NOV25' or '05JAN26'."),
        )
    })
}

/// One required expiry as `DDMMMYY` (every toolkit reads the same
/// spellings).
pub fn normalise_expiry(value: Option<&Value>, field: &str) -> Result<String, ToolOutcome> {
    normalise_expiry_argument(value, "", false, field)
}

/// A strike offset: `ATM`, `ITM1`..`ITM50`, `OTM1`..`OTM50`; spaces,
/// hyphens and case ignored; a signed step from ATM is refused with the
/// spelling to use.
pub fn normalise_offset(value: Option<&Value>) -> Result<String, ToolOutcome> {
    let spaced = SPACES.replace_all(&text(value), "").to_uppercase();
    if let Some(c) = ATM_STEP.captures(&spaced) {
        if c[2].parse::<u64>().is_ok_and(|n| n == 0) {
            return Ok("ATM".into());
        }
        return Err(ToolOutcome::invalid_argument(
            "offset",
            &format!("{} is not a strike offset.", spaced),
            Some("Count steps from ATM with ITM or OTM followed by the number of strikes, for example 'OTM2' or 'ITM2'. For a CE a higher strike is OTMn and a lower one is ITMn; for a PE a higher strike is ITMn and a lower one is OTMn."),
        ));
    }
    let t = spaced.replace('-', "");
    if !OFFSET.is_match(&t) {
        let shown = if t.is_empty() { "it".to_string() } else { t };
        return Err(ToolOutcome::invalid_argument(
            "offset",
            &format!("{} is not a strike offset.", shown),
            Some("Use 'ATM', or 'ITM' or 'OTM' followed by 1 to 50, for example 'OTM2'. Each step is one listed strike, not one point of price."),
        ));
    }
    Ok(t)
}

/// `CE` or `PE` (`C`, `CALL`, `P`, `PUT` in any case).
pub fn normalise_option_type(value: Option<&Value>) -> Result<String, ToolOutcome> {
    let t = text(value).to_uppercase();
    match t.as_str() {
        "CE" | "C" | "CALL" | "CALLS" => Ok("CE".into()),
        "PE" | "P" | "PUT" | "PUTS" => Ok("PE".into()),
        _ => {
            let shown = if t.is_empty() { "it".to_string() } else { t };
            Err(ToolOutcome::invalid_argument(
                "option_type",
                &format!("{} is not an option type.", shown),
                Some("Use 'CE' for a call or 'PE' for a put."),
            ))
        }
    }
}

/// An index named on its cash exchange moved to its index exchange (NIFTY
/// on NSE is NSE_INDEX), only when the master says so; a futures symbol is
/// looked up by its base; a stock stays.
pub fn index_underlying(
    listed: Listed<'_>,
    underlying: &str,
    exchange: &str,
) -> (String, Option<String>) {
    let home = match exchange {
        "NSE" => "NSE_INDEX",
        "BSE" => "BSE_INDEX",
        _ => return (exchange.to_string(), None),
    };
    let base = EMBEDDED_BASE
        .captures(underlying)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| underlying.to_string());
    let (resolved, notice) = resolve_exchange(listed, &base, exchange);
    if resolved != home {
        return (exchange.to_string(), None);
    }
    (resolved, notice)
}

/// A whole number in `minimum..=maximum` (`"5"` and `5.0` accepted, a
/// boolean refused), the refusal naming the field's own default.
pub fn normalise_int(
    value: Option<&Value>,
    field: &str,
    minimum: i64,
    maximum: i64,
    default: Option<i64>,
) -> Result<i64, ToolOutcome> {
    let number: Option<f64> = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(_)) => None,
        other => text(other).parse::<f64>().ok(),
    };
    let whole = number.filter(|n| n.is_finite() && n.fract() == 0.0);
    match whole {
        Some(n) if (minimum as f64..=maximum as f64).contains(&n) => Ok(n as i64),
        _ => {
            let fix = match default {
                Some(d) if (minimum..=maximum).contains(&d) => {
                    format!(
                        "Pass an integer in that range; the default {} is {}.",
                        field, d
                    )
                }
                _ => "Pass an integer in that range.".to_string(),
            };
            Err(ToolOutcome::invalid_argument(
                field,
                &format!(
                    "{} is not a whole number between {} and {}.",
                    py_repr(value.unwrap_or(&Value::Null)),
                    minimum,
                    maximum
                ),
                Some(&fix),
            ))
        }
    }
}

/// True or false from the words a model uses.
pub fn normalise_bool(value: Option<&Value>, field: &str) -> Result<bool, ToolOutcome> {
    match value {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Number(n)) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
        Some(other) => {
            let t = crate::agent::tools::values::py_str(other)
                .trim()
                .to_lowercase();
            match t.as_str() {
                "true" | "t" | "yes" | "y" | "1" | "on" => Ok(true),
                "false" | "f" | "no" | "n" | "0" | "off" | "" => Ok(false),
                _ => Err(ToolOutcome::invalid_argument(
                    field,
                    &format!("{} is not true or false.", py_repr(other)),
                    Some("Pass true or false."),
                )),
            }
        }
    }
}

/// A service payload with the tool's correction attached as `notices`.
pub fn with_notice(payload: Value, notice: Option<String>) -> Value {
    match (payload, notice) {
        (Value::Object(mut m), Some(n)) => {
            m.insert("notices".into(), json!([n]));
            Value::Object(m)
        }
        (p, _) => p,
    }
}

/// A chain too large for one result, narrowed to a contiguous window grown
/// outward from the ATM row (or the middle) while it fits, with what was
/// dropped stated.
pub fn narrow_chain_to_budget(payload: Value) -> Value {
    let Value::Object(map) = &payload else {
        return payload;
    };
    let Some(Value::Array(chain)) = map.get("chain") else {
        return payload;
    };
    if chain.len() <= 1 || rendered_length(&payload) <= MAX_RESULT_CHARS {
        return payload;
    }
    let atm = map.get("atm_strike");
    let centre = chain
        .iter()
        .position(|row| row.is_object() && atm.is_some_and(|a| row.get("strike") == Some(a)))
        .unwrap_or(chain.len() / 2);
    let mut skeleton = map.clone();
    skeleton.insert("chain".into(), json!([]));
    skeleton.insert("strikes_returned".into(), json!(chain.len()));
    skeleton.insert("strikes_omitted".into(), json!(chain.len()));
    skeleton.insert("note".into(), json!(CHAIN_TRIMMED_NOTE));
    let mut used = rendered_length(&Value::Object(skeleton));
    let sizes: Vec<usize> = chain.iter().map(|r| rendered_length(r) + 1).collect();
    let (mut low, mut high) = (centre, centre);
    used += sizes[centre];
    loop {
        let mut grew = false;
        if low > 0 && used + sizes[low - 1] <= MAX_RESULT_CHARS {
            low -= 1;
            used += sizes[low];
            grew = true;
        }
        if high + 1 < chain.len() && used + sizes[high + 1] <= MAX_RESULT_CHARS {
            high += 1;
            used += sizes[high];
            grew = true;
        }
        if !grew {
            break;
        }
    }
    let kept = chain[low..=high].to_vec();
    tracing::info!(
        "Agent options chain trimmed to {} of {} strikes to fit the result budget",
        kept.len(),
        chain.len()
    );
    let mut trimmed = map.clone();
    let total = chain.len();
    trimmed.insert("strikes_returned".into(), json!(kept.len()));
    trimmed.insert("strikes_omitted".into(), json!(total - kept.len()));
    trimmed.insert("chain".into(), Value::Array(kept));
    trimmed.insert("note".into(), json!(CHAIN_TRIMMED_NOTE));
    Value::Object(trimmed)
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "options",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 40,
    description: "Option chain, strike resolution, Greeks and the synthetic future.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    Chain,
    Symbol,
    Greeks,
    Synthetic,
}

struct OptionsTool {
    spec: ToolSpec,
    kind: Kind,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [
        ("get_option_chain", Kind::Chain),
        ("get_option_symbol", Kind::Symbol),
        ("get_option_greeks", Kind::Greeks),
        ("get_synthetic_future", Kind::Synthetic),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("options", name)?;
        Some(Arc::new(OptionsTool { spec, kind }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for OptionsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::Chain => get_option_chain(ctx, &args).await,
            Kind::Symbol => get_option_symbol(ctx, &args).await,
            Kind::Greeks => get_option_greeks(ctx, &args).await,
            Kind::Synthetic => get_synthetic_future(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

fn expiry_label(expiry: &str) -> Label<'static> {
    ("expiry", (!expiry.is_empty()).then(|| expiry.to_string()))
}

async fn get_option_chain(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let underlying = normalise_symbol(args.get("underlying"), "underlying")?;
    let exchange = normalise_exchange(args.get("exchange"), &UNDERLYING_EXCHANGES)?;
    let expiry =
        normalise_expiry_argument(args.get("expiry_date"), &underlying, true, "expiry_date")?;
    let count = match args.get("strike_count") {
        None => DEFAULT_STRIKE_COUNT,
        given => normalise_int(
            given,
            "strike_count",
            ALL_STRIKES,
            MAX_STRIKE_COUNT,
            Some(DEFAULT_STRIKE_COUNT),
        )?,
    };
    let greeks = normalise_bool(args.get("with_greeks"), "with_greeks")?;
    let listed = master(&ctx.app);
    let (exchange, notice) = index_underlying(&listed, &underlying, &exchange);
    let mut request = Map::new();
    request.insert("underlying".into(), json!(underlying));
    request.insert("exchange".into(), json!(exchange));
    request.insert("expiry_date".into(), json!(expiry));
    if count != ALL_STRIKES {
        request.insert("strike_count".into(), json!(count));
    }
    request.insert("with_greeks".into(), json!(greeks));
    let payload = ctx
        .service_read(
            "option_chain_service.get_option_chain",
            "optionchain",
            request,
        )
        .await?;
    let shaped = narrow_chain_to_budget(with_notice(payload, notice));
    Ok(json_result(
        "get_option_chain",
        &shaped,
        &[
            label("underlying", &underlying),
            label("exchange", &exchange),
            expiry_label(&expiry),
        ],
    ))
}

async fn get_option_symbol(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let underlying = normalise_symbol(args.get("underlying"), "underlying")?;
    let exchange = normalise_exchange(args.get("exchange"), &UNDERLYING_EXCHANGES)?;
    let expiry =
        normalise_expiry_argument(args.get("expiry_date"), &underlying, true, "expiry_date")?;
    let offset = normalise_offset(args.get("offset"))?;
    let option_type = normalise_option_type(args.get("option_type"))?;
    let listed = master(&ctx.app);
    let (exchange, notice) = index_underlying(&listed, &underlying, &exchange);
    let mut request = Map::new();
    request.insert("underlying".into(), json!(underlying));
    request.insert("exchange".into(), json!(exchange));
    if !expiry.is_empty() {
        request.insert("expiry_date".into(), json!(expiry));
    }
    // No strike interval: the strikes the exchange actually lists are used,
    // so an uneven ladder never yields a symbol that does not exist.
    request.insert("offset".into(), json!(offset));
    request.insert("option_type".into(), json!(option_type));
    let payload = ctx
        .service_read(
            "option_symbol_service.get_option_symbol",
            "optionsymbol",
            request,
        )
        .await?;
    Ok(json_result(
        "get_option_symbol",
        &with_notice(payload, notice),
        &[
            label("underlying", &underlying),
            label("exchange", &exchange),
            expiry_label(&expiry),
            label("offset", &offset),
            label("option_type", &option_type),
        ],
    ))
}

async fn get_option_greeks(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let symbol = normalise_symbol(args.get("symbol"), "symbol")?;
    let exchange = normalise_exchange(args.get("exchange"), &OPTION_EXCHANGES)?;
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    let payload = ctx
        .service_read(
            "option_greeks_service.get_option_greeks",
            "optiongreeks",
            request,
        )
        .await?;
    Ok(json_result(
        "get_option_greeks",
        &payload,
        &[label("symbol", &symbol), label("exchange", &exchange)],
    ))
}

async fn get_synthetic_future(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let underlying = normalise_symbol(args.get("underlying"), "underlying")?;
    let exchange = normalise_exchange(args.get("exchange"), &UNDERLYING_EXCHANGES)?;
    let expiry =
        normalise_expiry_argument(args.get("expiry_date"), &underlying, false, "expiry_date")?;
    if has_embedded_expiry(&underlying) {
        return Err(ToolOutcome::invalid_argument(
            "underlying",
            &format!("{} carries its own expiry, which this tool cannot take.", quote(&underlying)),
            Some("Pass the base symbol on its own, such as 'NIFTY', and name the expiry in expiry_date."),
        ));
    }
    let listed = master(&ctx.app);
    let (exchange, notice) = index_underlying(&listed, &underlying, &exchange);
    let mut request = Map::new();
    request.insert("underlying".into(), json!(underlying));
    request.insert("exchange".into(), json!(exchange));
    request.insert("expiry_date".into(), json!(expiry));
    let payload = ctx
        .service_read(
            "synthetic_future_service.calculate_synthetic_future",
            "syntheticfuture",
            request,
        )
        .await?;
    Ok(json_result(
        "get_synthetic_future",
        &with_notice(payload, notice),
        &[
            label("underlying", &underlying),
            label("exchange", &exchange),
            expiry_label(&expiry),
        ],
    ))
}
