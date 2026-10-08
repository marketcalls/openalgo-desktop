//! Socket.IO subscriber (web `subscribers/socketio_subscriber.py`). Event
//! names and payloads are copied from the web so the carried-over frontend
//! hooks (`useSocket`, `useOrderEventRefresh`) work unchanged.

use crate::events::{Event, OrderMeta, SessionEndReason, Subscriber, Topic};
use serde_json::{json, Value};
use std::sync::Arc;

/// Where Socket.IO events go. Production wraps `socketioxide::SocketIo`;
/// tests record.
#[async_trait::async_trait]
pub trait UiEmitter: Send + Sync {
    async fn emit(&self, event: &str, payload: Value);
}

/// Production emitter. Set once the HTTP server (and its Socket.IO layer)
/// exists; until then events are dropped, which only happens at startup.
#[derive(Default)]
pub struct SocketEmitter {
    io: parking_lot::RwLock<Option<socketioxide::SocketIo>>,
}

impl SocketEmitter {
    pub fn set(&self, io: Option<socketioxide::SocketIo>) {
        *self.io.write() = io;
    }

    /// The Socket.IO handle, for room-addressed pushes (strategy rooms).
    pub fn io(&self) -> Option<socketioxide::SocketIo> {
        self.io.read().clone()
    }
}

#[async_trait::async_trait]
impl UiEmitter for SocketEmitter {
    async fn emit(&self, event: &str, payload: Value) {
        let io = self.io.read().clone();
        if let Some(io) = io {
            if let Err(e) = io.emit(event.to_string(), &payload).await {
                tracing::debug!("Socket.IO emit '{}' failed: {}", event, e);
            }
        }
    }
}

pub struct SocketIoSubscriber {
    ui: Arc<dyn UiEmitter>,
}

impl SocketIoSubscriber {
    pub fn new(ui: Arc<dyn UiEmitter>) -> Self {
        Self { ui }
    }
}

fn analyzer_update(meta: &OrderMeta) -> Value {
    json!({"request": meta.request_data, "response": meta.response_data})
}

fn is_analyze(meta: &OrderMeta) -> bool {
    meta.mode == crate::events::Mode::Analyze
}

