//! Basket Order (the web's `execute_basket_order`,
//! `flow_executor_service.py` 1781-1960): several orders in one call, from
//! CSV lines `SYMBOL,EXCHANGE,ACTION,QTY` or a list of order objects (or a
//! `{{var}}` holding either), every row checked before anything is sent so
//! a bad later row never leaves a partial basket.
//!
//! Two web defects are fixed here: rows go out with `trigger_price`, the
//! key the basket service reads (the web sent `triggerprice`, so an SL
//! row's trigger was lost, N-basketOrder-06), and row symbols are
//! upper-cased as the single-order nodes do (N-basketOrder-04).

use super::orders::{
    finish, invalid_price_reason, VALID_ACTIONS, VALID_PRICE_TYPES, VALID_PRODUCT_TYPES,
};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::flow::contract::vars::has_unresolved;
use crate::flow::node_contracts::default_product_for_exchange;
use crate::services::schemas::VALID_EXCHANGES;
use async_trait::async_trait;
use serde_json::{json, Map, Value};

/// The node.
pub struct BasketOrderNode;

/// The fields every row inherits unless it names its own.
struct Common {
    /// Empty when the node names no product: each row takes its own
    /// exchange's default.
    product: String,
    price_type: String,
    price: f64,
    trigger_price: f64,
}

/// One row before it is checked: its 1-based line or item number, its
/// fields, and whether it came from CSV text (which takes product, price
/// type and prices only from the node).
struct RawRow {
    index: usize,
    order: Map<String, Value>,
    csv: bool,
}

/// One basket's resolution against the run's variables.
struct Resolver<'a> {
    ctx: &'a NodeCtx,
}

impl Resolver<'_> {
    /// A value as the order uses it: a `{{...}}` text resolved, refused when
    /// a reference is left.
    fn resolve(&self, value: &Value, field: &str) -> Result<Value, String> {
        let v = self.ctx.vars.resolve_raw(value);
        if let Value::String(s) = &v {
            if has_unresolved(s) {
                return Err(format!(
                    "{} has unresolved value {}",
                    field,
                    pyfmt::quote(s)
                ));
            }
        }
        Ok(v)
    }

    fn number(&self, value: &Value, field: &str) -> Result<f64, String> {
        let v = self.resolve(value, field)?;
        if let Value::Bool(_) = v {
            return Err(format!(
                "{} must be numeric, got boolean {}",
                field,
                pyfmt::repr(&v)
            ));
        }
        if let Value::String(s) = &v {
            if s.trim().is_empty() {
                return Err(format!("{} must not be blank", field));
            }
        }
        let n = pyfmt::to_float(&v)
            .ok_or_else(|| format!("{} must be numeric, got {}", field, pyfmt::repr(&v)))?;
        if !n.is_finite() {
            return Err(format!("{} must be finite, got {}", field, pyfmt::repr(&v)));
        }
        Ok(n)
    }

    fn required_text(&self, value: &Value, field: &str) -> Result<String, String> {
        let v = self.resolve(value, field)?;
        if v.is_null() {
            return Err(format!("{} is required", field));
        }
        let text = pyfmt::str(&v).trim().to_string();
        if text.is_empty() {
            return Err(format!("{} is required", field));
        }
        Ok(text)
    }

    /// The node-level fields (`common_value`: the default only when the key
    /// is absent).
    fn common(&self, data: &Map<String, Value>) -> Result<Common, String> {
        let value = |key: &str, default: Value| -> Result<Value, String> {
            match data.get(key) {
                None => Ok(default),
                Some(v) => self.resolve(v, key),
            }
        };
        let product = match data.get("product") {
            None => String::new(),
            Some(_) => self
                .required_text(&value("product", Value::Null)?, "product")?
                .to_uppercase(),
        };
        let price_type = self
            .required_text(&value("priceType", json!("MARKET"))?, "priceType")?
            .to_uppercase();
        if !product.is_empty() && !VALID_PRODUCT_TYPES.contains(&product.as_str()) {
            return Err(format!("invalid product {}", pyfmt::quote(&product)));
        }
        if !VALID_PRICE_TYPES.contains(&price_type.as_str()) {
            return Err(format!("invalid pricetype {}", pyfmt::quote(&price_type)));
        }
        let price = self.number(&value("price", json!(0.0))?, "price")?;
        let trigger_price = self.number(&value("triggerPrice", json!(0.0))?, "triggerPrice")?;
        Ok(Common {
            product,
            price_type,
            price,
            trigger_price,
        })
    }

    /// One row checked into the order the service receives.
    fn row(&self, row: &RawRow, common: &Common) -> Result<Value, String> {
        let get = |k: &str| row.order.get(k).cloned().unwrap_or(Value::Null);
        // A row's own value for `key` (or an alias); the node's otherwise.
        let own = |key: &str, aliases: &[&str], default: Value| -> Result<Value, String> {
            if row.csv {
                return Ok(default);
            }
            for candidate in std::iter::once(key).chain(aliases.iter().copied()) {
                if let Some(v) = row.order.get(candidate) {
                    return self.resolve(v, candidate);
                }
            }
            Ok(default)
        };
        let symbol = self.required_text(&get("symbol"), "symbol")?.to_uppercase();
        let exchange = self
            .required_text(&get("exchange"), "exchange")?
            .to_uppercase();
        let action = self.required_text(&get("action"), "action")?.to_uppercase();
        let quantity_value = self.resolve(&get("quantity"), "quantity")?;
        let quantity = self.number(&quantity_value, "quantity")?.trunc() as i64;
        if quantity <= 0 {
            return Err(format!("quantity must be positive, got {}", quantity));
        }
        let default_product = if common.product.is_empty() {
            default_product_for_exchange(Some(&exchange)).to_string()
        } else {
            common.product.clone()
        };
        let product = self
            .required_text(&own("product", &[], json!(default_product))?, "product")?
            .to_uppercase();
        let price_type = self
            .required_text(
                &own("pricetype", &["priceType"], json!(common.price_type))?,
                "pricetype",
            )?
            .to_uppercase();
        let checks = [
            (VALID_EXCHANGES, &exchange, "exchange"),
            (&VALID_ACTIONS[..], &action, "action"),
            (&VALID_PRODUCT_TYPES[..], &product, "product"),
            (&VALID_PRICE_TYPES[..], &price_type, "pricetype"),
        ];
        for (allowed, value, name) in checks {
            if !allowed.contains(&value.as_str()) {
                return Err(format!("invalid {} {}", name, pyfmt::quote(value)));
            }
        }
        let price = self.number(&own("price", &[], json!(common.price))?, "price")?;
        let trigger_price = self.number(
            &own(
                "triggerprice",
                &["triggerPrice"],
                json!(common.trigger_price),
            )?,
            "triggerprice",
        )?;
        if let Some(reason) = invalid_price_reason(&price_type, price, trigger_price) {
            return Err(reason);
        }
        Ok(json!({
            "symbol": symbol, "exchange": exchange, "action": action, "quantity": quantity,
            "product": product, "pricetype": price_type, "price": price,
            "trigger_price": trigger_price,
        }))
    }
}

