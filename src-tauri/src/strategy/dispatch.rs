//! The single place a strategy run turns a decision into an order (web
//! `services/strategy_module/order_dispatch.py`).
//!
//! Departures from how the rest of the product places orders, all deliberate:
//!
//! * **Mode is per run, not global.** A sandbox run goes to the sandbox
//!   engine whatever the analyzer toggle says; a live run goes to the broker
//!   with `force_live`, so an operator switching analyzer mode on while a live
//!   run holds real positions cannot divert its exits into the sandbox (which
//!   reports success and would leave the real position with nothing managing
//!   it).
//! * **Action Center is bypassed** (`internal`): a stop-loss exit that waits
//!   for a human to approve it is not a stop loss.
//! * **The product is translated to the venue**: MIS is intraday everywhere;
//!   anything else means carry, NRML on a derivatives venue and CNC on cash.
//! * **Entries and exits are MARKET.**
//! * **Broker authorisation is resolved per order**, never cached in run
//!   state; when it is missing the order is refused and reported.

use crate::brokers::common::outcome::PlaceOutcome;
use crate::events::{Event, Mode};
use crate::services::core::{broker_handle, meta, safe_request, Reply};
use crate::services::order_service::{place_order, sandbox_order, Route};
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Weak;

/// Exits are always MARKET: a stop that cannot fill is not a stop.
pub const EXIT_PRICETYPE: &str = "MARKET";

/// Venues that list derivatives, for naming the product.
pub const DERIVATIVE_EXCHANGES_FOR_PRODUCT: &[&str] =
    &["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO"];

/// A run's destination, fixed when the run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Live,
    Sandbox,
}

impl RunMode {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "live" => Some(RunMode::Live),
            "sandbox" => Some(RunMode::Sandbox),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RunMode::Live => "live",
            RunMode::Sandbox => "sandbox",
        }
    }
}

/// The venue's spelling of the product the strategy asked for.
pub fn product_for_exchange(product: &str, exchange: &str) -> String {
    if product.eq_ignore_ascii_case("MIS") {
        return "MIS".into();
    }
    if DERIVATIVE_EXCHANGES_FOR_PRODUCT.contains(&exchange.to_ascii_uppercase().as_str()) {
        "NRML".into()
    } else {
        "CNC".into()
    }
}

/// One order payload, in the shape the placement services expect.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderPayload {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub product: String,
    pub pricetype: String,
    pub strategy: String,
}

impl OrderPayload {
    /// The `/api/v1/placeorder` body this order is.
    pub fn to_request(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "exchange": self.exchange,
            "action": self.action,
            "quantity": self.quantity,
            "product": self.product,
            "pricetype": self.pricetype,
            "price": 0,
            "trigger_price": 0,
            "disclosed_quantity": 0,
            "strategy": self.strategy,
        })
    }
}

pub fn build_order(
    symbol: &str,
    exchange: &str,
    action: &str,
    quantity: i64,
    product: &str,
    strategy_name: &str,
    pricetype: &str,
) -> OrderPayload {
    OrderPayload {
        symbol: symbol.to_string(),
        exchange: exchange.to_string(),
        action: action.to_ascii_uppercase(),
        quantity,
        product: product_for_exchange(product, exchange),
        pricetype: if pricetype.is_empty() {
            "MARKET".into()
        } else {
            pricetype.to_string()
        },
        strategy: strategy_name.to_string(),
    }
}

/// The action that closes a leg, from the side it actually HOLDS. The
/// original read the configured side, which defaulted to `B` for every leg,
/// so an exit on a short placed another SELL and doubled the position.
pub fn exit_action(position: &str) -> Result<&'static str, String> {
    match position.trim().to_ascii_uppercase().as_str() {
        "B" => Ok("SELL"),
        "S" => Ok("BUY"),
        _ => Err(format!(
            "Cannot derive an exit action from position {:?}",
            position
        )),
    }
}

/// The action that opens a leg held on `position`.
pub fn entry_action(position: &str) -> &'static str {
    if position.eq_ignore_ascii_case("B") {
        "BUY"
    } else {
        "SELL"
    }
}

