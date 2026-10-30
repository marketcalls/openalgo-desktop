//! The data nodes over the real in-process `/api/v1` (N-dataCommon-05): in
//! analyzer mode the account nodes answer from the sandbox and never reach
//! the broker, as the web's services decide; the market-data nodes read
//! live data in both modes. Also the desktop's schema where the web called
//! its service directly (X-349), and the app's installed strategy book and
//! calendar (N-strategyPnl-06, N-calendar-06).

use super::support::run;
use crate::agent::tools::testkit::Kit;
use crate::brokers::mock::MockCall;
use crate::brokers::types::Quote;
use crate::flow::contract::client::app::AppFlowClient;
use crate::flow::contract::node::{NodeCtx, NodeCtxParts};
use crate::flow::contract::subscriptions::Subscriptions;
use crate::flow::contract::vars::DEFAULT_BOUNDARY;
use crate::security::Secret;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn ctx(kit: &Kit) -> NodeCtx {
    let app = kit.app();
    NodeCtx::new(NodeCtxParts {
        client: Arc::new(AppFlowClient::new(
            Arc::downgrade(app),
            Secret::new(kit.key.clone()),
        )),
        clock: app.clock.clone(),
        cancel: CancellationToken::new(),
        workflow_id: None,
        default_strategy: "Golden Flow".into(),
        subscriptions: Arc::new(Subscriptions::default()),
        session_boundary: DEFAULT_BOUNDARY,
        env: app.flow.env(),
        app: Arc::downgrade(app),
    })
}

/// Whether a broker call is the one a node makes.
type Reached = fn(&MockCall) -> bool;

fn broker_saw(kit: &Kit, f: impl Fn(&MockCall) -> bool) -> bool {
    kit.mock.calls().iter().any(f)
}

fn status(v: &Option<Value>) -> Value {
    v.as_ref()
        .map(|v| v["status"].clone())
        .unwrap_or(Value::Null)
}

/// Funds, the books, holdings, order status, open position and strategy
/// P&L come from the sandbox in analyzer mode and from the broker in live
/// mode.
#[tokio::test]
async fn account_nodes_follow_analyzer_mode() {
    let nodes: [(&str, Value, Reached); 8] = [
        ("funds", json!({}), |c| matches!(c, MockCall::Funds)),
        ("orderBook", json!({}), |c| matches!(c, MockCall::OrderBook)),
        ("tradeBook", json!({}), |c| matches!(c, MockCall::TradeBook)),
        ("positionBook", json!({}), |c| {
            matches!(c, MockCall::Positions)
        }),
        ("holdings", json!({}), |c| matches!(c, MockCall::Holdings)),
        ("openPosition", json!({"symbol": "SBIN"}), |c| {
            matches!(c, MockCall::Positions)
        }),
        ("strategyPnl", json!({}), |c| {
            matches!(c, MockCall::Positions)
        }),
        ("getOrderStatus", json!({"orderId": "SB-1"}), |c| {
            matches!(c, MockCall::OrderBook)
        }),
    ];
    for (node, data, reached) in nodes {
        let kit = Kit::new();
        kit.analyzer(true);
        let out = run(&ctx(&kit), node, data.clone()).await;
        assert!(
            !broker_saw(&kit, reached),
            "{} reached the broker in analyzer mode",
            node
        );
        if node != "getOrderStatus" {
            assert_eq!(
                status(&out.value().cloned()),
                json!("success"),
                "{} {:?}",
                node,
                out
            );
        }
        let kit = Kit::new();
        kit.analyzer(false);
        run(&ctx(&kit), node, data).await;
        assert!(
            broker_saw(&kit, reached),
            "{} did not reach the broker live",
            node
        );
    }
}

/// The sandbox's funds are the 1 crore it starts with.
#[tokio::test]
async fn analyzer_funds_are_the_sandboxs() {
    let kit = Kit::new();
    kit.analyzer(true);
    let out = run(&ctx(&kit), "funds", json!({"outputVariable": "funds"})).await;
    let cash = out
        .value()
        .and_then(|v| v["data"]["availablecash"].as_f64())
        .unwrap_or_default();
    assert_eq!(cash, 10_000_000.0, "{:?}", out);
    assert_eq!(out.value().map(|v| v["mode"].clone()), Some(json!("analyze")));
}

