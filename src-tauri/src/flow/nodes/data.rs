//! The data family: every node that reads market, account or calendar data
//! (the web's `execute_get_quote` through `execute_margin`,
//! `flow_executor_service.py` 1995-2748), registered here with one line in
//! [`super::FAMILIES`], and the services the app installs for them.
//!
//! The nodes in this file are the plain ones: read the fields with the
//! forgiving accessors, call one `/api/v1` service through the client, log
//! the web's lines, store the reply whole under `outputVariable` (a failed
//! reply too) and return it, so a failure stops the branch with the
//! service's own words. A `{{...}}` that does not resolve is sent as text,
//! as on the web (N-dataCommon-09); only the option nodes and the margin
//! basket refuse it. The other files of the family:
//!
//! | File | Nodes |
//! | --- | --- |
//! | `books.rs` | Order Book, Trade Book, Position Book, Holdings, Funds, Strategy P&L, Margin |
//! | `symbols.rs` | Symbol, Expiry, Option Symbol, Option Chain, Synthetic Future |
//! | `history.rs` | History, Prior Period OHLC, Bar Offset, the history cache |
//! | `indicator.rs` | Indicator |
//! | `calendar.rs` | Calendar, the trading-calendar port |

use super::books::{AppStrategyBook, Book, BookNode, MarginNode, StrategyPnlNode};
use super::calendar::{AppCalendar, CalendarNode};
use super::history::{BarOffsetNode, HistoryNode, PriorPeriodOhlcNode};
use super::indicator::IndicatorNode;
use super::symbols::{
    ExpiryNode, OptionChainNode, OptionSymbolNode, SymbolNode, SyntheticFutureNode,
};
use super::NodeFamily;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::flow::contract::time::ist_now;
use crate::flow::node_contracts::default_product_for_exchange;
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "data",
    build: || {
        vec![
            Arc::new(GetQuote),
            Arc::new(GetDepth),
            Arc::new(GetOrderStatus),
            Arc::new(OpenPosition),
            Arc::new(HistoryNode),
            Arc::new(StrategyPnlNode),
            Arc::new(PriorPeriodOhlcNode),
            Arc::new(BarOffsetNode),
            Arc::new(IndicatorNode),
            Arc::new(SymbolNode),
            Arc::new(OptionSymbolNode),
            Arc::new(ExpiryNode),
            Arc::new(Intervals),
            Arc::new(MultiQuotes),
            Arc::new(OptionChainNode),
            Arc::new(SyntheticFutureNode),
            Arc::new(CalendarNode),
            Arc::new(Holidays),
            Arc::new(Timings),
            Arc::new(BookNode(Book::Orders)),
            Arc::new(BookNode(Book::Trades)),
            Arc::new(BookNode(Book::Positions)),
            Arc::new(BookNode(Book::Holdings)),
            Arc::new(BookNode(Book::Funds)),
            Arc::new(MarginNode),
        ]
    },
};

/// Install the services the data nodes read besides the client: the
/// strategy book (Strategy P&L) and the market calendar (Calendar).
pub fn install_env(app: &Arc<AppState>) {
    let book = app.strategy.book.clone();
    let weak = Arc::downgrade(app);
    app.flow.install_env(move |env| {
        env.strategy_book = Arc::new(AppStrategyBook(book));
        env.calendar = Arc::new(AppCalendar(weak));
    });
}

/// Store and return a client reply.
fn keep(ctx: &NodeCtx, data: &Map<String, Value>, result: Value) -> NodeOutcome {
    ctx.store_output(data, result.clone());
    NodeOutcome::result(result)
}

/// The reply as the web logs it: `{label}: {result}` (Python's dict text).
fn shown(label: &str, result: &Value) -> String {
    format!("{}: {}", label, pyfmt::str(result))
}

/// Get Quote: one symbol's quote (`{{q.data.ltp}}`).
struct GetQuote;

#[async_trait]
impl FlowNode for GetQuote {
    fn node_type(&self) -> &'static str {
        "getQuote"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        ctx.info(format!("Getting quote for: {}", symbol));
        let result = ctx.client.get_quotes(&symbol, &exchange).await;
        ctx.info(shown("Quote result", &result));
        keep(ctx, data, result)
    }
}

