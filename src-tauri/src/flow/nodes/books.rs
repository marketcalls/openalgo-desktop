//! The account nodes: Order Book, Trade Book, Position Book, Holdings,
//! Funds (the web's `execute_order_book` ... `execute_funds`), Strategy P&L
//! (`execute_strategy_pnl` over a port of `services/strategy_pnl_service.py`)
//! and the strategy book the app installs for it.
//!
//! The books go through `/api/v1`, so in analyzer mode they answer from the
//! sandbox, as the web's services do. Strategy P&L reads the per-strategy
//! book (`strategy/book.rs`) for realized P&L and cost, and one position
//! book read for live prices.

use crate::agent::tools::numbers::py_round;
use crate::flow::contract::client::Margin;
use crate::flow::contract::env::StrategyBook;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Which book a [`BookNode`] reads.
#[derive(Clone, Copy)]
pub enum Book {
    Orders,
    Trades,
    Positions,
    Holdings,
    Funds,
}

/// A node that reads one book and stores it whole.
pub struct BookNode(pub Book);

impl BookNode {
    fn words(&self) -> (&'static str, &'static str, &'static str) {
        match self.0 {
            Book::Orders => ("orderBook", "Fetching order book", "Order book received"),
            Book::Trades => ("tradeBook", "Fetching trade book", "Trade book received"),
            Book::Positions => (
                "positionBook",
                "Fetching position book",
                "Position book received",
            ),
            Book::Holdings => ("holdings", "Fetching holdings", "Holdings received"),
            Book::Funds => ("funds", "Fetching funds", "Funds received"),
        }
    }
}