/// The Socket.IO event (name, payload) the web emits for `event`, if any.
pub fn translate(event: &Event) -> Option<(&'static str, Value)> {
    if let Some(meta) = event.meta() {
        let always_analyzer = matches!(
            event,
            Event::SandboxOrderFilled { .. }
                | Event::SandboxAutoSquareoff { .. }
                | Event::SandboxT1Settlement { .. }
        );
        if is_analyze(meta) || always_analyzer {
            return Some(("analyzer_update", analyzer_update(meta)));
        }
    }
    match event {
        Event::OrderPlaced {
            symbol,
            action,
            orderid,
            exchange,
            pricetype,
            product,
            ..
        } => Some((
            "order_event",
            json!({
                "symbol": symbol, "action": action, "orderid": orderid, "exchange": exchange,
                "price_type": pricetype, "product_type": product, "mode": "live",
            }),
        )),
        Event::OrderNoAction {
            symbol, message, ..
        } => Some((
            "order_notification",
            json!({"symbol": symbol, "status": "info", "message": message}),
        )),
        Event::OrderModified { orderid, .. } => Some((
            "modify_order_event",
            json!({"status": "success", "orderid": orderid, "mode": "live"}),
        )),
        Event::OrderCancelled {
            orderid, status, ..
        } => Some((
            "cancel_order_event",
            json!({"status": status, "orderid": orderid, "mode": "live"}),
        )),
        Event::AllOrdersCancelled {
            canceled_count,
            failed_count,
            ..
        } => Some((
            "cancel_order_event",
            json!({
                "status": "success",
                "orderid": format!("{} orders canceled", canceled_count),
                "mode": "live", "batch_order": true, "is_last_order": true,
                "canceled_count": canceled_count, "failed_count": failed_count,
            }),
        )),
        Event::PositionClosed { message, .. } => Some((
            "close_position_event",
            json!({
                "status": "success",
                "message": message.clone().filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "All Open Positions Squared Off".to_string()),
                "mode": "live",
            }),
        )),
        Event::BasketCompleted {
            strategy,
            successful,
            total,
            ..
        } => Some((
            "order_event",
            json!({
                "symbol": strategy.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "Basket".into()),
                "action": format!("{}/{} orders", successful, total),
                "orderid": format!("basket_{}", successful),
                "exchange": "MULTI", "price_type": "BASKET", "product_type": "BASKET",
                "mode": "live", "batch_order": true, "is_last_order": true,
            }),
        )),
        Event::SplitCompleted {
            symbol,
            action,
            exchange,
            pricetype,
            product,
            successful,
            total,
            ..
        } => Some((
            "order_event",
            json!({
                "symbol": symbol.clone().unwrap_or_else(|| "Split".into()),
                "action": action.clone().unwrap_or_else(|| "SPLIT".into()),
                "orderid": format!("{}/{} orders", successful, total),
                "exchange": exchange.clone().unwrap_or_else(|| "Unknown".into()),
                "price_type": pricetype.clone().unwrap_or_else(|| "MARKET".into()),
                "product_type": product.clone().unwrap_or_else(|| "MIS".into()),
                "mode": "live", "batch_order": true, "is_last_order": true,
            }),
        )),
        Event::OptionsCompleted {
            symbol,
            action,
            exchange,
            pricetype,
            product,
            successful,
            total,
            ..
        } => Some((
            "order_event",
            json!({
                "symbol": symbol, "action": action,
                "orderid": format!("{}/{} orders", successful, total),
                "exchange": exchange,
                "price_type": pricetype.clone().unwrap_or_else(|| "MARKET".into()),
                "product_type": product.clone().unwrap_or_else(|| "MIS".into()),
                "mode": "live", "batch_order": true, "is_last_order": true,
            }),
        )),
        Event::MultiOrderCompleted {
            underlying,
            strategy,
            exchange,
            successful_legs,
            failed_legs,
            total,
            ..
        } => Some((
            "order_event",
            json!({
                "symbol": underlying,
                "action": strategy.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "Multi-Order".into()),
                "orderid": format!("{}/{} legs", successful_legs, total),
                "exchange": exchange, "price_type": "MULTI", "product_type": "OPTIONS",
                "mode": "live", "batch_order": true, "is_last_order": true,
                "multiorder_summary": true,
                "successful_legs": successful_legs, "failed_legs": failed_legs,
            }),
        )),
        Event::Gtt {
            kind,
            symbol,
            exchange,
            trigger_id,
            triggered_order_id,
            ..
        } => Some((
            "gtt_event",
            json!({
                "event": kind.name(), "symbol": symbol, "exchange": exchange,
                "trigger_id": trigger_id, "triggered_order_id": triggered_order_id, "mode": "live",
            }),
        )),
        Event::OrderUpdate(u) => Some((
            "order_update",
            json!({
                "mode": u.mode, "broker": u.broker, "orderid": u.orderid, "symbol": u.symbol,
                "exchange": u.exchange, "action": u.action, "quantity": u.quantity,
                "price": u.price, "trigger_price": u.trigger_price, "pricetype": u.pricetype,
                "product": u.product, "order_status": u.order_status,
                "filled_quantity": u.filled_quantity, "pending_quantity": u.pending_quantity,
                "average_price": u.average_price, "rejection_reason": u.rejection_reason,
            }),
        )),
        // The web's broker modules emit `{status, message}` only.
        Event::MasterContractDownload {
            status, message, ..
        } => Some((
            "master_contract_download",
            json!({"status": status, "message": message}),
        )),
        Event::CacheLoaded { payload } => Some(("cache_loaded", payload.clone())),
        Event::ForceLogout { message } => Some(("force_logout", json!({"message": message}))),
        Event::PendingOrderCreated { payload } => Some(("pending_order_created", payload.clone())),
        Event::PendingOrderUpdated { payload } => Some(("pending_order_updated", payload.clone())),
        Event::Historify { event, payload } => Some((event, payload.clone())),
        Event::BrokerConnected { .. } => Some((
            "active_sessions_update",
            json!({"count": 1, "sessions": []}),
        )),
        Event::BrokerSessionEnded { reason } => match reason {
            SessionEndReason::DailyExpiry => Some((
                "force_logout",
                json!({"message": "Your broker session ended for the day. Sign in again to continue trading."}),
            )),
            SessionEndReason::Logout => Some((
                "active_sessions_update",
                json!({"count": 0, "sessions": []}),
            )),
            SessionEndReason::PasswordChanged => None,
        },
        // Live failures have no Socket.IO event on the web.
        _ => None,
    }
}

