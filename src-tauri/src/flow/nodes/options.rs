//! Options Order and Multi-Leg Options (the web's `execute_options_order`
//! and `execute_options_multi_order`, `flow_executor_service.py`
//! 1065-1594) and what both read: which exchange quotes the underlying and
//! which lists its options, the lot size from the master contract (failing
//! closed), and the date a relative expiry names.
//!
//! Quantities on these nodes are in lots: the node multiplies by the lot
//! size the master contract lists. A lot size that cannot be read refuses
//! the order rather than guessing it.

use super::legs::{
    custom_legs, expiry_type_error, generate_strategy_legs, leg_line, Generated, LegDefaults,
    OPTION_TYPES,
};
use super::orders::{
    finish, invalid_price_reason, runtime_order_error, VALID_ACTIONS, VALID_PRICE_TYPES,
    VALID_PRODUCT_TYPES,
};
use super::NodeFamily;
use crate::brokers::common::underlying::extract_underlying;
use crate::flow::contract::client::{OptionsMultiOrder, OptionsOrder};
use crate::flow::contract::env::SymbolQuery;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::resolve::OrderFields;
use crate::flow::node_contracts::{
    is_expiry_date, is_option_offset, select_expiry, VALID_EXPIRY_TYPES,
};
use crate::services::schemas::VALID_EXCHANGES;
use async_trait::async_trait;
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "options",
    build: || vec![Arc::new(OptionsOrderNode), Arc::new(OptionsMultiOrderNode)],
};

/// MCX products that list options. MCX has no spot instrument: the ATM
/// reference is the near-month future, and the future, the option and the
/// quote all live on MCX (`MCX_OPTION_UNDERLYINGS`).
pub const MCX_OPTION_UNDERLYINGS: [&str; 11] = [
    "GOLD",
    "GOLDM",
    "SILVER",
    "SILVERM",
    "CRUDEOIL",
    "CRUDEOILM",
    "NATURALGAS",
    "NATGASMINI",
    "COPPER",
    "ZINC",
    "MCXBULLDEX",
];

/// The BSE indices whose options trade on BFO.
const BSE_OPTION_INDICES: [&str; 3] = ["SENSEX", "BANKEX", "SENSEX50"];

/// Exchanges with no spot instrument: a declared one quotes and trades on
/// itself. The desktop option-symbol service's own list (X-135), never a
/// Flow copy.
pub use crate::services::options_service::NO_SPOT as NO_SPOT_EXCHANGES;

/// `DEFAULT_OPTION_EXCHANGES`.
pub const DEFAULT_OPTION_EXCHANGES: (&str, &str) = ("NSE_INDEX", "NFO");

/// `resolve_option_exchanges`: (the exchange that quotes the underlying,
/// the exchange its options trade on). A named underlying decides on its
/// own (an imported workflow's stale `exchange` must not send SENSEX to
/// NFO); the node's declared exchange is the fallback for anything else.
///
/// WEB-DEFECT X-134 (fixed): the web quoted every underlying declared on
/// NFO/NSE (BFO/BSE) on the index segment, so a stock option's ATM
/// reference was looked up where the stock is not listed. A declared
/// derivative or equity segment now quotes an index on its index segment
/// and anything else on the equity segment, as `/api/v1/optionsorder`
/// reads its exchange.
pub fn resolve_option_exchanges(underlying: &str, declared: &str) -> (String, String) {
    let name = underlying.trim().to_uppercase();
    let pair = |q: &str, o: &str| (q.to_string(), o.to_string());
    if BSE_OPTION_INDICES.contains(&name.as_str()) {
        return pair("BSE_INDEX", "BFO");
    }
    if MCX_OPTION_UNDERLYINGS.contains(&name.as_str()) {
        return pair("MCX", "MCX");
    }
    let quote = crate::services::options_service::quote_exchange;
    match declared.trim().to_uppercase().as_str() {
        "BFO" | "BSE" => (quote(&name, "BFO"), "BFO".into()),
        "BSE_INDEX" => pair("BSE_INDEX", "BFO"),
        "NFO" | "NSE" => (quote(&name, "NFO"), "NFO".into()),
        "NSE_INDEX" => pair("NSE_INDEX", "NFO"),
        d if NO_SPOT_EXCHANGES.contains(&d) => pair(d, d),
        _ => pair(DEFAULT_OPTION_EXCHANGES.0, DEFAULT_OPTION_EXCHANGES.1),
    }
}

