//! The streaming nodes against the web's executor (goldens
//! `tests/fixtures/web/flow/goldens/triggers/subscribe*__*.json`,
//! `unsubscribe__*.json`) and the decided departures (decisions.md 9).

use super::*;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::NodeEnv;
use crate::flow::contract::subscriptions::Subscriptions;
use parking_lot::Mutex;

/// A feed that answers from a script and records what it was asked.
#[derive(Default)]
struct Feed {
    frames: Vec<Value>,
    open: Option<FeedError>,
    refuse: Option<FeedError>,
    ops: Mutex<Vec<String>>,
}

#[async_trait]
impl MarketFeed for Feed {
    async fn first_frame(
        &self,
        symbol: &str,
        exchange: &str,
        mode: &str,
        timeout: Duration,
        accept: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
    ) -> Result<Option<Value>, FeedError> {
        if let Some(e) = &self.open {
            return Err(e.clone());
        }
        self.ops.lock().push(format!(
            "subscribe {}:{} {} {}",
            exchange,
            symbol,
            mode,
            timeout.as_secs_f64()
        ));
        if let Some(e) = &self.refuse {
            return Err(e.clone());
        }
        Ok(self.frames.iter().find(|f| accept(f)).cloned())
    }

    async fn session(&self) -> Result<(), FeedError> {
        self.open.clone().map_or(Ok(()), Err)
    }

    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> Result<(), FeedError> {
        self.ops
            .lock()
            .push(format!("unsubscribe {}:{} {}", exchange, symbol, mode));
        self.refuse.clone().map_or(Ok(()), Err)
    }
}

struct T {
    ctx: NodeCtx,
    feed: Arc<Feed>,
    client: Arc<FakeFlowClient>,
}

fn t(feed: Feed, workflow: Option<i64>, subs: Arc<Subscriptions>) -> T {
    let feed = Arc::new(feed);
    let client = Arc::new(FakeFlowClient::new());
    let mut env = NodeEnv::default();
    env.feed = feed.clone();
    let mut ctx = NodeCtx::bare(client.clone()).with_env(env);
    ctx.workflow_id = workflow;
    ctx.subscriptions = subs;
    T { ctx, feed, client }
}

fn logs(ctx: &NodeCtx) -> Vec<String> {
    ctx.logs()
        .iter()
        .map(|l| {
            format!(
                "{} {}",
                l["level"].as_str().unwrap(),
                l["message"].as_str().unwrap()
            )
        })
        .collect()
}

fn node(kind: &str) -> Arc<dyn FlowNode> {
    (FAMILY.build)()
        .into_iter()
        .find(|n| n.node_type() == kind)
        .unwrap()
}

async fn run(t: &T, kind: &str, data: Value) -> NodeOutcome {
    node(kind).run(&t.ctx, data.as_object().unwrap()).await
}

fn tick(mode: u8, data: Value) -> Value {
    json!({"symbol": "SBIN", "exchange": "NSE", "mode": mode, "data": data})
}

/// parity N-subscribeLtp-01..03, N-stream-02, N-stream-09: the first frame
/// of the right symbol, exchange and mode wins; the subscription is
/// recorded for the workflow; the LTP is stored as the documented value
/// (N-subscribeLtp-05, decisions.md 9: the web stored the result dict).
#[tokio::test]
async fn subscribe_ltp_reads_the_first_matching_frame_and_keeps_the_subscription() {
    let feed = Feed {
        frames: vec![
            tick(2, json!({"ltp": 1.0})),
            json!({"symbol": "INFY", "exchange": "NSE", "mode": 1, "data": {"ltp": 2.0}}),
            tick(1, json!({"ltp": 812.65})),
        ],
        ..Default::default()
    };
    let subs = Arc::new(Subscriptions::default());
    let t = t(feed, Some(7), subs.clone());
    let out = run(
        &t,
        "subscribeLtp",
        json!({"symbol": "SBIN", "exchange": "NSE", "outputVariable": " ltp "}),
    )
    .await;
    assert_eq!(
        out.value(),
        Some(
            &json!({"status": "success", "type": "ltp", "symbol": "SBIN", "exchange": "NSE", "ltp": 812.65, "source": "websocket"})
        )
    );
    assert_eq!(t.ctx.vars.get("ltp"), Some(json!(812.65)));
    assert_eq!(
        logs(&t.ctx),
        vec![
            "info Subscribing to LTP stream: SBIN (NSE)",
            "info LTP for SBIN: 812.65 (via WebSocket)",
            "info Stored result in variable:  ltp ",
        ]
    );
    assert_eq!(*t.feed.ops.lock(), vec!["subscribe NSE:SBIN LTP 5"]);
    assert_eq!(
        subs.entries(7),
        vec![("SBIN".into(), "NSE".into(), "LTP".into())]
    );
    assert!(t.client.calls().is_empty());
}

