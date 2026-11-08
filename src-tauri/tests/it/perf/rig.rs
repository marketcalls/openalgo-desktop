//! The app the benchmarks measure: the real `AppState`, HTTP server (with
//! Socket.IO) and 8765 feed server on ephemeral ports in a temporary data
//! folder, `MockBroker` registered as `zerodha` and signed in through its
//! OAuth callback, a synthetic broker market socket and a broker order
//! socket (both speaking the `MockFeed` protocol), and a clock that runs
//! in real time from a fixed trading-day morning so no session boundary
//! or square-off falls inside a run.

use crate::broker_session_e2e::{free_port, pin_ports, state_from_kite_url};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use openalgo_desktop_lib::brokers::mock::MockBroker;
use openalgo_desktop_lib::brokers::types::{Position, Quote};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
use openalgo_desktop_lib::feed::FeedService;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::server::ServerHandle;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::{BrokerAuthService, CallbackOrigin};
use openalgo_desktop_lib::state::{AppState, OpenOptions, ServerStatus};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

/// The web's and the development listeners; a benchmark never binds them.
pub const RESERVED_PORTS: [u16; 4] = [5000, 8765, 5500, 8766];
/// Every synthetic instrument trades around this price.
pub const BASE_PRICE: f64 = 1000.0;

/// A wall clock that starts at a fixed instant and runs in real time; a
/// fault can move it (`jump`) as a clock change or a sleep would.
pub struct RunningClock {
    start: chrono::DateTime<chrono::Utc>,
    mono: Instant,
    offset_ms: AtomicI64,
}

impl RunningClock {
    /// Monday 5 October 2026, 09:20 IST: inside market hours, hours before
    /// the 15:15 square-off and the 03:00 session boundary.
    pub fn trading_morning() -> Arc<RunningClock> {
        let start =
            chrono::TimeZone::with_ymd_and_hms(&chrono_tz::Asia::Kolkata, 2026, 10, 5, 9, 20, 0)
                .unwrap()
                .with_timezone(&chrono::Utc);
        Arc::new(RunningClock {
            start,
            mono: Instant::now(),
            offset_ms: AtomicI64::new(0),
        })
    }

    /// Move the wall clock by `by` (negative: back).
    pub fn jump(&self, by: chrono::Duration) {
        self.offset_ms
            .fetch_add(by.num_milliseconds(), Ordering::SeqCst);
    }
}

impl Clock for RunningClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        let ran = chrono::Duration::from_std(self.mono.elapsed()).unwrap_or_default();
        self.start + ran + chrono::Duration::milliseconds(self.offset_ms.load(Ordering::SeqCst))
    }
}

/// `n` NSE equities, `SYM0000`..: the synthetic universe.
pub fn universe(n: usize) -> Vec<SymToken> {
    (0..n)
        .map(|i| {
            let s = format!("SYM{:04}", i);
            SymToken {
                symbol: s.clone(),
                brsymbol: s.clone(),
                name: s.clone(),
                exchange: "NSE".into(),
                brexchange: "NSE".into(),
                token: format!("NSE::::{}", 10_000 + i),
                expiry: String::new(),
                strike: 0.0,
                lot_size: 1,
                instrument_type: "EQ".into(),
                tick_size: 0.05,
            }
        })
        .collect()
}

/// One frame for the broker sockets to send. A stamped frame records the
/// moment it is handed to the socket under its stamp.
#[derive(Clone)]
struct Frame {
    symbol: Option<Arc<str>>,
    text: Arc<str>,
    stamp: Option<u64>,
}

