//! Telegram and WhatsApp: the bots, their routes, alerts and notify
//! endpoints, driven in-process against the full router.
//!
//! Telegram runs against a local fake Bot API server (axum); nothing talks
//! to Telegram. WhatsApp runs with its socket pointed at a closed local port
//! and its HTTP refused, so nothing talks to WhatsApp either; what needs a
//! real phone is in docs/port/messaging-manual-test.md.
//!
//! Self-contained so it can be folded into a consolidated integration crate.

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::mock::MockBroker;
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::events::subscribers::UiEmitter;
use openalgo_desktop_lib::events::{Event, Mode, OrderMeta};
use openalgo_desktop_lib::messaging::telegram::{db as tg_db, Timing};
use openalgo_desktop_lib::messaging::whatsapp::{self, ConnSettings, PAIR_TIMEOUT_MESSAGE};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The descriptor checks count the whole process, so tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use tower::ServiceExt;

const USER: &str = "trader";
const TOKEN: &str = "123456:TEST-TOKEN";

fn now() -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(2026, 10, 7, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

// ------------------------------------------------------------ fake Bot API

#[derive(Default)]
struct Fake {
    updates: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
    fail_updates: AtomicUsize,
    next_id: AtomicI64,
    wake: tokio::sync::Notify,
}

impl Fake {
    fn push(&self, u: Value) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let mut u = u;
        u["update_id"] = json!(id);
        self.updates.lock().push_back(u);
        self.wake.notify_waiters();
    }

    fn sent(&self) -> Vec<Value> {
        self.calls
            .lock()
            .iter()
            .filter(|(m, _)| m == "sendMessage" || m == "editMessageText")
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn texts(&self) -> Vec<String> {
        self.sent()
            .iter()
            .map(|b| b["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    fn count(&self, method: &str) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }
}

async fn fake_handler(
    State(f): State<Arc<Fake>>,
    Path((bot, method)): Path<(String, String)>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    if bot != format!("bot{}", TOKEN) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"})),
        );
    }
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(json!({"multipart": true}));
    if method != "getUpdates" {
        f.calls.lock().push((method.clone(), parsed.clone()));
    }
    match method.as_str() {
        "getMe" => (
            StatusCode::OK,
            Json(json!({"ok": true, "result": {"id": 1, "is_bot": true, "username": "test_bot"}})),
        ),
        "getUpdates" => {
            if f.fail_updates.load(Ordering::SeqCst) > 0 {
                f.fail_updates.fetch_sub(1, Ordering::SeqCst);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error_code": 500, "description": "boom"})),
                );
            }
            let offset = parsed["offset"].as_i64().unwrap_or(0);
            let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
            loop {
                let ready: Vec<Value> = {
                    let mut q = f.updates.lock();
                    q.retain(|u| u["update_id"].as_i64().unwrap_or(0) >= offset);
                    q.iter().cloned().collect()
                };
                if !ready.is_empty() || tokio::time::Instant::now() >= deadline {
                    return (StatusCode::OK, Json(json!({"ok": true, "result": ready})));
                }
                let _ = tokio::time::timeout(Duration::from_millis(50), f.wake.notified()).await;
            }
        }
        "sendMessage" | "editMessageText" | "sendPhoto" | "sendMediaGroup" => {
            let mid = f.next_id.fetch_add(1, Ordering::SeqCst) + 1000;
            (
                StatusCode::OK,
                Json(
                    json!({"ok": true, "result": {"message_id": mid, "chat": {"id": parsed["chat_id"]}}}),
                ),
            )
        }
        _ => (StatusCode::OK, Json(json!({"ok": true, "result": true}))),
    }
}

async fn spawn_fake() -> (Arc<Fake>, String, tokio::task::JoinHandle<()>) {
    let f = Arc::new(Fake::default());
    let app = Router::new()
        .route("/{bot}/{method}", post(fake_handler))
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let j = tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (f, format!("http://{}", addr), j)
}

/// A TCP listener that only counts connections (outbound-request probe).
async fn spawn_probe() -> (Arc<AtomicUsize>, u16, tokio::task::JoinHandle<()>) {
    let n = Arc::new(AtomicUsize::new(0));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let c = n.clone();
    let j = tokio::spawn(async move {
        while let Ok((_s, _)) = l.accept().await {
            c.fetch_add(1, Ordering::SeqCst);
        }
    });
    (n, port, j)
}

async fn wait_for(mut f: impl FnMut() -> bool, ms: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

// ------------------------------------------------------------ harness

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Value)>>);

#[async_trait::async_trait]
impl UiEmitter for Recorder {
    async fn emit(&self, event: &str, payload: Value) {
        self.0.lock().push((event.to_string(), payload));
    }
}

