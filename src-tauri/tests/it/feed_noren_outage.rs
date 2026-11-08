//! Web `test/test_flattrade_issue_2324.py` (marketcalls/openalgo#2324,
//! #2359, `a92418967`): a long network outage never stops the market data
//! feed. On the web, Flattrade's adapter gave up after 10 failed reconnects
//! (about seven minutes) and an error before the socket opened scheduled no
//! retry at all, so the feed stayed down for the day.
//!
//! The desktop's Noren family (Shoonya, Flattrade, Zebu, TradeSmart) runs
//! its feed on the shared manager, which had neither defect: one owned
//! supervisor retries every failed connect with a capped backoff for as
//! long as the feed runs, closes the old socket first, and stops only when
//! the broker refuses the login. These tests pin that for every member,
//! with the real Noren feed against a local broker that is unreachable for
//! a while and then answers.
//!
//! Deviation: the web retries a refused token up to three times with a
//! fresh read of the stored token; the desktop stops at the first refusal
//! and waits for the trader to log in again (`FeedStatus::AuthFailed`).

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::families::noren::streaming::NorenFeed;
use openalgo_desktop_lib::brokers::families::noren::NorenConfig;
use openalgo_desktop_lib::brokers::{flattrade, shoonya, tradesmart, zebu};
use openalgo_desktop_lib::websocket::{FeedConfig, FeedStatus, WebSocketManager};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// The web's `Config.MAX_RECONNECT_ATTEMPTS`, where it gave up.
const WEB_GIVE_UP: usize = 10;
const BACKOFF_MAX: Duration = Duration::from_millis(10);

/// What the local broker saw.
#[derive(Default)]
struct Seen {
    /// Connections made, refused ones included.
    connects: AtomicUsize,
    /// Connections open now, and the most ever open at once.
    open: AtomicUsize,
    most_open: AtomicUsize,
}

/// A connection is counted closed before the client can see it closed, so
/// the client's next connect never overlaps it in the count.
fn closing(s: &Seen) {
    s.open.fetch_sub(1, Ordering::SeqCst);
}

/// A Noren market socket that drops the first `outage` connections before
/// the handshake (an unreachable broker), then answers the login with
/// `{"t":"ak","s":ack}` and holds each later socket for `hold`.
async fn broker(outage: usize, ack: &'static str, hold: Duration) -> (String, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/NorenWSTP/", listener.local_addr().unwrap());
    let seen = Arc::new(Seen::default());
    let s = seen.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let n = s.connects.fetch_add(1, Ordering::SeqCst) + 1;
            let open = s.open.fetch_add(1, Ordering::SeqCst) + 1;
            s.most_open.fetch_max(open, Ordering::SeqCst);
            let s = s.clone();
            tokio::spawn(async move {
                if n <= outage {
                    closing(&s);
                    drop(tcp);
                    return;
                }
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    closing(&s);
                    return;
                };
                let _ = ws.next().await; // the login frame
                let answer = format!(r#"{{"t":"ak","s":"{}","emsg":"Session Expired"}}"#, ack);
                let _ = ws.send(Message::Text(answer)).await;
                let _ = tokio::time::timeout(hold, async {
                    while let Some(Ok(_)) = ws.next().await {}
                })
                .await;
                closing(&s);
                let _ = ws.close(None).await;
            });
        }
    });
    (url, seen)
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(1),
        backoff_max: BACKOFF_MAX,
        stall_timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(2),
        // A session that lasted this long starts the backoff afresh.
        stable_after: Duration::from_millis(100),
        ..FeedConfig::default()
    })
}

fn feed(cfg: &'static NorenConfig, url: &str) -> Box<NorenFeed> {
    Box::new(NorenFeed::new(
        cfg,
        url,
        "UID1",
        "session-token",
        SymbolResolver::new(),
    ))
}

/// The statuses until `done` holds (at most 20 s), with the highest
/// attempt and the longest delay seen on the way.
async fn watch_until(
    m: &WebSocketManager,
    done: impl Fn(&FeedStatus) -> bool,
) -> (FeedStatus, u32, Duration) {
    let mut rx = m.watch_status();
    let (mut attempt, mut delay) = (0u32, Duration::ZERO);
    let last = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = rx.borrow_and_update().clone();
            match &s {
                FeedStatus::Connecting { attempt: a, .. } => attempt = attempt.max(*a),
                FeedStatus::Reconnecting {
                    attempt: a,
                    delay_ms,
                    ..
                } => {
                    attempt = attempt.max(*a);
                    delay = delay.max(Duration::from_millis(*delay_ms));
                }
                _ => {}
            }
            if done(&s) {
                return s;
            }
            if rx.changed().await.is_err() {
                return s;
            }
        }
    })
    .await
    .expect("the feed reached the state within 20 s");
    (last, attempt, delay)
}

