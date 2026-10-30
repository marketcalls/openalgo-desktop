//! The order nodes that name one instrument (the web's
//! `execute_place_order`, `execute_smart_order`, `execute_split_order`,
//! `execute_modify_order`, `execute_cancel_order`,
//! `execute_cancel_all_orders`, `execute_close_positions`,
//! `flow_executor_service.py` 837-1000 and 1596-1993), and what every order
//! node shares: the runtime resolution of an order's fields, the price
//! checks, the result log line.
//!
//! A node never retries: each run calls the client once per order it
//! places. A call that got no answer comes back as an error dict that says
//! the order may have been placed (`contract::client::app::NO_ANSWER`), so
//! the walk stops the branch instead of trying again.

use super::NodeFamily;
use crate::flow::contract::client::{
    failure_text, ModifyOrder, PlaceOrder, SmartOrder, SplitOrder,
};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, ERROR, INFO};
use crate::flow::contract::pyfmt;
use crate::flow::contract::resolve::OrderFields;
use crate::flow::node_contracts::default_product_for_exchange;
use crate::services::schemas::VALID_EXCHANGES;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "orders",
    build: || {
        vec![
            Arc::new(PlaceOrderNode),
            Arc::new(SmartOrderNode),
            Arc::new(SplitOrderNode),
            Arc::new(ModifyOrderNode),
            Arc::new(CancelOrderNode),
            Arc::new(CancelAllOrdersNode),
            Arc::new(ClosePositionsNode),
            Arc::new(super::basket::BasketOrderNode),
        ]
    },
};

/// `utils.constants.VALID_PRODUCT_TYPES`.
pub const VALID_PRODUCT_TYPES: [&str; 3] = ["CNC", "NRML", "MIS"];
/// `utils.constants.VALID_PRICE_TYPES`.
pub const VALID_PRICE_TYPES: [&str; 4] = ["MARKET", "LIMIT", "SL", "SL-M"];
/// `utils.constants.VALID_ACTIONS`.
pub const VALID_ACTIONS: [&str; 2] = ["BUY", "SELL"];

/// The fields regular, smart and split orders share, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct StandardOrder {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price_type: String,
    pub product: String,
    pub price: f64,
    pub trigger_price: f64,
}

impl StandardOrder {
    /// The client's order with the run's `strategy` tag.
    fn to_place(&self, strategy: String) -> PlaceOrder {
        PlaceOrder {
            price_type: self.price_type.clone(),
            product_type: self.product.clone(),
            price: self.price,
            trigger_price: self.trigger_price,
            strategy,
            ..PlaceOrder::new(&self.symbol, &self.exchange, &self.action, self.quantity)
        }
    }
}

/// `resolve_standard_order`: symbol (upper-cased: an alert does not
/// control its own casing), exchange, action, quantity, price type,
/// product (the exchange's default when the node names none), price and
/// trigger, in this order, so the first bad field is the one reported.
pub fn resolve_standard_order(
    f: &OrderFields<'_>,
    allow_zero_quantity: bool,
) -> Result<StandardOrder, String> {
    let symbol = f.text("symbol", None, &[])?.to_uppercase();
    let exchange = f.choice("exchange", VALID_EXCHANGES, Some("NSE"), &[])?;
    let action = f.choice("action", &VALID_ACTIONS, Some("BUY"), &[])?;
    let minimum = if allow_zero_quantity { 0 } else { 1 };
    let quantity = f.integer("quantity", Some(1), &[], Some(minimum))?;
    let price_type = f.choice("priceType", &VALID_PRICE_TYPES, Some("MARKET"), &[])?;
    let default_product = default_product_for_exchange(Some(&exchange));
    let product = f.choice("product", &VALID_PRODUCT_TYPES, Some(default_product), &[])?;
    let price = f.number("price", Some(0.0), &[])?;
    let trigger_price = f.number("triggerPrice", Some(0.0), &[])?;
    if let Some(reason) = invalid_price_reason(&price_type, price, trigger_price) {
        return Err(reason);
    }
    Ok(StandardOrder {
        symbol,
        exchange,
        action,
        quantity,
        price_type,
        product,
        price,
        trigger_price,
    })
}

