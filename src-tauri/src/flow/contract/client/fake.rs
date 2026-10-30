//! A [`FlowClient`] for tests: records every call and answers from a
//! script, so a node's test pins the exact payload it sends and reads the
//! exact dict the web's client would hand back.
//!
//! ```ignore
//! let client = FakeFlowClient::new();
//! client.reply("quotes", json!({"status": "success", "data": {"ltp": 99.5}}));
//! client.reply_error("funds", "Broker session expired", 401);
//! // ... run the node ...
//! assert_eq!(client.calls()[0].endpoint, "quotes");
//! ```

use super::{ApiCall, FlowClient};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};

/// What the fake answers an endpoint it has no script for.
pub const UNSCRIPTED: &str = "No reply was scripted for this call";

/// One recorded alert.
#[derive(Debug, Clone, PartialEq)]
pub struct SentAlert {
    pub channel: &'static str,
    pub message: String,
    pub to: Option<String>,
}

/// The scripted client. Replies are queued per endpoint (`placeorder`,
/// `market/holidays`); the last one of a queue repeats.
#[derive(Default)]
pub struct FakeFlowClient {
    calls: Mutex<Vec<ApiCall>>,
    replies: Mutex<HashMap<String, VecDeque<Value>>>,
    alerts: Mutex<Vec<SentAlert>>,
    alert_reply: Mutex<Option<Value>>,
}

impl FakeFlowClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `endpoint` with `reply` (already in the client's dict shape).
    pub fn reply(&self, endpoint: &str, reply: Value) -> &Self {
        self.replies
            .lock()
            .entry(endpoint.to_string())
            .or_default()
            .push_back(reply);
        self
    }

    /// Answer `endpoint` with the client's failure dict.
    pub fn reply_error(&self, endpoint: &str, message: &str, code: u16) -> &Self {
        self.reply(
            endpoint,
            json!({"status": "error", "error": message, "code": code}),
        )
    }

    /// Answer every alert with `reply` instead of success.
    pub fn alert_reply(&self, reply: Value) {
        *self.alert_reply.lock() = Some(reply);
    }

    /// Every `/api/v1` call made, in order.
    pub fn calls(&self) -> Vec<ApiCall> {
        self.calls.lock().clone()
    }

    /// The calls made to one endpoint.
    pub fn calls_to(&self, endpoint: &str) -> Vec<ApiCall> {
        self.calls
            .lock()
            .iter()
            .filter(|c| c.endpoint == endpoint)
            .cloned()
            .collect()
    }

    /// Every alert sent, in order.
    pub fn alerts(&self) -> Vec<SentAlert> {
        self.alerts.lock().clone()
    }

    fn alert(&self, channel: &'static str, message: &str, to: Option<&str>) -> Value {
        self.alerts.lock().push(SentAlert {
            channel,
            message: message.to_string(),
            to: to.map(str::to_string),
        });
        self.alert_reply.lock().clone().unwrap_or_else(
            || json!({"status": "success", "data": {"message": "Alert queued successfully"}}),
        )
    }
}

#[async_trait]
impl FlowClient for FakeFlowClient {
    async fn api(&self, call: ApiCall) -> Value {
        let endpoint = call.endpoint.to_string();
        self.calls.lock().push(call);
        let mut replies = self.replies.lock();
        match replies.get_mut(&endpoint) {
            Some(queue) if queue.len() > 1 => queue.pop_front().unwrap_or(Value::Null),
            Some(queue) => queue.front().cloned().unwrap_or(Value::Null),
            None => json!({"status": "error", "error": UNSCRIPTED, "code": 500}),
        }
    }

    async fn telegram(&self, message: &str) -> Value {
        self.alert("telegram", message, None)
    }

    async fn whatsapp(&self, message: &str, to: Option<&str>) -> Value {
        self.alert("whatsapp", message, to)
    }
}

#[cfg(test)]
mod tests {
    use super::super::PlaceOrder;
    use super::*;

    #[tokio::test]
    async fn calls_are_recorded_and_replies_follow_the_script() {
        let c = FakeFlowClient::new();
        c.reply("placeorder", json!({"status": "success", "orderid": "1"}))
            .reply("placeorder", json!({"status": "success", "orderid": "2"}));
        let o = PlaceOrder::new("SBIN", "NSE", "buy", 5);
        assert_eq!(c.place_order(&o).await["orderid"], "1");
        assert_eq!(c.place_order(&o).await["orderid"], "2");
        assert_eq!(c.place_order(&o).await["orderid"], "2");
        let sent = &c.calls_to("placeorder")[0];
        assert_eq!(sent.method, "place_order");
        assert_eq!(sent.payload["action"], "BUY");
        assert_eq!(sent.payload["pricetype"], "MARKET");
        assert_eq!(sent.payload["product"], "MIS");
        assert_eq!(sent.payload["strategy"], "flow_workflow");
        assert_eq!(c.funds().await["error"], UNSCRIPTED);
    }

    #[tokio::test]
    async fn an_open_position_is_read_from_the_position_book() {
        let c = FakeFlowClient::new();
        c.reply(
            "positionbook",
            json!({"status": "success", "data": [
                {"symbol": "SBIN", "exchange": "NSE", "product": "CNC", "quantity": 5, "pnl": 1.0},
                {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": -3, "pnl": 2.5}
            ]}),
        );
        let mis = c.get_open_position("SBIN", "NSE", Some("MIS")).await;
        assert_eq!(
            mis,
            json!({"status": "success", "quantity": -3, "pnl": 2.5})
        );
        let any = c.get_open_position("SBIN", "NSE", None).await;
        assert_eq!(any["quantity"], 5);
        let none = c.get_open_position("TCS", "NSE", None).await;
        assert_eq!(none, json!({"status": "success", "quantity": 0, "pnl": 0}));
        let fake2 = FakeFlowClient::new();
        fake2.reply_error("positionbook", "Broker session expired", 401);
        assert_eq!(
            fake2.get_open_position("SBIN", "NSE", None).await["status"],
            "error"
        );
    }
}
