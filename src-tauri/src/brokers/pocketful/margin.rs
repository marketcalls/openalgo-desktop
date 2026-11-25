//! Margin calculator (web `api/margin_api.py`, `mapping/margin_data.py`;
//! marketcalls/openalgo#2467).
//!
//! `POST /api/space/v1/orderapis/marginCalculations` (Pocketful docs,
//! "Margin Calculator"; the SDK has no route for it) takes every leg in one
//! `data` array and answers a portfolio-netted `combinedMargin` block, so a
//! basket is always sent whole and its hedges net at Pocketful, never summed
//! here. The docs list a `clientid` header beside the session's own.
//!
//! One leg, field by field, as the docs' request table defines it:
//!
//! * `exchange`: the master's `brexchange`; `mode`: `NEW`;
//! * `price`: the limit price as text, `0` for a market order;
//! * `product`: `MIS`, `CNC` or `NRML` by name. The docs also describe a
//!   number, but Pocketful's own position pages report 2 on an MIS position
//!   and 0 on an NRML one, so a name is sent: it cannot be read as the
//!   other product;
//! * `quantity`: units as text; `segment`: `Capital` for NSE and BSE,
//!   `FutOpt` for NFO and BFO;
//! * `series`: `EQ`, `FUTIDX`, `FUTSTK`, `OPTIDX` or `OPTSTK`, or the series
//!   Pocketful's own instrument file gives the row when the master kept it;
//! * `side`, `symbol` (the master's broker symbol) and `token`;
//! * `underlying`: the token of the underlying, the index first and then
//!   the cash market; a cash leg is its own underlying.
//!
//! MCX and currency legs are refused: the docs do not say which series or
//! underlying a commodity or currency contract takes, and a guess would
//! price the wrong contract. A leg that cannot be priced refuses the whole
//! basket before anything is sent: pricing the rest would drop its hedge
//! and report a margin that is not the basket's.
//!
//! Every refusal is an `AppError::Validation` or `AppError::Broker` with the
//! web's sentence, which `/api/v1/margin` answers with 400, the desktop's
//! one rule for a margin that was not calculated. The web answers
//! Pocketful's own HTTP status for its refusal and 502 when the calculator
//! did not answer or sent nothing readable. A session Pocketful refuses
//! (401 or 403) reads as the adapter's "log in to Pocketful again"
//! sentence, where the web gives its generic margin sentence with the 401.
//!
//! A derivative's underlying is read from its OpenAlgo symbol
//! (`common::underlying`), not from the master's `name` column, which is
//! not an identity (CLAUDE.md, the symbol master); the web reads `name`.

use super::client::{Answer, Lane, Request};
use super::{gtt, mapping, orders, PocketfulBroker};
use crate::brokers::common::mpp::{instrument_type_from_symbol, py_round};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::common::underlying::row_underlying;
use crate::brokers::types::{AuthToken, MarginLeg, MarginResult};
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

/// The calculator's route (docs, "Margin Calculator").
pub(crate) const MARGIN_PATH: &str = "/api/space/v1/orderapis/marginCalculations";

const NO_CLIENT_ID: &str =
    "Your Pocketful session carries no client id. Log in to Pocketful again and retry.";
const NO_LEGS: &str = "There are no positions to calculate margin for.";
const NO_ANSWER: &str =
    "Pocketful's margin calculator did not answer. Check your connection and try again.";
const UNREADABLE: &str =
    "Pocketful's margin calculator sent an answer OpenAlgo could not read. Try again in a moment.";
const REFUSED: &str = "Pocketful could not calculate the margin";

/// Instrument types OpenAlgo itself writes into the master. Anything else
/// in that column came from Pocketful's instrument file and is already the
/// series.
const GENERIC_TYPES: &[&str] = &["", "EQ", "FUT", "CE", "PE", "OPT", "INDEX", "IDX"];

/// `combinedMargin` members that add to the requirement, and the one that
/// subtracts (docs, "Response Parameters"). The docs give no total, so the
/// requirement is the sum of the add-ons less the premium benefit, the
/// reading HDFC Sky's adapter makes of the same calculator's answer.
const MARGIN_COMPONENTS: &[&str] = &[
    "span",
    "exposureMargin",
    "premiumMargin",
    "varMargin",
    "extremeLossMargin",
    "deliveryMargin",
    "additionalMargin",
    "spanSpreadMargin",
    "somtierMargin",
];
const MARGIN_BENEFIT: &str = "premiumBenefit";

