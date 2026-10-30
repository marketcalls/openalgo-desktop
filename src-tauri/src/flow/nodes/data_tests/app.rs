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
    assert_eq!(
        out.value().map(|v| v["mode"].clone()),
        Some(json!("analyze"))
    );
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
/// a bad option type with its field errors, before any quote.
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
    // The API's field errors, as the client hands them to the node
    // (decision 21, N-dataCommon-04).
    assert_eq!(
        v["error"],
        json!({"option_type": ["Must be one of: CE, PE, ce, pe."]}),
        "{}",
        v
    );
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
    // N-calendar-06: the seeded calendar, read through the app. A
    // weekends-only calendar would call this Friday a trading day and the
    // Sunday Muhurat session a closed day.
    let flags = |v: Option<&Value>| {
        v.map(|v| {
            (
                v["is_trading_day"].clone(),
                v["is_trading_holiday"].clone(),
                v["is_special_session"].clone(),
            )
        })
    };
    let gandhi = run(&c, "calendar", json!({"date": "2026-10-02"})).await;
    assert_eq!(
        flags(gandhi.value()),
        Some((json!(false), json!(true), json!(false)))
    );
    let muhurat = run(&c, "calendar", json!({"date": "2026-11-08"})).await;
    assert_eq!(
        flags(muhurat.value()),
        Some((json!(true), json!(false), json!(true)))
    );
}

/// N-holidays-02: a year outside 2020..2050 reads the web service's sentence
/// (the web's Flow asked the service, not the REST route's schema).
#[tokio::test]
async fn a_holidays_year_out_of_range_reads_the_services_sentence() {
    let kit = Kit::new();
    let c = ctx(&kit);
    let out = run(
        &c,
        "holidays",
        json!({"year": 1999, "outputVariable": "hol"}),
    )
    .await;
    assert_eq!(
        out.value().cloned(),
        Some(
            json!({"status": "error", "error": "Year must be between 2020 and 2050", "code": 400})
        )
    );
    let ok = run(&c, "holidays", json!({"year": 2026})).await;
    assert_eq!(ok.value().map(|v| v["year"].clone()), Some(json!(2026)));
}

/// N-dataCommon-05, N-getDepth-03: the market-data, option and calendar
/// nodes have no analyzer branch; in analyzer mode they answer exactly as
/// live and the market-data ones reach the broker.
#[tokio::test]
async fn data_nodes_answer_live_in_analyzer_mode() {
    let quote = || Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24412.0,
        ..Default::default()
    };
    let opt = json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "27OCT26"});
    let mut chain = opt.clone();
    chain["strikeCount"] = json!(2);
    let mut symbol = opt.clone();
    symbol["offset"] = json!("ATM");
    let nodes: [(&str, Value, Option<Reached>); 11] = [
        (
            "getDepth",
            json!({"symbol": "SBIN"}),
            Some(|c| matches!(c, MockCall::Depth(_))),
        ),
        (
            "history",
            json!({"symbol": "SBIN", "interval": "D", "startDate": "2026-10-01", "endDate": "2026-10-08"}),
            Some(|c| matches!(c, MockCall::History(_))),
        ),
        (
            "multiQuotes",
            json!({"symbols": "SBIN,INFY"}),
            Some(|c| matches!(c, MockCall::MultiQuotes(_))),
        ),
        (
            "optionSymbol",
            symbol,
            Some(|c| matches!(c, MockCall::Quote(_))),
        ),
        (
            "optionChain",
            chain,
            Some(|c| matches!(c, MockCall::Quote(_))),
        ),
        (
            "syntheticFuture",
            opt,
            Some(|c| matches!(c, MockCall::Quote(_))),
        ),
        ("symbol", json!({"symbol": "SBIN"}), None),
        (
            "expiry",
            json!({"symbol": "NIFTY", "exchange": "NFO"}),
            None,
        ),
        ("intervals", json!({}), None),
        ("holidays", json!({"year": 2026}), None),
        ("timings", json!({"date": "2026-10-09"}), None),
    ];
    for (node, data, reached) in nodes {
        let mut answers = Vec::new();
        for analyzer in [true, false] {
            let kit = Kit::new();
            kit.analyzer(analyzer);
            kit.mock.set_quote(quote());
            answers.push(run(&ctx(&kit), node, data.clone()).await.value().cloned());
            if let (true, Some(r)) = (analyzer, reached) {
                assert!(
                    broker_saw(&kit, r),
                    "{} did not reach the broker in analyzer mode",
                    node
                );
            }
        }
        assert_eq!(
            answers[0], answers[1],
            "{} differs between analyzer and live",
            node
        );
        assert!(answers[0].is_some(), "{}", node);
    }
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
    assert_eq!(
        v["error"],
        json!({"strike_count": ["Must be greater than or equal to 1 and less than or equal to 100."]}),
        "{}",
        v
    );
}

