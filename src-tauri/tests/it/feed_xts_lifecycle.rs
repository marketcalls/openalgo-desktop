//! XTS subscriptions end to end (review 107b, B1 to B3): the real XTS feed,
//! manager, bridge and public session, with only the broker's REST and
//! Socket.IO doubled. What the broker refuses, whenever it refuses it,
//! reaches whoever asked, and the public replies, the registry, the
//! manager, the bridge and the broker's own state agree.

use crate::feed_support::{brokers, eventually, Client, API_KEY, USER_ID};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::{start, FeedConfig, FeedDeps, FeedHandle};
use openalgo_desktop_lib::websocket::{FeedMode, WebSocketManager};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;

const RELIANCE: i64 = 2885;
const SBIN: i64 = 3045;
const LTP: i64 = 1512;
const QUOTE: i64 = 1501;
const DEPTH: i64 = 1502;
const SETTLE: Duration = Duration::from_secs(20);

/// The fake broker's REST API: what it was asked, what it holds, and what
/// the test wants it to answer.
#[derive(Default)]
struct Upstream {
    requests: Vec<(String, i64, Vec<i64>)>,
    /// The (instrument, code) subscriptions it accepted.
    active: HashSet<(i64, i64)>,
    refuse_post_code: HashSet<i64>,
    refuse_put_code: HashSet<i64>,
    /// A POST naming one of these is refused whole.
    refuse_instrument: HashSet<i64>,
    /// The next POSTs answered 503.
    fail_posts: VecDeque<()>,
    put_gate: Option<oneshot::Receiver<()>>,
}

fn named(body: &Value) -> (i64, Vec<i64>) {
    let code = body["xtsMessageCode"].as_i64().unwrap_or_default();
    let instruments = body["instruments"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["exchangeInstrumentID"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    (code, instruments)
}

type Answer = (axum::http::StatusCode, axum::Json<Value>);

fn refused(description: &str) -> Answer {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"type": "error", "description": description})),
    )
}

fn accepted() -> Answer {
    (
        axum::http::StatusCode::OK,
        axum::Json(json!({"type": "success", "result": {}})),
    )
}

async fn on_post(state: Arc<Mutex<Upstream>>, body: Value) -> Answer {
    let (code, instruments) = named(&body);
    let mut s = state.lock();
    s.requests.push(("POST".into(), code, instruments.clone()));
    if s.fail_posts.pop_front().is_some() {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"type": "error", "description": "busy"})),
        );
    }
    let bad_instrument = instruments.iter().any(|i| s.refuse_instrument.contains(i));
    if s.refuse_post_code.contains(&code) || bad_instrument {
        return refused("instrument not allowed");
    }
    for i in instruments {
        s.active.insert((i, code));
    }
    accepted()
}

async fn on_put(state: Arc<Mutex<Upstream>>, body: Value) -> Answer {
    let (code, instruments) = named(&body);
    let gate = {
        let mut s = state.lock();
        s.requests.push(("PUT".into(), code, instruments.clone()));
        s.put_gate.take()
    };
    if let Some(gate) = gate {
        let _ = gate.await;
    }
    let mut s = state.lock();
    if s.refuse_put_code.contains(&code) {
        return refused("release denied");
    }
    for i in instruments {
        s.active.remove(&(i, code));
    }
    accepted()
}

/// The fake broker: its REST API, and a Socket.IO that takes connections
/// and accepts each only once the test says so, and can drop them all.
struct FakeBroker {
    upstream: Arc<Mutex<Upstream>>,
    rest: String,
    socket: String,
    /// Open while connections are accepted (the Socket.IO `40`).
    accept: watch::Sender<bool>,
    /// Moved to end every connection open now.
    drop_all: watch::Sender<u64>,
    tasks: JoinSet<()>,
}

/// Until the connections open at `epoch` are to end.
async fn dropped(drop_all: &mut watch::Receiver<u64>, epoch: u64) {
    let _ = drop_all.wait_for(|e| *e != epoch).await;
}

