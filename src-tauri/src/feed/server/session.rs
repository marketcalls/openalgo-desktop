//! One client's protocol state and its handlers (web
//! `websocket_proxy/server.py`): authenticate, subscribe, unsubscribe, the
//! order-update stream and broker info, with the web's Python-shaped
//! validation.

use super::{Shared, AUTH_TIMEOUT_CODE};
use crate::feed::auth::{AuthOutcome, FEED_LOCKED};
use crate::feed::dialect::subscribe_depth;
use crate::feed::outbox::Outbox;
use crate::feed::protocol::{self as p, code, py_str, py_truthy, py_type_name, request_id};
use crate::feed::registry::{Admission, Answer, ClientId, Release};
use crate::feed::source::{InstrumentKey, MarketDataSource, Mode, SourceOutcome, DEFAULT_DEPTH};
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// What a subscribe item answers when the source has not taken it in time.
const NOT_TAKEN_IN_TIME: &str = "The live market data feed did not take this subscription in \
time. Subscribe again in a moment.";

/// What an unsubscribe item answers when the broker has not confirmed the
/// release in time; the client keeps the subscription (audit C01).
const NOT_RELEASED_IN_TIME: &str = "The broker has not confirmed yet that it stopped streaming \
this instrument. Unsubscribe again to retry.";

/// The source's answer, or `late` once `limit` has passed.
async fn within(
    limit: Duration,
    answer: impl Future<Output = SourceOutcome>,
    late: &str,
) -> SourceOutcome {
    tokio::time::timeout(limit, answer)
        .await
        .unwrap_or_else(|_| Err(late.to_string()))
}

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
    /// Releases answered after their reply deadline: each settles the
    /// client's ownership when the broker's answer comes (review R107-02).
    /// Aborted with the session, which `remove_client` follows.
    pub(super) late: parking_lot::Mutex<tokio::task::JoinSet<()>>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The depth a subscribe asks for (`depth`, else `depth_level`): the
/// default when neither is given, `None` when it is not a whole number.
/// The web's proxy checks no level: it hands any value to the broker's
/// adapter, whose rules ([`subscribe_depth`]) decide the level streamed and
/// the one acknowledged (7 streams the largest level up to it, 0 the
/// smallest), and the desktop takes any whole number the same way
/// (docs-8). Some web adapters echo a value that is not a whole number and
/// the others fail on it; the desktop refuses it.
fn requested_depth(data: &Value) -> Option<i64> {
    let depth_v = match data.get("depth") {
        Some(v) if !v.is_null() => Some(v),
        _ => data.get("depth_level").filter(|v| !v.is_null()),
    };
    match depth_v {
        None => Some(DEFAULT_DEPTH as i64),
        Some(v) => v.as_i64(),
    }
}

/// What a subscribe whose depth is not a whole number is told.
const DEPTH_NOT_WHOLE: &str = "Depth must be a whole number, such as 5, 20, 30 or 50";

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

/// One instrument's unsubscribe, as the acknowledgement lists it: done, or
/// why the broker still streams it, under its canonical mode either way
/// (web `unsubscribe_client`).
fn unsubscribed(
    sym: Value,
    exch: Value,
    mode: Mode,
    broker: &str,
    outcome: SourceOutcome,
) -> p::UnsubscribeItem {
    let (status, message) = match outcome {
        Ok(()) => ("success", None),
        Err(why) => ("error", Some(why)),
    };
    p::UnsubscribeItem {
        symbol: sym,
        exchange: exch,
        mode: Some(mode.label()),
        status,
        message,
        broker: broker.to_string(),
    }
}

/// An unsubscribe item whose release is decided or still with the source.
struct Unsubscribing {
    sym: Value,
    exch: Value,
    key: InstrumentKey,
    mode: Mode,
    release: Release,
}

