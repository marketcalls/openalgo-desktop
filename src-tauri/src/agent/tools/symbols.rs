//! Symbol tools: search the instrument master, confirm one contract, and
//! list an underlying's live expiries (the web's
//! `services/agent/tools/symbols.py`, toolkit `symbols`).
//!
//! On a miss these tools answer with the listed contracts nearest to what
//! was asked (a correction naming them), never a bare "not found", so the
//! model uses a real symbol rather than assembling one. Every name put in a
//! correction is checked against the shape of a real code first, because a
//! correction is plain text outside any wrapped block.

#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::tools::answer::{json_result, Label};
use crate::agent::tools::market::data_of;
use crate::agent::tools::market::normalise::clean_exchange_text;
use crate::agent::tools::service::{ServiceReply, SERVICE_TIMEOUT};
use crate::agent::tools::specs;
use crate::agent::tools::values::{py_repr, quote, Pattern};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Rows one search returns.
pub const MAX_SEARCH_RESULTS: usize = 40;

/// Near matches named in a correction.
pub const MAX_SUGGESTIONS: usize = 8;

/// The cash exchanges.
pub const CASH_EXCHANGES: [&str; 2] = ["NSE", "BSE"];

/// The exchanges futures and options are listed on.
pub const DERIVATIVE_EXCHANGES: [&str; 8] =
    ["NFO", "BFO", "CDS", "BCD", "MCX", "NCDEX", "NCO", "CRYPTO"];

/// The quote-only index feeds: they list no contract.
pub const INDEX_EXCHANGES: [&str; 4] = ["NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "GLOBAL_INDEX"];

/// Every exchange code, sorted as the web sorts it for a message.
fn all_exchanges_sorted() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = CASH_EXCHANGES
        .iter()
        .chain(DERIVATIVE_EXCHANGES.iter())
        .chain(INDEX_EXCHANGES.iter())
        .copied()
        .collect();
    all.sort_unstable();
    all
}

fn is_known_exchange(code: &str) -> bool {
    CASH_EXCHANGES.contains(&code)
        || DERIVATIVE_EXCHANGES.contains(&code)
        || INDEX_EXCHANGES.contains(&code)
}

/// Where the derivatives of a cash or index exchange are listed.
fn derivative_venue(exchange: &str) -> Option<&'static str> {
    match exchange {
        "NSE" | "NSE_INDEX" => Some("NFO"),
        "BSE" | "BSE_INDEX" => Some("BFO"),
        "MCX_INDEX" => Some("MCX"),
        _ => None,
    }
}

/// What the model may call an instrument type.
fn instrument_kind(text: &str) -> Option<&'static str> {
    match text {
        "options" | "option" | "opt" | "opts" | "ce" | "pe" | "optidx" | "optstk" | "optfut"
        | "optcur" => Some("options"),
        "futures" | "future" | "fut" | "futs" | "futidx" | "futstk" | "futcom" | "futcur" => {
            Some("futures")
        }
        _ => None,
    }
}

static NOT_FOUND: Pattern = Pattern::new(r"(?i)HTTP 404|not found|no matching|does not exist");
static ROOT: Pattern = Pattern::new(r"\A[A-Za-z&]+");
static UNDERLYING: Pattern = Pattern::new(r"\A(.+?)\d{2}[A-Z]{3}\d{0,2}");
static EXPIRY: Pattern = Pattern::new(r"\A(\d{1,2})-?([A-Z]{3})-?(\d{2}|\d{4})\z");
static SAFE_SYMBOL: Pattern = Pattern::new(r"\A[A-Za-z0-9._&/-]{1,40}\z");
static SAFE_EXCHANGE: Pattern = Pattern::new(r"\A[A-Z_]{1,20}\z");

/// The fields a search row keeps, in the order the model sees them; the
/// broker's own symbol and exchange are left out.
const SEARCH_FIELDS: [&str; 9] = [
    "symbol",
    "exchange",
    "name",
    "instrumenttype",
    "expiry",
    "strike",
    "lotsize",
    "tick_size",
    "freeze_qty",
];

/// A master-stored expiry as an OpenAlgo symbol spells it: `31-JUL-25` and
/// `31-JUL-2025` are `31JUL25`; `None` when it is not an expiry.
pub fn symbol_expiry(value: &Value) -> Option<String> {
    let text = value.as_str()?.trim().to_uppercase();
    let c = EXPIRY.captures(&text)?;
    let day: u32 = c[1].parse().ok()?;
    let year = &c[3];
    Some(format!("{:02}{}{}", day, &c[2], &year[year.len() - 2..]))
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "symbols",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 20,
    description: "Symbol search, contract lookup and expiry dates.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    Search,
    Symbol,
    Expiries,
}