impl Recorder {
    fn named(&self, n: &str) -> Vec<Value> {
        self.0
            .lock()
            .iter()
            .filter(|(e, _)| e == n)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

struct H {
    ctx: Arc<AppState>,
    cookie: String,
    csrf: String,
    key: String,
    events: Arc<Recorder>,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

impl H {
    fn new(broker: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(now());
        let symbols = SymbolResolver::new();
        let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![mock as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        AuthService::setup(&ctx, USER, "trader@example.com", "Secret@123").unwrap();
        let key = ApiKeyService::current(&ctx)
            .unwrap()
            .unwrap()
            .expose()
            .to_string();
        if broker {
            BrokerAuthService::persist(
                &ctx,
                &BrokerSession {
                    broker_id: "zerodha".into(),
                    auth_token: "mock-access-token".into(),
                    feed_token: None,
                    user_id: "AB1234".into(),
                    user_name: None,
                    authenticated_at: ctx.now(),
                },
            )
            .unwrap();
        }
        let s = ctx.sessions.create(ctx.now());
        ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
        let events = Arc::new(Recorder::default());
        ctx.messaging
            .set_emitter(Some(events.clone() as Arc<dyn UiEmitter>));
        ctx.messaging.telegram.set_timing(Timing {
            poll_timeout_secs: 1,
            start_backoff: Duration::from_millis(5),
            poll_backoff: Duration::from_millis(10),
            poll_backoff_max: Duration::from_millis(40),
        });
        ctx.messaging.whatsapp.set_settings(ConnSettings {
            ws_url: Some("ws://127.0.0.1:9/ws/chat".into()),
            http_blocked: true,
            version: Some((2, 3000, 1)),
            pair_timeout: Duration::from_millis(800),
            reconnect_base: Duration::from_millis(20),
            reconnect_max: Duration::from_millis(100),
            first_save: Duration::from_millis(200),
            save_interval: Duration::from_millis(200),
        });
        H {
            ctx,
            cookie: format!("session={}", s.id),
            csrf: s.csrf_token,
            key,
            events,
            clock,
            _dir: dir,
        }
    }

    async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        session: bool,
        csrf: bool,
        accept_json: bool,
    ) -> (StatusCode, Vec<u8>) {
        let mut b = Request::builder().method(method).uri(path);
        if accept_json {
            b = b.header(header::ACCEPT, "application/json");
        }
        if session {
            b = b.header(header::COOKIE, &self.cookie);
        }
        if csrf {
            b = b.header("x-csrftoken", &self.csrf);
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = b.body(body).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        crate::with_host(&mut req, &self.ctx);
        let resp = openalgo_desktop_lib::server::app(self.ctx.clone())
            .oneshot(req)
            .await
            .unwrap();
        let s = resp.status();
        (
            s,
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, b) = self
            .raw(Method::POST, path, Some(body), true, true, true)
            .await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (s, b) = self.raw(Method::GET, path, None, true, false, true).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn api(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, b) = self
            .raw(Method::POST, path, Some(body), false, false, true)
            .await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn start_telegram(&self, base: &str) {
        self.ctx.messaging.telegram.set_api_base(base);
        let (s, v) = self.post("/telegram/config", json!({"token": TOKEN})).await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        let (s, v) = self.post("/telegram/bot/start", json!({})).await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        assert_eq!(
            v,
            json!({"status": "success", "message": "Bot started successfully"})
        );
    }

    fn link(&self, telegram_id: i64) {
        let c = self.ctx.sqlite.conn().unwrap();
        tg_db::create_or_update_user(
            &c,
            &self.ctx.security,
            &tg_db::Link {
                telegram_id,
                username: USER,
                api_key: Some(&self.key),
                host_url: Some("http://127.0.0.1:5000"),
                first_name: "Asha",
                last_name: "",
                telegram_username: "asha",
                broker: "zerodha",
            },
            self.ctx.now(),
        )
        .unwrap();
    }
}

fn msg(from: i64, chat: i64, text: &str) -> Value {
    json!({"message": {"message_id": 1, "text": text,
        "chat": {"id": chat, "type": if chat == from { "private" } else { "group" }},
        "from": {"id": from, "first_name": "Asha", "username": "asha"}}})
}

fn button(from: i64, chat: i64, data: &str) -> Value {
    json!({"callback_query": {"id": "cb1", "data": data,
        "from": {"id": from, "first_name": "Asha"},
        "message": {"message_id": 77, "chat": {"id": chat}}}})
}

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

// ------------------------------------------------------------ Telegram

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_polls_answers_commands_links_and_stops_cleanly() {
    let _serial = SERIAL.lock().await;
    let h = H::new(true);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    assert!(fake.count("deleteWebhook") >= 1);
    let (_, st) = h.get("/telegram/bot/status").await;
    assert_eq!(
        st["data"],
        json!({"is_running": true, "is_configured": true, "bot_username": "test_bot", "is_active": true})
    );

    fake.push(msg(42, 42, "/start"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.starts_with("Welcome to OpenAlgo Bot, Asha!")),
            5000
        )
        .await
    );

    // Not linked: account commands are refused.
    fake.push(msg(42, 42, "/funds"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t == "Please link your account first using /link"),
            5000
        )
        .await
    );

    // Link in-process; the host given is stored, never contacted.
    let (probe, port, _p) = spawn_probe().await;
    fake.push(msg(
        42,
        42,
        &format!("/link {} http://127.0.0.1:{}", h.key, port),
    ));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.starts_with("Account linked successfully!")),
            5000
        )
        .await
    );
    let user = tg_db::get_user(&h.ctx.sqlite.conn().unwrap(), 42)
        .unwrap()
        .unwrap();
    assert_eq!(user.openalgo_username, USER);
    assert_eq!(user.broker.as_deref(), Some("zerodha"));