/// What one placement attempt produced. `ok` is whether the order reached
/// the broker or the sandbox and was acknowledged, not whether it filled.
///
/// Three outcomes, never two (LOG-08): accepted (`ok`), refused
/// ([`DispatchResult::is_refused`]) and uncertain (`uncertain`: the order
/// may be at the broker although no acknowledgement came back). An
/// uncertain placement keeps its claim; it is never placed again until the
/// order reconciler has found it in the broker's order book, or confirmed
/// by repeated reads that it is not there.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DispatchResult {
    pub ok: bool,
    pub broker_order_id: Option<String>,
    pub response: Value,
    pub error: Option<String>,
    /// The broker may hold this order: neither accepted nor refused.
    pub uncertain: bool,
    /// The client tag the order was sent with, when the adapter tags orders.
    pub client_tag: Option<String>,
}

impl DispatchResult {
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Default::default()
        }
    }

    /// The order was not placed: safe to release its claim and decide again.
    pub fn is_refused(&self) -> bool {
        !self.ok && !self.uncertain
    }

    /// The dispatch result for a service reply, carrying its placement
    /// outcome.
    pub fn from_reply(reply: &Reply) -> Self {
        let orderid = reply.body.get("orderid").and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        if reply.is_success() {
            Self {
                ok: true,
                broker_order_id: orderid,
                response: reply.body.clone(),
                ..Default::default()
            }
        } else {
            let msg = reply.message();
            let client_tag = match &reply.placement {
                Some(PlaceOutcome::Uncertain { client_tag, .. }) => client_tag.clone(),
                _ => None,
            };
            Self {
                ok: false,
                broker_order_id: orderid,
                response: reply.body.clone(),
                error: Some(if msg.is_empty() {
                    "Order rejected".to_string()
                } else {
                    msg
                }),
                uncertain: reply.is_uncertain(),
                client_tag,
            }
        }
    }
}

/// One order-book row as the order reconciler reads it: one read per pass
/// per destination, shared by every owner of open orders.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BookOrder {
    pub order_id: String,
    pub symbol: String,
    pub exchange: String,
    /// `BUY` / `SELL`.
    pub action: String,
    pub quantity: i64,
    pub product: String,
    /// Lowercase status.
    pub status: String,
    pub filled_quantity: i64,
    pub average_price: f64,
    /// The client tag the order was sent with, where the broker reports it.
    pub client_tag: Option<String>,
    /// The row in the shape `order_status` returns, for the fill fold.
    pub row: Value,
}

impl BookOrder {
    /// A row from a service order book (`order_row` keys plus fills).
    pub fn from_row(row: &Value) -> Option<Self> {
        let s = |k: &str| {
            row.get(k)
                .map(|v| match v {
                    Value::String(s) => s.trim().to_string(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                })
                .unwrap_or_default()
        };
        let n = |k: &str| row.get(k).and_then(crate::risk::value_to_f64).unwrap_or(0.0);
        let order_id = s("orderid");
        if order_id.is_empty() {
            return None;
        }
        Some(Self {
            order_id,
            symbol: s("symbol"),
            exchange: s("exchange").to_ascii_uppercase(),
            action: s("action").to_ascii_uppercase(),
            quantity: n("quantity") as i64,
            product: s("product").to_ascii_uppercase(),
            status: s("order_status").to_ascii_lowercase(),
            filled_quantity: n("filled_quantity") as i64,
            average_price: n("average_price"),
            client_tag: None,
            row: row.clone(),
        })
    }
}

/// One broker orderbook fact for an order, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderStatusResult {
    pub ok: bool,
    pub order: Value,
    pub error: Option<String>,
}

/// Which account book a view reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Book {
    Orders,
    Trades,
    Positions,
}