struct SymbolsTool {
    spec: ToolSpec,
    kind: Kind,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [
        ("search_symbols", Kind::Search),
        ("get_symbol", Kind::Symbol),
        ("get_expiry_dates", Kind::Expiries),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("symbols", name)?;
        Some(Arc::new(SymbolsTool { spec, kind }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for SymbolsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::Search => search_symbols(ctx, &args).await,
            Kind::Symbol => get_symbol(ctx, &args).await,
            Kind::Expiries => get_expiry_dates(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

fn require_text(field: &str, value: Option<&Value>, fix: &str) -> Result<String, ToolOutcome> {
    match value.and_then(Value::as_str).map(str::trim) {
        Some(t) if !t.is_empty() => Ok(t.to_string()),
        _ => Err(ToolOutcome::invalid_argument(
            field,
            "it was empty.",
            Some(fix),
        )),
    }
}

/// An exchange code OpenAlgo knows, or empty when blank is allowed (every
/// exchange).
pub fn exchange_argument(value: Option<&Value>, allow_blank: bool) -> Result<String, ToolOutcome> {
    let text = value
        .and_then(Value::as_str)
        .map(clean_exchange_text)
        .unwrap_or_default();
    let list = format!("Pass one of: {}.", all_exchanges_sorted().join(", "));
    if text.is_empty() {
        if allow_blank {
            return Ok(String::new());
        }
        return Err(ToolOutcome::invalid_argument(
            "exchange",
            "it was empty, and a symbol only identifies a contract together with its exchange.",
            Some(&list),
        ));
    }
    if !is_known_exchange(&text) {
        return Err(ToolOutcome::invalid_argument(
            "exchange",
            &format!("{} is not an OpenAlgo exchange code.", quote(&text)),
            Some(&list),
        ));
    }
    Ok(text)
}

/// The derivatives exchange for an expiry lookup: a cash or index code is
/// moved to where its derivatives are listed, with a notice.
pub fn expiry_exchange(value: Option<&Value>) -> Result<(String, Option<String>), ToolOutcome> {
    let text = exchange_argument(value, false)?;
    if DERIVATIVE_EXCHANGES.contains(&text.as_str()) {
        return Ok((text, None));
    }
    if let Some(venue) = derivative_venue(&text) {
        let index = if INDEX_EXCHANGES.contains(&text.as_str()) {
            " and is a quote-only index feed"
        } else {
            ""
        };
        return Ok((
            venue.to_string(),
            Some(format!(
                "{} lists no futures or options{}, so the expiries were read from {}, where its derivatives are listed.",
                text, index, venue
            )),
        ));
    }
    Err(ToolOutcome::invalid_argument(
        "exchange",
        &format!("{} lists no futures or options.", text),
        Some(&format!(
            "Pass a derivatives exchange: {}.",
            DERIVATIVE_EXCHANGES.join(", ")
        )),
    ))
}

/// `options` or `futures`, from the spellings a model uses.
pub fn instrument_type(value: Option<&Value>) -> Result<&'static str, ToolOutcome> {
    let text = value
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase().replace(['-', '_'], ""))
        .unwrap_or_default();
    instrument_kind(&text).ok_or_else(|| {
        ToolOutcome::invalid_argument(
            "instrument_type",
            &format!(
                "{} is not an instrument type.",
                py_repr(value.unwrap_or(&Value::Null))
            ),
            Some("Pass 'options' or 'futures'."),
        )
    })
}

fn rows_of(payload: Value) -> Vec<Map<String, Value>> {
    match data_of(payload) {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn search_request(query: &str, exchange: &str) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("query".into(), json!(query));
    if !exchange.is_empty() {
        p.insert("exchange".into(), json!(exchange));
    }
    p
}

/// A search for suggestions, while another call has already failed: every
/// failure here is swallowed so the original answer still reaches the
/// model.
async fn quiet_search(ctx: &ToolCtx, query: &str, exchange: &str) -> Vec<Map<String, Value>> {
    if query.is_empty() {
        return Vec::new();
    }
    match ctx
        .api_post("search", search_request(query, exchange), SERVICE_TIMEOUT)
        .await
    {
        ServiceReply::Answered { status, body, .. }
            if (200..300).contains(&status) && body.get("status") != Some(&json!("error")) =>
        {
            rows_of(body)
        }
        _ => {
            tracing::warn!("Near-match search for {:?} failed", query);
            Vec::new()
        }
    }
}

/// The leading letters of a symbol, its underlying (`NIFTY` of
/// `NIFTY28MAR2420800CE`).
pub fn root(text: &str) -> String {
    ROOT.find(text.trim())
        .map(str::to_uppercase)
        .unwrap_or_default()
}

/// Search rows as symbol and exchange pairs safe to name in prose, unique,
/// in relevance order; an unsafe candidate is dropped, never repaired.
fn pairs(rows: &[Map<String, Value>], limit: Option<usize>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for row in rows {
        let text = |k: &str| match row.get(k) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => crate::agent::tools::values::py_str(other)
                .trim()
                .to_string(),
        };
        let symbol = text("symbol");
        let exchange = text("exchange").to_uppercase();
        if !SAFE_SYMBOL.is_match(&symbol) || !SAFE_EXCHANGE.is_match(&exchange) {
            continue;
        }
        if out
            .iter()
            .any(|(s, e)| s.to_uppercase() == symbol.to_uppercase() && *e == exchange)
        {
            continue;
        }
        out.push((symbol, exchange));
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
    }
    out
}

fn render(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(s, e)| format!("{} ({})", s, e))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The distinct underlyings behind contract symbols, with their exchange
/// when the search was not filtered to one.
fn underlyings(rows: &[Map<String, Value>], exclude: &str, with_exchange: bool) -> Vec<String> {
    let skip = exclude.trim().to_uppercase();
    let mut names: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (symbol, exchange) in pairs(rows, None) {
        let upper = symbol.to_uppercase();
        let name = UNDERLYING
            .captures(&upper)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| upper.clone());
        let key = if with_exchange {
            format!("{}:{}", name, exchange)
        } else {
            name.clone()
        };
        if name.is_empty() || name == skip || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        names.push(if with_exchange {
            format!("{} ({})", name, exchange)
        } else {
            name
        });
        if names.len() >= MAX_SUGGESTIONS {
            break;
        }
    }
    names
}

/// A row narrowed to `fields`, absent and empty ones dropped (an equity's
/// zero strike and empty expiry are absences, not facts).
fn compact(row: &Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    for field in fields {
        let Some(value) = row.get(*field) else {
            continue;
        };
        if value.is_null() || value == "" {
            continue;
        }
        if (*field == "expiry" || *field == "strike") && !crate::agent::tools::values::truthy(value)
        {
            continue;
        }
        out.insert((*field).to_string(), value.clone());
    }
    out
}

fn labels<'a>(first: (&'a str, &str), exchange: &str) -> Vec<Label<'a>> {
    vec![
        (first.0, Some(first.1.to_string())),
        (
            "exchange",
            (!exchange.is_empty()).then(|| exchange.to_string()),
        ),
    ]
}

