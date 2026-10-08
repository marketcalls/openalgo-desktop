//! Soak: the whole trading day in a loop, on the real `AppState`, HTTP
//! server (with Socket.IO) and 8765 feed server, with `MockBroker`
//! (registered as `zerodha`) and a fake broker market socket.
//!
//! Each cycle: broker sign-in (OAuth callback) -> master contract load ->
//! broker feed connected, an 8765 client subscribes and receives streaming
//! ticks -> one live order (the mock broker) and one sandbox order over
//! `/api/v1/placeorder` -> the broker socket is dropped and the feed
//! reconnects and resubscribes -> 110 `/api/v1` requests on a sample of
//! endpoints (some on fresh connections) -> Socket.IO clients connect and
//! disconnect (signed in and not) -> the 8765 client unsubscribes and
//! leaves -> logout.
//!
//! After a warm-up, descriptors, memory and live tokio tasks are sampled
//! every few cycles. Memory is the resident set on Linux and Windows and the
//! physical footprint on macOS (resident plus compressed: macOS compresses
//! idle pages, so its resident set falls and rises with no change in use). Descriptors must be flat (the database pool may add two per
//! connection it keeps, see `broker_session_e2e::settled_fds`), live tasks
//! back to their warm-up count, memory growth under a bound, and the second
//! half of the run must grow less than the first: a plateau (a cache
//! filling) passes, a line (a leak) fails.
//!
//! Ignored by default (it takes minutes):
//! `cargo test --test it soak -- --ignored --nocapture`
//! Numbers from a run are recorded in `docs/audit/soak.md`.

use crate::broker_session_e2e::{
    free_port, pin_ports, sbin, settled_fds, state_from_kite_url, until,
};
use crate::feed_support::Client;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use openalgo_desktop_lib::brokers::mock::MockBroker;
use openalgo_desktop_lib::brokers::types::Quote;
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
use openalgo_desktop_lib::events::SessionEndReason;
use openalgo_desktop_lib::feed::FeedService;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::{BrokerAuthService, CallbackOrigin};
use openalgo_desktop_lib::state::{AppState, OpenOptions, ServerStatus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// The web's and the development listeners; a test never binds them.
const RESERVED_PORTS: [u16; 4] = [5000, 8765, 5500, 8766];
/// Cycles in the measured run (after the warm-up).
const CYCLES: usize = 150;
/// Cycles before the baseline sample: lets the allocator, the database
/// pool, the runtime's worker state and the registries reach working size.
const WARMUP: usize = 10;
/// Sample every this many cycles.
const SAMPLE_EVERY: usize = 10;
/// `/api/v1` requests per cycle.
const API_REQUESTS: usize = 110;
/// Spacing between them: stays under the web's 100 per second per address.
const API_SPACING: Duration = Duration::from_millis(11);
/// Descriptors allowed beyond the warm-up count and the pool's growth.
const FD_SLACK: usize = 4;
/// Live tokio tasks allowed beyond the warm-up count (a keep-alive
/// connection still closing when sampled).
const TASK_SLACK: usize = 4;
/// Memory growth allowed over the measured run, in KiB.
const MEM_BOUND_KIB: i64 = 48 * 1024;
/// Noise floor for the half-versus-half memory check, in KiB.
const MEM_NOISE_KIB: i64 = 4 * 1024;

/// Resident set size of this process in KiB (sysinfo; no child process,
/// so sampling does not open descriptors of its own).
fn rss_kib() -> u64 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map(|p| p.memory() / 1024).unwrap_or(0)
}

/// The memory the assertions use, in KiB: the physical footprint on macOS
/// (what Activity Monitor shows; includes compressed pages), the resident
/// set elsewhere.
fn mem_kib() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: `info` is a valid, writable rusage_info_v2 for the
        // RUSAGE_INFO_V2 flavor; the kernel fills it for this process.
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr() as *mut libc::rusage_info_t,
            )
        };
        if rc == 0 {
            // SAFETY: filled by the successful call above.
            return unsafe { info.assume_init() }.ri_phys_footprint / 1024;
        }
    }
    rss_kib()
}

fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    cycle: usize,
    fds: usize,
    pool: usize,
    rss_kib: u64,
    mem_kib: u64,
    tasks: usize,
}

/// Descriptors and pool once settled, live tasks once they stop falling
/// (a closed keep-alive connection's task ends shortly after its peer).
async fn sample(ctx: &AppState, cycle: usize, task_floor: Option<usize>) -> Sample {
    let mut tasks = alive_tasks();
    for _ in 0..100 {
        if task_floor.is_some_and(|f| tasks <= f) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let now = alive_tasks();
        if task_floor.is_none() && now == tasks {
            break;
        }
        tasks = now;
    }
    let (fds, pool) = settled_fds(ctx).await;
    Sample {
        cycle,
        fds,
        pool,
        rss_kib: rss_kib(),
        mem_kib: mem_kib(),
        tasks,
    }
}

fn master() -> Vec<SymToken> {
    let eq = |s: &str| SymToken {
        symbol: s.into(),
        brsymbol: s.into(),
        name: s.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: format!("NSE::::{}", s),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    };
    vec![sbin(), eq("RELIANCE"), eq("INFY"), eq("TCS")]
}

/// A fake broker market feed: on `sub` it streams ticks for SBIN every
/// 25 ms until `unsub`; a message on `kick` drops every connection without
/// a close frame (a network drop), so the app must reconnect.
async fn market_server(kick: broadcast::Sender<()>) -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = l.accept().await {
            while conns.try_join_next().is_some() {}
            let mut kicked = kick.subscribe();
            conns.spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let mut ticker = tokio::time::interval(Duration::from_millis(25));
                let mut streaming = false;
                let mut n = 0u64;
                loop {
                    tokio::select! {
                        m = ws.next() => match m {
                            Some(Ok(Message::Text(t))) => {
                                if t.contains("\"unsub\"") {
                                    streaming = false;
                                } else if t.contains("\"sub\"") && t.contains("SBIN") {
                                    streaming = true;
                                }
                            }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                        _ = ticker.tick(), if streaming => {
                            n += 1;
                            let p = 812.5 + (n % 20) as f64 * 0.05;
                            let tick = json!({"t": "SBIN", "x": "NSE", "p": p}).to_string();
                            if ws.send(Message::Text(tick)).await.is_err() {
                                break;
                            }
                        }
                        _ = kicked.recv() => break,
                    }
                }
            });
        }
    });
    (url, task)
}

/// The next `market_data` frame on an 8765 client.
async fn next_tick(client: &mut Client) -> Value {
    loop {
        let v = client.recv().await;
        if v["type"] == "market_data" {
            return v;
        }
    }
}

async fn next_text<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Option<String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(t),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