/// Built-in lot sizes, used only when the master contract has no rows for
/// the exchange at all (`_FALLBACK_LOT_SIZES`).
const FALLBACK_LOT_SIZES: [(&str, i64); 8] = [
    ("NIFTY", 65),
    ("BANKNIFTY", 30),
    ("FINNIFTY", 60),
    ("MIDCPNIFTY", 120),
    ("NIFTYNXT50", 25),
    ("SENSEX", 20),
    ("BANKEX", 30),
    ("SENSEX50", 25),
];

/// The lot size the master lists for `underlying` on `fo_exchange`, read
/// once; `None` when no row names it.
fn listed_lot_size(
    ctx: &NodeCtx,
    underlying: &str,
    fo_exchange: &str,
) -> Result<Option<i64>, String> {
    let query = |name: Option<&str>, prefix: Option<&str>, limit: Option<usize>| SymbolQuery {
        name: name.map(str::to_string),
        symbol_prefix: prefix.map(str::to_string),
        exchange: Some(fo_exchange.to_string()),
        limit,
        ..SymbolQuery::default()
    };
    let symbols = &ctx.env.symbols;
    let by_name = symbols.rows(&query(Some(underlying), None, None))?;
    if let Some(row) = by_name.iter().find(|r| r.lot_size > 0) {
        return Ok(Some(i64::from(row.lot_size)));
    }
    // Some brokers put the contract description in `name`; the OpenAlgo
    // symbol reads the same on every broker. NIFTY must not take
    // NIFTYNXT50's lot, so each candidate's underlying is read back.
    let by_prefix = symbols.rows(&query(None, Some(underlying), None))?;
    Ok(by_prefix
        .iter()
        .filter(|r| r.lot_size > 0)
        .find(|r| extract_underlying(&r.symbol, fo_exchange).as_deref() == Some(underlying))
        .map(|r| i64::from(r.lot_size)))
}

/// `_resolve_lot_size`: the lot size from the master contract, failing
/// closed; the built-in table only when the exchange has no contracts
/// loaded at all.
pub fn resolve_lot_size(ctx: &NodeCtx, underlying: &str, fo_exchange: &str) -> Result<i64, String> {
    let unreadable = |e: String| {
        tracing::warn!(
            "Lot size lookup failed for {} on {}: {}",
            underlying,
            fo_exchange,
            e
        );
        format!(
            "Could not read the lot size for {} on {}. Refusing to size an order on a guess.",
            underlying, fo_exchange
        )
    };
    if let Some(lot) = listed_lot_size(ctx, underlying, fo_exchange).map_err(unreadable)? {
        return Ok(lot);
    }
    let seeded = ctx
        .env
        .symbols
        .rows(&SymbolQuery {
            exchange: Some(fo_exchange.to_string()),
            limit: Some(1),
            ..SymbolQuery::default()
        })
        .map_err(unreadable)?;
    if !seeded.is_empty() {
        return Err(format!(
            "{} has no contract on {} in the master contract. Check the symbol, or re-download the master contract.",
            underlying, fo_exchange
        ));
    }
    let Some((_, fallback)) = FALLBACK_LOT_SIZES.iter().find(|(u, _)| *u == underlying) else {
        return Err(format!(
            "The master contract has no {} contracts loaded and no built-in lot size for {}. Download the master contract first.",
            fo_exchange, underlying
        ));
    };
    ctx.log(
        format!(
            "Master contract has no {} contracts loaded; using the built-in lot size {} for {}",
            fo_exchange, fallback, underlying
        ),
        WARNING,
    );
    Ok(*fallback)
}