/// The rows of `orders` as resolved: CSV lines (blank ones skipped, each
/// numbered by its line) or list items; `Err((index, detail))` for the
/// first row that is not a row at all.
fn raw_rows(orders: &Value) -> Result<Vec<RawRow>, (usize, String)> {
    let mut rows = Vec::new();
    match orders {
        Value::String(text) => {
            for (n, line) in text.trim().split('\n').enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                if parts.len() < 4 {
                    return Err((n + 1, "expected SYMBOL,EXCHANGE,ACTION,QTY".into()));
                }
                let mut order = Map::new();
                for (key, part) in ["symbol", "exchange", "action", "quantity"]
                    .iter()
                    .zip(&parts)
                {
                    order.insert((*key).into(), json!(part));
                }
                rows.push(RawRow {
                    index: n + 1,
                    order,
                    csv: true,
                });
            }
        }
        Value::Array(items) => {
            for (n, item) in items.iter().enumerate() {
                let Value::Object(order) = item else {
                    return Err((n + 1, "must be an order object".into()));
                };
                rows.push(RawRow {
                    index: n + 1,
                    order: order.clone(),
                    csv: false,
                });
            }
        }
        _ => {}
    }
    Ok(rows)
}

fn basket_error(ctx: &NodeCtx, index: usize, detail: &str) -> NodeOutcome {
    let message = format!("Basket order row {}: {}", index, detail);
    ctx.error(format!("Basket order failed: {}", message));
    NodeOutcome::error(message)
}

#[async_trait]
impl FlowNode for BasketOrderNode {
    fn node_type(&self) -> &'static str {
        "basketOrder"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let orders_raw = ctx
            .vars
            .resolve_raw(data.get("orders").unwrap_or(&json!("")));
        let basket_name = ctx.get_str(data, "basketName", "flow_basket");
        let r = Resolver { ctx };
        let common = match r.common(data) {
            Ok(c) => c,
            Err(e) => return basket_error(ctx, 1, &e),
        };
        let rows = match raw_rows(&orders_raw) {
            Ok(rows) => rows,
            Err((index, detail)) => return basket_error(ctx, index, &detail),
        };
        if rows.is_empty() {
            ctx.error("Basket order failed: No valid orders");
            return NodeOutcome::error("No valid orders to place");
        }
        let mut orders = Vec::with_capacity(rows.len());
        for row in &rows {
            match r.row(row, &common) {
                Ok(o) => orders.push(o),
                Err(e) => return basket_error(ctx, row.index, &e),
            }
        }
        ctx.info(format!(
            "Placing basket order '{}' with {} orders",
            basket_name,
            orders.len()
        ));
        for (i, o) in orders.iter().enumerate() {
            let field = |k: &str| pyfmt::str(&o[k]);
            ctx.info(format!(
                "  Order {}: {} {} {} qty={}",
                i + 1,
                field("symbol"),
                field("exchange"),
                field("action"),
                field("quantity")
            ));
        }
        let result = ctx.client.basket_order(&orders, &basket_name).await;
        finish(ctx, data, "Basket order result", result)
    }
}

#[cfg(test)]
mod tests;