/// Margin for `legs`, priced as one basket (web `calculate_margin_api`).
pub async fn calculate_margin(
    b: &PocketfulBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    // Checked first, so a session without its account sends nothing.
    let account = b
        .client_id(auth)
        .map_err(|_| AppError::Validation(NO_CLIENT_ID.into()))?;
    let basket = basket(b.resolver(), legs)?;
    tracing::info!("Pocketful margin request: {} leg(s)", basket.len());
    let req = Request::new(Method::POST, MARGIN_PATH)
        .json(json!({ "data": basket }))
        .header("clientid", account);
    let answer = match b.send(Lane::Data, auth, req).await {
        Ok(answer) => answer,
        // Already without its URL (`client::send`).
        Err(AppError::Http(e)) => {
            tracing::error!("Pocketful margin request failed: {}", e);
            return Err(AppError::Broker(NO_ANSWER.into()));
        }
        Err(other) => return Err(other),
    };
    margin_of(&answer)
}

/// Every leg as Pocketful takes it, or the first leg's refusal.
fn basket(symbols: &SymbolResolver, legs: &[MarginLeg]) -> Result<Vec<Value>> {
    let basket = legs
        .iter()
        .map(|leg| margin_leg(symbols, leg))
        .collect::<Result<Vec<_>>>()
        .inspect_err(|refusal| {
            tracing::info!(
                "Pocketful margin request refused: {}",
                refusal.client_message()
            )
        })?;
    if basket.is_empty() {
        return Err(AppError::Validation(NO_LEGS.into()));
    }
    Ok(basket)
}

fn refused(message: String) -> AppError {
    AppError::Validation(message)
}

/// One OpenAlgo leg as a Pocketful margin leg (web `build_margin_leg`).
fn margin_leg(symbols: &SymbolResolver, leg: &MarginLeg) -> Result<Value> {
    let (symbol, exchange) = (leg.key.symbol.as_str(), leg.key.exchange.as_str());
    // Where a derivative's underlying is listed: the index first, then the
    // cash market. Found on the index exchange means an index derivative.
    let venues = match exchange {
        "NSE" | "BSE" => None,
        "NFO" => Some(["NSE_INDEX", "NSE"]),
        "BFO" => Some(["BSE_INDEX", "BSE"]),
        _ => {
            return Err(refused(format!(
                "Margin for {} contracts is not available with Pocketful yet. Check the margin for this leg in Pocketful's app.",
                exchange
            )))
        }
    };
    let row = symbols
        .by_symbol(exchange, symbol)
        .filter(|r| !r.token.is_empty())
        .ok_or_else(|| {
            refused(format!(
                "{} on {} is not in the Pocketful master contract. Check the symbol, or download the master contract again and retry.",
                symbol, exchange
            ))
        })?;
    let (segment, series, underlying) = match venues {
        None => ("Capital", "EQ".to_string(), row.token.clone()),
        Some(venues) => {
            let (token, is_index) = underlying_token(symbols, leg, &row, venues)?;
            ("FutOpt", series_of(leg, &row, is_index)?, token)
        }
    };
    let pocketful_exchange = if row.brexchange.is_empty() {
        exchange
    } else {
        row.brexchange.as_str()
    };
    Ok(json!({
        "exchange": pocketful_exchange,
        "mode": "NEW",
        "price": number_text(leg.price),
        "product": leg.product.as_str(),
        "quantity": leg.quantity.to_string(),
        "segment": segment,
        "series": series,
        "side": leg.action.as_str(),
        "symbol": row.brsymbol,
        "token": row.token,
        "underlying": underlying,
    }))
}

/// The token of a derivative's underlying and whether it is an index
/// (web `_underlying`).
fn underlying_token(
    symbols: &SymbolResolver,
    leg: &MarginLeg,
    row: &SymToken,
    venues: [&str; 2],
) -> Result<(String, bool)> {
    let name = row_underlying(row);
    for venue in venues {
        let found = name
            .as_deref()
            .and_then(|n| symbols.by_symbol(venue, n))
            .filter(|f| !f.token.is_empty());
        if let Some(found) = found {
            return Ok((found.token, venue.ends_with("_INDEX")));
        }
    }
    Err(refused(format!(
        "The underlying of {} is not in the master contract, so its margin cannot be worked out. Download the master contract again and retry.",
        leg.key.symbol
    )))
}