/// N-history-05: a History node with neither dates nor days. The web's node
/// and history service fill no range (they hand empty dates to the broker,
/// whose own parsing then fails the call); the desktop's `/api/v1/history`
/// refuses the empty range before any broker call. Either way the node
/// fails and stores the failure, and the branch stops.
#[tokio::test]
async fn a_history_without_a_range_fails_before_the_broker() {
    let kit = Kit::new();
    let c = ctx(&kit);
    let out = run(
        &c,
        "history",
        json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D", "outputVariable": "h"}),
    )
    .await;
    let v = out.value().cloned().unwrap_or(Value::Null);
    assert_eq!(
        (v["status"].clone(), v["code"].clone()),
        (json!("error"), json!(400)),
        "{}",
        v
    );
    assert_eq!(c.vars.get("h"), Some(v));
    assert!(!broker_saw(&kit, |c| matches!(c, MockCall::History(_))));
}

/// N-optionChain-04: the web's chain service never clamps strikeCount (0
/// is the ATM row alone, a large count the whole chain); the desktop keeps
/// `/api/v1/optionchain`'s 1..100 and refuses the rest with its own
/// validation answer (decisions.md 4: the public schema is not widened), so
/// a count of 0 or 150 fails the node before any broker call.
#[tokio::test]
async fn a_strike_count_outside_the_api_range_is_refused_not_clamped() {
    for count in [0, 101, 150] {
        let kit = Kit::new();
        let out = run(
            &ctx(&kit),
            "optionChain",
            json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiryDate": "27OCT26", "strikeCount": count}),
        )
        .await;
        let v = out.value().cloned().unwrap_or(Value::Null);
        assert_eq!(
            (v["status"].clone(), v["code"].clone()),
            (json!("error"), json!(400)),
            "{}: {}",
            count,
            v
        );
        assert!(
            !broker_saw(&kit, |c| matches!(
                c,
                MockCall::Quote(_) | MockCall::MultiQuotes(_)
            )),
            "{}",
            count
        );
    }
}

/// The history cache's account (IND-011): the broker account's tag while a
/// broker session exists, `anon` with the app and no session (the web's
/// `anon` for a client without a key), and a run without the app is its
/// own client's.
#[tokio::test]
async fn the_history_cache_is_keyed_by_the_broker_account() {
    use crate::flow::nodes::history::{account_tag, broker_account_tag};
    let kit = Kit::new();
    let c = ctx(&kit);
    let (tag, pin) = account_tag(&c);
    assert_eq!(tag, broker_account_tag("zerodha", "AB1234"));
    assert!(pin.is_none());
    kit.app().set_broker_session(None);
    let (tag, pin) = account_tag(&c);
    assert_eq!(tag, "anon");
    assert!(pin.is_none());
    let bare = super::support::ctx(Arc::new(
        crate::flow::contract::client::fake::FakeFlowClient::new(),
    ));
    let (tag, pin) = account_tag(&bare);
    assert!(tag.starts_with("run:"), "{tag}");
    assert!(
        pin.is_some(),
        "a run without the app keeps its client with its answers"
    );
}