/// parity N-subscribeLtp-04, N-stream-07, N-stream-08, N-stream-10: each
/// feed failure logs the web's warning and the REST quote answers; a
/// refused subscription is not recorded.
#[tokio::test]
async fn a_feed_failure_falls_back_to_the_rest_quote() {
    for (err, line) in [
        (FeedError::InvalidKey, "warning WebSocket: Invalid API key"),
        (
            FeedError::Connect("down".into()),
            "warning WebSocket connection failed: down",
        ),
        (
            FeedError::Failed("locked".into()),
            "warning WebSocket error: locked",
        ),
    ] {
        let feed = Feed {
            open: Some(err),
            ..Default::default()
        };
        let t = t(feed, Some(1), Arc::new(Subscriptions::default()));
        t.client.reply(
            "quotes",
            json!({"status": "success", "data": {"ltp": 812.5}}),
        );
        let out = run(&t, "subscribeLtp", json!({"symbol": "SBIN"})).await;
        assert_eq!(out.value().unwrap()["source"], "rest_api");
        assert_eq!(
            t.ctx.vars.get("ltp"),
            Some(json!(812.5)),
            "the default name keeps the scalar"
        );
        assert_eq!(
            logs(&t.ctx),
            vec![
                "info Subscribing to LTP stream: SBIN (NSE)".to_string(),
                line.to_string(),
                "info WebSocket timeout/failed, falling back to REST API for SBIN".into(),
                "info LTP for SBIN: 812.5 (via REST API)".into(),
            ]
        );
    }
    let feed = Feed {
        refuse: Some(FeedError::Subscribe("Subscription limit reached".into())),
        ..Default::default()
    };
    let subs = Arc::new(Subscriptions::default());
    let t = t(feed, Some(1), subs.clone());
    t.client
        .reply_error("quotes", "Invalid openalgo apikey", 403);
    let out = run(&t, "subscribeLtp", json!({"symbol": "SBIN"})).await;
    assert_eq!(
        out.value(),
        Some(
            &json!({"status": "error", "type": "ltp", "symbol": "SBIN", "exchange": "NSE", "error": "Invalid openalgo apikey"})
        )
    );
    assert!(logs(&t.ctx)
        .contains(&"warning WebSocket subscribe failed: Subscription limit reached".to_string()));
    assert!(
        logs(&t.ctx).contains(&"error Subscribe LTP error: Invalid openalgo apikey".to_string())
    );
    assert!(!subs.has(1), "a refused subscription is not kept");
}

/// parity N-subscribeQuote-02..04: Python's `or` between the nested and
/// top-level fields, and the documented quote stored.
#[tokio::test]
async fn subscribe_quote_reads_nested_then_top_level_fields() {
    let feed = Feed {
        frames: vec![
            json!({"symbol": "SBIN", "exchange": "NSE", "mode": 2, "ltp": 0, "close": 799.5, "volume": 10}),
        ],
        ..Default::default()
    };
    let t = t(feed, None, Arc::new(Subscriptions::default()));
    let out = run(
        &t,
        "subscribeQuote",
        json!({"symbol": "SBIN", "outputVariable": "quote"}),
    )
    .await;
    let want = json!({"ltp": 0, "open": 0, "high": 0, "low": 0, "close": 799.5, "volume": 10, "prev_close": 0});
    assert_eq!(out.value().unwrap()["data"], want);
    assert_eq!(t.ctx.vars.get("quote"), Some(want));
    assert_eq!(
        logs(&t.ctx)[1],
        "info Quote for SBIN: LTP=0 (via WebSocket)"
    );
    // REST: `close` falls back to `prev_close` by key, and an empty reply is {}.
    let t2 = super::tests::t(
        Feed {
            open: Some(FeedError::Connect("down".into())),
            ..Default::default()
        },
        None,
        Arc::new(Subscriptions::default()),
    );
    t2.client
        .reply("quotes", json!({"status": "success", "data": {}}));
    let out = run(&t2, "subscribeQuote", json!({"symbol": "SBIN"})).await;
    assert_eq!(out.value().unwrap()["data"], json!({}));
    assert!(logs(&t2.ctx).contains(&"info Quote for SBIN: LTP=None (via REST API)".to_string()));
}