async fn socket_connection(
    tcp: tokio::net::TcpStream,
    mut accept: watch::Receiver<bool>,
    mut drop_all: watch::Receiver<u64>,
) {
    let epoch = *drop_all.borrow_and_update();
    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
        return;
    };
    let open = r#"0{"sid":"s1","upgrades":[],"pingInterval":25000,"pingTimeout":20000}"#;
    if ws.send(Message::Text(open.into())).await.is_err() {
        return;
    }
    while let Some(Ok(m)) = ws.next().await {
        if matches!(m, Message::Text(ref t) if t == "40") {
            break;
        }
    }
    if accept.wait_for(|open| *open).await.is_err() {
        return;
    }
    if ws
        .send(Message::Text(r#"40{"sid":"n1"}"#.into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            () = dropped(&mut drop_all, epoch) => {
                let _ = ws.close(None).await;
                return;
            }
            m = ws.next() => match m {
                Some(Ok(Message::Ping(v))) => { let _ = ws.send(Message::Pong(v)).await; }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

impl FakeBroker {
    async fn start(accepting: bool) -> FakeBroker {
        let upstream: Arc<Mutex<Upstream>> = Arc::default();
        let (posted, put) = (upstream.clone(), upstream.clone());
        let app = axum::Router::new().route(
            "/apimarketdata/instruments/subscription",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                on_post(posted.clone(), body)
            })
            .put(move |axum::Json(body): axum::Json<Value>| on_put(put.clone(), body)),
        );
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rest = format!("http://{}", http.local_addr().unwrap());
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let _ = axum::serve(http, app).await;
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = format!("ws://{}", listener.local_addr().unwrap());
        let (accept, accepted) = watch::channel(accepting);
        let (drop_all, every) = watch::channel(0);
        tasks.spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((tcp, _)) = listener.accept().await {
                connections.spawn(socket_connection(tcp, accepted.clone(), every.clone()));
            }
        });
        FakeBroker {
            upstream,
            rest,
            socket,
            accept,
            drop_all,
            tasks,
        }
    }

    fn requests(&self) -> Vec<(String, i64, Vec<i64>)> {
        self.upstream.lock().requests.clone()
    }

    fn holds(&self, instrument: i64, code: i64) -> bool {
        self.upstream.lock().active.contains(&(instrument, code))
    }
}

/// The running stack over a [`FakeBroker`].
struct Stack {
    broker: FakeBroker,
    manager: Arc<WebSocketManager>,
    bridge: Arc<BrokerBridge>,
    server: FeedHandle,
}

