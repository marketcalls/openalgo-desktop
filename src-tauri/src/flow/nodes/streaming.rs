//! The streaming nodes: subscribeLtp, subscribeQuote, subscribeDepth and
//! unsubscribe (the web's `execute_subscribe_*`, `execute_unsubscribe` and
//! `_get_websocket_data`, `flow_executor_service.py` 3757-4320).
//!
//! A subscribe node asks the feed for the first frame of its mode (5 s for
//! LTP and Quote, 3 s for Depth), falls back to the REST quote or depth when
//! none comes, and keeps the subscription for the workflow: it is recorded
//! against the run's workflow and given back when the workflow is
//! deactivated or deleted, or by an Unsubscribe node.
//!
//! Departures from the web, each decided (decisions.md 9):
//!
//! - The output variable holds the documented value (the LTP, the quote
//!   fields, the depth book) under the trimmed name, as the import-format
//!   doc and the flow-builder skill promise. The web overwrote it with the
//!   whole result dict, so `{{ltp}}` printed a dict (N-subscribeLtp-05,
//!   N-subscribeQuote-04, N-subscribeDepth-04). The run log is unchanged.
//! - `unsubscribe all` gives back this workflow's own subscriptions, never
//!   the feed's others (`/trading`, the sandbox, the SDK clients); a named
//!   symbol drops this workflow's claim, and the feed only when no other
//!   workflow holds it (N-unsubscribe-03, N-unsubscribe-05). The refusal
//!   of a mode with no symbol says so, instead of the web's warning that
//!   `all` clears the sandbox's subscriptions too.
//! - A feed that fails during an unsubscribe is reported as a failure, not
//!   as success (N-unsubscribe-08).

use super::NodeFamily;
use crate::flow::contract::env::{FeedError, MarketFeed};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::subscriptions::{Entry, FeedRelease};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "streaming",
    build: || {
        vec![
            Arc::new(Subscribe(&LTP)),
            Arc::new(Subscribe(&QUOTE)),
            Arc::new(Subscribe(&DEPTH)),
            Arc::new(Unsubscribe),
        ]
    },
};

/// One subscribe node's words and defaults.
struct Stream {
    node_type: &'static str,
    /// The feed mode, as recorded (`LTP`, `Quote`, `Depth`).
    mode: &'static str,
    /// The frame's numeric mode.
    mode_number: f64,
    /// The default output variable.
    output: &'static str,
    /// How long the first frame is waited for.
    timeout: Duration,
    /// `"ltp"`, `"quote"`, `"depth"`: the result's `type`.
    kind: &'static str,
    /// What the REST fallback says when it failed without a reason.
    rest_failed: &'static str,
}

static LTP: Stream = Stream {
    node_type: "subscribeLtp",
    mode: "LTP",
    mode_number: 1.0,
    output: "ltp",
    timeout: Duration::from_secs(5),
    kind: "ltp",
    rest_failed: "Failed to get LTP",
};

static QUOTE: Stream = Stream {
    node_type: "subscribeQuote",
    mode: "Quote",
    mode_number: 2.0,
    output: "quote",
    timeout: Duration::from_secs(5),
    kind: "quote",
    rest_failed: "Failed to get quote",
};

static DEPTH: Stream = Stream {
    node_type: "subscribeDepth",
    mode: "Depth",
    mode_number: 3.0,
    output: "depth",
    timeout: Duration::from_secs(3),
    kind: "depth",
    rest_failed: "Failed to get depth",
};

/// `d.get(key)` as Python reads it: the value, or `None` when absent.
fn get<'a>(d: &'a Value, key: &str) -> Option<&'a Value> {
    d.get(key)
}

/// `d.get(key, default)`: present (even `None`) wins over the default.
fn get_or(d: &Value, key: &str, default: Value) -> Value {
    d.get(key).cloned().unwrap_or(default)
}