/// Get Depth: one symbol's market depth.
struct GetDepth;

#[async_trait]
impl FlowNode for GetDepth {
    fn node_type(&self) -> &'static str {
        "getDepth"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        ctx.info(format!("Getting depth for: {}", symbol));
        let result = ctx.client.get_depth(&symbol, &exchange).await;
        ctx.info("Depth result received");
        keep(ctx, data, result)
    }
}

/// Get Order Status: one order's status. The desktop's `/api/v1/orderstatus`
/// needs a strategy label, which the web's service never read; the node
/// sends the run's tag (the workflow's name), as its order nodes do.
struct GetOrderStatus;

#[async_trait]
impl FlowNode for GetOrderStatus {
    fn node_type(&self) -> &'static str {
        "getOrderStatus"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let order_id = ctx.get_str(data, "orderId", "");
        ctx.info(format!("Getting order status for: {}", order_id));
        let call = crate::flow::contract::client::ApiCall::new(
            "get_order_status",
            "orderstatus",
            json!({"orderid": order_id, "strategy": ctx.strategy_tag(data)}),
        );
        let result = ctx.client.api(call).await;
        ctx.info(shown("Order status result", &result));
        keep(ctx, data, result)
    }
}

/// Open Position: one position's quantity and P&L, read from the position
/// book (not `/api/v1/openposition`, whose shape differs), matched on the
/// exact symbol, exchange and product.
struct OpenPosition;

#[async_trait]
impl FlowNode for OpenPosition {
    fn node_type(&self) -> &'static str {
        "openPosition"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let product = ctx.get_str(
            data,
            "product",
            default_product_for_exchange(Some(&exchange)),
        );
        ctx.info(format!("Getting open position for: {}", symbol));
        let result = ctx
            .client
            .get_open_position(&symbol, &exchange, Some(&product))
            .await;
        ctx.info(shown("Open position result", &result));
        keep(ctx, data, result)
    }
}

/// Intervals: the history intervals the connected broker offers.
struct Intervals;

#[async_trait]
impl FlowNode for Intervals {
    fn node_type(&self) -> &'static str {
        "intervals"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        ctx.info("Getting available intervals");
        let result = ctx.client.get_intervals().await;
        ctx.info(shown("Intervals result", &result));
        keep(ctx, data, result)
    }
}

/// Multi Quotes: quotes for a comma-separated list, all on one exchange.
struct MultiQuotes;

#[async_trait]
impl FlowNode for MultiQuotes {
    fn node_type(&self) -> &'static str {
        "multiQuotes"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let raw = ctx.get_str(data, "symbols", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let symbols: Vec<Value> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| json!({"symbol": s, "exchange": exchange}))
            .collect();
        ctx.info(format!("Getting quotes for {} symbols", symbols.len()));
        let result = ctx.client.get_multi_quotes(&symbols).await;
        ctx.info(shown("Multi quotes result", &result));
        keep(ctx, data, result)
    }
}

/// Holidays: the market holidays of a year (blank or 0 is this year).
struct Holidays;

#[async_trait]
impl FlowNode for Holidays {
    fn node_type(&self) -> &'static str {
        "holidays"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let year = ctx.get_int(data, "year", 0);
        let shown_year = if year == 0 {
            "current".to_string()
        } else {
            year.to_string()
        };
        ctx.info(format!("Fetching holidays for year: {}", shown_year));
        // A year beyond what the service reads is still its own refusal.
        let asked = (year != 0).then(|| i32::try_from(year).unwrap_or(i32::MAX));
        let result = ctx.client.holidays(asked).await;
        ctx.info("Holidays result received");
        keep(ctx, data, result)
    }
}

/// Timings: the exchanges' trading windows on a date (blank is today in
/// IST, on any machine).
struct Timings;

#[async_trait]
impl FlowNode for Timings {
    fn node_type(&self) -> &'static str {
        "timings"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let mut date = ctx.get_str(data, "date", "");
        if date.is_empty() {
            date = ist_now(ctx.clock.as_ref()).format("%Y-%m-%d").to_string();
        }
        ctx.info(format!("Fetching market timings for date: {}", date));
        let result = ctx.client.timings(&date).await;
        ctx.info(shown("Timings result", &result));
        keep(ctx, data, result)
    }
}
