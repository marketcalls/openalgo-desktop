//! One client's protocol state and its handlers (web
//! `websocket_proxy/server.py`): authenticate, subscribe, unsubscribe, the
//! order-update stream and broker info, with the web's Python-shaped
//! validation.

use super::{Shared, AUTH_TIMEOUT_CODE};
use crate::feed::auth::{AuthOutcome, FEED_LOCKED};
use crate::feed::outbox::Outbox;
use crate::feed::protocol::{self as p, code, py_str, py_truthy, py_type_name, request_id};
use crate::feed::registry::ClientId;
use crate::feed::source::{InstrumentKey, MarketDataSource, Mode, DEFAULT_DEPTH};
use serde_json::Value;
use std::sync::Arc;

/// One client's protocol state.
pub(super) struct Session {
    pub(super) id: ClientId,
    /// Who is calling, for the per-caller guard and failure budget
    /// ([`handshake_source`](super::handshake_source)).
    pub(super) caller: std::net::IpAddr,
    /// A close the connection loop sends after the frames already queued
    /// (an `authenticate` refused once the caller's budget is spent).
    pub(super) close: Option<(u16, &'static str)>,
    /// The last failed `authenticate` came from a caller whose failure
    /// budget was already spent.
    pub(super) closing_on_failure: bool,
    pub(super) shared: Arc<Shared>,
    pub(super) outbox: Arc<Outbox>,
    pub(super) user_id: Option<String>,
    pub(super) broker: Option<String>,
    /// What the client may still send before it is closed (RATE-01).
    pub(super) meter: super::budget::Meter,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The depth a subscribe asks for (`depth`, else `depth_level`): the
/// default when neither is given, `None` when it is not a positive whole
/// number.
fn requested_depth(data: &Value) -> Option<i64> {
    let depth_v = match data.get("depth") {
        Some(v) if !v.is_null() => Some(v),
        _ => data.get("depth_level").filter(|v| !v.is_null()),
    };
    match depth_v {
        None => Some(DEFAULT_DEPTH as i64),
        Some(v) => match v.as_i64() {
            Some(d) if !v.is_f64() && d > 0 => Some(d),
            _ => None,
        },
    }
}

/// The instruments a subscribe lists: `symbols`, else the single
/// `symbol` and `exchange` (Python iteration over `symbols`).
fn subscribe_items(data: &Value) -> Result<Vec<Value>, String> {
    let symbols_v = data.get("symbols").unwrap_or(&Value::Null);
    let mut items: Vec<Value> = if py_truthy(symbols_v) {
        py_iter(symbols_v)?
    } else {
        Vec::new()
    };
    if items.is_empty() {
        let s = data.get("symbol").unwrap_or(&Value::Null);
        let e = data.get("exchange").unwrap_or(&Value::Null);
        if py_truthy(s) && py_truthy(e) {
            items.push(serde_json::json!({"symbol": s, "exchange": e}));
        }
    }
    Ok(items)
}

/// One instrument unsubscribed, as the acknowledgement lists it.
fn unsubscribed(sym: Value, exch: Value, mode: Mode, broker: &str) -> p::UnsubscribeItem {
    p::UnsubscribeItem {
        symbol: sym,
        exchange: exch,
        mode: Some(mode.label()),
        status: "success",
        message: None,
        broker: broker.to_string(),
    }
}

/// Python `a or b` on two optional JSON fields.
fn or_field<'a>(data: &'a Value, a: &str, b: &str) -> &'a Value {
    match data.get(a) {
        Some(v) if py_truthy(v) => v,
        _ => data.get(b).unwrap_or(&Value::Null),
    }
}

/// What Python raises when a handler calls `.get` on, or iterates, a
/// non-dict element.
fn not_a_dict(v: &Value) -> String {
    format!("'{}' object has no attribute 'get'", py_type_name(v))
}

/// Python `for x in value` over a truthy non-list: the items (or the error).
fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err(format!("'{}' object is not iterable", py_type_name(other))),
    }
}

impl Session {
    fn send(&self, frame: String) {
        self.outbox.push_control(frame);
    }

    pub(super) fn error(&self, code: &str, message: &str, rid: Option<&Value>) {
        self.send(p::error_frame(code, message, rid));
    }