/// `_invalid_price_reason`: why this price type cannot be sent with these
/// prices, or `None`. A LIMIT or SL order whose price never reached the
/// broker executes at an unintended level, so it is refused rather than
/// sent with a zero.
pub fn invalid_price_reason(price_type: &str, price: f64, trigger_price: f64) -> Option<String> {
    let price_type = if price_type.is_empty() {
        "MARKET".to_string()
    } else {
        price_type.to_uppercase()
    };
    let (p, t) = (pyfmt::float(price), pyfmt::float(trigger_price));
    if !price.is_finite() {
        return Some(format!(
            "{} order needs a finite price, got {}",
            price_type, p
        ));
    }
    if !trigger_price.is_finite() {
        return Some(format!(
            "{} order needs a finite trigger price, got {}",
            price_type, t
        ));
    }
    if matches!(price_type.as_str(), "LIMIT" | "SL") && price <= 0.0 {
        return Some(format!(
            "{} order needs a positive price, got {}",
            price_type, p
        ));
    }
    if matches!(price_type.as_str(), "SL" | "SL-M") && trigger_price <= 0.0 {
        return Some(format!(
            "{} order needs a positive trigger price, got {}",
            price_type, t
        ));
    }
    None
}

/// `runtime_order_error`: the one fail-closed result for an unusable order
/// value, logged.
pub fn runtime_order_error(ctx: &NodeCtx, label: &str, error: &str) -> NodeOutcome {
    let message = format!("{} has invalid runtime order data: {}", label, error);
    ctx.error(message.clone());
    NodeOutcome::error(message)
}

/// Whether a client dict reports success.
pub fn succeeded(result: &Value) -> bool {
    result.get("status").and_then(Value::as_str) == Some("success")
}

/// Log `{label}: {result}` (info on success, error otherwise), store the
/// result under the node's output variable and return it: how every
/// action node ends.
pub fn finish(ctx: &NodeCtx, data: &Map<String, Value>, label: &str, result: Value) -> NodeOutcome {
    let level = if succeeded(&result) { INFO } else { ERROR };
    ctx.log(format!("{}: {}", label, pyfmt::repr(&result)), level);
    ctx.store_output(data, result.clone());
    NodeOutcome::result(result)
}

/// `_supplied`: the first of `keys` present in the node whose interpolated
/// text is not blank (an absent key and a blank one both mean "not
/// supplied").
pub fn supplied(ctx: &NodeCtx, data: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let raw = data.get(*key)?;
        let text = if pyfmt::truthy(raw) {
            pyfmt::str(raw)
        } else {
            String::new()
        };
        let resolved = ctx.vars.interpolate(&text).trim().to_string();
        (!resolved.is_empty()).then_some(resolved)
    })
}

/// Place Order: one regular order.
struct PlaceOrderNode;

#[async_trait]
impl FlowNode for PlaceOrderNode {
    fn node_type(&self) -> &'static str {
        "placeOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let order = match resolve_standard_order(&ctx.order_fields(data), false) {
            Ok(o) => o,
            Err(e) => return runtime_order_error(ctx, "Place order", &e),
        };
        ctx.info(format!(
            "Placing order: {} {} qty={}",
            order.symbol, order.action, order.quantity
        ));
        let result = ctx
            .client
            .place_order(&order.to_place(ctx.strategy_tag(data)))
            .await;
        finish(ctx, data, "Order result", result)
    }
}

/// Smart Order: an order sized against the open position; a quantity of 0
/// is allowed (the position size alone decides).
struct SmartOrderNode;

#[async_trait]
impl FlowNode for SmartOrderNode {
    fn node_type(&self) -> &'static str {
        "smartOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let fields = ctx.order_fields(data);
        let resolved = resolve_standard_order(&fields, true)
            .and_then(|o| Ok((o, fields.integer("positionSize", Some(0), &[], None)?)));
        let (order, position_size) = match resolved {
            Ok(r) => r,
            Err(e) => return runtime_order_error(ctx, "Smart order", &e),
        };
        ctx.info(format!(
            "Placing smart order: {} {}",
            order.symbol, order.action
        ));
        let smart = SmartOrder {
            order: order.to_place(ctx.strategy_tag(data)),
            position_size,
        };
        let result = ctx.client.place_smart_order(&smart).await;
        finish(ctx, data, "Smart order result", result)
    }
}

/// Split Order: one quantity sent as orders of at most `splitSize`.
struct SplitOrderNode;

#[async_trait]
impl FlowNode for SplitOrderNode {
    fn node_type(&self) -> &'static str {
        "splitOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let fields = ctx.order_fields(data);
        let resolved = resolve_standard_order(&fields, false)
            .and_then(|o| Ok((o, fields.integer("splitSize", Some(10), &[], Some(1))?)));
        let (order, split_size) = match resolved {
            Ok(r) => r,
            Err(e) => return runtime_order_error(ctx, "Split order", &e),
        };
        ctx.info(format!(
            "Placing split order: {} qty={} split={}",
            order.symbol, order.quantity, split_size
        ));
        let split = SplitOrder {
            order: order.to_place(ctx.strategy_tag(data)),
            split_size,
        };
        let result = ctx.client.split_order(&split).await;
        finish(ctx, data, "Split order result", result)
    }
}