    fake.push(msg(42, 42, "/funds"));
    assert!(
        wait_for(
            || fake.texts().iter().any(|t| t.starts_with("*FUNDS*")),
            5000
        )
        .await
    );
    fake.push(msg(42, 42, "/menu"));
    assert!(
        wait_for(
            || fake
                .sent()
                .iter()
                .any(
                    |b| b["reply_markup"]["inline_keyboard"][0][0]["callback_data"] == "orderbook"
                ),
            5000
        )
        .await
    );
    fake.push(msg(42, 42, "/help"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.contains("*Available Commands:*")),
            5000
        )
        .await
    );
    assert_eq!(
        probe.load(Ordering::SeqCst),
        0,
        "the linked host was contacted"
    );

    // Analytics see the commands.
    let (s, a) = h.get("/telegram/api/analytics").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["data"]["total_users"], 1);
    assert!(a["data"]["stats_7d"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["command"] == "help"));

    let (s, v) = h.post("/telegram/bot/stop", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Bot stopped successfully"))
    );
    assert!(!h.ctx.messaging.telegram.task_alive().await);
    assert!(!h.ctx.messaging.telegram.is_running());
    let (_, st) = h.get("/telegram/bot/status").await;
    assert_eq!(st["data"]["is_active"], false);
    let (s, v) = h.post("/telegram/bot/stop", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!("Bot is not running")
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_replies_are_capped_and_groups_get_none() {
    let _serial = SERIAL.lock().await;
    let h = H::new(true);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    // 500 account commands from one stranger: exactly one reply.
    for _ in 0..500 {
        fake.push(msg(7, 7, "/funds"));
    }
    // Group messages, linked or not: none.
    h.link(42);
    for _ in 0..50 {
        fake.push(msg(42, -100, "/funds"));
        fake.push(msg(8, -100, "/start"));
    }
    assert!(wait_for(|| fake.updates.lock().is_empty(), 15000).await);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let to = |chat: i64| fake.sent().iter().filter(|b| b["chat_id"] == chat).count();
    assert_eq!(to(7), 1);
    assert_eq!(to(-100), 0);
    // A linked user over the limit gets nothing more until the minute passes.
    for _ in 0..30 {
        fake.push(msg(42, 42, "/help"));
    }
    assert!(wait_for(|| fake.updates.lock().is_empty(), 15000).await);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(to(42), 20);
    h.clock.advance(chrono::Duration::seconds(61));
    fake.push(msg(42, 42, "/help"));
    assert!(wait_for(|| to(42) == 21, 5000).await);
    h.ctx.messaging.telegram.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_mode_buttons_need_the_linked_user_in_a_private_chat() {
    let _serial = SERIAL.lock().await;
    let h = H::new(true);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    h.link(42);
    assert!(!h.ctx.sqlite.get_analyze_mode().unwrap());

    // An unlinked user pressing the button changes nothing.
    fake.push(button(7, 7, "mode_analyze"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t == "Please link your account first using /link"),
            5000
        )
        .await
    );
    // Another member of a group pressing the linked user's button: nothing.
    fake.push(button(99, -100, "mode_analyze"));
    // The linked user in a group: ignored too, and nothing is said there.
    fake.push(button(42, -100, "mode_analyze"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!h.ctx.sqlite.get_analyze_mode().unwrap());
    assert!(!fake.sent().iter().any(|b| b["chat_id"] == -100));

    // The linked user in a private chat succeeds.
    fake.push(button(42, 42, "mode_analyze"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.contains("Now in: Analyze Mode")),
            5000
        )
        .await
    );
    assert!(h.ctx.sqlite.get_analyze_mode().unwrap());
    assert!(!h.events.named("app_mode_changed").is_empty());
    fake.push(button(42, 42, "mode_live"));
    assert!(
        wait_for(
            || fake.texts().iter().any(|t| t.contains("Now in: Live Mode")),
            5000
        )
        .await
    );

    // Once the API key is regenerated, the old link no longer changes the mode.
    ApiKeyService::regenerate(&h.ctx, USER).unwrap();
    fake.push(button(42, 42, "mode_analyze"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.contains("linked API key is no longer valid")),
            5000
        )
        .await
    );
    assert!(!h.ctx.sqlite.get_analyze_mode().unwrap());
    h.ctx.messaging.telegram.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_link_never_calls_the_given_host_even_without_a_broker() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    let (probe, port, _p) = spawn_probe().await;
    for host in [
        format!("http://127.0.0.1:{}", port),
        "http://169.254.169.254".into(),
        "http://10.0.0.1:5000".into(),
    ] {
        fake.push(msg(42, 42, &format!("/link {} {}", h.key, host)));
    }
    fake.push(msg(
        42,
        42,
        &format!("/link wrong-key http://127.0.0.1:{}", port),
    ));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .filter(|t| t.starts_with("Failed to validate API key."))
                .count()
                == 4,
            8000
        )
        .await
    );
    assert_eq!(probe.load(Ordering::SeqCst), 0);
    assert!(tg_db::get_user(&h.ctx.sqlite.conn().unwrap(), 42)
        .unwrap()
        .is_none());
    h.ctx.messaging.telegram.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_link_guesses_are_throttled_per_sender() {
    let _serial = SERIAL.lock().await;
    let h = H::new(true);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    let failed = |f: &Fake| {
        f.texts()
            .iter()
            .filter(|t| t.starts_with("Failed to validate API key."))
            .count()
    };
    for i in 0..10 {
        fake.push(msg(
            42,
            42,
            &format!("/link wrong-{} http://127.0.0.1:5000", i),
        ));
        assert!(wait_for(|| failed(&fake) == i + 1, 5000).await);
    }
    // The 11th within the minute is refused without trying the key, even
    // the right one, and says nothing about keys.
    fake.push(msg(
        42,
        42,
        &format!("/link {} http://127.0.0.1:5000", h.key),
    ));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.starts_with("Too many link attempts.")),
            5000
        )
        .await
    );
    assert_eq!(failed(&fake), 10);
    assert!(tg_db::get_user(&h.ctx.sqlite.conn().unwrap(), 42)
        .unwrap()
        .is_none());
    // Another sender is not affected.
    fake.push(msg(43, 43, "/link wrong http://127.0.0.1:5000"));
    assert!(wait_for(|| failed(&fake) == 11, 5000).await);
    // After the window the right key links.
    h.clock.advance(chrono::Duration::seconds(61));
    fake.push(msg(
        42,
        42,
        &format!("/link {} http://127.0.0.1:5000", h.key),
    ));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.starts_with("Account linked successfully!")),
            5000
        )
        .await
    );
    assert!(tg_db::get_user(&h.ctx.sqlite.conn().unwrap(), 42)
        .unwrap()
        .is_some());
    h.ctx.messaging.telegram.shutdown().await;
}