/// parity N-subscribeDepth-02..04, N-stream-09: an empty depth frame is
/// skipped, the Fyers `depth.buy/sell` form converted, the book stored.
#[tokio::test]
async fn subscribe_depth_waits_for_a_book_and_converts_the_fyers_form() {
    let feed = Feed {
        frames: vec![
            tick(3, json!({"ltp": 1})),
            json!({"symbol": "SBIN", "exchange": "NSE", "mode": 3, "ltp": 812.5,
                   "depth": {"buy": [{"price": 812.4, "volume": 150}], "sell": [{"price": 812.6, "quantity": 120}]}}),
        ],
        ..Default::default()
    };
    let t = t(feed, Some(3), Arc::new(Subscriptions::default()));
    let out = run(
        &t,
        "subscribeDepth",
        json!({"symbol": "SBIN", "outputVariable": "depth"}),
    )
    .await;
    let book = json!({"bids": [{"price": 812.4, "quantity": 150}], "asks": [{"price": 812.6, "quantity": 120}], "totalbuyqty": 0, "totalsellqty": 0, "ltp": 812.5});
    assert_eq!(out.value().unwrap()["data"], book);
    assert_eq!(t.ctx.vars.get("depth"), Some(book));
    assert_eq!(*t.feed.ops.lock(), vec!["subscribe NSE:SBIN Depth 3"]);
    assert_eq!(
        logs(&t.ctx)[1],
        "info Depth for SBIN: Bid=812.4, Ask=812.6 (1 bids, 1 asks) via WebSocket"
    );
}

/// parity N-subscribeLtp-02, N-subscribeQuote-01, N-subscribeDepth-01.
#[tokio::test]
async fn a_subscribe_without_a_symbol_stops_the_branch() {
    for (kind, word) in [
        ("subscribeLtp", "LTP"),
        ("subscribeQuote", "Quote"),
        ("subscribeDepth", "Depth"),
    ] {
        let t = t(Feed::default(), None, Arc::new(Subscriptions::default()));
        let out = run(&t, kind, json!({})).await;
        assert_eq!(
            out.value(),
            Some(&json!({"status": "error", "message": "No symbol specified"}))
        );
        assert_eq!(
            logs(&t.ctx),
            vec![format!("error Subscribe {}: No symbol specified", word)]
        );
    }
}

/// parity N-unsubscribe-03 (decisions.md 9): `all` gives back this
/// workflow's own subscriptions, never another workflow's; the web cleared
/// the whole feed, the Sandbox engine's included.
#[tokio::test]
async fn unsubscribe_all_releases_only_this_workflows_subscriptions() {
    let subs = Arc::new(Subscriptions::default());
    subs.record(Some(1), "SBIN", "NSE", "LTP");
    subs.record(Some(1), "TCS", "NSE", "Quote");
    subs.record(Some(2), "TCS", "NSE", "Quote");
    let t = t(Feed::default(), Some(1), subs.clone());
    let out = run(&t, "unsubscribe", json!({})).await;
    assert_eq!(
        out.value().unwrap()["message"],
        "Unsubscribed from all stream"
    );
    assert_eq!(
        *t.feed.ops.lock(),
        vec!["unsubscribe NSE:SBIN LTP"],
        "TCS stays for workflow 2"
    );
    assert!(!subs.has(1));
    assert!(subs.has(2));
    assert_eq!(
        logs(&t.ctx),
        vec![
            "info Unsubscribing from all stream: all (NSE)",
            "info Unsubscribed from all streams"
        ]
    );
}