/// `a or b` on two Python values.
fn or(a: Option<&Value>, b: Value) -> Value {
    match a {
        Some(v) if pyfmt::truthy(v) => v.clone(),
        _ => b,
    }
}

/// The frame's `data` when it is an object, else `{}`.
fn nested(frame: &Value) -> Value {
    match frame.get("data") {
        Some(d @ Value::Object(_)) => d.clone(),
        _ => json!({}),
    }
}

/// `nested.ltp if nested.ltp is not None else frame.get("ltp", 0)`.
fn frame_ltp(frame: &Value, inner: &Value) -> Value {
    match get(inner, "ltp") {
        Some(v) if !v.is_null() => v.clone(),
        _ => get_or(frame, "ltp", json!(0)),
    }
}

/// The book sides of a depth frame (top-level or nested, the Fyers
/// `depth.buy` / `depth.sell` form too), as the web's frame check reads them.
fn has_book(frame: &Value) -> bool {
    let inner = nested(frame);
    let mut bids = or(get(frame, "bids"), or(get(&inner, "bids"), json!([])));
    let mut asks = or(get(frame, "asks"), or(get(&inner, "asks"), json!([])));
    let book = or(get(frame, "depth"), get_or(&inner, "depth", json!({})));
    if book.is_object() {
        bids = or(Some(&bids), get_or(&book, "buy", json!([])));
        asks = or(Some(&asks), get_or(&book, "sell", json!([])));
    }
    pyfmt::truthy(&bids) || pyfmt::truthy(&asks)
}

/// The run's stored value under the node's output variable: the trimmed
/// `outputVariable`, else the stream's default name (decisions.md 9).
fn store_documented(ctx: &NodeCtx, data: &Map<String, Value>, default: &str, value: Value) {
    match data.get("outputVariable").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => {
            ctx.vars.set(name.trim(), value);
            ctx.info(format!("Stored result in variable: {}", name));
        }
        _ => ctx.vars.set(default, value),
    }
}

/// The first frame of `stream` from the feed, recording the subscription
/// it opened; `None` (with the web's warning) when the feed did not give
/// one, so the REST fallback runs.
async fn websocket_data(
    ctx: &NodeCtx,
    stream: &Stream,
    symbol: &str,
    exchange: &str,
) -> Option<Value> {
    let (want_symbol, want_exchange) = (symbol.to_string(), exchange.to_string());
    let (number, depth) = (stream.mode_number, stream.mode == "Depth");
    let accept = move |frame: &Value| {
        frame.get("symbol").and_then(Value::as_str) == Some(want_symbol.as_str())
            && frame.get("exchange").and_then(Value::as_str) == Some(want_exchange.as_str())
            && frame.get("mode").and_then(Value::as_f64) == Some(number)
            && (!depth || has_book(frame))
    };
    let got = ctx
        .env
        .feed
        .first_frame(symbol, exchange, stream.mode, stream.timeout, &accept)
        .await;
    match got {
        Ok(frame) => {
            ctx.subscriptions
                .record(ctx.workflow_id, symbol, exchange, stream.mode);
            frame
        }
        Err(e) => {
            let line = match e {
                FeedError::InvalidKey => "WebSocket: Invalid API key".to_string(),
                FeedError::Connect(m) => format!("WebSocket connection failed: {}", m),
                FeedError::Subscribe(m) => format!("WebSocket subscribe failed: {}", m),
                FeedError::Failed(m) => format!("WebSocket error: {}", m),
            };
            ctx.log(line, WARNING);
            None
        }
    }
}

/// What a stream node keeps from a frame (websocket) or a REST reply.
fn frame_value(stream: &Stream, frame: &Value) -> Value {
    let inner = nested(frame);
    match stream.kind {
        "ltp" => frame_ltp(frame, &inner),
        "quote" => {
            let pick = |k: &str| or(get(&inner, k), get_or(frame, k, json!(0)));
            json!({
                "ltp": frame_ltp(frame, &inner),
                "open": pick("open"),
                "high": pick("high"),
                "low": pick("low"),
                "close": or(get(&inner, "close"), or(get(&inner, "prev_close"),
                    get_or(frame, "close", get_or(frame, "prev_close", json!(0))))),
                "volume": pick("volume"),
                "prev_close": pick("prev_close"),
            })
        }
        _ => frame_depth(frame, &inner),
    }
}