fn placed(mode: Mode) -> Event {
    Event::OrderPlaced {
        meta: OrderMeta {
            mode,
            api_type: "placeorder".into(),
            request_data: json!({"symbol": "SBIN", "action": "BUY", "quantity": "1",
                "pricetype": "MARKET", "exchange": "NSE", "product": "MIS"}),
            response_data: json!({"status": "success", "orderid": "OID1"}),
        },
        strategy: String::new(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        quantity: 1,
        pricetype: "MARKET".into(),
        product: "MIS".into(),
        orderid: "OID1".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_alerts_follow_bus_events_while_the_bot_is_started() {
    let _serial = SERIAL.lock().await;
    let h = H::new(true);
    let (fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    h.link(42);
    h.ctx.bus.publish(placed(Mode::Live));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.starts_with("*Order Placed*\n*LIVE MODE - Real Order*")),
            5000
        )
        .await
    );
    h.ctx.bus.publish(placed(Mode::Analyze));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.contains("*ANALYZE MODE - No Real Order*")
                    && t.contains("Order ID: `OID1`")),
            5000
        )
        .await
    );
    let alert = fake
        .sent()
        .into_iter()
        .find(|b| {
            b["text"]
                .as_str()
                .unwrap_or("")
                .starts_with("*Order Placed*")
        })
        .unwrap();
    assert_eq!(alert["chat_id"], 42);
    assert_eq!(alert["parse_mode"], "Markdown");

    // Stopped bot: no alerts.
    h.post("/telegram/bot/stop", json!({})).await;
    let before = fake.texts().len();
    h.ctx.bus.publish(placed(Mode::Live));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.texts().len(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_backs_off_and_gives_up_or_recovers_and_stops_cleanly() {
    let _serial = SERIAL.lock().await;
    // Nothing listening: the connect retries back off, then give up.
    let h = H::new(false);
    let (_n, port, probe_join) = spawn_probe().await;
    probe_join.abort();
    let _ = probe_join.await;
    h.ctx
        .messaging
        .telegram
        .set_api_base(&format!("http://127.0.0.1:{}", port));
    h.post("/telegram/config", json!({"token": TOKEN})).await;
    let (ok, why) = h.ctx.messaging.telegram.start(&h.ctx).await;
    assert!(!ok, "{}", why);
    assert!(wait_for(|| true, 0).await);
    assert!(!h.ctx.messaging.telegram.is_running());
    // The start can report its timeout while the task is still retrying:
    // on Windows a refused loopback connect takes about two seconds, so the
    // five attempts outlast the start's wait. Wait for the task itself to
    // give up, without cancelling it.
    assert!(
        h.ctx
            .messaging
            .telegram
            .wait_task_end(Duration::from_secs(60))
            .await,
        "the connect retries did not give up"
    );
    assert!(!h.ctx.messaging.telegram.task_alive().await);
    assert!(!h.ctx.messaging.telegram.is_running());

    // Failing polls are retried with backoff; the bot stays up.
    let (fake, base, _j) = spawn_fake().await;
    fake.fail_updates.store(3, Ordering::SeqCst);
    h.ctx.messaging.telegram.set_api_base(&base);
    let (ok, _) = h.ctx.messaging.telegram.start(&h.ctx).await;
    assert!(ok);
    assert!(wait_for(|| fake.fail_updates.load(Ordering::SeqCst) == 0, 5000).await);
    let polls = h.ctx.messaging.telegram.poll_count();
    fake.push(msg(5, 5, "/help"));
    assert!(
        wait_for(
            || fake
                .texts()
                .iter()
                .any(|t| t.contains("*Available Commands:*")),
            5000
        )
        .await
    );
    assert!(h.ctx.messaging.telegram.poll_count() > polls);
    assert!(h.ctx.messaging.telegram.is_running());
    h.ctx.messaging.telegram.stop(&h.ctx).await;
    assert!(!h.ctx.messaging.telegram.task_alive().await);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_hundred_start_stop_cycles_leave_no_tasks_or_descriptors() {
    // Counts this process's descriptors: runs alone in a child process.
    crate::isolated!(telegram_hundred_start_stop_cycles_leave_no_tasks_or_descriptors);
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (_fake, base, _j) = spawn_fake().await;
    h.start_telegram(&base).await;
    h.ctx.messaging.telegram.stop(&h.ctx).await;
    let tasks_before = h.ctx.task_count();
    let fds_before = open_fds();
    for _ in 0..100 {
        let (ok, why) = h.ctx.messaging.telegram.start(&h.ctx).await;
        assert!(ok, "{}", why);
        let (ok, _) = h.ctx.messaging.telegram.stop(&h.ctx).await;
        assert!(ok);
        assert!(!h.ctx.messaging.telegram.task_alive().await);
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let fds_after = open_fds();
    assert!(
        fds_after <= fds_before + 4,
        "fds {} -> {}",
        fds_before,
        fds_after
    );
    assert_eq!(h.ctx.task_count(), tasks_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_notify_endpoint() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (fake, base, _j) = spawn_fake().await;
    let (s, v) = h
        .api(
            "/api/v1/telegram/notify",
            json!({"apikey": "bad", "username": USER, "message": "x"}),
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::UNAUTHORIZED,
            json!({"status": "error", "message": "Invalid or missing API key"})
        )
    );
    let (s, v) = h
        .api(
            "/api/v1/telegram/notify",
            json!({"apikey": h.key, "username": USER, "message": "x"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v["message"],
        "Telegram bot is stopped. Start the bot to send notifications."
    );
    h.start_telegram(&base).await;
    let (s, v) = h
        .api(
            "/api/v1/telegram/notify",
            json!({"apikey": h.key, "message": "x"}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Username and message are required")
        )
    );
    let (s, v) = h
        .api(
            "/api/v1/telegram/notify",
            json!({"apikey": h.key, "username": USER, "message": "x"}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::NOT_FOUND,
            json!("User not found or not linked to Telegram")
        )
    );
    h.link(42);
    let (s, v) = h.api("/api/v1/telegram/notify",
        json!({"apikey": h.key, "username": USER, "message": "Hello *there*", "wait_for_delivery": true})).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Notification sent successfully"})
        )
    );
    assert!(fake.texts().iter().any(|t| t == "Hello *there*"));
    let (s, v) = h
        .api(
            "/api/v1/telegram/notify",
            json!({"apikey": h.key, "username": USER, "message": "later"}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Notification queued for delivery"))
    );
    assert!(wait_for(|| fake.texts().iter().any(|t| t == "later"), 5000).await);
    h.ctx.messaging.telegram.shutdown().await;
}

// ------------------------------------------------------------ routes

const SESSION_ROUTES: &[(&str, &str)] = &[
    ("POST", "/telegram/config"),
    ("POST", "/telegram/bot/start"),
    ("POST", "/telegram/bot/stop"),
    ("GET", "/telegram/bot/status"),
    ("POST", "/telegram/broadcast"),
    ("POST", "/telegram/user/42/unlink"),
    ("POST", "/telegram/test-message"),
    ("POST", "/telegram/send-message"),
    ("GET", "/telegram/api/index"),
    ("GET", "/telegram/api/config"),
    ("GET", "/telegram/api/users"),
    ("GET", "/telegram/api/analytics"),
    ("GET", "/whatsapp/config"),
    ("POST", "/whatsapp/config"),
    ("POST", "/whatsapp/pair"),
    ("GET", "/whatsapp/pair/status"),
    ("POST", "/whatsapp/unlink"),
    ("POST", "/whatsapp/bot/start"),
    ("POST", "/whatsapp/bot/stop"),
    ("GET", "/whatsapp/bot/status"),
    ("GET", "/whatsapp/users"),
    ("POST", "/whatsapp/user/91%40s.whatsapp.net/unlink"),
    ("POST", "/whatsapp/broadcast"),
    ("POST", "/whatsapp/test-message"),
    ("POST", "/whatsapp/send"),
    ("GET", "/whatsapp/stats"),
];

#[tokio::test]
async fn every_route_needs_the_user_and_writes_need_csrf() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    for (m, p) in SESSION_ROUTES {
        let method = Method::from_bytes(m.as_bytes()).unwrap();
        let (s, _) = h
            .raw(method.clone(), p, Some(json!({})), false, false, true)
            .await;
        assert!(
            s == StatusCode::UNAUTHORIZED || s == StatusCode::BAD_REQUEST,
            "{} {} gave {}",
            m,
            p,
            s
        );
        if *m == "POST" {
            let (s, b) = h.raw(method, p, Some(json!({})), true, false, true).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {} without CSRF", m, p);
            assert!(String::from_utf8_lossy(&b).contains("session has expired"));
        }
    }
    // The settings page itself is the app for a browser.
    let (s, b) = h
        .raw(Method::GET, "/telegram/config", None, false, false, false)
        .await;
    assert_eq!(s, StatusCode::OK);
    // Any HTML page: the built app, the not-built fallback or the CI stand-in.
    assert!(String::from_utf8_lossy(&b)
        .to_lowercase()
        .starts_with("<!doctype html"));
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[tokio::test]
async fn route_shapes_match_the_web() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (_, v) = h.get("/telegram/api/index").await;
    assert_eq!(
        keys(&v["data"]),
        [
            "active_users_7d",
            "bot_status",
            "config",
            "stats",
            "telegram_user",
            "total_commands",
            "users"
        ]
    );
    assert_eq!(
        keys(&v["data"]["config"]),
        [
            "bot_username",
            "broadcast_enabled",
            "is_active",
            "rate_limit_per_minute"
        ]
    );
    let (_, v) = h.get("/telegram/api/config").await;
    assert_eq!(
        v["data"],
        json!({"has_token": false, "bot_username": null, "broadcast_enabled": true, "rate_limit_per_minute": 30, "is_active": false})
    );
    let (_, v) = h.get("/telegram/api/users").await;
    assert_eq!(keys(&v["data"]), ["stats", "total_commands", "users"]);
    h.link(42);
    let (_, v) = h.get("/telegram/api/users").await;
    let u = &v["data"]["users"][0];
    assert_eq!(
        keys(u),
        [
            "broker",
            "created_at",
            "first_name",
            "id",
            "last_command_at",
            "last_name",
            "notifications_enabled",
            "openalgo_username",
            "telegram_id",
            "telegram_username"
        ]
    );
    assert!(u["created_at"].as_str().unwrap().ends_with(" GMT"));
    // The token is never returned.
    h.post(
        "/telegram/config",
        json!({"token": TOKEN, "broadcast_enabled": false}),
    )
    .await;
    let (_, b) = h
        .raw(Method::GET, "/telegram/api/config", None, true, false, true)
        .await;
    assert!(!String::from_utf8_lossy(&b).contains("TEST-TOKEN"));
    let (s, v) = h
        .post("/telegram/broadcast", json!({"message": "hi"}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::FORBIDDEN, json!("Broadcast is disabled"))
    );
    let (s, v) = h.post("/telegram/user/42/unlink", json!({})).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "User unlinked"})
        )
    );

    let (_, v) = h.get("/whatsapp/config").await;
    assert_eq!(keys(&v["data"]), ["config", "pair_state"]);
    assert_eq!(
        v["data"]["pair_state"],
        json!({"status": "idle", "qr_data_url": null, "pair_code": null, "error": null, "started_at": null, "paired_at": null})
    );
    for k in [
        "is_paired",
        "is_active",
        "own_jid",
        "owner_username",
        "broadcast_enabled",
        "is_running",
        "status_message",
        "rate_limit_per_minute",
    ] {
        assert!(v["data"]["config"].get(k).is_some(), "{}", k);
    }
    let (_, v) = h.get("/whatsapp/bot/status").await;
    assert_eq!(
        keys(&v["data"]),
        [
            "bot_username",
            "is_active",
            "is_paired",
            "is_running",
            "own_jid",
            "own_phone",
            "paired_at",
            "status_message"
        ]
    );
    let (_, v) = h.get("/whatsapp/stats?days=900").await;
    assert_eq!(
        v["data"],
        json!({"total_commands": 0, "by_command": {}, "days": 365})
    );
    let (_, v) = h.get("/whatsapp/users").await;
    assert_eq!(v, json!({"status": "success", "data": [], "count": 0}));
    let (s, v) = h
        .post("/whatsapp/config", json!({"rate_limit_per_minute": 7}))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Configuration updated"})
        )
    );
}