/// One Socket.IO client over the WebSocket transport (Engine.IO v4):
/// open, join the default namespace, leave. Returns whether the server
/// accepted it (signed-in sessions only).
async fn socketio_client(addr: SocketAddr, cookie: Option<&str>) -> bool {
    let mut req = format!("ws://{}/socket.io/?EIO=4&transport=websocket", addr)
        .into_client_request()
        .unwrap();
    if let Some(c) = cookie {
        req.headers_mut().insert("cookie", c.parse().unwrap());
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("Socket.IO connects");
    let open = next_text(&mut ws).await.expect("Engine.IO open packet");
    assert!(open.starts_with('0'), "{}", open);
    ws.send(Message::Text("40".into())).await.unwrap();
    let mut accepted = false;
    // "40{sid}" on connect; an unauthenticated socket is then told "41".
    while let Some(t) = next_text(&mut ws).await {
        if t.starts_with("40") {
            accepted = true;
            if cookie.is_some() {
                break;
            }
        } else if t.starts_with("41") {
            accepted = false;
            break;
        }
    }
    if accepted {
        let _ = ws.send(Message::Text("41".into())).await;
    }
    let _ = ws.close(None).await;
    while let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {}
    accepted
}

/// The sample of `/api/v1` calls, cycled through.
fn api_sample() -> Vec<(&'static str, Value)> {
    let sbin = json!({"symbol": "SBIN", "exchange": "NSE"});
    vec![
        ("ping", json!({})),
        ("funds", json!({})),
        ("orderbook", json!({})),
        ("tradebook", json!({})),
        ("positionbook", json!({})),
        ("holdings", json!({})),
        ("quotes", sbin.clone()),
        ("depth", sbin.clone()),
        ("symbol", sbin.clone()),
        ("search", json!({"query": "SBI", "exchange": "NSE"})),
        ("analyzer", json!({})),
        ("intervals", json!({})),
        (
            "multiquotes",
            json!({"symbols": [
                {"symbol": "SBIN", "exchange": "NSE"},
                {"symbol": "INFY", "exchange": "NSE"}
            ]}),
        ),
        (
            "openposition",
            json!({"strategy": "soak", "symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        ),
    ]
}

async fn api(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    path: &str,
    mut body: Value,
    close: bool,
) -> (u16, Value) {
    body["apikey"] = json!(key);
    let mut req = http
        .post(format!("{}/api/v1/{}", base, path))
        .json(&body)
        .timeout(Duration::from_secs(10));
    if close {
        req = req.header("connection", "close");
    }
    let resp = req.send().await.expect("API request");
    let status = resp.status().as_u16();
    let v = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, v)
}

fn order() -> Value {
    json!({
        "strategy": "soak", "symbol": "SBIN", "action": "BUY", "exchange": "NSE",
        "pricetype": "MARKET", "product": "MIS", "quantity": "1"
    })
}

struct Rig {
    ctx: Arc<AppState>,
    mock: Arc<MockBroker>,
    key: String,
    base: String,
    http_addr: SocketAddr,
    ws_url: String,
    cookie: String,
    kick: broadcast::Sender<()>,
}

async fn cycle(r: &Rig, n: usize) {
    let ctx = &r.ctx;
    let key = r.key.as_str();
    r.mock.calls.lock().clear();

    // ---- sign in ----
    let url = BrokerAuthService::start_oauth(ctx, "zerodha", Some("soak-session"))
        .await
        .unwrap();
    let mut params = HashMap::new();
    params.insert("request_token".to_string(), format!("rt-{}", n));
    params.insert("state".to_string(), state_from_kite_url(&url));
    BrokerAuthService::complete_oauth(
        ctx,
        "zerodha",
        &params,
        CallbackOrigin::Redirect {
            session_id: Some("soak-session"),
        },
    )
    .await
    .unwrap();

    // ---- master contract loaded ----
    until("the master contract", || {
        ctx.symbol_count() == master().len()
    })
    .await;

    // ---- feed: an 8765 client subscribes and gets streaming ticks ----
    until("the broker feed", || ctx.websocket.is_connected()).await;
    let mut client = Client::connect(&r.ws_url).await;
    let v = client
        .request(json!({"action": "authenticate", "api_key": key}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    let v = client
        .request(json!({"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "mode": 1}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    for _ in 0..3 {
        let t = next_tick(&mut client).await;
        assert_eq!(t["symbol"], "SBIN");
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // ---- orders: live (the mock broker), then sandbox ----
    ctx.sqlite.set_analyze_mode(false).unwrap();
    let (s, v) = api(&http, &r.base, key, "placeorder", order(), false).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("success")), "{}", v);
    assert!(
        v["orderid"]
            .as_str()
            .is_some_and(|o| o.starts_with("MOCK-")),
        "{}",
        v
    );
    ctx.sqlite.set_analyze_mode(true).unwrap();
    let (s, v) = api(&http, &r.base, key, "placeorder", order(), false).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("success")), "{}", v);
    assert_eq!(v["mode"], "analyze", "{}", v);
    ctx.sqlite.set_analyze_mode(false).unwrap();

    // ---- forced feed drop: reconnect, resubscribe, ticks resume ----
    let connects = ctx.websocket.stats().connects.load(Ordering::Relaxed);
    let _ = r.kick.send(());
    until("the feed reconnect", || {
        ctx.websocket.stats().connects.load(Ordering::Relaxed) > connects
            && ctx.websocket.is_connected()
    })
    .await;
    // Ticks queued before the drop may still arrive; one from the new
    // connection must follow.
    let events = ctx.websocket.stats().events.load(Ordering::Relaxed);
    until("a tick on the new connection", || {
        ctx.websocket.stats().events.load(Ordering::Relaxed) > events
    })
    .await;
    let t = next_tick(&mut client).await;
    assert_eq!(t["symbol"], "SBIN");

    // ---- /api/v1 traffic ----
    let sample = api_sample();
    let mut pace = tokio::time::interval(API_SPACING);
    for i in 0..API_REQUESTS {
        pace.tick().await;
        let (path, body) = &sample[i % sample.len()];
        let (s, v) = api(&http, &r.base, key, path, body.clone(), i % 10 == 9).await;
        assert_eq!(s, 200, "{} answered {}: {}", path, s, v);
    }
    drop(http);

    // ---- Socket.IO: signed-in browsers and a stranger ----
    for _ in 0..2 {
        assert!(socketio_client(r.http_addr, Some(&r.cookie)).await);
    }
    assert!(!socketio_client(r.http_addr, None).await);

    // ---- the 8765 client leaves (close frame or a dropped socket) ----
    if n.is_multiple_of(2) {
        let v = client
            .request(
                json!({"action": "unsubscribe", "symbol": "SBIN", "exchange": "NSE", "mode": 1}),
            )
            .await;
        // A tick may race the unsubscribe acknowledgement.
        if v["type"] != "market_data" {
            assert_eq!(v["status"], "success", "{}", v);
        }
        let _ = client.ws.close(None).await;
    }
    drop(client);

    // ---- logout ----
    BrokerAuthService::revoke(ctx, SessionEndReason::Logout)
        .await
        .unwrap();
    assert_eq!(ctx.runtime.task_count(), 0, "session tasks left running");
    assert!(!ctx.websocket.is_running());
    assert_eq!(ctx.websocket.instrument_count(), 0);
    assert!(ctx.bridge.applied().is_empty());
    assert_eq!(ctx.symbol_count(), 0);
    let io = ctx.ui.io().expect("Socket.IO is served");
    until("Socket.IO sockets to close", || io.sockets().is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn soak_session_cycles_hold_descriptors_rss_and_tasks_flat() {
    crate::isolated!(soak_session_cycles_hold_descriptors_rss_and_tasks_flat);

    let (kick, _) = broadcast::channel(4);
    let (feed_url, market_task) = market_server(kick.clone()).await;

    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockBroker::new("zerodha"));
    *mock.master.lock() = Some(Ok(master()));
    *mock.feed_url.lock() = Some(feed_url);
    for s in ["SBIN", "RELIANCE", "INFY", "TCS"] {
        mock.set_quote(Quote {
            symbol: s.into(),
            exchange: "NSE".into(),
            ltp: 812.5,
            close: 810.0,
            ..Default::default()
        });
    }
    let registry =
        BrokerRegistry::with_symbols(mock.symbols.clone(), vec![mock.clone() as Arc<dyn Broker>]);
    // Monday 10:00 IST, inside market hours; fixed so no session boundary
    // or square-off runs during the soak.
    let ten_ist =
        chrono::TimeZone::with_ymd_and_hms(&chrono_tz::Asia::Kolkata, 2026, 10, 5, 10, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
    let ctx = AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: ManualClock::new(ten_ist),
            brokers: Arc::new(registry),
        },
    )
    .unwrap();

    AuthService::setup(&ctx, "alice", "alice@example.com", "Secret@123").unwrap();
    {
        let conn = ctx.sqlite.conn().unwrap();
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
    let key = ApiKeyService::current(&ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();

    // HTTP (with Socket.IO) and the feed server on ephemeral ports, pinned
    // so the sign-in's settings reload keeps them.
    pin_ports(&ctx, free_port().await, free_port().await);
    let server = openalgo_desktop_lib::server::start(ctx.clone())
        .await
        .expect("HTTP server starts");
    let feed = FeedService::new(ctx.clone());
    assert!(matches!(feed.start().await, ServerStatus::Running { .. }));
    let feed_addr = feed.local_addr().await.unwrap();
    for port in [server.addr.port(), feed_addr.port()] {
        assert!(
            !RESERVED_PORTS.contains(&port),
            "bound reserved port {}",
            port
        );
    }

    let web = ctx.sessions.create(ctx.now());
    ctx.sessions
        .update(&web.id, |s| s.user = Some("alice".into()));

    let rig = Rig {
        ctx: ctx.clone(),
        mock: mock.clone(),
        key,
        base: format!("http://{}", server.addr),
        http_addr: server.addr,
        ws_url: format!("ws://{}", feed_addr),
        cookie: format!("session={}", web.id),
        kick,
    };

    let started = std::time::Instant::now();
    for n in 0..WARMUP {
        cycle(&rig, n).await;
    }
    let warm = sample(&ctx, WARMUP, None).await;
    let mut samples = vec![warm];
    for n in WARMUP..WARMUP + CYCLES {
        cycle(&rig, n).await;
        let done = n + 1 - WARMUP;
        if done.is_multiple_of(SAMPLE_EVERY) {
            samples.push(sample(&ctx, n + 1, Some(warm.tasks + TASK_SLACK)).await);
        }
    }
    // The sign-ins reloaded the settings; the listeners never moved.
    assert_eq!(feed.local_addr().await, Some(feed_addr));
    assert_eq!(ctx.server_config().ws_port, feed_addr.port());
    let end = *samples.last().unwrap();
    let mid = samples[samples.len() / 2];

    eprintln!(
        "soak: {} cycles after {} warm-up in {:.0?}",
        CYCLES,
        WARMUP,
        started.elapsed()
    );
    eprintln!("cycle  fds  pool  rss_kib  mem_kib  tasks");
    for s in &samples {
        eprintln!(
            "{:>5} {:>4} {:>5} {:>8} {:>8} {:>6}",
            s.cycle, s.fds, s.pool, s.rss_kib, s.mem_kib, s.tasks
        );
    }

    // Descriptors: flat, beyond what the pool's extra connections hold.
    let fd_allowed = warm.fds + 2 * end.pool.saturating_sub(warm.pool) + FD_SLACK;
    assert!(
        end.fds <= fd_allowed,
        "descriptors grew: {} -> {} (pool {} -> {}, allowed {})",
        warm.fds,
        end.fds,
        warm.pool,
        end.pool,
        fd_allowed
    );
    // Tasks: back to the warm-up count.
    assert!(
        end.tasks <= warm.tasks + TASK_SLACK,
        "live tokio tasks grew: {} -> {}",
        warm.tasks,
        end.tasks
    );
    // Memory: bounded, and the second half grows less than the first.
    let kib = |s: &Sample| s.mem_kib as i64;
    let growth = kib(&end) - kib(&warm);
    let first = kib(&mid) - kib(&warm);
    let second = kib(&end) - kib(&mid);
    eprintln!(
        "memory growth: {} KiB total, {} KiB first half, {} KiB second half",
        growth, first, second
    );
    assert!(
        growth <= MEM_BOUND_KIB,
        "memory grew {} KiB over {} cycles (bound {} KiB)",
        growth,
        CYCLES,
        MEM_BOUND_KIB
    );
    assert!(
        second <= first.max(0) / 2 + MEM_NOISE_KIB,
        "memory still climbing: {} KiB in the first half, {} KiB in the second",
        first,
        second
    );
    assert_eq!(
        *mock.logouts.lock() as usize,
        WARMUP + CYCLES,
        "every cycle logged out"
    );

    feed.stop().await;
    server.stop().await;
    market_task.abort();
    ctx.shutdown().await;
}