/// A Fyers-form side as `[{price, quantity}]` when its first level is an
/// object with a price.
fn fyers_side(side: Value) -> Value {
    let Value::Array(levels) = &side else {
        return side;
    };
    if !levels.first().is_some_and(|l| l.get("price").is_some()) {
        return side;
    }
    Value::Array(
        levels
            .iter()
            .map(|l| {
                json!({
                    "price": get_or(l, "price", json!(0)),
                    "quantity": get_or(l, "quantity", get_or(l, "volume", json!(0))),
                })
            })
            .collect(),
    )
}

fn frame_depth(frame: &Value, inner: &Value) -> Value {
    let book = or(get(frame, "depth"), get_or(inner, "depth", json!({})));
    let fyers = book.is_object()
        && (pyfmt::truthy(&get_or(&book, "buy", Value::Null))
            || pyfmt::truthy(&get_or(&book, "sell", Value::Null)));
    let (bids, asks) = if fyers {
        (
            fyers_side(get_or(&book, "buy", json!([]))),
            fyers_side(get_or(&book, "sell", json!([]))),
        )
    } else {
        (
            or(get(inner, "bids"), get_or(frame, "bids", json!([]))),
            or(get(inner, "asks"), get_or(frame, "asks", json!([]))),
        )
    };
    json!({
        "bids": bids,
        "asks": asks,
        "totalbuyqty": or(get(inner, "totalbuyqty"), get_or(frame, "totalbuyqty", json!(0))),
        "totalsellqty": or(get(inner, "totalsellqty"), get_or(frame, "totalsellqty", json!(0))),
        "ltp": frame_ltp(frame, inner),
    })
}

/// The REST reply's `data` as the stream node keeps it.
fn rest_value(stream: &Stream, data: &Value) -> Value {
    let present = pyfmt::truthy(data);
    match stream.kind {
        "ltp" if present => get_or(data, "ltp", json!(0)),
        "ltp" => json!(0),
        "quote" if present => json!({
            "ltp": get_or(data, "ltp", json!(0)),
            "open": get_or(data, "open", json!(0)),
            "high": get_or(data, "high", json!(0)),
            "low": get_or(data, "low", json!(0)),
            "close": get_or(data, "close", get_or(data, "prev_close", json!(0))),
            "volume": get_or(data, "volume", json!(0)),
            "prev_close": get_or(data, "prev_close", json!(0)),
        }),
        "quote" => json!({}),
        _ if present => json!({
            "bids": get_or(data, "bids", json!([])),
            "asks": get_or(data, "asks", json!([])),
            "totalbuyqty": get_or(data, "totalbuyqty", json!(0)),
            "totalsellqty": get_or(data, "totalsellqty", json!(0)),
            "ltp": get_or(data, "ltp", json!(0)),
        }),
        _ => json!({"bids": [], "asks": []}),
    }
}

