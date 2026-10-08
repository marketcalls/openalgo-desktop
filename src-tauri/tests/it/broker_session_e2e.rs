//! A broker session end to end, on the real `AppState`, feed server and
//! managers, with `MockBroker` (registered as `zerodha`) and fake broker
//! sockets speaking the `MockFeed` protocol:
//!
//! sign-in by OAuth callback -> master contract download (the Socket.IO
//! `master_contract_download` and `cache_loaded` pushes, the status row)
//! -> market feed connected -> an 8765 client subscribes and receives the
//! broker's tick -> a broker order update reaches the bus (`order.update`,
//! the Socket.IO `order_update` push) and the 8765 order stream -> logout
//! tears it all down: owned tasks, sockets (descriptor count), manager and
//! bridge subscriptions back to their baseline, the adapter's logout hook
//! called. Ports are ephemeral; 5000/8765 are never used.

use crate::feed_support::{fd_count, Client};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use openalgo_desktop_lib::brokers::mock::MockBroker;
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
use openalgo_desktop_lib::db::sqlite::{master_contract_status, user};
use openalgo_desktop_lib::events::subscribers::socketio::SocketIoSubscriber;
use openalgo_desktop_lib::events::subscribers::UiEmitter;
use openalgo_desktop_lib::events::{Event, Lane, SessionEndReason, Subscriber, Topic};
use openalgo_desktop_lib::feed::FeedService;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::broker_auth_service::{BrokerAuthService, CallbackOrigin};
use openalgo_desktop_lib::state::{AppState, OpenOptions, ServerStatus};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// What the Socket.IO clients would receive.
#[derive(Default)]
struct SocketPushes(Mutex<Vec<(String, Value)>>);

#[async_trait::async_trait]
impl UiEmitter for SocketPushes {
    async fn emit(&self, event: &str, payload: Value) {
        self.0.lock().push((event.to_string(), payload));
    }
}

impl SocketPushes {
    fn named(&self, name: &str) -> Vec<Value> {
        self.0
            .lock()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

/// `order.update` events on the bus.
#[derive(Default)]
struct OrderEvents(Mutex<Vec<Arc<Event>>>);

#[async_trait::async_trait]
impl Subscriber for OrderEvents {
    fn name(&self) -> &'static str {
        "e2e-orders"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![Topic::OrderUpdate]
    }
    async fn handle(&self, event: Arc<Event>) {
        self.0.lock().push(event);
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {}", what);
}

/// A fake broker market feed: answers every subscribe with a tick.
async fn market_server() -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = l.accept().await {
            conns.spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                while let Some(Ok(m)) = ws.next().await {
                    if let Message::Text(t) = m {
                        if t.contains("\"sub\"") && t.contains("SBIN") {
                            let tick = json!({"t": "SBIN", "x": "NSE", "p": 812.5}).to_string();
                            let _ = ws.send(Message::Text(tick)).await;
                        }
                    }
                }
            });
        }
    });
    (url, task)
}

/// A fake broker order socket: sends one order update when `go` fires.
async fn order_server(go: Arc<Notify>) -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = l.accept().await {
            let go = go.clone();
            conns.spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let (mut w, mut r) = ws.split();
                let reader = tokio::spawn(async move { while let Some(Ok(_)) = r.next().await {} });
                go.notified().await;
                let order = json!({"order": {
                    "orderid": "251003000123", "symbol": "SBIN", "exchange": "NSE",
                    "action": "BUY", "quantity": 10, "price": 812.5, "trigger_price": 0.0,
                    "pricetype": "LIMIT", "product": "MIS", "order_status": "complete",
                    "filled_quantity": 10, "pending_quantity": 0, "average_price": 812.45,
                    "rejection_reason": ""
                }});
                let _ = w.send(Message::Text(order.to_string())).await;
                let _ = reader.await;
            });
        }
    });
    (url, task)
}