/// Everything the engine needs from the outside world to act on an order.
/// Production goes through the app's services; tests inject a fake.
#[async_trait]
pub trait OrderGateway: Send + Sync {
    /// Place one order through the run's own pipe.
    async fn place(&self, mode: RunMode, order: &OrderPayload) -> DispatchResult;
    /// Cancel one order through the run's own pipe.
    async fn cancel(&self, mode: RunMode, broker_order_id: &str) -> DispatchResult;
    /// Read one order's status through the run's own pipe.
    async fn order_status(&self, mode: RunMode, broker_order_id: &str) -> OrderStatusResult;
    /// Whether orders can be sent for this mode right now (a live run needs a
    /// broker session; the sandbox always can).
    fn authorised(&self, mode: RunMode) -> Result<(), String>;
    /// The broker a run is bound to, snapshotted at start.
    fn broker_name(&self, mode: RunMode) -> String;
    /// One account book through the run's pipe: the service's own envelope.
    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value>;
    /// A last price for the underlying an ATM strike is measured against.
    async fn ltp(&self, symbol: &str, exchange: &str) -> Result<f64, String>;
    /// The whole order book through the run's pipe, one read, for the order
    /// reconciler. Default: the `Book::Orders` envelope, without tags.
    async fn order_book_rows(&self, mode: RunMode) -> Result<Vec<BookOrder>, String> {
        match self.book(mode, Book::Orders).await {
            Ok(body) => {
                let data = body.get("data").unwrap_or(&Value::Null);
                let rows = data
                    .get("orders")
                    .or(Some(data))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                Ok(rows.iter().filter_map(BookOrder::from_row).collect())
            }
            Err(body) => Err(body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The order book could not be read")
                .to_string()),
        }
    }
}

pub const NO_BROKER_SESSION: &str =
    "Broker session is not available or has expired. Log in to your broker to restore it.";

/// The production gateway, over the app's services.
pub struct AppGateway {
    ctx: Weak<AppState>,
}

impl AppGateway {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self { ctx }
    }
}

fn sandbox_reply<T: serde::Serialize>(r: crate::sandbox::SbResult<T>) -> Reply {
    match r {
        Ok(v) => Reply::from_ser(&v),
        Err(e) => Reply::sandbox(&e),
    }
}

#[async_trait]
impl OrderGateway for AppGateway {
    async fn place(&self, mode: RunMode, order: &OrderPayload) -> DispatchResult {
        let Some(ctx) = self.ctx.upgrade() else {
            return DispatchResult::refused("OpenAlgo is shutting down");
        };
        let req = order.to_request();
        match mode {
            RunMode::Sandbox => {
                // The sandbox pipe directly, never the global toggle.
                let reply = sandbox_reply(ctx.sandbox.place_order(sandbox_order(&req)).await);
                let orderid = reply
                    .body
                    .get("orderid")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                ctx.bus.publish(Event::OrderPlaced {
                    meta: meta(Mode::Analyze, "placeorder", safe_request(&req), &reply.body),
                    strategy: order.strategy.clone(),
                    symbol: order.symbol.clone(),
                    exchange: order.exchange.clone(),
                    action: order.action.clone(),
                    quantity: order.quantity,
                    pricetype: order.pricetype.clone(),
                    product: order.product.clone(),
                    orderid,
                });
                DispatchResult::from_reply(&reply)
            }
            RunMode::Live => {
                if let Err(e) = self.authorised(mode) {
                    return DispatchResult::refused(e);
                }
                // force_live: the run already decided this is live.
                // internal: a strategy order never waits in the Action Center.
                let reply = place_order(
                    &ctx,
                    &req,
                    Route {
                        force_live: true,
                        internal: true,
                    },
                )
                .await;
                DispatchResult::from_reply(&reply)
            }
        }
    }