/// The series of a derivative leg (web `_series`).
fn series_of(leg: &MarginLeg, row: &SymToken, is_index: bool) -> Result<String> {
    let instrument = row.instrument_type.to_uppercase();
    if !GENERIC_TYPES.contains(&instrument.as_str()) {
        return Ok(instrument);
    }
    let kind = match instrument.as_str() {
        "FUT" | "CE" | "PE" => instrument.as_str(),
        _ => instrument_type_from_symbol(&leg.key.symbol),
    };
    match (kind, is_index) {
        ("FUT", true) => Ok("FUTIDX".into()),
        ("FUT", false) => Ok("FUTSTK".into()),
        ("CE" | "PE", true) => Ok("OPTIDX".into()),
        ("CE" | "PE", false) => Ok("OPTSTK".into()),
        _ => Err(refused(format!(
            "{} on {} is not a future or an option in the master contract, so its margin cannot be worked out. Download the master contract again and retry.",
            leg.key.symbol, leg.key.exchange
        ))),
    }
}

/// A price as the docs write it: "2954", "2939.7" (web `_number_text`).
fn number_text(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else if v.is_finite() && v.fract() == 0.0 {
        format!("{:.0}", v)
    } else {
        format!("{}", v)
    }
}

/// Pocketful's answer as the margin, or the refusal a trader reads.
fn margin_of(answer: &Answer) -> Result<MarginResult> {
    // First, whatever the body: an expired session needs a new login, not
    // another look at the positions.
    if answer.session_refused() {
        return Err(session_refused(answer));
    }
    if matches!(answer.body, None | Some(Value::Null)) {
        tracing::error!(
            status = answer.status.as_u16(),
            "Pocketful margin answered with a body that is not JSON"
        );
        return Err(AppError::Broker(UNREADABLE.into()));
    }
    if !answer.ok() {
        let message = answer.message();
        tracing::error!(
            status = answer.status.as_u16(),
            "Pocketful margin refused: {:?}",
            message
        );
        // The web imports it from gtt_data too.
        let reason = gtt::broker_reason(&message);
        return Err(AppError::Broker(if reason.is_empty() {
            format!("{}. Check the positions and try again.", REFUSED)
        } else {
            format!("{}: {}", REFUSED, reason)
        }));
    }
    parse_margin(answer.payload()).ok_or_else(|| {
        tracing::error!("Pocketful margin answer has no combinedMargin block");
        AppError::Broker(UNREADABLE.into())
    })
}

/// The "log in to Pocketful again" sentence every book and order of this
/// adapter gives (`orders::refused`), as a refusal: `/api/v1/margin`
/// answers it with 400 like every margin refusal, where the
/// `AppError::Auth` the books use would be its 500 "internal error". The
/// web passes Pocketful's 401 or 403 on.
fn session_refused(answer: &Answer) -> AppError {
    match orders::refused(answer, "margin", false) {
        AppError::Auth(text) => AppError::Broker(text),
        other => other,
    }
}

/// The `data` block as OpenAlgo's margin, or `None` if it is unreadable
/// (web `parse_margin_response`). `combinedMargin` is the basket figure
/// with offsets applied. When it comes back all zero while
/// `individualMarginValues` has entries, the entries are summed instead:
/// HDFC Sky's adapter saw the same calculator leave the combined block
/// empty on a single leg. The docs do not list an entry's members; they
/// are read with the combined block's names.
fn parse_margin(payload: Option<&Value>) -> Option<MarginResult> {
    let payload = payload?;
    let combined = payload.get("combinedMargin").filter(|c| c.is_object())?;
    let (mut total, mut span, mut exposure) = requirement(combined);
    let legs = payload
        .get("individualMarginValues")
        .and_then(Value::as_array)
        .filter(|legs| !legs.is_empty());
    if let Some(legs) = legs.filter(|_| total <= 0.0) {
        tracing::debug!("Pocketful combinedMargin empty; summing individual legs");
        (total, span, exposure) = legs
            .iter()
            .map(requirement)
            .fold((0.0, 0.0, 0.0), |sum, leg| {
                (sum.0 + leg.0, sum.1 + leg.1, sum.2 + leg.2)
            });
    }
    Some(MarginResult {
        total_margin_required: py_round(total, 2),
        span_margin: py_round(span, 2),
        exposure_margin: py_round(exposure, 2),
    })
}

/// (total, span, exposure) of one margin block (web `_requirement`).
fn requirement(block: &Value) -> (f64, f64, f64) {
    if !block.is_object() {
        return (0.0, 0.0, 0.0);
    }
    let added: f64 = MARGIN_COMPONENTS
        .iter()
        .map(|key| mapping::num(block, key))
        .sum();
    let total = added - mapping::num(block, MARGIN_BENEFIT);
    (
        total.max(0.0),
        mapping::num(block, "span"),
        mapping::num(block, "exposureMargin"),
    )
}