    pub(super) async fn handle(&mut self, text: &str) {
        let data: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return self.error(code::INVALID_JSON, "Invalid JSON message", None),
        };
        if !data.is_object() {
            return self.error(code::SERVER_ERROR, &not_a_dict(&data), None);
        }
        let action = or_field(&data, "action", "type");
        let result = match action.as_str() {
            Some("authenticate") | Some("auth") => {
                self.authenticate(&data).await;
                Ok(())
            }
            Some("subscribe") => self.subscribe(&data),
            Some("unsubscribe") | Some("unsubscribe_all") => self.unsubscribe(&data),
            Some("subscribe_orders") => {
                self.orders(&data, true);
                Ok(())
            }
            Some("unsubscribe_orders") => {
                self.orders(&data, false);
                Ok(())
            }
            Some("get_broker_info") => {
                self.broker_info();
                Ok(())
            }
            Some("get_supported_brokers") => {
                self.send(p::to_json(&p::SupportedBrokers {
                    kind: "supported_brokers",
                    status: "success",
                    brokers: &self.shared.brokers,
                    count: self.shared.brokers.len(),
                }));
                Ok(())
            }
            Some("ping") => {
                self.send(p::pong(&data, now_ms()));
                Ok(())
            }
            _ => {
                let msg = format!("Invalid action: {}", py_str(action));
                self.error(code::INVALID_ACTION, &msg, None);
                Ok(())
            }
        };
        if let Err(msg) = result {
            self.error(code::SERVER_ERROR, &msg, None);
        }
    }

    async fn authenticate(&mut self, data: &Value) {
        let key = or_field(data, "api_key", "apikey");
        if !py_truthy(key) {
            return self.error(code::AUTHENTICATION_ERROR, "API key is required", None);
        }
        let auth = &self.shared.auth;
        let outcome = match key.as_str() {
            // Every `authenticate` passes the resource guard, then the key is
            // checked (a cheap lookup). A failed key is counted against the
            // caller; when the caller was over the guard or its budget is
            // spent, the connection is closed after the answer (security
            // review S-02). A valid key is never refused because of others.
            Some(k) => {
                let within_guard = auth.admit(self.caller);
                let outcome = auth.authenticate(k).await;
                if outcome == AuthOutcome::Invalid {
                    let spent = auth.spent(self.caller);
                    if !spent {
                        auth.failed(self.caller);
                    }
                    self.closing_on_failure = spent || !within_guard;
                }
                outcome
            }
            None => AuthOutcome::Invalid,
        };
        match outcome {
            AuthOutcome::Invalid => {
                // The web's error frame. A caller whose failure budget is
                // already spent is then closed (4401, as an unauthenticated
                // connection is), so each further guess costs it a new
                // connection; a valid key was checked above and is never
                // refused by the budget (security review S-02).
                self.error(code::AUTHENTICATION_ERROR, "Invalid API key", None);
                if self.closing_on_failure {
                    self.close = Some((AUTH_TIMEOUT_CODE, "Invalid API key"));
                }
            }
            AuthOutcome::Locked => self.error(code::AUTHENTICATION_ERROR, FEED_LOCKED, None),
            AuthOutcome::NoBroker { user_id } => {
                // The web records the user before the broker check, so the
                // socket counts as authenticated from here on.
                self.user_id = Some(user_id);
                self.broker = None;
                self.error(
                    code::BROKER_ERROR,
                    "No broker configuration found for user",
                    None,
                );
            }
            AuthOutcome::Ok { user_id, broker } => {
                self.shared
                    .registry
                    .set_identity(self.id, &user_id, &broker);
                self.send(p::auth_ack(&broker, &user_id));
                self.user_id = Some(user_id);
                self.broker = Some(broker);
            }
        }
    }

    /// Subscribes one instrument: the depth it got (the largest the broker
    /// supports up to the one asked for, in depth mode), or why not. A
    /// feed known not to stream refuses first, as the web's adapter does
    /// before it looks the symbol up (WS-01).
    fn subscribe_key(
        &self,
        source: &Arc<dyn MarketDataSource>,
        key: &InstrumentKey,
        mode: Mode,
        requested_depth: i64,
    ) -> Result<i64, String> {
        if let Some(reason) = source.unavailable() {
            Err(reason)
        } else if !source.resolve(key) {
            Err(format!(
                "Token not found for {} on {}",
                key.symbol, key.exchange
            ))
        } else if mode == Mode::Depth {
            let supported = source.supported_depths(&key.exchange);
            let actual = if supported.iter().any(|d| *d as i64 == requested_depth) {
                Some(requested_depth as u8)
            } else {
                supported
                    .iter()
                    .copied()
                    .filter(|d| (*d as i64) <= requested_depth)
                    .max()
            };
            match actual {
                Some(d) => self
                    .shared
                    .registry
                    .subscribe(self.id, key, mode, d)
                    .map(|_| d as i64),
                None => Err(format!(
                    "Depth level {} is not supported by this broker",
                    requested_depth
                )),
            }
        } else {
            self.shared
                .registry
                .subscribe(self.id, key, mode, DEFAULT_DEPTH)
                .map(|_| requested_depth)
        }
    }

    fn subscribe(&mut self, data: &Value) -> Result<(), String> {
        let rid = request_id(data);
        if self.user_id.is_none() {
            self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
            return Ok(());
        }
        let default_mode = Value::String("Quote".into());
        let raw_mode = data.get("mode").unwrap_or(&default_mode);
        let mode = match p::normalize_mode(raw_mode) {
            Ok(m) => m,
            Err(e) => {
                self.error(code::INVALID_MODE, &e, rid);
                return Ok(());
            }
        };
        let Some(requested_depth) = requested_depth(data) else {
            self.error(
                code::INVALID_PARAMETERS,
                "Depth must be one of 5, 20, 30 or 50",
                rid,
            );
            return Ok(());
        };
        let items = subscribe_items(data)?;
        if items.is_empty() {
            self.error(
                code::INVALID_PARAMETERS,
                "At least one symbol must be specified",
                rid,
            );
            return Ok(());
        }
        let Some(broker) = self.broker.clone() else {
            self.error(code::BROKER_ERROR, "Broker adapter not found", rid);
            return Ok(());
        };

        let source = self.shared.registry.source().clone();
        let mut results = Vec::with_capacity(items.len());
        let mut all_ok = true;
        for item in &items {
            if !item.is_object() {
                return Err(not_a_dict(item));
            }
            let sym = item.get("symbol").unwrap_or(&Value::Null);
            let exch = item.get("exchange").unwrap_or(&Value::Null);
            if !py_truthy(sym) || !py_truthy(exch) {
                continue;
            }
            let key = InstrumentKey::new(py_str(sym), py_str(exch));
            let outcome = self.subscribe_key(&source, &key, mode, requested_depth);
            results.push(match outcome {
                Ok(depth) => p::SubscribeItem::Ok {
                    symbol: sym.clone(),
                    exchange: exch.clone(),
                    status: "success",
                    mode: mode.label(),
                    depth,
                    broker: broker.clone(),
                },
                Err(message) => {
                    all_ok = false;
                    p::SubscribeItem::Err {
                        symbol: sym.clone(),
                        exchange: exch.clone(),
                        status: "error",
                        message,
                        broker: broker.clone(),
                    }
                }
            });
        }
        self.send(p::to_json(&p::SubscribeAck {
            kind: "subscribe",
            status: if all_ok { "success" } else { "partial" },
            subscriptions: results,
            message: "Subscription processing complete",
            broker: &broker,
            request_id: rid,
        }));
        Ok(())
    }

    /// Unsubscribes the listed instruments, each under its own mode or the
    /// request's: those done, and those whose mode was invalid.
    fn unsubscribe_items(
        &self,
        items: &[Value],
        top_mode: &Value,
        broker: &str,
    ) -> Result<(Vec<p::UnsubscribeItem>, Vec<p::UnsubscribeItem>), String> {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for it in items {
            let raw_mode = match it {
                Value::Object(o) => o.get("mode").unwrap_or(top_mode),
                Value::Array(_) | Value::String(_) => top_mode,
                other => {
                    return Err(format!(
                        "argument of type '{}' is not iterable",
                        py_type_name(other)
                    ))
                }
            };
            if !it.is_object() {
                // `"mode" in item` worked on a str/list; `.get` does not.
                return Err(not_a_dict(it));
            }
            let sym = it.get("symbol").cloned().unwrap_or(Value::Null);
            let exch = it.get("exchange").cloned().unwrap_or(Value::Null);
            let mode = match p::normalize_mode(raw_mode) {
                Ok(m) => m,
                Err(e) => {
                    failed.push(p::UnsubscribeItem {
                        symbol: sym,
                        exchange: exch,
                        mode: None,
                        status: "error",
                        message: Some(e),
                        broker: broker.to_string(),
                    });
                    continue;
                }
            };
            if !py_truthy(&sym) || !py_truthy(&exch) {
                continue;
            }
            let key = InstrumentKey::new(py_str(&sym), py_str(&exch));
            self.shared.registry.unsubscribe(self.id, &key, mode);
            ok.push(unsubscribed(sym, exch, mode, broker));
        }
        Ok((ok, failed))
    }

    fn unsubscribe(&mut self, data: &Value) -> Result<(), String> {
        let rid = request_id(data);
        if self.user_id.is_none() {
            self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
            return Ok(());
        }
        let is_all = data.get("type").and_then(Value::as_str) == Some("unsubscribe_all")
            || data.get("action").and_then(Value::as_str) == Some("unsubscribe_all");
        let symbols_v = data.get("symbols").unwrap_or(&Value::Null);
        let mut items: Vec<Value> = if py_truthy(symbols_v) {
            py_iter(symbols_v)?
        } else {
            Vec::new()
        };
        let default_mode = Value::from(2);
        let top_mode = data.get("mode").unwrap_or(&default_mode);
        if items.is_empty() && !is_all {
            let s = data.get("symbol").unwrap_or(&Value::Null);
            let e = data.get("exchange").unwrap_or(&Value::Null);
            if py_truthy(s) && py_truthy(e) {
                let m = match p::normalize_mode(top_mode) {
                    Ok(m) => m,
                    Err(err) => {
                        self.error(code::INVALID_MODE, &err, rid);
                        return Ok(());
                    }
                };
                items.push(serde_json::json!({"symbol": s, "exchange": e, "mode": m.as_u8()}));
            }
        }
        if items.is_empty() && !is_all {
            self.error(
                code::INVALID_PARAMETERS,
                "Either symbols or unsubscribe_all is required",
                rid,
            );
            return Ok(());
        }
        let Some(broker) = self.broker.clone() else {
            self.error(code::BROKER_ERROR, "Broker adapter not found", rid);
            return Ok(());
        };

        let (ok, failed) = if is_all {
            let mut ok = Vec::new();
            for (key, mode) in self.shared.registry.unsubscribe_all(self.id) {
                let sym = Value::String(key.symbol.clone());
                let exch = Value::String(key.exchange.clone());
                ok.push(unsubscribed(sym, exch, mode, &broker));
            }
            (ok, Vec::new())
        } else {
            self.unsubscribe_items(&items, top_mode, &broker)?
        };
        let status = match (ok.is_empty(), failed.is_empty()) {
            (_, true) => "success",
            (false, false) => "partial",
            (true, false) => "error",
        };
        self.send(p::to_json(&p::UnsubscribeAck {
            kind: "unsubscribe",
            status,
            message: "Unsubscription processing complete",
            successful: ok,
            failed,
            broker: &broker,
            request_id: rid,
        }));
        Ok(())
    }

    fn orders(&mut self, data: &Value, on: bool) {
        let rid = request_id(data);
        if self.user_id.is_none() {
            return self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
        }
        self.shared.registry.set_orders(self.id, on);
        self.send(p::to_json(&p::SimpleAck {
            kind: if on {
                "subscribe_orders"
            } else {
                "unsubscribe_orders"
            },
            status: "success",
            message: if on {
                "Subscribed to order updates"
            } else {
                "Unsubscribed from order updates"
            },
            request_id: rid,
        }));
    }

    fn broker_info(&self) {
        if self.user_id.is_none() {
            return self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, None);
        }
        let Some(broker) = self.broker.clone() else {
            return self.error(code::BROKER_ERROR, "Broker information not available", None);
        };
        let user = self.user_id.clone().unwrap_or_default();
        self.send(p::to_json(&p::BrokerInfo {
            kind: "broker_info",
            status: "success",
            broker: &broker,
            adapter_status: self.shared.auth.adapter_status(),
            user_id: &user,
        }));
    }
}
