//! What the Flow editor looks up while a node is configured (the web's
//! `blueprints/flow.py` 1416-1770): the lot size of every underlying the
//! options nodes offer, the lot sizes of a margin basket's exact symbols,
//! and the listed expiries and strikes of one underlying for the manual
//! leg builder. Each answers the web's codes and shapes; the routes in
//! `server/routes/flow/lookups.rs` only read the request and call these.
//!
//! Lot sizes come from the loaded master contract (in memory); expiries and
//! the strike ladder from the desktop's own expiry and option-chain
//! services, in process. The ladder is the chain's structure only (one
//! underlying quote, no per-strike quotes), as the web asks for it.

use crate::brokers::common::underlying::extract_underlying;
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::{
    format_expiry_for_api, parse_underlying_symbol, select_expiry, VALID_EXPIRY_TYPES,
};
use crate::flow::nodes::options::{resolve_option_exchanges, MCX_OPTION_UNDERLYINGS};
use crate::services::core::Reply;
use crate::state::AppState;
use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// NSE index underlyings, in the order the editor lists them.
const NSE_INDICES: [&str; 5] = ["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY", "NIFTYNXT50"];
/// BSE index underlyings.
const BSE_INDICES: [&str; 3] = ["SENSEX", "BANKEX", "SENSEX50"];
/// `MAX_LOT_SIZE_LOOKUP`: a margin basket holds at most 50 positions.
pub const MAX_LOT_SIZE_LOOKUP: usize = 50;
/// `OPTION_STRIKE_WINDOW`: strikes either side of ATM in the leg builder.
pub const OPTION_STRIKE_WINDOW: i64 = 25;

/// `GET /flow/api/index-symbols`: every options underlying with a usable
/// lot size; one without is left out, never listed with null.
pub fn index_symbols(app: &AppState) -> Reply {
    let snap = app.symbols.snapshot();
    let data = index_symbol_rows(snap.rows());
    Reply::new(200, json!({"status": "success", "data": data}))
}

/// The underlyings the options nodes offer, in the editor's order (the NSE
/// indices on NFO, the BSE indices on BFO, then the MCX commodities), each
/// with its lot size from `rows`.
pub(crate) fn index_symbol_rows(rows: &[crate::brokers::common::symbols::SymToken]) -> Vec<Value> {
    let candidates = NSE_INDICES
        .iter()
        .map(|n| (*n, "NFO"))
        .chain(BSE_INDICES.iter().map(|n| (*n, "BFO")))
        .chain(MCX_OPTION_UNDERLYINGS.iter().map(|n| (*n, "MCX")));
    candidates
        .filter_map(|(name, exchange)| {
            let lot = index_lot_size(rows, name, exchange)?;
            Some(json!({"value": name, "label": name, "exchange": exchange, "lotSize": lot}))
        })
        .collect()
}

/// `_lot_size`: the master row named for the underlying first, else a
/// contract whose symbol starts with it and whose underlying really is it
/// (NIFTYNXT50 contracts never size NIFTY).
fn index_lot_size(
    rows: &[crate::brokers::common::symbols::SymToken],
    name: &str,
    exchange: &str,
) -> Option<i32> {
    let usable =
        |r: &&crate::brokers::common::symbols::SymToken| r.exchange == exchange && r.lot_size > 0;
    if let Some(r) = rows.iter().filter(usable).find(|r| r.name == name) {
        return Some(r.lot_size);
    }
    rows.iter()
        .filter(usable)
        .filter(|r| r.symbol.starts_with(name))
        .find(|r| extract_underlying(&r.symbol, &r.exchange).as_deref() == Some(name))
        .map(|r| r.lot_size)
}

fn refusal(status: u16, message: &str) -> Reply {
    Reply::new(status, json!({"status": "error", "message": message}))
}

/// `POST /flow/api/symbol-lotsizes`: the lot size of each exact
/// `(symbol, exchange)` pair, null where the master has none, every pair
/// echoed. `body` is the request's JSON, or `None` when it was not JSON.
pub fn symbol_lot_sizes(app: &AppState, body: Option<&Value>) -> Reply {
    let symbols = body.and_then(|b| b.get("symbols"));
    let list = match symbols {
        Some(Value::Array(list)) if !list.is_empty() => list,
        _ => return refusal(400, "symbols must be a non-empty array"),
    };
    if list.len() > MAX_LOT_SIZE_LOOKUP {
        return refusal(
            400,
            &format!("at most {} symbols per request", MAX_LOT_SIZE_LOOKUP),
        );
    }
    let mut pairs = Vec::with_capacity(list.len());
    for entry in list {
        let Value::Object(entry) = entry else {
            return refusal(400, "each symbol must be an object");
        };
        let field = |k: &str| {
            entry
                .get(k)
                .filter(|v| pyfmt::truthy(v))
                .map(pyfmt::str)
                .unwrap_or_default()
                .trim()
                .to_uppercase()
        };
        let (symbol, exchange) = (field("symbol"), field("exchange"));
        if symbol.is_empty() || exchange.is_empty() {
            return refusal(400, "symbol and exchange are required");
        }
        pairs.push((symbol, exchange));
    }
    let snap = app.symbols.snapshot();
    let sizes: Map<String, Value> = pairs
        .iter()
        .map(|(symbol, exchange)| {
            let lot = snap
                .by_symbol(exchange, symbol)
                .and_then(|r| whole_lot(&snap, r));
            (format!("{}:{}", exchange, symbol), json!(lot))
        })
        .collect();
    Reply::new(200, json!({"status": "success", "lotSizes": sizes}))
}