async fn search_symbols(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let text = require_text(
        "query",
        args.get("query"),
        "Pass a base symbol, a company name, or the terms to match, such as 'BANKNIFTY 56000 PE'.",
    )?;
    let venue = exchange_argument(args.get("exchange"), true)?;
    let response = ctx
        .service_read(
            "search_service.search_symbols",
            "search",
            search_request(&text, &venue),
        )
        .await?;
    let rows = rows_of(response);
    let all = if venue.is_empty() {
        "all".to_string()
    } else {
        venue.clone()
    };
    if rows.is_empty() {
        suggest_broader_query(ctx, &text, &venue).await?;
        let payload = json!({
            "query": text,
            "exchange": all,
            "total_matches": 0,
            "returned": 0,
            "matches": [],
            "message": "Nothing in the instrument master matches that query, on any exchange. The instrument may not be listed, or the name may be wrong. Ask the operator which instrument they mean rather than guessing a symbol.",
        });
        return Ok(json_result(
            "search_symbols",
            &payload,
            &labels(("query", &text), &venue),
        ));
    }
    let shown: Vec<Value> = rows
        .iter()
        .take(MAX_SEARCH_RESULTS)
        .map(|r| Value::Object(compact(r, &SEARCH_FIELDS)))
        .collect();
    let mut result = Map::new();
    result.insert("query".into(), json!(text));
    result.insert("exchange".into(), json!(all));
    result.insert("total_matches".into(), json!(rows.len()));
    result.insert("returned".into(), json!(shown.len()));
    let further = rows.len() - shown.len();
    result.insert("matches".into(), Value::Array(shown));
    if further > 0 {
        result.insert("truncated".into(), json!(true));
        result.insert(
            "note".into(),
            json!(format!(
                "{} further matches were not returned. Add a term to the query (a strike, an expiry, CE or PE) or set the exchange to narrow it.",
                further
            )),
        );
    }
    Ok(json_result(
        "search_symbols",
        &Value::Object(result),
        &labels(("query", &text), &venue),
    ))
}