/// The values `select_expiry` reads from an expiry reply's `data`, as
/// Python iterates them; `Err` for a value Python cannot iterate.
fn listed_expiries(data: &Value) -> Result<Vec<String>, String> {
    match data {
        Value::Array(items) => Ok(items
            .iter()
            .map(|v| v.as_str().map(str::to_string).unwrap_or_default())
            .collect()),
        Value::String(s) => Ok(s.chars().map(String::from).collect()),
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        other => Err(format!(
            "'{}' object is not iterable",
            pyfmt::type_name(other)
        )),
    }
}

/// `_resolve_expiry_date`: the date (DDMMMYY) a relative expiry type names
/// for `symbol` on `exchange`, counted from today in IST; `None` (logged)
/// when it cannot be read or matched.
pub async fn resolve_expiry_date(
    ctx: &NodeCtx,
    symbol: &str,
    exchange: &str,
    expiry_type: &str,
) -> Option<String> {
    let response = ctx.client.get_expiry(symbol, exchange, "options").await;
    if response.get("status").and_then(Value::as_str) != Some("success") {
        ctx.error(format!(
            "Failed to fetch expiry: {}",
            pyfmt::repr(&response)
        ));
        return None;
    }
    let data = response.get("data").cloned().unwrap_or_else(|| json!([]));
    if !pyfmt::truthy(&data) {
        ctx.error(format!(
            "No expiry dates found for {} on {}",
            symbol, exchange
        ));
        return None;
    }
    let listed = match listed_expiries(&data) {
        Ok(l) => l,
        Err(e) => {
            ctx.error(format!("Error resolving expiry: {}", e));
            return None;
        }
    };
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let selected = select_expiry(&listed, expiry_type, today);
    if selected.is_none() {
        ctx.error(format!(
            "No expiry matches {} for {} on {} among {} listed contract(s)",
            expiry_type,
            symbol,
            exchange,
            listed.len()
        ));
    }
    selected
}

/// The node's expiry inputs: (relative type, explicit DDMMMYY date or
/// empty). The editor's single Expiry control holds either form, so a date
/// under `expiryType` is an explicit date.
fn node_expiry(f: &OrderFields<'_>) -> Result<(String, String), String> {
    let expiry_type = f
        .text("expiryType", Some("current_week"), &[])?
        .to_lowercase();
    let mut date = f.optional_text("expiryDate")?.to_uppercase();
    if date.is_empty() && is_expiry_date(&expiry_type.to_uppercase()) {
        date = expiry_type.to_uppercase();
    }
    if !date.is_empty() && !is_expiry_date(&date) {
        return Err(format!(
            "expiryDate must be in DDMMMYY format such as 28OCT25, got {}",
            pyfmt::quote(&date)
        ));
    }
    if date.is_empty() && !VALID_EXPIRY_TYPES.contains(&expiry_type.as_str()) {
        return Err(expiry_type_error(&expiry_type));
    }
    Ok((expiry_type, date))
}

/// An ATM/ITMn/OTMn text field, upper-cased.
fn offset_field(f: &OrderFields<'_>, key: &str, default: &str) -> Result<String, String> {
    let offset = f.text(key, Some(default), &[])?.to_uppercase();
    if !is_option_offset(&offset) {
        return Err(format!(
            "{} must be ATM, ITM1-ITM50, or OTM1-OTM50, got {}",
            key,
            pyfmt::quote(&offset)
        ));
    }
    Ok(offset)
}

/// The `exchange` field, checked only when the node has one.
fn check_exchange(f: &OrderFields<'_>, data: &Map<String, Value>) -> Result<(), String> {
    if data.contains_key("exchange") {
        f.choice("exchange", VALID_EXCHANGES, None, &[])?;
    }
    Ok(())
}