/// A row's lot size as a whole number of units, or `None`: a size counts
/// only when `int(lotsize) >= 1` and equals the size itself, so a fractional
/// size (Delta spot's 0.0001, kept exact beside the row's whole lot of 1)
/// is not a lot (R-62, decision 24).
fn whole_lot(
    snap: &crate::brokers::common::symbols::SymbolGeneration,
    r: &crate::brokers::common::symbols::SymToken,
) -> Option<i64> {
    match snap.exact_lot_size(&r.exchange, &r.token) {
        Some(exact) => (exact >= 1.0 && exact.fract() == 0.0).then_some(exact as i64),
        None => (r.lot_size >= 1).then_some(i64::from(r.lot_size)),
    }
}

/// Where the leg builder's expiries and chain come from (the in-process
/// services; a test's fake).
#[async_trait]
pub trait StrikeSource: Send + Sync {
    /// The listed option expiries, as `/api/v1/expiry` answers (`data` of
    /// `DD-MMM-YY` dates), or the failure to pass through.
    async fn expiries(&self, symbol: &str, exchange: &str) -> Reply;
    /// The chain's structure for one expiry, or the failure to pass through.
    async fn chain(
        &self,
        symbol: &str,
        exchange: &str,
        expiry: &str,
        count: i64,
    ) -> Result<Value, Reply>;
    /// Today, IST: the month the monthly expiry types count from.
    fn today(&self) -> NaiveDate;
}

/// The desktop's services.
struct AppStrikes(Arc<AppState>);

#[async_trait]
impl StrikeSource for AppStrikes {
    async fn expiries(&self, symbol: &str, exchange: &str) -> Reply {
        crate::services::symbol_service::expiry(&self.0, symbol, exchange, "options")
    }

    async fn chain(
        &self,
        symbol: &str,
        exchange: &str,
        expiry: &str,
        count: i64,
    ) -> Result<Value, Reply> {
        crate::services::options_service::option_chain_structure(
            &self.0,
            symbol,
            exchange,
            expiry,
            Some(count),
        )
        .await
    }

    fn today(&self) -> NaiveDate {
        crate::services::options_service::today_ist(&self.0)
    }
}

/// The query of `GET /flow/api/option-strikes`, read as the web reads it.
#[derive(Debug, Default, Clone)]
pub struct StrikeQuery {
    pub underlying: String,
    pub option_type: String,
    pub expiry: String,
    pub expiry_type: String,
    pub exchange: String,
}

impl StrikeQuery {
    pub fn from_params(q: &HashMap<String, String>) -> Self {
        let arg = |k: &str| q.get(k).map(String::as_str).unwrap_or_default();
        let option_type = arg("optionType").trim().to_uppercase();
        Self {
            underlying: arg("underlying").trim().to_uppercase(),
            option_type: if arg("optionType").is_empty() {
                "CE".into()
            } else {
                option_type
            },
            expiry: format_expiry_for_api(arg("expiry")),
            expiry_type: arg("expiryType").trim().to_lowercase(),
            exchange: arg("exchange").trim().to_uppercase(),
        }
    }
}

/// `GET /flow/api/option-strikes` over the app's services.
pub async fn option_strikes(app: &Arc<AppState>, q: &StrikeQuery) -> Reply {
    let has_key = crate::flow::service::api_key(app).await.is_some();
    option_strikes_from(&AppStrikes(app.clone()), has_key, q).await
}