    async fn cancel(&self, mode: RunMode, broker_order_id: &str) -> DispatchResult {
        let Some(ctx) = self.ctx.upgrade() else {
            return DispatchResult::refused("OpenAlgo is shutting down");
        };
        if broker_order_id.is_empty() {
            return DispatchResult::refused("Broker order id is unavailable");
        }
        match mode {
            RunMode::Sandbox => {
                let mut r = DispatchResult::from_reply(&sandbox_reply(
                    ctx.sandbox.cancel_order(broker_order_id).await,
                ));
                r.broker_order_id
                    .get_or_insert_with(|| broker_order_id.to_string());
                r
            }
            RunMode::Live => {
                let h = match broker_handle(&ctx) {
                    Ok(h) => h,
                    Err(_) => return DispatchResult::refused(NO_BROKER_SESSION),
                };
                match h.broker.cancel_order(&h.auth, broker_order_id).await {
                    Ok(_) => DispatchResult {
                        ok: true,
                        broker_order_id: Some(broker_order_id.to_string()),
                        response: json!({"status": "success", "orderid": broker_order_id}),
                        ..Default::default()
                    },
                    Err(e) => DispatchResult {
                        ok: false,
                        broker_order_id: Some(broker_order_id.to_string()),
                        error: Some(e.client_message()),
                        ..Default::default()
                    },
                }
            }
        }
    }

    async fn order_status(&self, mode: RunMode, broker_order_id: &str) -> OrderStatusResult {
        let Some(ctx) = self.ctx.upgrade() else {
            return OrderStatusResult {
                error: Some("OpenAlgo is shutting down".into()),
                ..Default::default()
            };
        };
        match mode {
            RunMode::Sandbox => match ctx.sandbox.order_status(broker_order_id).await {
                Ok(r) => OrderStatusResult {
                    ok: true,
                    order: serde_json::to_value(&r.data).unwrap_or(Value::Null),
                    error: None,
                },
                Err(e) => OrderStatusResult {
                    ok: false,
                    order: Value::Null,
                    error: Some(e.message),
                },
            },
            RunMode::Live => {
                let h = match broker_handle(&ctx) {
                    Ok(h) => h,
                    Err(_) => {
                        return OrderStatusResult {
                            error: Some(NO_BROKER_SESSION.into()),
                            ..Default::default()
                        }
                    }
                };
                let orders = match h.broker.get_order_book(&h.auth).await {
                    Ok(o) => o,
                    Err(e) => {
                        return OrderStatusResult {
                            error: Some(e.client_message()),
                            ..Default::default()
                        }
                    }
                };
                let Some(order) = orders.iter().find(|o| o.order_id == broker_order_id) else {
                    return OrderStatusResult {
                        error: Some(format!("Order {} not found", broker_order_id)),
                        ..Default::default()
                    };
                };
                let mut average_price = order.average_price;
                if average_price <= 0.0 && order.status.eq_ignore_ascii_case("complete") {
                    if let Ok(trades) = h.broker.get_trade_book(&h.auth).await {
                        if let Some(t) = trades.iter().find(|t| t.order_id == broker_order_id) {
                            average_price = t.average_price;
                        }
                    }
                }
                let mut row = crate::services::account_service::order_row(order);
                row["average_price"] = json!(average_price);
                row["filled_quantity"] = json!(order.filled_quantity);
                OrderStatusResult {
                    ok: true,
                    order: row,
                    error: None,
                }
            }
        }
    }

    fn authorised(&self, mode: RunMode) -> Result<(), String> {
        match mode {
            RunMode::Sandbox => Ok(()),
            RunMode::Live => {
                let ctx = self
                    .ctx
                    .upgrade()
                    .ok_or_else(|| "OpenAlgo is shutting down".to_string())?;
                broker_handle(&ctx)
                    .map(|_| ())
                    .map_err(|_| NO_BROKER_SESSION.to_string())
            }
        }
    }