#[async_trait::async_trait]
impl Subscriber for SocketIoSubscriber {
    fn name(&self) -> &'static str {
        "socketio"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderPlaced,
            Topic::OrderFailed,
            Topic::OrderNoAction,
            Topic::OrderModified,
            Topic::OrderModifyFailed,
            Topic::OrderCancelled,
            Topic::OrderCancelFailed,
            Topic::OrderUpdate,
            Topic::AllOrdersCancelled,
            Topic::PositionClosed,
            Topic::BasketCompleted,
            Topic::SplitCompleted,
            Topic::OptionsCompleted,
            Topic::MultiOrderCompleted,
            Topic::AnalyzerError,
            Topic::SandboxOrderFilled,
            Topic::SandboxAutoSquareoff,
            Topic::SandboxT1Settlement,
            Topic::GttPlaced,
            Topic::GttFailed,
            Topic::GttModified,
            Topic::GttModifyFailed,
            Topic::GttCancelled,
            Topic::GttCancelFailed,
            Topic::GttTriggered,
            Topic::GttExpired,
            Topic::BrokerConnected,
            Topic::BrokerSessionEnded,
            Topic::MasterContractDownload,
            Topic::CacheLoaded,
            Topic::ForceLogout,
            Topic::PendingOrderCreated,
            Topic::PendingOrderUpdated,
            Topic::Historify,
        ]
    }

    async fn handle(&self, event: Arc<Event>) {
        if let Some((name, payload)) = translate(&event) {
            self.ui.emit(name, payload).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{GttKind, Mode};

    fn meta(mode: Mode) -> OrderMeta {
        OrderMeta {
            mode,
            api_type: "placeorder".into(),
            request_data: json!({"symbol": "SBIN"}),
            response_data: json!({"status": "success"}),
        }
    }

    #[test]
    fn order_placed_live_matches_web_payload() {
        let e = Event::OrderPlaced {
            meta: meta(Mode::Live),
            strategy: "s".into(),
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            quantity: 1,
            pricetype: "MARKET".into(),
            product: "MIS".into(),
            orderid: "123".into(),
        };
        let (name, p) = translate(&e).unwrap();
        assert_eq!(name, "order_event");
        assert_eq!(
            p,
            json!({"symbol":"SBIN","action":"BUY","orderid":"123","exchange":"NSE",
                   "price_type":"MARKET","product_type":"MIS","mode":"live"})
        );
    }

    #[test]
    fn analyze_mode_becomes_analyzer_update() {
        let e = Event::OrderCancelled {
            meta: meta(Mode::Analyze),
            orderid: "1".into(),
            status: "success".into(),
        };
        let (name, p) = translate(&e).unwrap();
        assert_eq!(name, "analyzer_update");
        assert_eq!(p["request"]["symbol"], "SBIN");
    }

    #[test]
    fn live_failures_emit_nothing() {
        let e = Event::OrderFailed {
            meta: meta(Mode::Live),
            symbol: "X".into(),
            exchange: "NSE".into(),
            error_message: "e".into(),
        };
        assert!(translate(&e).is_none());
    }

    #[test]
    fn all_cancelled_and_gtt() {
        let e = Event::AllOrdersCancelled {
            meta: meta(Mode::Live),
            canceled_count: 3,
            failed_count: 1,
        };
        let (n, p) = translate(&e).unwrap();
        assert_eq!(n, "cancel_order_event");
        assert_eq!(p["orderid"], "3 orders canceled");
        assert_eq!(p["batch_order"], true);
        let g = Event::Gtt {
            kind: GttKind::Triggered,
            meta: meta(Mode::Live),
            symbol: "S".into(),
            exchange: "NSE".into(),
            trigger_id: "t".into(),
            triggered_order_id: "o".into(),
        };
        let (n, p) = translate(&g).unwrap();
        assert_eq!(n, "gtt_event");
        assert_eq!(p["event"], "triggered");
    }

    #[test]
    fn sandbox_fill_is_always_analyzer_update() {
        let e = Event::SandboxOrderFilled {
            meta: meta(Mode::Live),
        };
        assert_eq!(translate(&e).unwrap().0, "analyzer_update");
    }
}