// ------------------------------------------------------------ WhatsApp

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_unpaired_refuses_sends_and_starts() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (s, v) = h.post("/whatsapp/bot/start", json!({})).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "message": "Device not paired. Pair from /whatsapp first."})
        )
    );
    for (p, b) in [
        (
            "/whatsapp/send",
            json!({"phone": "919876543210", "message": "x"}),
        ),
        ("/whatsapp/broadcast", json!({"message": "x"})),
        ("/whatsapp/test-message", json!({})),
    ] {
        let (s, v) = h.post(p, b).await;
        assert_eq!(
            (s, v["message"].clone()),
            (
                StatusCode::CONFLICT,
                json!("WhatsApp is not paired. Pair the device first to send messages.")
            ),
            "{}",
            p
        );
    }
    let (s, v) = h
        .api(
            "/api/v1/whatsapp/notify",
            json!({"apikey": h.key, "self": true, "message": "x"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .starts_with("WhatsApp is not paired or not connected."));
    let (s, _) = h
        .api(
            "/api/v1/whatsapp/notify",
            json!({"apikey": "nope", "self": true, "message": "x"}),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = h.post("/whatsapp/bot/stop", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Bot is not running"))
    );
    let (s, v) = h.post("/whatsapp/unlink", json!({})).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Device unlinked"})
        )
    );
    assert_eq!(
        h.events.named("whatsapp_status").last().unwrap(),
        &json!({"is_running": false, "is_paired": false, "status_message": null})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_pairing_times_out_without_reaching_whatsapp() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    let (s, v) = h
        .post("/whatsapp/pair", json!({"phone": "+91 98765 43210"}))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "Pairing started. Watch for QR or pair code.");
    assert_eq!(v["data"]["status"], "starting");
    let (s, v) = h.post("/whatsapp/pair", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Pairing already in progress")
        )
    );
    assert!(
        wait_for(
            || h.ctx.messaging.whatsapp.pair_state().status == "failed",
            8000
        )
        .await
    );
    let (_, v) = h.get("/whatsapp/pair/status").await;
    assert_eq!(v["data"]["error"], PAIR_TIMEOUT_MESSAGE);
    let ev = h.events.named("whatsapp_pair_status");
    assert_eq!(ev.last().unwrap()["status"], "failed");
    assert!(wait_for(|| true, 0).await);
    let mut alive = (true, true);
    for _ in 0..200 {
        alive = h.ctx.messaging.whatsapp.tasks_alive().await;
        if !alive.0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive.0 && !alive.1);
    assert!(!whatsapp::WhatsAppService::is_paired(&h.ctx));
}

