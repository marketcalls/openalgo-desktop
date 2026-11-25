//! The charges of an order before it is placed, and which rate card a page
//! is shown (web `services/charges_service.py`, `blueprints/sandbox.py`
//! `_charge_market`; marketcalls/openalgo#2496).
//!
//! The figures come from the calculation that charges sandbox fills
//! (`sandbox::charges`), under the plan saved on `/sandbox/commissions`. No
//! broker is called: the answer is the same in live and analyzer mode and
//! does not depend on which broker is logged in.

use crate::brokers::catalog;
use crate::sandbox::charge_manager::OrderCharges;
use crate::sandbox::charges::{market_for_broker_type, parse_amount, Market};
use crate::services::core::Reply;
use crate::state::AppState;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

/// The market of the broker the trader is logged in to: a crypto venue is
/// shown the crypto rate card, every other broker (and no broker) the
/// Indian one. The server decides, so the pages keep no list of markets.
pub fn market(ctx: &AppState) -> Market {
    let broker = ctx.get_broker_session().map(|s| s.broker_id);
    market_for_broker_type(broker.map(|b| {
        if catalog::is_crypto(&b) {
            "crypto"
        } else {
            "IN_stock"
        }
    }))
}

/// A positive, finite number (web `float(...)` then `> 0`).
fn positive(v: Option<&Value>) -> Option<Decimal> {
    let text = match v? {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    parse_amount(&text).filter(|d| *d > Decimal::ZERO)
}

/// The request in the form the calculation takes (web
/// `validate_charges_data`), after the schema: symbol trimmed, the codes
/// upper-cased, every field present.
pub fn validate(data: &Map<String, Value>) -> Result<OrderCharges, String> {
    for name in [
        "symbol", "exchange", "product", "action", "quantity", "price",
    ] {
        match data.get(name) {
            None | Some(Value::Null) => return Err(format!("Missing mandatory field: {name}")),
            Some(Value::String(s)) if s.is_empty() => {
                return Err(format!("Missing mandatory field: {name}"))
            }
            _ => {}
        }
    }
    let text = |k: &str| match data.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let symbol = text("symbol");
    if symbol.is_empty() || symbol.chars().count() > 50 {
        return Err("Symbol must be between 1 and 50 characters".to_string());
    }
    let quantity = positive(data.get("quantity"))
        .ok_or_else(|| "Quantity must be a positive number".to_string())?;
    let price =
        positive(data.get("price")).ok_or_else(|| "Price must be a positive number".to_string())?;
    Ok(OrderCharges {
        symbol,
        exchange: text("exchange").to_uppercase(),
        product: text("product").to_uppercase(),
        action: text("action").to_uppercase(),
        quantity,
        price,
    })
}

/// A quantity as the web echoes it: whole numbers as integers.
fn quantity_json(q: Decimal) -> Value {
    use rust_decimal::prelude::ToPrimitive;
    if q.fract().is_zero() {
        if let Some(i) = q.to_i64() {
            return json!(i);
        }
    }
    json!(q.to_f64().unwrap_or(0.0))
}

/// `POST /api/v1/charges` after the schema and the key: the order echoed,
/// its segment, trade value and charges.
pub async fn calculate(ctx: &AppState, request: &Value) -> Reply {
    let empty = Map::new();
    let order = match validate(request.as_object().unwrap_or(&empty)) {
        Ok(o) => o,
        Err(m) => return Reply::error(400, m),
    };
    use rust_decimal::prelude::ToPrimitive;
    let mut out = json!({
        "symbol": order.symbol,
        "exchange": order.exchange,
        "product": order.product,
        "action": order.action,
        "quantity": quantity_json(order.quantity),
        "price": order.price.to_f64().unwrap_or(0.0),
    });
    match ctx.sandbox.order_charges(order).await {
        Ok(Value::Object(priced)) => {
            if let Value::Object(m) = &mut out {
                m.extend(priced);
            }
            Reply::ok(json!({"status": "success", "data": out}))
        }
        Ok(_) => Reply::error(500, "Failed to calculate charges due to internal error"),
        Err(e) if e.http_status == 400 => Reply::error(400, e.message),
        Err(e) => {
            tracing::error!("Could not work out the charges of an order: {}", e.message);
            Reply::error(500, "Failed to calculate charges due to internal error")
        }
    }
}