/// Quotes, depth and margin have no analyzer branch: live data in both
/// modes.
#[tokio::test]
async fn market_data_is_live_in_both_modes() {
    for analyzer in [true, false] {
        let kit = Kit::new();
        kit.analyzer(analyzer);
        kit.mock.set_quote(Quote {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            ltp: 812.5,
            ..Default::default()
        });
        let c = ctx(&kit);
        let out = run(
            &c,
            "getQuote",
            json!({"symbol": "SBIN", "outputVariable": "q"}),
        )
        .await;
        assert_eq!(
            out.value().map(|v| v["data"]["ltp"].clone()),
            Some(json!(812.5)),
            "{:?}",
            out
        );
        assert!(broker_saw(&kit, |c| matches!(c, MockCall::Quote(_))));
        run(
            &c,
            "margin",
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 1}),
        )
        .await;
        assert!(
            broker_saw(&kit, |c| matches!(c, MockCall::Margin(_))),
            "analyzer={}",
            analyzer
        );
    }
}

/// X-349: the web's client called the option-symbol resolver directly and
/// skipped the API schema; here the in-process `/api/v1/optionsymbol` refuses
/// a bad option type with its validation message, before any quote.
#[tokio::test]
async fn an_option_symbol_with_a_bad_type_is_refused_by_the_api() {
    let kit = Kit::new();
    let out = run(
        &ctx(&kit),
        "optionSymbol",
        json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "27OCT26", "optionType": "CALL"}),
    )
    .await;
    let v = out.value().cloned().unwrap_or(Value::Null);
    assert_eq!(v["status"], "error");
    assert_eq!(v["code"], 400, "{}", v);
    assert_eq!(v["error"], "Validation error", "{}", v);
    assert!(!broker_saw(&kit, |c| matches!(c, MockCall::Quote(_))));
}

/// The app's environment: the strategy book (empty here) and the market
/// calendar (the seeded holidays) reach the nodes.
#[tokio::test]
async fn the_app_installs_the_book_and_the_calendar() {
    let kit = Kit::new();
    kit.analyzer(true);
    let c = ctx(&kit);
    let pnl = run(&c, "strategyPnl", json!({})).await;
    assert_eq!(
        pnl.value()
            .map(|v| (v["strategy"].clone(), v["total"].clone())),
        Some((json!("Golden Flow"), json!(0.0)))
    );
    let cal = run(&c, "calendar", json!({"date": "2026-10-09"})).await;
    assert_eq!(
        cal.value().map(|v| v["is_trading_day"].clone()),
        Some(json!(true)),
        "{:?}",
        cal
    );
}

/// N-optionChain-04: the desktop's `/api/v1/optionchain` takes the blank
/// expiry the node sends for an underlying that embeds one (the service
/// reads the embedded date), and refuses a strike count outside 1..100
/// with its own validation message rather than a clamp.
#[tokio::test]
async fn an_option_chain_runs_on_an_embedded_expiry_and_refuses_a_bad_count() {
    let kit = Kit::new();
    kit.mock.set_quote(Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24412.0,
        ..Default::default()
    });
    let c = ctx(&kit);
    let out = run(
        &c,
        "optionChain",
        json!({"underlying": "NIFTY27OCT26", "exchange": "NSE_INDEX", "expiryDate": "", "strikeCount": 2}),
    )
    .await;
    let v = out.value().cloned().unwrap_or(Value::Null);
    assert_eq!(v["status"], "success", "{}", v);
    assert_eq!(v["expiry_date"], "27OCT26");
    assert_eq!(v["atm_strike"], 24400.0);
    let refused = run(
        &c,
        "optionChain",
        json!({"underlying": "NIFTY", "expiryDate": "27OCT26", "strikeCount": 0}),
    )
    .await;
    let v = refused.value().cloned().unwrap_or(Value::Null);
    assert_eq!(
        (v["status"].clone(), v["code"].clone()),
        (json!("error"), json!(400)),
        "{}",
        v
    );
    assert_eq!(v["error"], "Validation error", "{}", v);
}