#[async_trait]
impl FlowNode for BookNode {
    fn node_type(&self) -> &'static str {
        self.words().0
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let (_, before, after) = self.words();
        ctx.info(before);
        let result = match self.0 {
            Book::Orders => ctx.client.orderbook().await,
            Book::Trades => ctx.client.tradebook().await,
            Book::Positions => ctx.client.positionbook().await,
            Book::Holdings => ctx.client.holdings().await,
            Book::Funds => ctx.client.funds().await,
        };
        ctx.info(after);
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// Strategy P&L: realized, unrealized and total for one strategy (the
/// workflow's own name unless the node names another).
pub struct StrategyPnlNode;

#[async_trait]
impl FlowNode for StrategyPnlNode {
    fn node_type(&self) -> &'static str {
        "strategyPnl"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let named = ctx.get_str(data, "strategy", "");
        // The workflow's name: the tag its own order nodes apply.
        let strategy = if named.is_empty() {
            ctx.strategy_tag(&Map::new())
        } else {
            named
        };
        let result = strategy_pnl(ctx, &strategy).await;
        if result.get("status").and_then(Value::as_str) == Some("error") {
            ctx.error(format!(
                "Strategy P&L error: {}",
                pyfmt::str(result.get("message").unwrap_or(&Value::Null))
            ));
            return NodeOutcome::result(result);
        }
        let shown = |k: &str| pyfmt::str(result.get(k).unwrap_or(&Value::Null));
        ctx.info(format!(
            "Strategy P&L [{}]: realized={} unrealized={} total={} openQty={}",
            strategy,
            shown("realized"),
            shown("unrealized"),
            shown("total"),
            shown("open_quantity")
        ));
        let unpriced = result
            .get("unpriced_legs")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if unpriced != 0 {
            ctx.log(
                format!(
                    "{} open leg(s) had no live price and were excluded from unrealized P&L",
                    unpriced
                ),
                WARNING,
            );
        }
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// `get_strategy_pnl(client, strategy)`: the book's legs marked against one
/// position book read. An unreadable book or position book is an error, never
/// a P&L of zero an exit trigger would act on.
async fn strategy_pnl(ctx: &NodeCtx, strategy: &str) -> Value {
    let book = ctx.env.strategy_book.clone();
    let wanted = strategy.to_string();
    let legs = match tokio::task::spawn_blocking(move || book.legs(&wanted)).await {
        Ok(Ok(legs)) => legs,
        Ok(Err(e)) => {
            tracing::error!("Strategy P&L unavailable: {}", e);
            return json!({"status": "error", "message": format!("Strategy book unavailable: {}", e)});
        }
        Err(e) => {
            tracing::error!("Strategy P&L unavailable: {}", e);
            return json!({"status": "error", "message": "Strategy book unavailable: Could not read the strategy book"});
        }
    };
    let positions = ctx.client.positionbook().await;
    if positions.get("status").and_then(Value::as_str) == Some("error") {
        let message = ["error", "message"]
            .iter()
            .filter_map(|k| positions.get(*k))
            .find(|v| pyfmt::truthy(v))
            .map(pyfmt::str)
            .unwrap_or_else(|| "unavailable".into());
        return json!({
            "status": "error",
            "message": format!("Position book unavailable, cannot value open legs: {}", message),
        });
    }
    let rows = match positions.get("data") {
        Some(Value::Array(rows)) => rows.as_slice(),
        _ => &[],
    };
    let mut out = Map::new();
    out.insert("status".into(), json!("success"));
    out.extend(pnl_from_book(&legs, rows, strategy));
    Value::Object(out)
}

/// `_f`: Python's `float(value)`, the default for what it refuses.
fn as_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => pyfmt::parse_float(s).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn r4(x: f64) -> f64 {
    py_round(x, 4)
}

/// One leg's identity in the position book: symbol, exchange, product.
fn leg_key(v: &Value) -> [Value; 3] {
    ["symbol", "exchange", "product"].map(|k| v.get(k).cloned().unwrap_or(Value::Null))
}

/// `pnl_from_book(legs, positions, strategy)` for one strategy: realized
/// from the book, unrealized marked at the position book's `ltp` (then
/// `last_price`), open legs first so `{{pnl.legs[0]}}` is an open leg.
pub fn pnl_from_book(legs: &[Value], positions: &[Value], strategy: &str) -> Map<String, Value> {
    let ltp: Vec<([Value; 3], f64)> = positions
        .iter()
        .map(|p| {
            let price = p
                .get("ltp")
                .filter(|v| !v.is_null())
                .or_else(|| p.get("last_price"));
            (leg_key(p), as_f64(price))
        })
        .collect();
    // The last position of a key wins, as a dict built from the list.
    let price_of = |key: &[Value; 3]| ltp.iter().rev().find(|(k, _)| k == key).map(|(_, p)| *p);
    let (mut realized, mut today, mut unrealized, mut open_qty) = (0.0, 0.0, 0.0, 0.0);
    let mut unpriced = 0i64;
    let mut rows: Vec<(bool, Value)> = Vec::new();
    for leg in legs {
        let name = leg
            .get("strategy")
            .filter(|v| pyfmt::truthy(v))
            .map(pyfmt::str)
            .unwrap_or_else(|| "untagged".into());
        if !strategy.is_empty() && name != strategy {
            continue;
        }
        let qty = as_f64(leg.get("quantity"));
        let avg = as_f64(leg.get("average_price"));
        let leg_realized = as_f64(leg.get("realized_pnl"));
        let leg_today = as_f64(leg.get("today_realized_pnl"));
        let price = price_of(&leg_key(leg));
        let mut leg_unrealized = 0.0;
        if qty.abs() > 1e-9 {
            match price {
                Some(p) => leg_unrealized = qty * (p - avg),
                None => unpriced += 1,
            }
            open_qty += qty;
        }
        realized += leg_realized;
        today += leg_today;
        unrealized += leg_unrealized;
        let [symbol, exchange, product] = leg_key(leg);
        let row = json!({
            "symbol": symbol, "exchange": exchange, "product": product,
            "quantity": r4(qty), "average_price": r4(avg),
            "ltp": price, "realized": r4(leg_realized),
            "today_realized": r4(leg_today), "unrealized": r4(leg_unrealized),
        });
        rows.push((r4(qty).abs() <= 1e-9, row));
    }
    // Stable: open legs keep their booked order, then the flat ones.
    rows.sort_by_key(|(flat, _)| *flat);
    let (realized, today, unrealized) = (r4(realized), r4(today), r4(unrealized));
    let mut out = Map::new();
    out.insert("strategy".into(), json!(strategy));
    out.insert("realized".into(), json!(realized));
    out.insert("today_realized".into(), json!(today));
    out.insert("unrealized".into(), json!(unrealized));
    out.insert("total".into(), json!(r4(realized + unrealized)));
    out.insert("today_total".into(), json!(r4(today + unrealized)));
    out.insert("open_quantity".into(), json!(r4(open_qty)));
    out.insert("unpriced_legs".into(), json!(unpriced));
    out.insert(
        "legs".into(),
        Value::Array(rows.into_iter().map(|(_, r)| r).collect()),
    );
    out
}

/// The app's strategy book, as the Strategy P&L node reads it.
pub struct AppStrategyBook(pub Arc<crate::strategy::book::StrategyBook>);

impl StrategyBook for AppStrategyBook {
    fn legs(&self, strategy: &str) -> Result<Vec<Value>, String> {
        self.0.get_strategy_legs(None, Some(strategy)).map_err(|e| {
            tracing::error!("Could not read strategy legs: {}", e);
            "Could not read the strategy book".to_string()
        })
    }
}

/// Margin: the margin a basket (`positionsJson`, or `positions`) or one
/// position needs (`execute_margin`). A basket is read without defaults,
/// position by position, as an order is: a value that does not resolve
/// fails the node instead of pricing a different basket.
///
/// One web defect is fixed (N-margin-06): a basket left blank (`""`, what
/// the editor writes for an emptied basket) is absent, so the single
/// position is priced; the web failed every run of such a node.
pub struct MarginNode;

#[async_trait]
impl FlowNode for MarginNode {
    fn node_type(&self) -> &'static str {
        "margin"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let positions = match margin_positions(ctx, data) {
            Ok(p) => p,
            Err(message) => {
                ctx.error(format!("Margin failed: {}", message));
                return NodeOutcome::error(message);
            }
        };
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let default_product =
            crate::flow::node_contracts::default_product_for_exchange(Some(&exchange));
        let order = Margin {
            quantity: ctx.get_int(data, "quantity", 1),
            price: margin_price(ctx, data),
            product_type: ctx.get_str(data, "product", default_product),
            action: ctx.get_str(data, "action", "BUY"),
            price_type: ctx.get_str(data, "priceType", "MARKET"),
            positions: positions.clone(),
            symbol: symbol.clone(),
            exchange: exchange.clone(),
        };
        match &positions {
            Some(p) => ctx.info(format!(
                "Calculating margin for a basket of {} position(s)",
                p.len()
            )),
            None if symbol.is_empty() => {
                let message = "Margin node has no positions and no symbol to price.";
                ctx.error(format!("Margin failed: {}", message));
                return NodeOutcome::error(message);
            }
            None => ctx.info(format!("Calculating margin for: {} ({})", symbol, exchange)),
        }
        let result = ctx.client.margin(&order).await;
        ctx.info(format!("Margin result: {}", pyfmt::str(&result)));
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// The single position's price as the web's `get_float(..., 0)` reads it:
/// the whole `0` when there is none (sent as `"0"`), else the number as a
/// float (`"0.0"`, `"812.5"`).
fn margin_price(ctx: &NodeCtx, data: &Map<String, Value>) -> Value {
    let absent = json!(0);
    let number = match data.get("price") {
        None | Some(Value::Null) => return absent,
        Some(Value::String(s)) if s.is_empty() => return absent,
        Some(Value::String(s)) => pyfmt::parse_float(&ctx.vars.interpolate(s)),
        Some(other) => pyfmt::to_float(other),
    };
    match number {
        Some(f) => json!(f),
        None => absent,
    }
}

/// `_parse_margin_positions`: the basket, normalised position by position,
/// or `None` when the node has none.
fn margin_positions(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
) -> Result<Option<Vec<Value>>, String> {
    let blank = |v: &Value| matches!(v, Value::String(s) if s.trim().is_empty());
    let Some(raw) = ["positionsJson", "positions"]
        .iter()
        .filter_map(|k| data.get(*k))
        .find(|v| !v.is_null() && !blank(v))
    else {
        return Ok(None);
    };
    let mut basket = match raw {
        Value::String(_) => ctx.vars.resolve_raw(raw),
        other => other.clone(),
    };
    if let Value::String(text) = &basket {
        if crate::flow::contract::vars::has_unresolved(text) {
            return Err(format!(
                "Margin positions has unresolved value {}.",
                pyfmt::quote(text)
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("Margin positions is empty after variable interpolation; check the {{variable}} references in the basket.".into());
        }
        basket = super::pyjson::loads(text).map_err(|e| {
            format!(
                "Margin positions is not valid JSON after variable interpolation: {}",
                e
            )
        })?;
    }
    let list = match basket {
        Value::Object(one) => vec![Value::Object(one)],
        Value::Array(list) => list,
        _ => return Err("Margin positions must be a JSON array of position objects.".into()),
    };
    if list.is_empty() {
        return Err("Margin positions is an empty basket; add at least one position.".into());
    }
    if list.iter().any(|p| !p.is_object()) {
        return Err(
            "Margin positions must all be objects; the basket contains a non-object entry.".into(),
        );
    }
    let mut normalised = Vec::with_capacity(list.len());
    for (i, position) in list.iter().enumerate() {
        let p = position.as_object().cloned().unwrap_or_default();
        let resolved =
            margin_position(ctx, &p).map_err(|e| format!("Margin position {}: {}", i + 1, e))?;
        normalised.push(resolved);
    }
    Ok(Some(normalised))
}

/// One basket position read as an order is (`RuntimeOrderResolver`), every
/// field required, and written back with the resolved values: numbers as
/// text, a whole number without `.0`.
fn margin_position(ctx: &NodeCtx, p: &Map<String, Value>) -> Result<Value, String> {
    use crate::agent::tools::market::VALID_EXCHANGES;
    use crate::agent::tools::order_vocab::{VALID_ACTIONS, VALID_PRICE_TYPES, VALID_PRODUCT_TYPES};
    let f = ctx.order_fields(p);
    let symbol = f.text("symbol", None, &[])?.to_uppercase();
    let exchange = f.choice("exchange", &VALID_EXCHANGES, None, &[])?;
    let action = f.choice("action", &VALID_ACTIONS, None, &[])?;
    let quantity = f.integer("quantity", None, &[], Some(1))?;
    let product = f.choice("product", &VALID_PRODUCT_TYPES, None, &[])?;
    let price_type = f.choice("pricetype", &VALID_PRICE_TYPES, None, &[])?;
    let price = f.number("price", Some(0.0), &[])?;
    let trigger = f.number("trigger_price", Some(0.0), &["triggerPrice"])?;
    if let Some(reason) = invalid_price_reason(&price_type, price, trigger) {
        return Err(reason);
    }
    let mut out = p.clone();
    out.insert("symbol".into(), json!(symbol));
    out.insert("exchange".into(), json!(exchange));
    out.insert("action".into(), json!(action));
    out.insert("quantity".into(), json!(quantity.to_string()));
    out.insert("product".into(), json!(product));
    out.insert("pricetype".into(), json!(price_type));
    if p.contains_key("price") {
        out.insert("price".into(), json!(numeric_text(price)));
    }
    if p.contains_key("trigger_price") || p.contains_key("triggerPrice") {
        out.remove("triggerPrice");
        out.insert("trigger_price".into(), json!(numeric_text(trigger)));
    }
    Ok(Value::Object(out))
}

/// `str(int(n)) if n.is_integer() else str(n)`.
fn numeric_text(n: f64) -> String {
    if n.fract() == 0.0 {
        // `+ 0.0` turns -0.0 into 0.0: Python's `int(-0.0)` is `0`.
        format!("{:.0}", n + 0.0)
    } else {
        pyfmt::float(n)
    }
}

/// `_invalid_price_reason`: why a price type cannot be sent with these
/// prices (a LIMIT or SL at zero executes at an unintended level).
fn invalid_price_reason(price_type: &str, price: f64, trigger: f64) -> Option<String> {
    let kind = if price_type.is_empty() {
        "MARKET".to_string()
    } else {
        price_type.to_uppercase()
    };
    if !price.is_finite() {
        return Some(format!(
            "{} order needs a finite price, got {}",
            kind,
            pyfmt::float(price)
        ));
    }
    if !trigger.is_finite() {
        return Some(format!(
            "{} order needs a finite trigger price, got {}",
            kind,
            pyfmt::float(trigger)
        ));
    }
    if matches!(kind.as_str(), "LIMIT" | "SL") && price <= 0.0 {
        return Some(format!(
            "{} order needs a positive price, got {}",
            kind,
            pyfmt::float(price)
        ));
    }
    if matches!(kind.as_str(), "SL" | "SL-M") && trigger <= 0.0 {
        return Some(format!(
            "{} order needs a positive trigger price, got {}",
            kind,
            pyfmt::float(trigger)
        ));
    }
    None
}