fn row(symbol: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: format!("{symbol}-EQ"),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSECM".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

impl Stack {
    async fn up(accepting: bool, op_timeout: Duration) -> Stack {
        let broker = FakeBroker::start(accepting).await;
        let symbols = SymbolResolver::new();
        symbols.load(vec![
            row("RELIANCE", &RELIANCE.to_string()),
            row("SBIN", &SBIN.to_string()),
        ]);
        let xts = XtsBroker::with_base_url(
            &openalgo_desktop_lib::brokers::compositedge::CONFIG,
            symbols.clone(),
            broker.rest.clone(),
        )
        .with_socket_base(broker.socket.clone());
        let token = AuthToken::new("inert-interactive")
            .with_feed(Some("inert-feed"))
            .with_user_id("inert-user");
        let manager = Arc::new(WebSocketManager::new());
        manager
            .connect(xts.create_feed(&token).unwrap())
            .await
            .unwrap();
        let bridge = BrokerBridge::new(manager.clone(), symbols);
        bridge.start();
        let server = start(
            FeedConfig {
                port: 0,
                op_timeout,
                ..FeedConfig::default()
            },
            FeedDeps {
                source: bridge.clone(),
                auth: Arc::new(StaticAuth {
                    api_key: API_KEY.into(),
                    user_id: USER_ID.into(),
                    broker: Some("compositedge".into()),
                }),
                orders: None,
                supported_brokers: brokers(),
            },
        )
        .await
        .unwrap();
        Stack {
            broker,
            manager,
            bridge,
            server,
        }
    }

    async fn connected() -> Stack {
        let s = Stack::up(true, Duration::from_secs(60)).await;
        assert!(eventually(SETTLE, || s.manager.is_connected()).await);
        s
    }

    async fn client(&self) -> Client {
        let mut c = Client::connect(&format!("ws://{}", self.server.local_addr())).await;
        assert_eq!(c.auth().await["status"], "success");
        c
    }

    fn registered(&self) -> usize {
        self.server.stats().client_subscriptions
    }

    fn manager_mode(&self, symbol: &str) -> Option<FeedMode> {
        self.manager
            .subscriptions()
            .into_iter()
            .find(|s| s.symbol == symbol)
            .map(|s| s.mode)
    }

    async fn stop(mut self) {
        self.server.stop().await;
        self.bridge.stop().await;
        self.manager.disconnect().await.unwrap();
        self.broker.tasks.abort_all();
    }
}

fn subscribe(symbol: &str, mode: i64) -> Value {
    json!({"action": "subscribe", "symbols": [{"symbol": symbol, "exchange": "NSE"}], "mode": mode})
}

fn unsubscribe(symbol: &str, mode: i64) -> Value {
    json!({"action": "unsubscribe", "symbol": symbol, "exchange": "NSE", "mode": mode})
}

/// An admission on the same REST worker: once it is answered, what was
/// before it on that worker has run.
async fn barrier(c: &mut Client) {
    let ack = c.request(subscribe("SBIN", 1)).await;
    assert_eq!(ack["status"], "success", "worker barrier: {}", ack);
}

fn refused_message(ack: &Value) -> String {
    ack["subscriptions"][0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// B1: A's release is with the broker; B joins and gives up; A asks again
/// and is not told success; the broker refuses. A stays the owner, and
/// every layer says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_waits_for_the_broker_after_a_joiner_gives_up() {
    let s = Stack::up(true, Duration::from_secs(2)).await;
    assert!(eventually(SETTLE, || s.manager.is_connected()).await);
    let mut a = s.client().await;
    let ack = a.request(subscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    let (release_put, gate) = oneshot::channel();
    {
        let mut up = s.broker.upstream.lock();
        up.refuse_put_code.insert(QUOTE);
        up.put_gate = Some(gate);
    }

    let first = a.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(first["status"], "error", "not answered in time: {}", first);
    let mut b = s.client().await;
    let joined = b.request(subscribe("RELIANCE", 2)).await;
    assert_eq!(
        joined["status"], "partial",
        "behind the release: {}",
        joined
    );
    assert_eq!(s.registered(), 1, "B left: only A holds it");

    let retry = a.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(
        retry["status"], "error",
        "never success on an unsettled release: {}",
        retry
    );

    release_put.send(()).unwrap();
    barrier(&mut a).await;
    assert_eq!(s.registered(), 2, "A holds RELIANCE still, and SBIN");
    assert_eq!(s.manager.instrument_count(), 2);
    let applied: Vec<String> = s.bridge.applied().into_iter().map(|x| x.symbol).collect();
    assert!(applied.contains(&"RELIANCE".to_string()), "{:?}", applied);
    assert!(s.broker.holds(RELIANCE, QUOTE), "the broker kept streaming");
    assert!(
        eventually(SETTLE, || s.bridge.operations().release_retrying == 0).await,
        "A owns it, so it is not a release to try again"
    );

    // A release that goes through ends it for everyone.
    s.broker.upstream.lock().refuse_put_code.clear();
    let done = a.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(done["status"], "success", "{}", done);
    assert_eq!(s.registered(), 1);
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, QUOTE)).await);
    s.stop().await;
}

/// B1: a refusal that finds no owner left brings nothing back: the bridge
/// lets go again, and the upstream subscription goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_release_with_no_owner_left_is_released_again() {
    let s = Stack::up(true, Duration::from_secs(2)).await;
    assert!(eventually(SETTLE, || s.manager.is_connected()).await);
    let mut a = s.client().await;
    let ack = a.request(subscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    let (release_put, gate) = oneshot::channel();
    {
        let mut up = s.broker.upstream.lock();
        up.refuse_put_code.insert(QUOTE);
        up.put_gate = Some(gate);
    }
    let first = a.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(first["status"], "error", "{}", first);
    drop(a);
    assert!(eventually(SETTLE, || s.registered() == 0).await);

    release_put.send(()).unwrap();
    assert!(
        eventually(SETTLE, || s
            .broker
            .requests()
            .iter()
            .filter(|r| r.0 == "PUT")
            .count()
            >= 2)
        .await,
        "released again"
    );
    s.broker.upstream.lock().refuse_put_code.clear();
    assert!(eventually(SETTLE, || s.manager.instrument_count() == 0).await);
    assert!(eventually(SETTLE, || s.bridge.applied().is_empty()).await);
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, QUOTE)).await);
    s.stop().await;
}

