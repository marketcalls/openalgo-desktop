//! Sandbox events on the crate's event bus (`crate::events`), with the web's
//! topics: `order.update` on every order status transition,
//! `sandbox.order_filled`, `sandbox.auto_squareoff`, `sandbox.t1_settlement`,
//! `gtt.triggered` and `gtt.expired`.
//!
//! Events are collected in an [`Outbox`] while a transaction runs and
//! published only after it commits, so a rolled-back change never announces
//! itself.

use super::db::OrderRow;
use super::types::{float, money, OrderStatus};
use crate::events::{Event, EventBus, GttKind, Mode, OrderMeta, OrderUpdate};
use rust_decimal::Decimal;
use serde_json::{json, Value};

/// Events waiting for a commit.
#[derive(Debug, Default)]
pub struct Outbox {
    events: Vec<Event>,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, e: Event) {
        self.events.push(e);
    }

    pub fn extend(&mut self, other: Outbox) {
        self.events.extend(other.events);
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Publish everything (after the commit).
    pub fn publish(self, bus: Option<&EventBus>) {
        if let Some(bus) = bus {
            for e in self.events {
                bus.publish(e);
            }
        }
    }

    /// The collected events (tests).
    pub fn into_events(self) -> Vec<Event> {
        self.events
    }
}

fn meta(api_type: &str, request: Value, response: Value) -> OrderMeta {
    OrderMeta {
        mode: Mode::Analyze,
        api_type: api_type.to_string(),
        request_data: request,
        response_data: response,
    }
}

/// `order.update` for an order transition (web `OrderUpdateEvent`,
/// broker `"sandbox"`).
pub fn order_update(order: &OrderRow, status: OrderStatus, rejection_reason: &str) -> Event {
    let (filled, pending, avg) = match status {
        OrderStatus::Complete => (
            order.quantity,
            0,
            float(order.average_price.unwrap_or(Decimal::ZERO)),
        ),
        _ => (0, 0, 0.0),
    };
    Event::OrderUpdate(OrderUpdate {
        mode: "analyze".to_string(),
        broker: "sandbox".to_string(),
        orderid: order.orderid.clone(),
        symbol: order.symbol.clone(),
        exchange: order.exchange.clone(),
        action: order.action.as_str().to_string(),
        quantity: order.quantity,
        price: money(order.price.unwrap_or(Decimal::ZERO)),
        trigger_price: money(order.trigger_price.unwrap_or(Decimal::ZERO)),
        pricetype: order.price_type.as_str().to_string(),
        product: order.product.as_str().to_string(),
        order_status: status.as_str().to_string(),
        filled_quantity: filled,
        pending_quantity: pending,
        average_price: avg,
        rejection_reason: rejection_reason.to_string(),
        session_generation: 0,
    })
}

/// The pair the web publishes on a fill: `sandbox.order_filled` and an
/// `order.update` with status `complete` (price = fill price).
pub fn fill_events(order: &OrderRow, tradeid: &str, price: Decimal) -> [Event; 2] {
    let filled = Event::SandboxOrderFilled {
        meta: meta(
            "sandbox.fill",
            json!({}),
            json!({
                "orderid": order.orderid, "tradeid": tradeid, "symbol": order.symbol,
                "exchange": order.exchange, "action": order.action.as_str(),
                "quantity": order.quantity, "price": float(price),
                "product": order.product.as_str(), "strategy": order.strategy.clone().unwrap_or_default(),
            }),
        ),
    };
    let update = Event::OrderUpdate(OrderUpdate {
        mode: "analyze".to_string(),
        broker: "sandbox".to_string(),
        orderid: order.orderid.clone(),
        symbol: order.symbol.clone(),
        exchange: order.exchange.clone(),
        action: order.action.as_str().to_string(),
        quantity: order.quantity,
        price: float(price),
        trigger_price: money(order.trigger_price.unwrap_or(Decimal::ZERO)),
        pricetype: order.price_type.as_str().to_string(),
        product: order.product.as_str().to_string(),
        order_status: "complete".to_string(),
        filled_quantity: order.quantity,
        pending_quantity: 0,
        average_price: float(price),
        rejection_reason: String::new(),
        session_generation: 0,
    });
    [filled, update]
}

/// `sandbox.auto_squareoff` after a sweep changed something.
pub fn auto_squareoff(cancelled_orders: usize, closed_positions: usize) -> Event {
    Event::SandboxAutoSquareoff {
        meta: meta(
            "sandbox.auto_squareoff",
            json!({}),
            json!({"cancelled_orders": cancelled_orders, "closed_positions": closed_positions}),
        ),
    }
}

/// `sandbox.t1_settlement` after CNC positions moved to holdings.
pub fn t1_settlement(settled_users: usize, settled_positions: usize) -> Event {
    Event::SandboxT1Settlement {
        meta: meta(
            "sandbox.t1_settlement",
            json!({}),
            json!({"settled_users": settled_users, "settled_positions": settled_positions}),
        ),
    }
}

/// `gtt.triggered`, with the web's request/response payloads.
pub fn gtt_triggered(
    trigger_id: &str,
    symbol: &str,
    exchange: &str,
    strategy: &str,
    trigger_prices: &[f64],
    orderid: &str,
) -> Event {
    Event::Gtt {
        kind: GttKind::Triggered,
        meta: meta(
            "gtttriggered",
            json!({
                "api_type": "gtttriggered", "trigger_id": trigger_id, "symbol": symbol,
                "exchange": exchange, "strategy": strategy, "trigger_prices": trigger_prices,
            }),
            json!({"status": "success", "mode": "analyze", "trigger_id": trigger_id, "orderid": orderid}),
        ),
        symbol: symbol.to_string(),
        exchange: exchange.to_string(),
        trigger_id: trigger_id.to_string(),
        triggered_order_id: orderid.to_string(),
    }
}

/// `gtt.expired`, with the web's request/response payloads.
pub fn gtt_expired(trigger_id: &str, symbol: &str, exchange: &str, strategy: &str) -> Event {
    Event::Gtt {
        kind: GttKind::Expired,
        meta: meta(
            "gttexpired",
            json!({
                "api_type": "gttexpired", "trigger_id": trigger_id, "symbol": symbol,
                "exchange": exchange, "strategy": strategy,
            }),
            json!({
                "status": "success", "mode": "analyze", "trigger_id": trigger_id,
                "message": "GTT expired without firing",
            }),
        ),
        symbol: symbol.to_string(),
        exchange: exchange.to_string(),
        trigger_id: trigger_id.to_string(),
        triggered_order_id: String::new(),
    }
}