/// A search that found nothing, retried broader; when that finds listed
/// contracts the model is told which query works.
async fn suggest_broader_query(
    ctx: &ToolCtx,
    query: &str,
    exchange: &str,
) -> Result<(), ToolOutcome> {
    let base = root(query);
    let mut attempts: Vec<(String, String, String)> = Vec::new();
    if !exchange.is_empty() {
        attempts.push((
            query.into(),
            String::new(),
            format!("dropping the exchange filter {}", exchange),
        ));
    }
    if !base.is_empty() && base != query.to_uppercase() {
        if !exchange.is_empty() {
            attempts.push((
                base.clone(),
                exchange.into(),
                format!("searching for the underlying {} instead", base),
            ));
        }
        attempts.push((
            base.clone(),
            String::new(),
            format!("searching for the underlying {} on every exchange", base),
        ));
    }
    for (text, venue, description) in attempts {
        let found = pairs(
            &quiet_search(ctx, &text, &venue).await,
            Some(MAX_SUGGESTIONS),
        );
        if found.is_empty() {
            continue;
        }
        let on = if exchange.is_empty() {
            String::new()
        } else {
            format!(" on {}", exchange)
        };
        return Err(ToolOutcome::Retry(format!(
            "Nothing matches {}{}, but {} finds listed contracts: {}. Call search_symbols again with the query and exchange that work, then use a symbol exactly as it comes back.",
            quote(query),
            on,
            description,
            render(&found)
        )));
    }
    Ok(())
}

async fn get_symbol(ctx: &ToolCtx, args: &Map<String, Value>) -> Result<ToolOutcome, ToolOutcome> {
    let text = require_text(
        "symbol",
        args.get("symbol"),
        "Pass one exact OpenAlgo symbol, such as 'RELIANCE' or 'NIFTY28MAR2420800CE'. Use search_symbols to find it.",
    )?;
    let venue = exchange_argument(args.get("exchange"), false)?;
    let mut candidates = vec![text.clone()];
    if text.to_uppercase() != text {
        candidates.push(text.to_uppercase());
    }
    let mut first_failure: Option<ToolOutcome> = None;
    for candidate in candidates {
        let mut request = Map::new();
        request.insert("symbol".into(), json!(candidate));
        request.insert("exchange".into(), json!(venue));
        let payload = match ctx
            .service_read("symbol_service.get_symbol_info", "symbol", request)
            .await
        {
            Ok(p) => p,
            Err(failure) => {
                // A platform failure is not a spelling problem.
                if !NOT_FOUND.is_match(failure.text()) {
                    return Err(failure);
                }
                first_failure = first_failure.or(Some(failure));
                continue;
            }
        };
        if let Value::Object(info) = data_of(payload) {
            if !info.is_empty() {
                let mut row = info;
                row.retain(|k, _| k != "id");
                let fields: Vec<String> = row.keys().cloned().collect();
                let field_refs: Vec<&str> = fields.iter().map(String::as_str).collect();
                let shown = compact(&row, &field_refs);
                return Ok(json_result(
                    "get_symbol",
                    &Value::Object(shown),
                    &labels(("symbol", &candidate), &venue),
                ));
            }
        }
    }
    Err(not_listed(ctx, &text, &venue, first_failure).await)
}

/// An unlisted contract, answered with the listed contracts nearest to it.
async fn not_listed(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
    original: Option<ToolOutcome>,
) -> ToolOutcome {
    let base = root(symbol);
    let attempts = [
        (
            symbol.to_string(),
            exchange.to_string(),
            format!("on {}", exchange),
        ),
        (
            symbol.to_string(),
            String::new(),
            "on another exchange".to_string(),
        ),
        (
            base.clone(),
            exchange.to_string(),
            format!("on {}", exchange),
        ),
        (base, String::new(), "on another exchange".to_string()),
    ];
    for (text, venue, where_) in attempts {
        let found: Vec<(String, String)> = pairs(
            &quiet_search(ctx, &text, &venue).await,
            Some(MAX_SUGGESTIONS),
        )
        .into_iter()
        .filter(|(s, e)| !(s.to_uppercase() == symbol.to_uppercase() && e == exchange))
        .collect();
        if found.is_empty() {
            continue;
        }
        return ToolOutcome::Retry(format!(
            "{} is not listed on {}. These contracts are listed {}: {}. Use one of those symbols exactly as written, or call search_symbols to see the rest. Do not adjust an expiry or a strike yourself to make a symbol fit.",
            symbol,
            exchange,
            where_,
            render(&found)
        ));
    }
    original.unwrap_or_else(|| {
        ToolOutcome::Retry(format!(
            "{} is not listed on {}, and nothing similar is listed anywhere. Ask the operator which instrument they mean rather than guessing a symbol.",
            symbol, exchange
        ))
    })
}