/// Python's `float(value)` of a decoded JSON value, with its error text.
fn py_float(v: &Value) -> Result<f64, String> {
    match v {
        Value::String(s) => pyfmt::parse_float(s)
            .ok_or_else(|| format!("could not convert string to float: {}", pyfmt::quote(s))),
        other => pyfmt::to_float(other).ok_or_else(|| {
            format!(
                "float() argument must be a string or a real number, not '{}'",
                pyfmt::type_name(other)
            )
        }),
    }
}

/// Python's `int(float(value))`: a NaN is a refusal the node reports, an
/// infinity is the `OverflowError` the web's handler does not catch (the
/// run ends).
enum IntError {
    Refused(String),
    Raised(String),
}

fn py_int_of_float(v: &Value) -> Result<i64, IntError> {
    let f = py_float(v).map_err(IntError::Refused)?;
    if f.is_nan() {
        return Err(IntError::Refused(
            "cannot convert float NaN to integer".into(),
        ));
    }
    if f.is_infinite() {
        return Err(IntError::Raised(
            "cannot convert float infinity to integer".into(),
        ));
    }
    Ok(f.trunc() as i64)
}

/// A live order field, `0` when absent or falsy (`live.get(k, 0) or 0`).
fn live_number(live: &Map<String, Value>, key: &str) -> Value {
    match live.get(key) {
        Some(v) if pyfmt::truthy(v) => v.clone(),
        _ => json!(0),
    }
}

/// The three values a modify changes; unspecified keeps the live order's.
fn modify_values(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    live: &Map<String, Value>,
) -> Result<(i64, f64, f64), IntError> {
    let quantity = match supplied(ctx, data, &["newQuantity", "quantity"]) {
        Some(t) => py_int_of_float(&json!(t))?,
        None => py_int_of_float(&live_number(live, "quantity"))?,
    };
    let number = |keys: &[&str], live_key: &str| match supplied(ctx, data, keys) {
        Some(t) => py_float(&json!(t)),
        None => py_float(&live_number(live, live_key)),
    };
    let price = number(&["newPrice", "price"], "price").map_err(IntError::Refused)?;
    let trigger =
        number(&["newTriggerPrice", "triggerPrice"], "trigger_price").map_err(IntError::Refused)?;
    Ok((quantity, price, trigger))
}

/// Modify Order: change an open order's quantity, price or trigger; every
/// other attribute is read back from the live order (a hardcoded default
/// would flip a live SELL to BUY or NRML to MIS on brokers that carry
/// them on a modify).
struct ModifyOrderNode;

impl ModifyOrderNode {
    fn aborted(ctx: &NodeCtx, message: String) -> NodeOutcome {
        ctx.error(format!("Modify order aborted: {}", message));
        NodeOutcome::error(message)
    }