/// A subscribe item: decided, or waiting for the source to take it.
enum Subscribing {
    Answered(p::SubscribeItem),
    Waiting {
        sym: Value,
        exch: Value,
        key: InstrumentKey,
        streamed: u8,
        named: Option<i64>,
        admission: Admission,
    },
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
            Some("subscribe") => self.subscribe(&data).await,
            Some("unsubscribe") | Some("unsubscribe_all") => self.unsubscribe(&data).await,
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
                self.shared.registry.source().resume();
                self.send(p::auth_ack(&broker, &user_id));
                self.user_id = Some(user_id);
                self.broker = Some(broker);
            }
        }
    }

    /// Holds one instrument: the depth streamed, the depth its
    /// acknowledgment names (see [`subscribe_depth`]; `None` is `null`)
    /// and the source's answer to wait on, or why not. A feed known not to
    /// stream refuses first; then what the broker's web adapter refuses, in
    /// its order and words: a depth or mode, an exchange (WS-01,
    /// openalgo#2341) and an unknown symbol (audit C02, bk-b RB-13).
    fn subscribe_key(
        &self,
        source: &Arc<dyn MarketDataSource>,
        key: &InstrumentKey,
        mode: Mode,
        requested_depth: i64,
    ) -> Result<(u8, Option<i64>, Admission), String> {
        use crate::feed::refusals;
        if let Some(reason) = source.unavailable() {
            return Err(reason);
        }
        let broker = self.broker.as_deref().unwrap_or_default();
        if let Some(reason) = refusals::refusal(broker, key, mode, requested_depth) {
            return Err(reason);
        }
        // In the order the broker's web adapter checks (bk-b RB-13): the
        // mode and depth, then the exchange and the symbol, or the symbol
        // and the exchange.
        let exchange_first = refusals::exchange_before_lookup(broker);
        let exchange = || source.refusal(&key.exchange);
        if let Some(reason) = exchange_first.then(exchange).flatten() {
            return Err(reason);
        }
        if !source.resolve(key) {
            return Err(refusals::not_found(broker, key));
        }
        if let Some(reason) = (!exchange_first).then(exchange).flatten() {
            return Err(reason);
        }
        let supported = source.supported_depths(&key.exchange);
        match subscribe_depth(broker, mode, requested_depth, &supported) {
            Some((streamed, named)) => self
                .shared
                .registry
                .subscribe(self.id, key, mode, streamed)
                .map(|admission| (streamed, named, admission)),
            None => Err(format!(
                "Depth level {} is not supported by this broker",
                requested_depth
            )),
        }
    }

    async fn subscribe(&mut self, data: &Value) -> Result<(), String> {
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
            self.error(code::INVALID_PARAMETERS, DEPTH_NOT_WHOLE, rid);
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
        let mut pending = Vec::with_capacity(items.len());
        for item in &items {
            if !item.is_object() {
                // The items before it stay subscribed, and stream.
                let held = Self::held_keys(&pending);
                self.shared.registry.acknowledged(self.id, mode, &held);
                return Err(not_a_dict(item));
            }
            let sym = item.get("symbol").unwrap_or(&Value::Null);
            let exch = item.get("exchange").unwrap_or(&Value::Null);
            if !py_truthy(sym) || !py_truthy(exch) {
                continue;
            }
            let key = InstrumentKey::new(py_str(sym), py_str(exch));
            pending.push(
                match self.subscribe_key(&source, &key, mode, requested_depth) {
                    Ok((streamed, named, admission)) => Subscribing::Waiting {
                        sym: sym.clone(),
                        exch: exch.clone(),
                        key,
                        streamed,
                        named,
                        admission,
                    },
                    Err(message) => Subscribing::Answered(p::SubscribeItem::Err {
                        symbol: sym.clone(),
                        exchange: exch.clone(),
                        status: "error",
                        message,
                        broker: broker.clone(),
                    }),
                },
            );
        }
        let held = Self::held_keys(&pending);
        let results = self.admitted(pending, mode, &broker).await;
        let all_ok = results
            .iter()
            .all(|r| matches!(r, p::SubscribeItem::Ok { .. }));
        self.send(p::to_json(&p::SubscribeAck {
            kind: "subscribe",
            status: if all_ok { "success" } else { "partial" },
            subscriptions: results,
            message: "Subscription processing complete",
            broker: &broker,
            request_id: rid,
        }));
        // Their prices only now: a client never hears an item's market data
        // before its subscribe reply, as on the web.
        self.shared.registry.acknowledged(self.id, mode, &held);
        Ok(())
    }

    /// The instruments a subscribe holds while it waits for the source.
    fn held_keys(pending: &[Subscribing]) -> Vec<InstrumentKey> {
        pending
            .iter()
            .filter_map(|p| match p {
                Subscribing::Waiting { key, .. } => Some(key.clone()),
                Subscribing::Answered(_) => None,
            })
            .collect()
    }

    /// Each held item once the source answered it (audit C02): taken is a
    /// success, refused or not answered in time lets go of it and is that
    /// item's error. The items are answered together and listed in order.
    async fn admitted(
        &self,
        pending: Vec<Subscribing>,
        mode: Mode,
        broker: &str,
    ) -> Vec<p::SubscribeItem> {
        let limit = self.shared.cfg.op_timeout;
        let answers = futures_util::future::join_all(pending.into_iter().map(|item| async move {
            match item {
                Subscribing::Answered(r) => (None, r),
                Subscribing::Waiting {
                    sym,
                    exch,
                    key,
                    streamed,
                    named,
                    admission,
                } => {
                    let replacing = admission.replacing();
                    let outcome = within(limit, admission.answer(), NOT_TAKEN_IN_TIME).await;
                    let settle = (key, streamed, replacing, outcome.clone());
                    let r = match outcome {
                        Ok(()) => p::SubscribeItem::Ok {
                            symbol: sym,
                            exchange: exch,
                            status: "success",
                            mode: mode.label(),
                            depth: named,
                            broker: broker.to_string(),
                        },
                        Err(message) => p::SubscribeItem::Err {
                            symbol: sym,
                            exchange: exch,
                            status: "error",
                            message,
                            broker: broker.to_string(),
                        },
                    };
                    (Some(settle), r)
                }
            }
        }))
        .await;
        let registry = &self.shared.registry;
        answers
            .into_iter()
            .map(|(settle, r)| {
                if let Some((key, depth, replacing, outcome)) = settle {
                    registry.settle_subscribe(self.id, &key, mode, depth, replacing, &outcome);
                }
                r
            })
            .collect()
    }

    /// Unsubscribes the listed instruments, each under its own mode or the
    /// request's: those done, and those whose mode was invalid.
    fn unsubscribe_items(
        &self,
        items: &[Value],
        top_mode: &Value,
        broker: &str,
    ) -> Result<(Vec<Unsubscribing>, Vec<p::UnsubscribeItem>), String> {
        let mut releasing = Vec::new();
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
            let release = self.shared.registry.begin_unsubscribe(self.id, &key, mode);
            releasing.push(Unsubscribing {
                sym,
                exch,
                key,
                mode,
                release,
            });
        }
        Ok((releasing, failed))
    }

    /// Each release once the broker answered it (audit C01): the client
    /// lets go of what the broker released and keeps what it did not, and
    /// each item says which, in order.
    async fn released(
        &self,
        releasing: Vec<Unsubscribing>,
        broker: &str,
    ) -> (Vec<p::UnsubscribeItem>, Vec<p::UnsubscribeItem>) {
        let limit = self.shared.cfg.op_timeout;
        let answers = futures_util::future::join_all(releasing.into_iter().map(|u| async move {
            let outcome = match u.release {
                Release::Done => None,
                Release::Waiting { answer, ticket } => {
                    let answered = tokio::time::timeout(limit, answer.clone()).await;
                    Some((ticket, answer, answered.ok()))
                }
            };
            (u.sym, u.exch, u.key, u.mode, outcome)
        }))
        .await;
        let (mut ok, mut failed) = (Vec::new(), Vec::new());
        for (sym, exch, key, mode, waited) in answers {
            let outcome = match waited {
                None => Ok(()),
                Some((ticket, _, Some(outcome))) => {
                    self.shared
                        .registry
                        .finish_unsubscribe(self.id, &key, mode, ticket, &outcome);
                    outcome
                }
                // The reply deadline is not the broker's answer: the
                // release stays one operation, settled when that comes.
                Some((ticket, answer, None)) => {
                    self.finish_late(key.clone(), mode, ticket, answer);
                    Err(NOT_RELEASED_IN_TIME.to_string())
                }
            };
            let item = unsubscribed(sym, exch, mode, broker, outcome);
            if item.message.is_some() {
                failed.push(item);
            } else {
                ok.push(item);
            }
        }
        (ok, failed)
    }

    /// Settle release `ticket` of `key` at `mode` when the broker answers
    /// after the reply went out (review R107-02).
    fn finish_late(&self, key: InstrumentKey, mode: Mode, ticket: u64, answer: Answer) {
        let (shared, id) = (self.shared.clone(), self.id);
        let mut late = self.late.lock();
        while late.try_join_next().is_some() {}
        late.spawn(async move {
            let outcome = answer.await;
            shared
                .registry
                .finish_unsubscribe(id, &key, mode, ticket, &outcome);
        });
    }

    async fn unsubscribe(&mut self, data: &Value) -> Result<(), String> {
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

        let (releasing, mut failed) = if is_all {
            let releasing = self
                .shared
                .registry
                .begin_unsubscribe_all(self.id)
                .into_iter()
                .map(|(key, mode, release)| Unsubscribing {
                    sym: Value::String(key.symbol.clone()),
                    exch: Value::String(key.exchange.clone()),
                    key,
                    mode,
                    release,
                })
                .collect();
            (releasing, Vec::new())
        } else {
            self.unsubscribe_items(&items, top_mode, &broker)?
        };
        let (ok, refused) = self.released(releasing, &broker).await;
        failed.extend(refused);
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