fn sbin() -> SymToken {
    SymToken {
        symbol: "SBIN".into(),
        brsymbol: "SBIN".into(),
        name: "STATE BANK OF INDIA".into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: "779521::::3045".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// The `state` on a Kite login URL (`redirect_params=state%3D<state>`).
fn state_from_kite_url(url: &str) -> String {
    let u = url::Url::parse(url).unwrap();
    let rp = u
        .query_pairs()
        .find(|(k, _)| k == "redirect_params")
        .unwrap()
        .1
        .to_string();
    rp.strip_prefix("state=").unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_streams_and_logout_tears_everything_down() {
    crate::isolated!(login_streams_and_logout_tears_everything_down);

    let order_go = Arc::new(Notify::new());
    let (feed_url, market_task) = market_server().await;
    let (orders_url, order_task) = order_server(order_go.clone()).await;

    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockBroker::new("zerodha"));
    *mock.master.lock() = Some(Ok(vec![sbin()]));
    *mock.feed_url.lock() = Some(feed_url);
    *mock.order_feed_url.lock() = Some(orders_url);
    let registry =
        BrokerRegistry::with_symbols(mock.symbols.clone(), vec![mock.clone() as Arc<dyn Broker>]);
    let ctx = AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(registry),
        },
    )
    .unwrap();
    let pushes = Arc::new(SocketPushes::default());
    ctx.bus.subscribe(
        Arc::new(SocketIoSubscriber::new(pushes.clone())),
        Lane::BestEffort,
    );
    let orders = Arc::new(OrderEvents::default());
    ctx.bus.subscribe(orders.clone(), Lane::BestEffort);

    // The trader's account, API key and broker app credentials.
    {
        let conn = ctx.sqlite.conn().unwrap();
        user::insert(
            &conn,
            &ctx.security,
            "alice",
            "alice@example.com",
            "not-a-real-hash",
            "JBSWY3DPEHPK3PXP",
        )
        .unwrap();
        credentials::save(
            &conn,
            &ctx.security,
            "zerodha",
            CredentialUpdate {
                api_key: Some(Secret::new("kitekey")),
                api_secret: Some(Secret::new("kitesecret")),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let key = ApiKeyService::regenerate(&ctx, "alice").unwrap();

    // The 8765 feed server, on an ephemeral port.
    {
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut c = ctx.config.write();
        c.bind_host = "127.0.0.1".into();
        c.ws_port = port;
    }
    let feed = FeedService::new(ctx.clone());
    assert!(matches!(feed.start().await, ServerStatus::Running { .. }));
    assert!(matches!(
        &*ctx.feed_status.read(),
        ServerStatus::Running { .. }
    ));
    let mut client = Client::connect(&format!("ws://{}", feed.local_addr().await.unwrap())).await;

    // Baseline: app running, a feed client connected, no broker session.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let fds_before = fd_count();
    let app_tasks_before = ctx.task_count();
    assert_eq!(ctx.runtime.task_count(), 0);

    // ---- sign in: the OAuth callback with the server-issued state ----
    let url = BrokerAuthService::start_oauth(&ctx, "zerodha", Some("browser-session"))
        .await
        .unwrap();
    let mut params = HashMap::new();
    params.insert("request_token".to_string(), "rt-1".to_string());
    params.insert("state".to_string(), state_from_kite_url(&url));
    let session = BrokerAuthService::complete_oauth(
        &ctx,
        "zerodha",
        &params,
        CallbackOrigin::Redirect {
            session_id: Some("browser-session"),
        },
    )
    .await
    .unwrap();
    assert_eq!(session.broker_id, "zerodha");
    assert_eq!(ctx.runtime.active_broker().as_deref(), Some("zerodha"));

    // ---- master contract: downloaded (never before), loaded, announced ----
    until("the master contract download push", || {
        !pushes.named("master_contract_download").is_empty()
    })
    .await;
    assert_eq!(
        pushes.named("master_contract_download"),
        vec![json!({"status": "success", "message": "Successfully Downloaded"})]
    );
    until("the cache_loaded push", || {
        !pushes.named("cache_loaded").is_empty()
    })
    .await;
    let cache = &pushes.named("cache_loaded")[0];
    assert_eq!(cache["status"], "success");
    assert_eq!(cache["broker"], "zerodha");
    assert_eq!(cache["total_symbols"], 1);
    assert_eq!(ctx.symbol_count(), 1);
    {
        let conn = ctx.sqlite.conn().unwrap();
        let row = master_contract_status::get(&conn, "zerodha", ctx.now())
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "success");
        assert!(row.is_ready);
        assert_eq!(row.total_symbols, "1");
        assert!(row.last_download_time.is_some());
    }
    // The web's session-count push for the new broker session.
    assert!(!pushes.named("active_sessions_update").is_empty());

    // ---- market feed connected; an 8765 client gets the broker's tick ----
    until("the broker feed", || ctx.websocket.is_connected()).await;
    let v = client
        .request(json!({"action": "authenticate", "api_key": key.expose()}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    assert_eq!(v["broker"], "zerodha");
    let v = client
        .request(json!({"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "mode": 1}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    let tick = loop {
        let v = client.recv().await;
        if v["type"] == "market_data" {
            break v;
        }
    };
    assert_eq!(tick["symbol"], "SBIN");
    assert_eq!(tick["exchange"], "NSE");
    assert_eq!(tick["data"]["ltp"], 812.5);
    assert_eq!(ctx.websocket.instrument_count(), 1);
    assert_eq!(ctx.bridge.applied().len(), 1);

    // ---- an order update: bus, Socket.IO and the 8765 order stream ----
    let v = client
        .request(json!({"action": "subscribe_orders", "request_id": "o1"}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    until("the order socket", || ctx.runtime.order_ws.is_connected()).await;
    order_go.notify_one();
    let frame = loop {
        let v = client.recv().await;
        if v["type"] == "order_update" {
            break v;
        }
    };
    assert_eq!(frame["orderid"], "251003000123");
    assert_eq!(frame["mode"], "live");
    assert_eq!(frame["broker"], "zerodha");
    assert_eq!(frame["user_id"], "alice");
    assert_eq!(frame["order_status"], "complete");
    assert_eq!(frame.as_object().unwrap().len(), 18);
    until("order.update on the bus", || !orders.0.lock().is_empty()).await;
    match &*orders.0.lock()[0] {
        Event::OrderUpdate(u) => {
            assert_eq!((u.mode.as_str(), u.broker.as_str()), ("live", "zerodha"));
            assert_eq!(u.filled_quantity, 10);
            assert_eq!(u.average_price, 812.45);
        }
        other => panic!("unexpected {:?}", other),
    }
    until("the order_update push", || {
        !pushes.named("order_update").is_empty()
    })
    .await;
    let push = &pushes.named("order_update")[0];
    assert_eq!(push["orderid"], "251003000123");
    assert_eq!(push["mode"], "live");
    assert!(ctx.runtime.task_count() > 0);

    // ---- logout: everything the session ran is gone ----
    BrokerAuthService::revoke(&ctx, SessionEndReason::Logout)
        .await
        .unwrap();
    assert_eq!(ctx.runtime.task_count(), 0, "session tasks left running");
    assert_eq!(ctx.task_count(), app_tasks_before);
    assert!(!ctx.websocket.is_running());
    assert!(!ctx.runtime.order_ws.is_running());
    assert!(!ctx.runtime.depth_ws.is_running());
    assert_eq!(ctx.websocket.instrument_count(), 0);
    assert!(ctx.bridge.applied().is_empty());
    assert!(ctx.bridge.applied_deep().is_empty());
    assert_eq!(ctx.symbol_count(), 0);
    assert_eq!(*mock.logouts.lock(), 1);
    assert!(ctx.runtime.active_broker().is_none());
    assert!(ctx.get_broker_session().is_none());
    // Descriptors back to the baseline: both broker sockets are closed.
    let mut fds_after = fd_count();
    for _ in 0..50 {
        if fds_after <= fds_before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        fds_after = fd_count();
    }
    eprintln!(
        "broker session fds: before={} after={}",
        fds_before, fds_after
    );
    assert!(
        fds_after <= fds_before,
        "descriptors left open after logout: {} -> {}",
        fds_before,
        fds_after
    );
    // A second teardown (shutdown after logout) is harmless.
    ctx.runtime.teardown(&ctx).await;
    assert_eq!(*mock.logouts.lock(), 1);

    drop(client);
    feed.stop().await;
    market_task.abort();
    order_task.abort();
    ctx.shutdown().await;
}

/// A session resumed after a restart rebuilds adapter state from the
/// stored credentials, loads the stored master (no second download the same
/// day) and starts streaming again; the second sign-in path (form fields,
/// state-less callbacks) is covered by the HTTP tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_restores_credentials_and_uses_the_stored_master() {
    use openalgo_desktop_lib::brokers::mock::MockCall;
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockBroker::new("zerodha"));
    *mock.master.lock() = Some(Ok(vec![sbin()]));
    let registry =
        BrokerRegistry::with_symbols(mock.symbols.clone(), vec![mock.clone() as Arc<dyn Broker>]);
    // 10:00 IST: after the 08:00 cutoff, so the day's download is reused.
    let ten_ist =
        chrono::TimeZone::with_ymd_and_hms(&chrono_tz::Asia::Kolkata, 2026, 10, 5, 10, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
    let ctx = AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: openalgo_desktop_lib::clock::ManualClock::new(ten_ist),
            brokers: Arc::new(registry),
        },
    )
    .unwrap();
    {
        let conn = ctx.sqlite.conn().unwrap();
        credentials::save(
            &conn,
            &ctx.security,
            "zerodha",
            CredentialUpdate {
                api_key: Some(Secret::new("kitekey")),
                api_secret: Some(Secret::new("kitesecret")),
                api_key_market: Some(Secret::new("mdkey")),
                api_secret_market: Some(Secret::new("mdsecret")),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let url = BrokerAuthService::start_oauth(&ctx, "zerodha", None)
        .await
        .unwrap();
    let mut params = HashMap::new();
    params.insert("request_token".to_string(), "rt-1".to_string());
    params.insert("state".to_string(), state_from_kite_url(&url));
    BrokerAuthService::complete_oauth(
        &ctx,
        "zerodha",
        &params,
        CallbackOrigin::Redirect { session_id: None },
    )
    .await
    .unwrap();
    // The login carried the stored market-data keys too (XTS needs them).
    let creds = mock.last_auth.lock().clone().unwrap();
    assert_eq!(creds.api_key_market.as_deref(), Some("mdkey"));
    assert_eq!(creds.api_secret_market.as_deref(), Some("mdsecret"));
    until("the first download", || ctx.symbol_count() == 1).await;

    // The app closes without logging out, and comes back.
    ctx.runtime.teardown(&ctx).await;
    ctx.set_broker_session(None);
    ctx.clear_symbol_cache();
    let resumed = BrokerAuthService::try_resume(&ctx).await.unwrap();
    assert!(resumed.is_some());
    let restored = mock.restored.lock().clone().unwrap();
    assert_eq!(restored.api_key, "kitekey");
    assert_eq!(restored.api_key_market.as_deref(), Some("mdkey"));
    assert!(restored.password.is_none() && restored.totp.is_none());
    until("the stored master", || ctx.symbol_count() == 1).await;
    let downloads = mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::MasterContract))
        .count();
    assert_eq!(
        downloads, 1,
        "the same day's master is not downloaded again"
    );
    {
        let conn = ctx.sqlite.conn().unwrap();
        let row = master_contract_status::get(&conn, "zerodha", ctx.now())
            .unwrap()
            .unwrap();
        assert_eq!(row.message, "Using cached master contract");
        assert!(row.is_ready);
    }
    ctx.shutdown().await;
    assert_eq!(ctx.runtime.task_count(), 0);
}