/// B2: a subscribe made before the session is up hears the broker's own
/// answer to the set that carries it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribe_before_the_session_is_up_hears_the_refusal() {
    let s = Stack::up(false, Duration::from_secs(60)).await;
    let mut c = s.client().await;
    s.broker.upstream.lock().refuse_post_code.insert(QUOTE);
    c.send(&subscribe("RELIANCE", 2)).await;
    assert!(eventually(SETTLE, || s.manager.instrument_count() == 1).await);
    assert!(
        c.recv_within(Duration::from_millis(300)).await.is_none(),
        "no answer until the broker gave one"
    );
    s.broker.accept.send_replace(true);
    let ack = c.recv().await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert!(
        refused_message(&ack).contains("instrument not allowed"),
        "{}",
        ack
    );
    assert_eq!(s.registered(), 0);
    assert_eq!(s.manager.instrument_count(), 0);
    s.stop().await;
}

/// B2: and what the broker takes is a success only once it took it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribe_before_the_session_is_up_is_taken_when_the_broker_takes_it() {
    let s = Stack::up(false, Duration::from_secs(60)).await;
    let mut c = s.client().await;
    c.send(&subscribe("RELIANCE", 2)).await;
    assert!(eventually(SETTLE, || s.manager.instrument_count() == 1).await);
    assert!(c.recv_within(Duration::from_millis(300)).await.is_none());
    s.broker.accept.send_replace(true);
    let ack = c.recv().await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert!(s.broker.holds(RELIANCE, QUOTE));
    s.stop().await;
}

/// B2: a 503 is not an answer: the retried subscribe keeps its waiter, and
/// the refusal that follows reaches the owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_after_a_passing_failure_reaches_the_owner() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    {
        let mut up = s.broker.upstream.lock();
        up.fail_posts.push_back(());
        up.refuse_post_code.insert(QUOTE);
    }
    let ack = c.request(subscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert!(
        refused_message(&ack).contains("instrument not allowed"),
        "{}",
        ack
    );
    assert_eq!(s.registered(), 0);
    assert_eq!(s.manager.instrument_count(), 0);
    let posts = s.broker.requests().iter().filter(|r| r.0 == "POST").count();
    assert_eq!(posts, 2, "the failed one and its retry");
    s.stop().await;
}

/// B2: at a reconnect one refused instrument does not stop the others; it
/// is let go of and the trader told on the Health page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_lets_go_only_of_what_the_broker_refuses() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    let both = json!({"action": "subscribe", "mode": 1, "symbols": [
        {"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "SBIN", "exchange": "NSE"}]});
    let ack = c.request(both).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert!(s.broker.holds(RELIANCE, LTP) && s.broker.holds(SBIN, LTP));

    s.broker.upstream.lock().refuse_instrument.insert(RELIANCE);
    s.broker.upstream.lock().active.clear();
    s.broker.drop_all.send_modify(|epoch| *epoch += 1);

    assert!(
        eventually(SETTLE, || s.broker.holds(SBIN, LTP)).await,
        "SBIN streams"
    );
    assert!(
        eventually(SETTLE, || s.manager_mode("RELIANCE").is_none()).await,
        "RELIANCE is let go"
    );
    assert_eq!(s.manager_mode("SBIN"), Some(FeedMode::Ltp));
    assert!(eventually(SETTLE, || !s.bridge.operations().refused.is_empty()).await);
    let refused = s.bridge.operations().refused;
    assert_eq!(refused.len(), 1, "{:?}", refused);
    assert_eq!(refused[0].symbol, "RELIANCE");
    assert!(
        refused[0].reason.contains("instrument not allowed"),
        "{:?}",
        refused
    );
    s.stop().await;
}

/// B3: the broker refuses to release Quote: the Quote owner keeps it and
/// hears why, and the LTP owner never loses its stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_release_keeps_the_quote_owner() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    assert_eq!(
        c.request(subscribe("RELIANCE", 1)).await["status"],
        "success"
    );
    assert_eq!(
        c.request(subscribe("RELIANCE", 2)).await["status"],
        "success"
    );
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, LTP)).await);
    s.broker.upstream.lock().refuse_put_code.insert(QUOTE);

    let ack = c.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "error", "{}", ack);
    let why = ack["failed"][0]["message"].as_str().unwrap_or_default();
    assert!(
        why.contains("release denied") && why.contains("Unsubscribe again"),
        "{}",
        ack
    );
    assert_eq!(s.registered(), 2, "LTP and Quote are both held");
    assert_eq!(s.manager_mode("RELIANCE"), Some(FeedMode::Quote));
    assert!(s.broker.holds(RELIANCE, QUOTE));
    // The lower code taken for the attempt is not left behind.
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, LTP)).await);
    s.stop().await;
}