    fn broker_name(&self, mode: RunMode) -> String {
        match mode {
            RunMode::Sandbox => "sandbox".into(),
            RunMode::Live => self
                .ctx
                .upgrade()
                .and_then(|c| c.get_broker_session())
                .map(|s| s.broker_id)
                .unwrap_or_default(),
        }
    }

    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value> {
        let Some(ctx) = self.ctx.upgrade() else {
            return Err(json!({"status": "error", "message": "OpenAlgo is shutting down"}));
        };
        use crate::services::account_service as acct;
        let reply = match mode {
            RunMode::Sandbox => match book {
                Book::Orders => sandbox_reply(ctx.sandbox.orderbook().await),
                Book::Trades => sandbox_reply(ctx.sandbox.tradebook().await),
                Book::Positions => sandbox_reply(ctx.sandbox.positionbook().await),
            },
            RunMode::Live => {
                // The live broker, whatever the analyzer toggle says.
                let h = match broker_handle(&ctx) {
                    Ok(h) => h,
                    Err(_) => return Err(json!({"status": "error", "message": NO_BROKER_SESSION})),
                };
                let fail = |e: crate::error::AppError| Reply::error(500, e.client_message());
                match book {
                    Book::Orders => match h.broker.get_order_book(&h.auth).await {
                        Ok(o) => Reply::ok(json!({"status": "success", "data": {
                            "orders": o.iter().map(acct::order_row).collect::<Vec<_>>(),
                            "statistics": acct::order_statistics(&o),
                        }})),
                        Err(e) => fail(e),
                    },
                    Book::Trades => match h.broker.get_trade_book(&h.auth).await {
                        Ok(t) => Reply::ok(json!({"status": "success",
                            "data": t.iter().map(acct::trade_row).collect::<Vec<_>>()})),
                        Err(e) => fail(e),
                    },
                    Book::Positions => match h.broker.get_positions(&h.auth).await {
                        Ok(p) => Reply::ok(json!({"status": "success",
                            "data": p.iter().map(acct::position_row).collect::<Vec<_>>()})),
                        Err(e) => fail(e),
                    },
                }
            }
        };
        if reply.is_success() {
            Ok(reply.body)
        } else {
            Err(reply.body)
        }
    }

    async fn order_book_rows(&self, mode: RunMode) -> Result<Vec<BookOrder>, String> {
        let ctx = self
            .ctx
            .upgrade()
            .ok_or_else(|| "OpenAlgo is shutting down".to_string())?;
        if mode == RunMode::Sandbox {
            let body = sandbox_reply(ctx.sandbox.orderbook().await).body;
            let rows = body["data"]["orders"].as_array().cloned().unwrap_or_default();
            return Ok(rows.iter().filter_map(BookOrder::from_row).collect());
        }
        let h = broker_handle(&ctx).map_err(|_| NO_BROKER_SESSION.to_string())?;
        let tagged = h
            .broker
            .get_order_book_tagged(&h.auth)
            .await
            .map_err(|e| e.client_message())?;
        // A complete order the book reports without a price takes it from
        // the trade book, read at most once per pass.
        let needs_trades = tagged
            .iter()
            .any(|t| t.order.average_price <= 0.0 && t.order.status.eq_ignore_ascii_case("complete"));
        let trades = if needs_trades {
            h.broker.get_trade_book(&h.auth).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(tagged
            .into_iter()
            .filter_map(|t| {
                let o = &t.order;
                let mut average_price = o.average_price;
                if average_price <= 0.0 && o.status.eq_ignore_ascii_case("complete") {
                    if let Some(tr) = trades.iter().find(|tr| tr.order_id == o.order_id) {
                        average_price = tr.average_price;
                    }
                }
                let mut row = crate::services::account_service::order_row(o);
                row["average_price"] = json!(average_price);
                row["filled_quantity"] = json!(o.filled_quantity);
                let mut b = BookOrder::from_row(&row)?;
                b.client_tag = t.client_tag;
                Some(b)
            })
            .collect())
    }

    async fn ltp(&self, symbol: &str, exchange: &str) -> Result<f64, String> {
        let ctx = self
            .ctx
            .upgrade()
            .ok_or_else(|| "OpenAlgo is shutting down".to_string())?;
        let h = broker_handle(&ctx).map_err(|_| NO_BROKER_SESSION.to_string())?;
        match crate::services::market_data_service::fetch_quote(&ctx, &h, symbol, exchange).await {
            Ok(q) if q.ltp.is_finite() && q.ltp > 0.0 => Ok(q.ltp),
            Ok(q) => Err(format!(
                "Unusable last price {} for {} on {}.",
                q.ltp, symbol, exchange
            )),
            Err(r) => Err(format!(
                "Could not fetch a price for {} on {}. {}",
                symbol,
                exchange,
                r.message()
            )),
        }
    }
}