fn connected(s: &FeedStatus) -> bool {
    matches!(s, FeedStatus::Connected { .. })
}

/// Web `test_retries_continue_past_the_old_give_up_limit`,
/// `test_the_delay_is_capped`, `test_a_failed_connect_with_no_close_callback`,
/// `test_an_error_before_the_socket_opens`,
/// `test_only_one_attempt_is_ever_pending` and
/// `test_a_reconnect_clears_the_outage_report`, for every Noren member: 30
/// connects refused in a row (three times where the web gave up) are each
/// retried after a capped delay, one at a time, and the feed comes back
/// when the broker does; a session that lasted starts the backoff afresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_outage_never_stops_the_noren_feed() {
    for cfg in [
        &flattrade::CONFIG,
        &shoonya::CONFIG,
        &zebu::CONFIG,
        &tradesmart::CONFIG,
    ] {
        let outage = 3 * WEB_GIVE_UP;
        let (url, seen) = broker(outage, "OK", Duration::from_millis(300)).await;
        let m = manager();
        m.connect(feed(cfg, &url)).await.unwrap();

        let (_, attempt, delay) = watch_until(&m, connected).await;
        assert!(
            seen.connects.load(Ordering::SeqCst) > outage,
            "{}: the feed stopped retrying",
            cfg.id
        );
        assert!(
            attempt as usize > WEB_GIVE_UP,
            "{}: highest attempt seen {}",
            cfg.id,
            attempt
        );
        assert!(delay <= BACKOFF_MAX, "{}: delay {:?}", cfg.id, delay);
        assert!(m.is_running(), "{}", cfg.id);

        // The broker closes the session after 300 ms: a session that long
        // starts the backoff afresh, and the feed connects again.
        let (_, attempt, _) =
            watch_until(&m, |s| matches!(s, FeedStatus::Reconnecting { .. })).await;
        assert_eq!(attempt, 1, "{}: the backoff starts afresh", cfg.id);
        watch_until(&m, connected).await;
        assert_eq!(
            seen.most_open.load(Ordering::SeqCst),
            1,
            "{}: more than one connect at once",
            cfg.id
        );
        m.disconnect().await.unwrap();
    }
}

/// Web `test_a_stopped_adapter_schedules_nothing`: a feed stopped (logout,
/// the 03:00 boundary) during an outage tries no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_feed_tries_no_more() {
    let (url, seen) = broker(usize::MAX, "OK", Duration::ZERO).await;
    let m = manager();
    m.connect(feed(&flattrade::CONFIG, &url)).await.unwrap();
    watch_until(
        &m,
        |s| matches!(s, FeedStatus::Reconnecting { attempt, .. } if *attempt >= 3),
    )
    .await;
    m.disconnect().await.unwrap();
    assert!(!m.is_running());
    let after = seen.connects.load(Ordering::SeqCst);
    // At a 10 ms cap a running feed would connect about 20 times here.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.connects.load(Ordering::SeqCst), after);
    assert_eq!(m.status(), FeedStatus::Disconnected);
}

/// Web `test_a_rejected_token_stops_after_its_budget`: a refused login is
/// not retried against the broker all day.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_login_stops_the_feed() {
    let (url, seen) = broker(0, "NOT_OK", Duration::from_secs(1)).await;
    let m = manager();
    m.connect(feed(&flattrade::CONFIG, &url)).await.unwrap();
    let (last, _, _) = watch_until(&m, |s| matches!(s, FeedStatus::AuthFailed { .. })).await;
    match last {
        FeedStatus::AuthFailed { message, .. } => {
            assert!(message.contains("Log in to Flattrade again"), "{}", message)
        }
        other => panic!("{:?}", other),
    }
    // At a 10 ms cap a retrying feed would connect about 20 times here.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.connects.load(Ordering::SeqCst), 1);
    m.disconnect().await.unwrap();
}