/// The answer from any [`StrikeSource`].
pub async fn option_strikes_from(src: &dyn StrikeSource, has_key: bool, q: &StrikeQuery) -> Reply {
    if q.underlying.is_empty() {
        return refusal(400, "underlying is required");
    }
    if q.option_type != "CE" && q.option_type != "PE" {
        return refusal(400, "optionType must be CE or PE");
    }
    if !has_key {
        return refusal(401, "API key not configured. Generate one at /apikey");
    }
    let (_, fo_exchange) = resolve_option_exchanges(&q.underlying, &q.exchange);
    let fo_exchange = fo_exchange.as_str();
    let (base, _) = parse_underlying_symbol(&q.underlying);
    let listed_reply = src.expiries(&base, fo_exchange).await;
    if listed_reply.status != 200 {
        return listed_reply;
    }
    let listed: Vec<Value> = match listed_reply.body.get("data") {
        Some(Value::Array(l)) => l.clone(),
        _ => Vec::new(),
    };
    let expiries = sorted_expiry_codes(&listed);
    let Some(nearest) = expiries.first().cloned() else {
        return refusal(
            404,
            &format!("No option expiries found for {} on {}", base, fo_exchange),
        );
    };
    let listed_text: Vec<String> = listed
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let today = src.today();
    let resolved: Map<String, Value> = VALID_EXPIRY_TYPES
        .iter()
        .map(|t| (t.to_string(), json!(select_expiry(&listed_text, t, today))))
        .collect();
    let mut expiry = String::new();
    if expiries.contains(&q.expiry) {
        expiry = q.expiry.clone();
    } else if !q.expiry_type.is_empty() {
        expiry = resolved
            .get(&q.expiry_type)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    if !expiries.contains(&expiry) {
        expiry = nearest;
    }
    let chain = match src
        .chain(&base, fo_exchange, &expiry, OPTION_STRIKE_WINDOW)
        .await
    {
        Ok(c) => c,
        Err(r) => return r,
    };
    let side = q.option_type.to_lowercase();
    let strikes: Vec<Value> = chain
        .get("chain")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|row| row.get("strike").is_some_and(|s| !s.is_null()))
        .map(|row| {
            let leg = row.get(&side).filter(|l| pyfmt::truthy(l));
            let of = |k: &str| leg.and_then(|l| l.get(k)).cloned().unwrap_or(Value::Null);
            json!({"strike": row["strike"], "symbol": of("symbol"), "label": of("label")})
        })
        .collect();
    let field = |k: &str| chain.get(k).cloned().unwrap_or(Value::Null);
    Reply::new(
        200,
        json!({"status": "success", "data": {
            "underlying": q.underlying,
            "exchange": fo_exchange,
            "expiry": chain.get("expiry_date").cloned().unwrap_or_else(|| json!(expiry)),
            "expiries": expiries,
            "resolved": resolved,
            "optionType": q.option_type,
            "strikes": strikes,
            "atm": field("atm_strike"),
            "underlyingLtp": field("underlying_ltp"),
            "underlyingSymbol": field("underlying_symbol"),
        }}),
    )
}

/// `_sorted_expiry_codes`: the listed expiries as DDMMMYY, each once,
/// earliest first; one that does not read as a date is kept, last.
pub fn sorted_expiry_codes(raw: &[Value]) -> Vec<String> {
    let mut seen: Vec<(Option<NaiveDate>, String)> = Vec::new();
    for value in raw {
        let Some(text) = value.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        let text = text.to_uppercase();
        let parsed = parse_listed_expiry(&text);
        let code = match parsed {
            Some(d) => d.format("%d%b%y").to_string().to_uppercase(),
            None => text,
        };
        if !seen.iter().any(|(_, c)| *c == code) {
            seen.push((parsed, code));
        }
    }
    // Stable: equal dates and every unread code keep their listed order.
    seen.sort_by_key(|(d, _)| (d.is_none(), d.unwrap_or(NaiveDate::MAX)));
    seen.into_iter().map(|(_, c)| c).collect()
}

/// `strptime` with `%d-%b-%y`, `%d-%b-%Y`, `%d%b%y`, `%d%b%Y`, in order.
fn parse_listed_expiry(text: &str) -> Option<NaiveDate> {
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let month_at = MONTHS.iter().enumerate().find_map(|(i, m)| {
        let at = text.find(m)?;
        Some((i, at))
    })?;
    let (month_index, at) = month_at;
    let (day_part, rest) = (&text[..at], &text[at + 3..]);
    let (day_text, year_text) = match (day_part.strip_suffix('-'), rest.strip_prefix('-')) {
        (Some(d), Some(y)) => (d, y),
        (None, None) => (day_part, rest),
        _ => return None,
    };
    let day_ok = matches!(day_text.len(), 1 | 2) && day_text.bytes().all(|b| b.is_ascii_digit());
    if !day_ok || day_text == "0" || day_text == "00" {
        return None;
    }
    if !(year_text.len() == 2 || year_text.len() == 4)
        || !year_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut year: i32 = year_text.parse().ok()?;
    if year_text.len() == 2 {
        year += if year <= 68 { 2000 } else { 1900 };
    }
    let month = u32::try_from(month_index + 1).ok()?;
    NaiveDate::from_ymd_opt(year, month, day_text.parse().ok()?)
}