/// Where an options node stands once its fields are checked: the
/// exchanges, the lot size and the node's expiry date. A failure is the
/// node's result, already logged with `failed_prefix`.
struct Sized {
    quote_exchange: String,
    fo_exchange: String,
    lot_size: i64,
    expiry_date: String,
}

/// Lot size, then expiry, as both options nodes do it.
async fn size_and_expiry(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    underlying: &str,
    lots: i64,
    (expiry_type, date_input): (&str, &str),
    failed_prefix: &str,
) -> Result<Sized, NodeOutcome> {
    let fail = |message: String| {
        ctx.error(format!("{}{}", failed_prefix, message));
        NodeOutcome::error(message)
    };
    let (quote_exchange, fo_exchange) =
        resolve_option_exchanges(underlying, &ctx.get_str(data, "exchange", ""));
    let lot_size = resolve_lot_size(ctx, underlying, &fo_exchange).map_err(fail)?;
    ctx.info(format!(
        "Lot size for {}: {} -> {} lot(s) = {}",
        underlying,
        lot_size,
        lots,
        lots.saturating_mul(lot_size)
    ));
    let expiry_date = if date_input.is_empty() {
        resolve_expiry_date(ctx, underlying, &fo_exchange, expiry_type).await
    } else {
        Some(date_input.to_string())
    };
    let Some(expiry_date) = expiry_date else {
        return Err(fail(format!(
            "Could not resolve expiry for {}",
            expiry_type
        )));
    };
    let shown = if date_input.is_empty() {
        expiry_type
    } else {
        date_input
    };
    ctx.info(format!("Resolved expiry: {} -> {}", shown, expiry_date));
    Ok(Sized {
        quote_exchange,
        fo_exchange,
        lot_size,
        expiry_date,
    })
}

/// The fields of an Options Order, checked.
struct SingleLeg {
    underlying: String,
    expiry: (String, String),
    quantity: i64,
    offset: String,
    option_type: String,
    action: String,
    price_type: String,
    product: String,
    split_size: i64,
    price: f64,
    trigger_price: f64,
}

fn single_leg(f: &OrderFields<'_>, data: &Map<String, Value>) -> Result<SingleLeg, String> {
    let underlying = f.text("underlying", Some("NIFTY"), &[])?.to_uppercase();
    let expiry = node_expiry(f)?;
    let leg = SingleLeg {
        underlying,
        expiry,
        quantity: f.integer("quantity", Some(1), &[], Some(1))?,
        offset: offset_field(f, "offset", "ATM")?,
        option_type: f.choice("optionType", &OPTION_TYPES, Some("CE"), &[])?,
        action: f.choice("action", &VALID_ACTIONS, Some("BUY"), &[])?,
        price_type: f.choice("priceType", &VALID_PRICE_TYPES, Some("MARKET"), &[])?,
        // An option is always a derivative: NRML whatever the
        // underlying's segment says.
        product: f.choice("product", &VALID_PRODUCT_TYPES, Some("NRML"), &[])?,
        split_size: f.integer("splitSize", Some(0), &[], Some(0))?,
        price: f.number("price", Some(0.0), &[])?,
        trigger_price: f.number("triggerPrice", Some(0.0), &[])?,
    };
    check_exchange(f, data)?;
    if let Some(reason) = invalid_price_reason(&leg.price_type, leg.price, leg.trigger_price) {
        return Err(reason);
    }
    Ok(leg)
}

/// Options Order: one option picked by offset from the live underlying.
struct OptionsOrderNode;