fn expiry_entries(dates: &Value) -> Vec<Value> {
    let Value::Array(items) = dates else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let expiry = s.to_uppercase();
            let mut entry = Map::new();
            entry.insert("expiry".into(), json!(expiry));
            if let Some(spelled) = symbol_expiry(&json!(expiry)) {
                entry.insert("symbol_expiry".into(), json!(spelled));
            }
            Value::Object(entry)
        })
        .collect()
}

async fn get_expiry_dates(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let underlying = require_text(
        "symbol",
        args.get("symbol"),
        "Pass the underlying alone, such as 'NIFTY' or 'RELIANCE', not a full contract symbol.",
    )?
    .to_uppercase();
    let (venue, notice) = expiry_exchange(args.get("exchange"))?;
    let kind = instrument_type(args.get("instrument_type"))?;
    let mut request = Map::new();
    request.insert("symbol".into(), json!(underlying));
    request.insert("exchange".into(), json!(venue));
    request.insert("instrumenttype".into(), json!(kind));
    let payload = ctx
        .service_read("expiry_service.get_expiry_dates", "expiry", request)
        .await?;
    let entries = expiry_entries(&data_of(payload));
    let mut result = Map::new();
    result.insert("symbol".into(), json!(underlying));
    result.insert("exchange".into(), json!(venue));
    result.insert("instrument_type".into(), json!(kind));
    result.insert("count".into(), json!(entries.len()));
    if entries.is_empty() {
        suggest_underlying(ctx, &underlying, &venue, kind).await?;
        result.insert("expiries".into(), json!([]));
        result.insert(
            "message".into(),
            json!(format!(
                "No live {} expiries for {} on {}, and no similar underlying is listed there either. Check the underlying with the operator rather than guessing an expiry.",
                kind, underlying, venue
            )),
        );
    } else {
        result.insert("expiries".into(), Value::Array(entries));
        result.insert(
            "note".into(),
            json!("symbol_expiry is the date segment of the OpenAlgo symbol. The strike is not derivable the same way, so confirm the full contract with search_symbols before using it."),
        );
    }
    if let Some(n) = notice {
        result.insert("notices".into(), json!([n]));
    }
    Ok(json_result(
        "get_expiry_dates",
        &Value::Object(result),
        &labels(("symbol", &underlying), &venue),
    ))
}

/// An expiry lookup that matched no underlying, answered with the similar
/// underlyings that do have contracts.
async fn suggest_underlying(
    ctx: &ToolCtx,
    underlying: &str,
    exchange: &str,
    kind: &str,
) -> Result<(), ToolOutcome> {
    let base = match root(underlying) {
        r if r.is_empty() => underlying.to_string(),
        r => r,
    };
    let attempts = [
        (
            exchange.to_string(),
            false,
            format!("do have {} contracts on {}", kind, exchange),
            "one of them",
        ),
        (
            String::new(),
            true,
            "are listed on other exchanges".to_string(),
            "the underlying and exchange that match",
        ),
    ];
    for (venue, with_exchange, where_, correction) in attempts {
        let rows = quiet_search(ctx, &base, &venue).await;
        let exclude = if with_exchange { "" } else { underlying };
        let mut names = underlyings(&rows, exclude, with_exchange);
        if with_exchange {
            let own = format!("{} ({})", underlying, exchange);
            names.retain(|n| *n != own);
        }
        if names.is_empty() {
            continue;
        }
        return Err(ToolOutcome::Retry(format!(
            "{} has no live {} expiries on {}. These underlyings {}: {}. Call get_expiry_dates again with {}, or use search_symbols if none of them is what the operator meant.",
            underlying,
            kind,
            exchange,
            where_,
            names.join(", "),
            correction
        )));
    }
    Ok(())
}