/// Store a session as pairing would (a fresh device, not linked to a phone).
async fn fake_pairing(h: &H) {
    use openalgo_desktop_lib::messaging::whatsapp::store::SnapshotStore;
    use whatsapp_rust::wacore::store::traits::DeviceStore;
    let s = SnapshotStore::new();
    s.create().await.unwrap();
    let snap = s.export().unwrap();
    let c = h.ctx.sqlite.conn().unwrap();
    whatsapp::db::save_session(
        &c,
        &h.ctx.security,
        &snap,
        &whatsapp::db::Owner {
            own_jid: Some("919876543210@s.whatsapp.net"),
            own_phone: Some("919876543210"),
            owner_user_id: Some(1),
            owner_username: Some(USER),
        },
        h.ctx.now(),
    )
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_bot_start_stop_cycles_are_clean() {
    // Counts this process's descriptors: runs alone in a child process.
    crate::isolated!(whatsapp_bot_start_stop_cycles_are_clean);
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    fake_pairing(&h).await;
    let raw: String = h
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT CAST(session_blob AS TEXT) FROM whatsapp_config",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.starts_with("v1:"));
    let (s, v) = h.post("/whatsapp/bot/start", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Bot started"))
    );
    let (s, v) = h.post("/whatsapp/bot/start", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Bot already running"))
    );
    assert!(h
        .events
        .named("whatsapp_status")
        .iter()
        .any(|p| p["is_running"] == true && p["is_paired"] == true));
    let (s, v) = h.post("/whatsapp/bot/stop", json!({})).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Bot stopped"))
    );
    assert_eq!(h.ctx.messaging.whatsapp.tasks_alive().await, (false, false));

    let fds_before = open_fds();
    let tasks_before = h.ctx.task_count();
    for _ in 0..100 {
        let (ok, m) = h.ctx.messaging.whatsapp.start_bot(&h.ctx).await;
        assert!(ok, "{}", m);
        h.ctx.messaging.whatsapp.stop_bot(&h.ctx).await;
        assert_eq!(h.ctx.messaging.whatsapp.tasks_alive().await, (false, false));
    }
    // Give aborted client tasks a moment to release their sockets.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let fds_after = open_fds();
    assert!(
        fds_after <= fds_before + 4,
        "fds {} -> {}",
        fds_before,
        fds_after
    );
    assert_eq!(h.ctx.task_count(), tasks_before);
    // Still paired, session intact.
    assert!(whatsapp::WhatsAppService::is_paired(&h.ctx));
    let (s, _) = h.post("/whatsapp/unlink", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!whatsapp::WhatsAppService::is_paired(&h.ctx));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_notify_validation_and_attachments_refused() {
    let _serial = SERIAL.lock().await;
    let h = H::new(false);
    fake_pairing(&h).await;
    let (ok, _) = h.ctx.messaging.whatsapp.start_bot(&h.ctx).await;
    assert!(ok);
    let k = h.key.clone();
    let cases = [
        (
            json!({"apikey": k, "message": "x"}),
            StatusCode::BAD_REQUEST,
            "Specify one of: 'self', 'username', 'phone', or 'phones'",
        ),
        (
            json!({"apikey": k, "phone": "12", "message": "x"}),
            StatusCode::BAD_REQUEST,
            "Invalid phone number",
        ),
        (
            json!({"apikey": k, "phones": "91", "message": "x"}),
            StatusCode::BAD_REQUEST,
            "'phones' must be a list",
        ),
        (
            json!({"apikey": k, "phones": ["1"], "message": "x"}),
            StatusCode::BAD_REQUEST,
            "No valid phones in list",
        ),
        (
            json!({"apikey": k, "self": true}),
            StatusCode::BAD_REQUEST,
            "Provide at least one of: message, image_path, document_path",
        ),
        (
            json!({"apikey": k, "self": true, "image_path": "/etc/passwd"}),
            StatusCode::BAD_REQUEST,
            "image_path is not allowed",
        ),
        (
            json!({"apikey": k, "username": "someone", "message": "x"}),
            StatusCode::NOT_FOUND,
            "Username not found or not linked to WhatsApp",
        ),
        (
            json!({"apikey": k, "self": true, "message": "x".repeat(4097)}),
            StatusCode::BAD_REQUEST,
            "Message must not exceed 4096 characters",
        ),
    ];
    for (body, code, message) in cases {
        let (s, v) = h.api("/api/v1/whatsapp/notify", body).await;
        assert_eq!((s, v["message"].clone()), (code, json!(message)));
    }
    let (s, v) = h
        .post(
            "/whatsapp/send",
            json!({"phone": "919876543210", "message": "x", "document_path": "/etc/hosts"}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("document_path is not allowed")
        )
    );
    // Fire-and-forget answers at once.
    let (s, v) = h
        .api(
            "/api/v1/whatsapp/notify",
            json!({"apikey": k, "username": USER, "message": "x", "wait_for_delivery": false}),
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Queued for 1 recipient(s)", "queued": 1})
        )
    );
    h.ctx.messaging.whatsapp.stop_bot(&h.ctx).await;
    h.ctx.shutdown().await;
}