#[async_trait]
impl FlowNode for OptionsOrderNode {
    fn node_type(&self) -> &'static str {
        "optionsOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let leg = match single_leg(&ctx.order_fields(data), data) {
            Ok(l) => l,
            Err(e) => return runtime_order_error(ctx, "Options order", &e),
        };
        ctx.info(format!(
            "Placing options order: {} {} {}",
            leg.underlying, leg.option_type, leg.offset
        ));
        let expiry = (leg.expiry.0.as_str(), leg.expiry.1.as_str());
        let sized = match size_and_expiry(
            ctx,
            data,
            &leg.underlying,
            leg.quantity,
            expiry,
            "Options order failed: ",
        )
        .await
        {
            Ok(s) => s,
            Err(outcome) => return outcome,
        };
        let order = OptionsOrder {
            underlying: leg.underlying,
            exchange: sized.quote_exchange,
            action: leg.action,
            quantity: leg.quantity.saturating_mul(sized.lot_size),
            expiry_date: sized.expiry_date,
            offset: leg.offset,
            option_type: leg.option_type,
            price_type: leg.price_type,
            product: leg.product,
            price: leg.price,
            splitsize: leg.split_size,
            strategy: ctx.strategy_tag(data),
            trigger_price: leg.trigger_price,
        };
        let result = ctx.client.options_order(&order).await;
        finish(ctx, data, "Options order result", result)
    }
}

/// The node-level fields of a Multi-Leg Options order, checked.
struct MultiNode {
    underlying: String,
    expiry: (String, String),
    strategy_type: String,
    action: String,
    quantity_lots: i64,
    product: String,
    strangle_width: String,
    price_type: String,
    price: f64,
    trigger_price: f64,
}

fn multi_node(f: &OrderFields<'_>, data: &Map<String, Value>) -> Result<MultiNode, String> {
    let underlying = f.text("underlying", Some("NIFTY"), &[])?.to_uppercase();
    let expiry = node_expiry(f)?;
    let node = MultiNode {
        underlying,
        expiry,
        // The editor writes `strategy`; `strategyType` is the legacy name.
        strategy_type: f
            .text("strategy", Some("custom"), &["strategyType"])?
            .to_lowercase(),
        action: f.choice("action", &VALID_ACTIONS, Some("SELL"), &[])?,
        quantity_lots: f.integer("quantity", Some(1), &[], Some(1))?,
        product: f.choice("product", &VALID_PRODUCT_TYPES, Some("NRML"), &[])?,
        strangle_width: offset_field(f, "strangleWidth", "OTM2")?,
        price_type: f.choice("priceType", &VALID_PRICE_TYPES, Some("MARKET"), &[])?,
        price: f.number("price", Some(0.0), &[])?,
        trigger_price: f.number("triggerPrice", Some(0.0), &[])?,
    };
    check_exchange(f, data)?;
    if node.strategy_type != "custom" {
        if !matches!(node.price_type.as_str(), "MARKET" | "LIMIT") {
            return Err(
                "generated option strategies support only MARKET and LIMIT price types".into(),
            );
        }
        if let Some(reason) = invalid_price_reason(&node.price_type, node.price, node.trigger_price)
        {
            return Err(reason);
        }
    }
    Ok(node)
}

/// Multi-Leg Options: a readymade strategy (straddle, strangle, iron
/// condor, bull call and bear put spreads) or custom legs, each naming an
/// offset or an absolute strike and optionally its own expiry.
struct OptionsMultiOrderNode;