    /// The order's identity: the node's value where supplied, else the
    /// live order's; never guessed.
    fn identity(
        ctx: &NodeCtx,
        data: &Map<String, Value>,
        live: &Map<String, Value>,
    ) -> [(&'static str, String); 5] {
        let pick = |keys: &[&str], live_key: &str| {
            supplied(ctx, data, keys)
                .unwrap_or_else(|| live.get(live_key).map(pyfmt::str).unwrap_or_default())
        };
        [
            ("symbol", pick(&["symbol"], "symbol")),
            ("exchange", pick(&["exchange"], "exchange")),
            ("action", pick(&["action"], "action").to_uppercase()),
            ("product", pick(&["product"], "product")),
            (
                "priceType",
                pick(&["priceType", "pricetype"], "pricetype").to_uppercase(),
            ),
        ]
    }
}

#[async_trait]
impl FlowNode for ModifyOrderNode {
    fn node_type(&self) -> &'static str {
        "modifyOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let order_id = ctx.get_str(data, "orderId", "");
        if order_id.is_empty() {
            return Self::aborted(ctx, "orderId is required to modify an order".into());
        }
        let existing = ctx.client.get_order_status(&order_id).await;
        let live = existing.get("data").filter(|d| pyfmt::truthy(d));
        let live = match live {
            Some(Value::Object(m)) if succeeded(&existing) => m.clone(),
            Some(other) if succeeded(&existing) => {
                return NodeOutcome::abort(format!(
                    "'{}' object has no attribute 'get'",
                    pyfmt::type_name(other)
                ))
            }
            _ => {
                let why = failure_text(&existing).unwrap_or_else(|| "order not found".into());
                return Self::aborted(
                    ctx,
                    format!("Could not read order {} to modify it: {}", order_id, why),
                );
            }
        };
        let identity = Self::identity(ctx, data, &live);
        let missing: Vec<&str> = identity
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| *k)
            .collect();
        if !missing.is_empty() {
            return Self::aborted(
                ctx,
                format!(
                    "Order {} did not report {}; refusing to modify it with a guessed value",
                    order_id,
                    missing.join(", ")
                ),
            );
        }
        let [(_, symbol), (_, exchange), (_, action), (_, product), (_, price_type)] = identity;
        let (quantity, price, trigger_price) = match modify_values(ctx, data, &live) {
            Ok(v) => v,
            Err(IntError::Refused(e)) => {
                return Self::aborted(
                    ctx,
                    format!("Modify order got a non-numeric price or quantity: {}", e),
                )
            }
            Err(IntError::Raised(e)) => return NodeOutcome::abort(e),
        };
        if quantity <= 0 {
            return Self::aborted(
                ctx,
                format!("Modify order needs a positive quantity, got {}", quantity),
            );
        }
        ctx.info(format!(
            "Modifying order: {} - {} {} qty={} price={} trigger={}",
            order_id,
            symbol,
            action,
            quantity,
            pyfmt::float(price),
            pyfmt::float(trigger_price)
        ));
        // The web passes no strategy here, so the client's default goes.
        let order = PlaceOrder {
            price_type,
            product_type: product,
            price,
            trigger_price,
            ..PlaceOrder::new(&symbol, &exchange, &action, quantity)
        };
        let result = ctx
            .client
            .modify_order(&ModifyOrder { order_id, order })
            .await;
        finish(ctx, data, "Modify order result", result)
    }
}

/// Cancel Order: cancel one order by id. The id is sent as written (an
/// empty one reaches the service, which refuses it).
struct CancelOrderNode;

#[async_trait]
impl FlowNode for CancelOrderNode {
    fn node_type(&self) -> &'static str {
        "cancelOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let raw = data.get("orderId").map(pyfmt::str).unwrap_or_default();
        let order_id = ctx.vars.interpolate(&raw);
        ctx.info(format!("Cancelling order: {}", order_id));
        let result = ctx.client.cancel_order(&order_id).await;
        // WEB-DEFECT N-cancelOrder-02: the web never stores this result, so
        // an output variable set on the node stayed unresolved downstream.
        // Stored here like every other action node's.
        finish(ctx, data, "Cancel result", result)
    }
}

/// Cancel All Orders: every open order.
struct CancelAllOrdersNode;

#[async_trait]
impl FlowNode for CancelAllOrdersNode {
    fn node_type(&self) -> &'static str {
        "cancelAllOrders"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        ctx.info("Cancelling all orders");
        let result = ctx.client.cancel_all_orders().await;
        finish(ctx, data, "Cancel all result", result)
    }
}

/// Close Positions: every open position, or only the one the node names
/// (`symbol`, with `exchange` NSE and the exchange's product by default).
///
/// The web's live close ignored the symbol and squared off everything
/// (WEB-DEFECT CL-09); the desktop's client sends a scoped close to a
/// service that closes that one position (`flow::nodes::internal`).
struct ClosePositionsNode;

#[async_trait]
impl FlowNode for ClosePositionsNode {
    fn node_type(&self) -> &'static str {
        "closePositions"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let Some(symbol) = supplied(ctx, data, &["symbol"]) else {
            ctx.info("Closing all positions");
            let result = ctx.client.close_all_positions().await;
            return finish(ctx, data, "Close all positions result", result);
        };
        let exchange = supplied(ctx, data, &["exchange"]).unwrap_or_else(|| "NSE".into());
        let product = supplied(ctx, data, &["product"])
            .unwrap_or_else(|| default_product_for_exchange(Some(&exchange)).to_string());
        ctx.info(format!(
            "Closing position: {}@{} ({})",
            symbol, exchange, product
        ));
        let result = ctx
            .client
            .close_position(&symbol, &exchange, &product, &ctx.strategy_tag(data))
            .await;
        finish(ctx, data, "Close position result", result)
    }
}

#[cfg(test)]
mod tests;