/// parity N-unsubscribe-05 (WEB-DEFECT, minor): a named unsubscribe drops
/// this workflow's claim (the web left it recorded), and leaves a feed
/// another workflow holds.
#[tokio::test]
async fn unsubscribe_one_drops_this_workflows_claim() {
    let subs = Arc::new(Subscriptions::default());
    subs.record(Some(1), "SBIN", "NSE", "Depth");
    let t = t(Feed::default(), Some(1), subs.clone());
    let out = run(
        &t,
        "unsubscribe",
        json!({"symbol": "SBIN", "streamType": "depth"}),
    )
    .await;
    assert_eq!(
        out.value().unwrap()["message"],
        "Unsubscribed from depth stream"
    );
    assert_eq!(*t.feed.ops.lock(), vec!["unsubscribe NSE:SBIN Depth"]);
    assert!(!subs.has(1));

    subs.record(Some(1), "SBIN", "NSE", "LTP");
    subs.record(Some(2), "SBIN", "NSE", "LTP");
    let t = super::tests::t(Feed::default(), Some(1), subs.clone());
    run(
        &t,
        "unsubscribe",
        json!({"symbol": "SBIN", "streamType": "ltp"}),
    )
    .await;
    assert!(t.feed.ops.lock().is_empty(), "workflow 2 still reads it");
    assert!(!subs.has(1) && subs.has(2));
    // An unknown stream type is a Quote unsubscribe, named as given.
    let t = super::tests::t(Feed::default(), Some(9), Arc::new(Subscriptions::default()));
    let out = run(
        &t,
        "unsubscribe",
        json!({"symbol": "SBIN", "streamType": "candles"}),
    )
    .await;
    assert_eq!(
        out.value().unwrap()["message"],
        "Unsubscribed from candles stream"
    );
    assert_eq!(*t.feed.ops.lock(), vec!["unsubscribe NSE:SBIN Quote"]);
}

/// parity N-unsubscribe-02, -04, -06, -08: no session is a quiet success;
/// a mode with no symbol is refused; a refusal and a failure stop the
/// branch (N-unsubscribe-08 WEB-DEFECT: the web reported a failure as
/// success).
#[tokio::test]
async fn unsubscribe_reports_what_the_feed_said() {
    let t1 = t(
        Feed {
            open: Some(FeedError::InvalidKey),
            ..Default::default()
        },
        Some(1),
        Arc::new(Subscriptions::default()),
    );
    let out = run(&t1, "unsubscribe", json!({"streamType": "all"})).await;
    assert_eq!(
        out.value(),
        Some(
            &json!({"status": "success", "type": "unsubscribe", "message": "No active WebSocket connection"})
        )
    );
    assert_eq!(logs(&t1.ctx)[1], "warning Unsubscribe: Invalid API key");

    let t2 = t(Feed::default(), Some(1), Arc::new(Subscriptions::default()));
    let out = run(&t2, "unsubscribe", json!({"streamType": "ltp"})).await;
    assert!(out.is_error());
    assert!(out
        .failure_message()
        .starts_with("Unsubscribe is set to 'ltp' with no symbol."));
    assert!(t2.feed.ops.lock().is_empty());

    let t3 = t(
        Feed {
            refuse: Some(FeedError::Subscribe("Not subscribed".into())),
            ..Default::default()
        },
        Some(1),
        Arc::new(Subscriptions::default()),
    );
    let out = run(
        &t3,
        "unsubscribe",
        json!({"symbol": "SBIN", "streamType": "ltp"}),
    )
    .await;
    assert_eq!(
        out.value(),
        Some(&json!({"status": "error", "type": "unsubscribe", "message": "Not subscribed"}))
    );
    assert_eq!(
        logs(&t3.ctx)[1..],
        [
            "info Unsubscribed from ltp for SBIN",
            "error Unsubscribe failed: Not subscribed"
        ]
    );

    let t4 = t(
        Feed {
            refuse: Some(FeedError::Failed("proxy went away".into())),
            ..Default::default()
        },
        Some(1),
        Arc::new(Subscriptions::default()),
    );
    let out = run(
        &t4,
        "unsubscribe",
        json!({"symbol": "SBIN", "streamType": "quote"}),
    )
    .await;
    assert_eq!(
        out.value(),
        Some(
            &json!({"status": "error", "type": "unsubscribe", "symbol": "SBIN", "exchange": "NSE", "stream_type": "quote", "message": "proxy went away"})
        )
    );
    assert_eq!(
        logs(&t4.ctx)[1],
        "warning Unsubscribe error: proxy went away"
    );
}