impl OptionsMultiOrderNode {
    /// The legs to send: custom legs with their own expiries resolved (one
    /// lookup per relative type), or the strategy's generated legs.
    async fn legs(
        ctx: &NodeCtx,
        node: &MultiNode,
        custom: Option<Vec<super::legs::CustomLeg>>,
        sized: &Sized,
    ) -> Result<Vec<Value>, String> {
        let Some(custom) = custom else {
            if matches!(node.price_type.as_str(), "SL" | "SL-M") {
                return Err(format!(
                    "{} is not available for a generated {} strategy: its legs carry no trigger price. Use MARKET or LIMIT, or build the legs individually as custom legs where each can set its own trigger price.",
                    node.price_type, node.strategy_type
                ));
            }
            if let Some(reason) = invalid_price_reason(&node.price_type, node.price, 0.0) {
                return Err(reason);
            }
            let generated = Generated {
                action: &node.action,
                quantity: node.quantity_lots.saturating_mul(sized.lot_size),
                product: &node.product,
                strangle_width: &node.strangle_width,
                price_type: &node.price_type,
                price: node.price,
            };
            return Ok(generate_strategy_legs(ctx, &node.strategy_type, &generated));
        };
        let mut cache: HashMap<String, String> = HashMap::new();
        let mut legs = Vec::with_capacity(custom.len());
        for (n, leg) in custom.iter().enumerate() {
            let mut expiry = leg.expiry_date.clone();
            if expiry.is_empty() && !leg.expiry_type.is_empty() {
                expiry = match cache.get(&leg.expiry_type) {
                    Some(e) => e.clone(),
                    None => {
                        let resolved = resolve_expiry_date(
                            ctx,
                            &node.underlying,
                            &sized.fo_exchange,
                            &leg.expiry_type,
                        )
                        .await
                        .ok_or_else(|| {
                            format!(
                                "leg {}: could not resolve expiry for {}",
                                n + 1,
                                leg.expiry_type
                            )
                        })?;
                        cache.insert(leg.expiry_type.clone(), resolved.clone());
                        resolved
                    }
                };
            }
            legs.push(leg.payload(sized.lot_size, &expiry));
        }
        Ok(legs)
    }
}

#[async_trait]
impl FlowNode for OptionsMultiOrderNode {
    fn node_type(&self) -> &'static str {
        "optionsMultiOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let keys: Vec<Value> = data.keys().map(|k| json!(k)).collect();
        ctx.info(format!(
            "Options multi-order node_data keys: {}",
            pyfmt::repr(&Value::Array(keys))
        ));
        const LABEL: &str = "Options multi-order";
        let f = ctx.order_fields(data);
        let node = match multi_node(&f, data) {
            Ok(n) => n,
            Err(e) => return runtime_order_error(ctx, LABEL, &e),
        };
        let custom = if node.strategy_type == "custom" {
            let defaults = LegDefaults {
                product: node.product.clone(),
                price_type: node.price_type.clone(),
                price: node.price,
                trigger_price: node.trigger_price,
            };
            match custom_legs(ctx, &f, &defaults) {
                Ok(legs) => Some(legs),
                Err(e) => return runtime_order_error(ctx, LABEL, &e),
            }
        } else {
            None
        };
        ctx.info(format!(
            "Strategy: {}, Action: {}, Quantity: {} lots, Product: {}",
            node.strategy_type, node.action, node.quantity_lots, node.product
        ));
        let expiry = (node.expiry.0.as_str(), node.expiry.1.as_str());
        let failed = "Options multi-order failed: ";
        let sized = match size_and_expiry(
            ctx,
            data,
            &node.underlying,
            node.quantity_lots,
            expiry,
            failed,
        )
        .await
        {
            Ok(s) => s,
            Err(outcome) => return outcome,
        };
        let legs = match Self::legs(ctx, &node, custom, &sized).await {
            Ok(legs) if !legs.is_empty() => legs,
            Ok(_) => {
                let message = format!("No legs generated for strategy: {}", node.strategy_type);
                ctx.error(format!("{}{}", failed, message));
                return NodeOutcome::error(message);
            }
            Err(message) => {
                ctx.error(format!("{}{}", failed, message));
                return NodeOutcome::error(message);
            }
        };
        ctx.info(format!(
            "Placing options multi-order: {} {} with {} legs",
            node.underlying,
            node.strategy_type,
            legs.len()
        ));
        for (n, leg) in legs.iter().enumerate() {
            ctx.info(leg_line(n + 1, leg));
        }
        let order = OptionsMultiOrder {
            underlying: node.underlying,
            exchange: sized.quote_exchange,
            expiry_date: sized.expiry_date,
            legs,
            strategy: ctx.strategy_tag(data),
            strike_int: None,
        };
        let result = ctx.client.options_multi_order(&order).await;
        finish(ctx, data, "Options multi-order result", result)
    }
}

#[cfg(test)]
mod tests;