/// The run-log line for a value, as the web words it per stream.
fn value_line(stream: &Stream, symbol: &str, value: &Value, via: &str) -> String {
    match stream.kind {
        "ltp" => format!("LTP for {}: {} (via {})", symbol, pyfmt::str(value), via),
        "quote" => format!(
            "Quote for {}: LTP={} (via {})",
            symbol,
            pyfmt::str(value.get("ltp").unwrap_or(&Value::Null)),
            via
        ),
        _ => {
            let side = |k: &str| {
                value
                    .get(k)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            let (bids, asks) = (side("bids"), side("asks"));
            let top = |levels: &[Value]| {
                levels
                    .first()
                    .map(|l| get_or(l, "price", json!(0)))
                    .unwrap_or(json!(0))
            };
            format!(
                "Depth for {}: Bid={}, Ask={} ({} bids, {} asks) via {}",
                symbol,
                pyfmt::str(&top(&bids)),
                pyfmt::str(&top(&asks)),
                bids.len(),
                asks.len(),
                via
            )
        }
    }
}

/// A subscribe node.
struct Subscribe(&'static Stream);

impl Subscribe {
    /// The success result: the value and where it came from.
    fn success(&self, symbol: &str, exchange: &str, value: &Value, source: &str) -> Value {
        let s = self.0;
        let mut out =
            json!({"status": "success", "type": s.kind, "symbol": symbol, "exchange": exchange});
        let key = if s.kind == "ltp" { "ltp" } else { "data" };
        out[key] = value.clone();
        out["source"] = json!(source);
        out
    }

    /// The REST fallback (`client.get_quotes`, or `get_depth` for Depth).
    async fn rest(
        &self,
        ctx: &NodeCtx,
        data: &Map<String, Value>,
        symbol: &str,
        exchange: &str,
    ) -> NodeOutcome {
        let s = self.0;
        ctx.info(format!(
            "WebSocket timeout/failed, falling back to REST API for {}",
            symbol
        ));
        let reply = if s.kind == "depth" {
            ctx.client.get_depth(symbol, exchange).await
        } else {
            ctx.client.get_quotes(symbol, exchange).await
        };
        if reply.get("status").and_then(Value::as_str) != Some("success") {
            let error = get_or(&reply, "error", json!(s.rest_failed));
            ctx.error(format!(
                "Subscribe {} error: {}",
                s.mode,
                pyfmt::str(&error)
            ));
            return NodeOutcome::result(json!({
                "status": "error", "type": s.kind, "symbol": symbol, "exchange": exchange, "error": error,
            }));
        }
        let value = rest_value(s, &get_or(&reply, "data", json!({})));
        ctx.info(value_line(s, symbol, &value, "REST API"));
        store_documented(ctx, data, s.output, value.clone());
        NodeOutcome::result(self.success(symbol, exchange, &value, "rest_api"))
    }
}

#[async_trait]
impl FlowNode for Subscribe {
    fn node_type(&self) -> &'static str {
        self.0.node_type
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let s = self.0;
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        if symbol.is_empty() {
            ctx.error(format!("Subscribe {}: No symbol specified", s.mode));
            return NodeOutcome::error("No symbol specified");
        }
        ctx.info(format!(
            "Subscribing to {} stream: {} ({})",
            s.mode, symbol, exchange
        ));
        let Some(frame) = websocket_data(ctx, s, &symbol, &exchange).await else {
            return self.rest(ctx, data, &symbol, &exchange).await;
        };
        let value = frame_value(s, &frame);
        ctx.info(value_line(s, &symbol, &value, "WebSocket"));
        store_documented(ctx, data, s.output, value.clone());
        NodeOutcome::result(self.success(&symbol, &exchange, &value, "websocket"))
    }
}

/// [`FeedRelease`] over the run's feed, for giving back a workflow's
/// subscriptions.
pub struct ReleaseTo<'a>(pub &'a dyn MarketFeed);

#[async_trait]
impl<'a> FeedRelease for ReleaseTo<'a> {
    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> bool {
        self.0.unsubscribe(symbol, exchange, mode).await.is_ok()
    }
}

/// The unsubscribe node.
struct Unsubscribe;

/// `{"status": "error", "type": "unsubscribe", "message": message}`.
fn refused(message: impl Into<String>) -> NodeOutcome {
    NodeOutcome::result(
        json!({"status": "error", "type": "unsubscribe", "message": message.into()}),
    )
}