/// B3: the broker refuses the lower code: nothing is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_lower_code_releases_nothing() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    assert_eq!(
        c.request(subscribe("RELIANCE", 1)).await["status"],
        "success"
    );
    assert_eq!(
        c.request(subscribe("RELIANCE", 2)).await["status"],
        "success"
    );
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, LTP)).await);
    s.broker.upstream.lock().refuse_post_code.insert(LTP);
    let before = s.broker.requests().len();

    let ack = c.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "error", "{}", ack);
    let why = ack["failed"][0]["message"].as_str().unwrap_or_default();
    assert!(why.contains("Unsubscribe again"), "{}", ack);
    assert!(
        !s.broker.requests()[before..]
            .iter()
            .any(|r| r.0 == "PUT" && r.1 == QUOTE),
        "the Quote code is not released: {:?}",
        s.broker.requests()
    );
    assert!(s.broker.holds(RELIANCE, QUOTE));
    assert_eq!(s.manager_mode("RELIANCE"), Some(FeedMode::Quote));
    assert_eq!(s.registered(), 2);
    s.stop().await;
}

/// B3: a downgrade the broker takes: the lower code first, the higher
/// released once it is, and the Quote owner is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_downgrade_takes_the_lower_code_before_it_releases_the_higher() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    assert_eq!(
        c.request(subscribe("RELIANCE", 1)).await["status"],
        "success"
    );
    assert_eq!(
        c.request(subscribe("RELIANCE", 2)).await["status"],
        "success"
    );
    assert!(eventually(SETTLE, || !s.broker.holds(RELIANCE, LTP)).await);
    let before = s.broker.requests().len();

    let ack = c.request(unsubscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "success", "{}", ack);
    let requests = s.broker.requests()[before..].to_vec();
    assert_eq!(
        requests,
        [
            ("POST".to_string(), LTP, vec![RELIANCE]),
            ("PUT".to_string(), QUOTE, vec![RELIANCE]),
        ],
    );
    assert_eq!(s.manager_mode("RELIANCE"), Some(FeedMode::Ltp));
    assert!(s.broker.holds(RELIANCE, LTP) && !s.broker.holds(RELIANCE, QUOTE));
    assert_eq!(s.registered(), 1);
    s.stop().await;
}

/// Review X2, on Codex's scenario: LTP held, Quote refused. The Quote
/// subscriber hears it, and the LTP stream stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ltp_to_quote_refused_keeps_the_ltp_stream() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    assert_eq!(
        c.request(subscribe("RELIANCE", 1)).await["status"],
        "success"
    );
    s.broker.upstream.lock().refuse_post_code.insert(QUOTE);
    let before = s.broker.requests().len();

    let ack = c.request(subscribe("RELIANCE", 2)).await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert!(
        refused_message(&ack).contains("instrument not allowed"),
        "{}",
        ack
    );
    barrier(&mut c).await;
    assert!(
        !s.broker.requests()[before..]
            .iter()
            .any(|r| r.0 == "PUT" && r.1 == LTP),
        "{:?}",
        s.broker.requests()
    );
    assert!(s.broker.holds(RELIANCE, LTP));
    assert_eq!(s.manager_mode("RELIANCE"), Some(FeedMode::Ltp));
    assert_eq!(s.registered(), 2, "RELIANCE LTP and the barrier's SBIN");
    s.stop().await;
}

/// Review X2, the next step up: Quote held, Depth refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quote_to_depth_refused_keeps_the_quote_stream() {
    let s = Stack::connected().await;
    let mut c = s.client().await;
    assert_eq!(
        c.request(subscribe("RELIANCE", 2)).await["status"],
        "success"
    );
    s.broker.upstream.lock().refuse_post_code.insert(DEPTH);
    let before = s.broker.requests().len();

    let ack = c.request(subscribe("RELIANCE", 3)).await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert!(
        refused_message(&ack).contains("instrument not allowed"),
        "{}",
        ack
    );
    barrier(&mut c).await;
    assert!(
        !s.broker.requests()[before..]
            .iter()
            .any(|r| r.0 == "PUT" && r.1 == QUOTE),
        "{:?}",
        s.broker.requests()
    );
    assert!(s.broker.holds(RELIANCE, QUOTE));
    assert_eq!(s.manager_mode("RELIANCE"), Some(FeedMode::Quote));
    s.stop().await;
}

// The large-batch scenario (a refused batch crossing the old 20 s deadline)
// is feed_corpus::compositedge::one_of_fifty_refused.