/// A synthetic broker market socket. It streams `rate` ticks a second
/// round-robin over the noisy symbols a connection subscribed (a wobble of
/// a quarter point plus an optional uptrend, never near a benchmark's stop
/// or target), and sends a chosen tick at once on `send_stamped`. `kick`
/// drops every connection without a close frame (a network drop).
pub struct MarketFeed {
    pub url: String,
    frames: broadcast::Sender<Frame>,
    kick: broadcast::Sender<()>,
    sent_at: Arc<Mutex<HashMap<u64, Instant>>>,
    next_stamp: AtomicU64,
    /// Ticks the generator produced, and frames a slow connection skipped.
    pub generated: Arc<AtomicU64>,
    pub skipped: Arc<AtomicU64>,
    pub connections: Arc<AtomicU64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl MarketFeed {
    pub async fn start(noisy: Vec<String>, rate: u64, trend_per_s: f64) -> MarketFeed {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let (frames, _) = broadcast::channel::<Frame>(1 << 16);
        let (kick, _) = broadcast::channel::<()>(4);
        let sent_at = Arc::new(Mutex::new(HashMap::new()));
        let skipped = Arc::new(AtomicU64::new(0));
        let connections = Arc::new(AtomicU64::new(0));
        let accept = tokio::spawn(serve(
            l,
            frames.clone(),
            kick.clone(),
            sent_at.clone(),
            skipped.clone(),
            connections.clone(),
        ));
        let generated = Arc::new(AtomicU64::new(0));
        let mut tasks = vec![accept];
        if rate > 0 && !noisy.is_empty() {
            tasks.push(tokio::spawn(generate(
                Noise {
                    symbols: noisy,
                    rate,
                    trend_per_s,
                },
                frames.clone(),
                generated.clone(),
            )));
        }
        MarketFeed {
            url,
            frames,
            kick,
            sent_at,
            next_stamp: AtomicU64::new(1),
            generated,
            skipped,
            connections,
            tasks,
        }
    }

    /// Send one tick now; returns its stamp (see `sent_at`).
    pub fn send_stamped(&self, symbol: &str, price: f64) -> u64 {
        let stamp = self.next_stamp.fetch_add(1, Ordering::Relaxed);
        let _ = self.frames.send(Frame {
            symbol: Some(symbol.into()),
            text: tick_json(symbol, price).into(),
            stamp: Some(stamp),
        });
        stamp
    }

    /// When the stamped tick went to the app's socket.
    pub fn sent_at(&self, stamp: u64) -> Option<Instant> {
        self.sent_at.lock().get(&stamp).copied()
    }

    /// Drop every connection without a close frame.
    pub fn kick(&self) {
        let _ = self.kick.send(());
    }

    pub fn stop(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn tick_json(symbol: &str, price: f64) -> String {
    format!(r#"{{"t":"{}","x":"NSE","p":{:.2}}}"#, symbol, price)
}

/// What the generator streams.
struct Noise {
    symbols: Vec<String>,
    rate: u64,
    /// Points a second every price rises by, so long stops keep trailing
    /// new highs (zero: a flat market).
    trend_per_s: f64,
}

/// `rate` ticks a second, paced by elapsed time (a late wake sends what is
/// due), each symbol in turn.
async fn generate(noise: Noise, frames: broadcast::Sender<Frame>, generated: Arc<AtomicU64>) {
    let Noise {
        symbols,
        rate,
        trend_per_s,
    } = noise;
    let symbols: Vec<Arc<str>> = symbols.into_iter().map(Arc::from).collect();
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = 0u64;
    loop {
        tick.tick().await;
        let due = (started.elapsed().as_secs_f64() * rate as f64) as u64;
        while sent < due {
            let s = &symbols[(sent as usize) % symbols.len()];
            // A deterministic wobble of up to +-0.25 around the trend.
            let wobble = ((sent.wrapping_mul(2_654_435_761) % 11) as f64 - 5.0) * 0.05;
            let trend = trend_per_s * started.elapsed().as_secs_f64();
            let _ = frames.send(Frame {
                symbol: Some(s.clone()),
                text: tick_json(s, BASE_PRICE + trend + wobble).into(),
                stamp: None,
            });
            sent += 1;
        }
        generated.store(sent, Ordering::Relaxed);
    }
}

async fn serve(
    l: TcpListener,
    frames: broadcast::Sender<Frame>,
    kick: broadcast::Sender<()>,
    sent_at: Arc<Mutex<HashMap<u64, Instant>>>,
    skipped: Arc<AtomicU64>,
    connections: Arc<AtomicU64>,
) {
    let mut conns = tokio::task::JoinSet::new();
    while let Ok((tcp, _)) = l.accept().await {
        while conns.try_join_next().is_some() {}
        let _ = tcp.set_nodelay(true);
        let (mut rx, mut kicked) = (frames.subscribe(), kick.subscribe());
        let (sent_at, skipped) = (sent_at.clone(), skipped.clone());
        connections.fetch_add(1, Ordering::Relaxed);
        conns.spawn(async move {
            let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                return;
            };
            let mut subscribed: HashSet<Arc<str>> = HashSet::new();
            loop {
                tokio::select! {
                    m = ws.next() => match m {
                        Some(Ok(Message::Text(t))) => apply_request(&t, &mut subscribed),
                        Some(Ok(_)) => {}
                        _ => break,
                    },
                    f = rx.recv() => match f {
                        Ok(f) => {
                            let wanted = f
                                .symbol
                                .as_ref()
                                .is_none_or(|s| subscribed.contains(s));
                            if !wanted {
                                continue;
                            }
                            if let Some(stamp) = f.stamp {
                                sent_at.lock().insert(stamp, Instant::now());
                            }
                            if ws.send(Message::Text(f.text.to_string())).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            skipped.fetch_add(n, Ordering::Relaxed);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = kicked.recv() => break,
                }
            }
        });
    }
}

/// `{"sub": ["NSE:SYM0001:1", ...]}` / `{"unsub": [...]}` from `MockFeed`.
fn apply_request(text: &str, subscribed: &mut HashSet<Arc<str>>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    for (key, add) in [("sub", true), ("unsub", false)] {
        for item in v[key].as_array().into_iter().flatten() {
            let Some(symbol) = item.as_str().and_then(|s| s.split(':').nth(1)) else {
                continue;
            };
            if add {
                subscribed.insert(symbol.into());
            } else {
                subscribed.remove(symbol);
            }
        }
    }
}

/// A synthetic broker order socket: sends what `push` is given to every
/// connection; `kick` drops them all without a close frame.
pub struct OrderSocket {
    pub url: String,
    frames: broadcast::Sender<Frame>,
    kick: broadcast::Sender<()>,
    pub connections: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl OrderSocket {
    pub async fn start() -> OrderSocket {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let (frames, _) = broadcast::channel::<Frame>(1024);
        let (kick, _) = broadcast::channel::<()>(4);
        let connections = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(serve(
            l,
            frames.clone(),
            kick.clone(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicU64::new(0)),
            connections.clone(),
        ));
        OrderSocket {
            url,
            frames,
            kick,
            connections,
            task,
        }
    }

    /// Send one order update (the `MockFeed` `{"order": {...}}` frame).
    pub fn push(&self, order: serde_json::Value) {
        let _ = self.frames.send(Frame {
            symbol: None,
            text: serde_json::json!({ "order": order }).to_string().into(),
            stamp: None,
        });
    }

    pub fn kick(&self) {
        let _ = self.kick.send(());
    }

    pub fn stop(&self) {
        self.task.abort();
    }
}

/// What a benchmark asks of the rig.
pub struct RigOptions {
    pub symbols: usize,
    /// Synthetic market ticks a second, over the subscribed noisy symbols.
    pub tick_rate: u64,
    /// Symbols the generator leaves alone (only `send_stamped` ticks them).
    pub quiet: Vec<String>,
    /// See `Noise::trend_per_s`.
    pub trend_per_s: f64,
}

pub struct Rig {
    pub ctx: Arc<AppState>,
    pub mock: Arc<MockBroker>,
    pub clock: Arc<RunningClock>,
    pub market: MarketFeed,
    pub orders: OrderSocket,
    pub key: String,
    pub cookie: String,
    pub server: ServerHandle,
    pub feed: Arc<FeedService>,
    pub base: String,
    pub ws_url: String,
    pub symbols: Vec<String>,
    pub dir: tempfile::TempDir,
}

impl Rig {
    /// Open the app, sign in to the mock broker and wait until the master
    /// is loaded and both broker sockets are connected.
    pub async fn start(o: RigOptions) -> Rig {
        let master = universe(o.symbols);
        let symbols: Vec<String> = master.iter().map(|r| r.symbol.clone()).collect();
        let noisy = symbols
            .iter()
            .filter(|s| !o.quiet.contains(s))
            .cloned()
            .collect();
        let market = MarketFeed::start(noisy, o.tick_rate, o.trend_per_s).await;
        let orders = OrderSocket::start().await;

        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockBroker::new("zerodha"));
        *mock.master.lock() = Some(Ok(master.clone()));
        *mock.feed_url.lock() = Some(market.url.clone());
        *mock.order_feed_url.lock() = Some(orders.url.clone());
        *mock.order_book.lock() = Some(Ok(vec![]));
        for s in &symbols {
            mock.set_quote(Quote {
                symbol: s.clone(),
                exchange: "NSE".into(),
                ltp: BASE_PRICE,
                close: BASE_PRICE,
                ..Default::default()
            });
        }
        let registry = BrokerRegistry::with_symbols(
            mock.symbols.clone(),
            vec![mock.clone() as Arc<dyn Broker>],
        );
        let clock = RunningClock::trading_morning();
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
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

        sign_in(&ctx).await;
        until_long("the master contract", || ctx.symbol_count() == o.symbols).await;
        until_long("the market feed", || ctx.websocket.is_connected()).await;
        until_long("the order socket", || ctx.runtime.order_ws.is_connected()).await;

        Rig {
            base: format!("http://{}", server.addr),
            ws_url: format!("ws://{}", feed_addr),
            cookie: format!("session={}", web.id),
            ctx,
            mock,
            clock,
            market,
            orders,
            key,
            server,
            feed,
            symbols,
            dir,
        }
    }

    /// The broker's position book: these MIS quantities at the base price.
    pub fn set_positions(&self, rows: &[(String, i32)]) {
        let rows = rows.iter().map(|(s, q)| position(s, *q)).collect();
        *self.mock.positions.lock() = Some(Ok(rows));
    }

    pub async fn stop(self) {
        self.feed.stop().await;
        self.server.stop().await;
        self.market.stop();
        self.orders.stop();
        self.ctx.shutdown().await;
    }
}

/// One MIS position row on NSE at the base price.
pub fn position(symbol: &str, qty: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: qty,
        average_price_basis: None,
        overnight_quantity: 0,
        average_price: BASE_PRICE,
        ltp: BASE_PRICE,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: qty.max(0),
        buy_value: 0.0,
        sell_quantity: (-qty).max(0),
        sell_value: 0.0,
    }
}

/// The OAuth sign-in a trader makes: it starts the master download and the
/// broker sockets.
async fn sign_in(ctx: &Arc<AppState>) {
    let url = BrokerAuthService::start_oauth(ctx, "zerodha", Some("perf-session"))
        .await
        .unwrap();
    let mut params = HashMap::new();
    params.insert("request_token".to_string(), "rt-perf".to_string());
    params.insert("state".to_string(), state_from_kite_url(&url));
    BrokerAuthService::complete_oauth(
        ctx,
        "zerodha",
        &params,
        CallbackOrigin::Redirect {
            session_id: Some("perf-session"),
        },
    )
    .await
    .unwrap();
}

/// Poll `f` every 10 ms for up to a minute (a 1,000-symbol master in a
/// debug build on a loaded machine takes a while).
pub async fn until_long(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