impl Unsubscribe {
    /// `all`: give back every subscription this workflow holds.
    async fn release_all(&self, ctx: &NodeCtx) -> Result<(), String> {
        if let Some(id) = ctx.workflow_id {
            ctx.subscriptions
                .release(id, &ReleaseTo(ctx.env.feed.as_ref()))
                .await;
        }
        ctx.info("Unsubscribed from all streams");
        match ctx.workflow_id {
            Some(id) if ctx.subscriptions.has(id) => Err("unsubscribe was rejected".into()),
            _ => Ok(()),
        }
    }

    /// One symbol and mode: drop this workflow's claim, and the feed's
    /// subscription when no other workflow holds it.
    async fn release_one(&self, ctx: &NodeCtx, entry: &Entry) -> Result<(), FeedError> {
        if !ctx.subscriptions.held_by_another(ctx.workflow_id, entry) {
            let (symbol, exchange, mode) = entry;
            ctx.env.feed.unsubscribe(symbol, exchange, mode).await?;
        }
        ctx.subscriptions.forget_entry(ctx.workflow_id, entry);
        Ok(())
    }
}

#[async_trait]
impl FlowNode for Unsubscribe {
    fn node_type(&self) -> &'static str {
        "unsubscribe"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let stream_type = ctx.get_str(data, "streamType", "all");
        let shown = if symbol.is_empty() {
            "all"
        } else {
            symbol.as_str()
        };
        ctx.info(format!(
            "Unsubscribing from {} stream: {} ({})",
            stream_type, shown, exchange
        ));
        let done = json!({
            "status": "success", "type": "unsubscribe", "symbol": symbol,
            "exchange": exchange, "stream_type": stream_type,
            "message": format!("Unsubscribed from {} stream", stream_type),
        });
        let failed = |message: String| {
            NodeOutcome::result(json!({
                "status": "error", "type": "unsubscribe", "symbol": symbol,
                "exchange": exchange, "stream_type": stream_type, "message": message,
            }))
        };
        let idle = json!({"status": "success", "type": "unsubscribe", "message": "No active WebSocket connection"});
        match ctx.env.feed.session().await {
            Ok(()) => {}
            Err(FeedError::InvalidKey) => {
                ctx.log("Unsubscribe: Invalid API key", WARNING);
                return NodeOutcome::result(idle);
            }
            Err(FeedError::Failed(e)) => {
                ctx.log(format!("Unsubscribe error: {}", e), WARNING);
                return failed(e);
            }
            Err(_) => return NodeOutcome::result(idle),
        }
        if stream_type.to_lowercase() == "all" {
            return match self.release_all(ctx).await {
                Ok(()) => NodeOutcome::result(done),
                Err(m) => {
                    ctx.error(format!("Unsubscribe failed: {}", m));
                    refused(m)
                }
            };
        }
        if symbol.is_empty() {
            let message = format!(
                "Unsubscribe is set to '{}' with no symbol. Name a symbol, or set Stream Type to 'all' to give back every subscription this workflow holds.",
                stream_type
            );
            ctx.error(format!("Unsubscribe aborted: {}", message));
            return refused(message);
        }
        let mode = match stream_type.to_lowercase().as_str() {
            "ltp" => "LTP",
            "depth" => "Depth",
            _ => "Quote",
        };
        let entry: Entry = (symbol.clone(), exchange.clone(), mode.to_string());
        match self.release_one(ctx, &entry).await {
            Ok(()) => {
                ctx.info(format!("Unsubscribed from {} for {}", stream_type, symbol));
                NodeOutcome::result(done)
            }
            Err(FeedError::Failed(e)) => {
                ctx.log(format!("Unsubscribe error: {}", e), WARNING);
                failed(e)
            }
            Err(e) => {
                ctx.info(format!("Unsubscribed from {} for {}", stream_type, symbol));
                let text = e.text();
                let message = if text.is_empty() {
                    "unsubscribe was rejected".to_string()
                } else {
                    text
                };
                ctx.error(format!("Unsubscribe failed: {}", message));
                refused(message)
            }
        }
    }
}

#[cfg(test)]
mod tests;
