//! The supervisor's state machine against the fake provider, on tokio's
//! paused clock: backoff, loss, refusals, draining, the connection cap.
//! The app-level wiring (sign-in, sign-out, settings, what a caller
//! through the tunnel reaches) is tested in `server::tests::tunnel`.

use super::fake::{wait_status, FakeProvider};
use super::provider::{OpenError, OpenRequest, TunnelIo};
use super::status::{Off, TunnelState, TunnelStatus};
use super::supervisor::{backoff, ServeConnection, Supervisor, MAX_CONNECTIONS, MAX_RETRY};
use crate::security::Secret;
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "2abcDEFghiJKLmnoPQRstu_vwxYZ0123456789";
const LONG: Duration = Duration::from_secs(3600);

struct Run {
    status: watch::Receiver<TunnelStatus>,
    stop: CancellationToken,
    join: JoinHandle<()>,
    /// Connections served so far.
    served: Arc<AtomicUsize>,
    /// Connections being served now.
    active: Arc<AtomicUsize>,
}

/// A supervisor whose connections are counted and held until the caller
/// hangs up or the tunnel drains.
fn run(provider: Arc<FakeProvider>, domain: Option<&str>) -> Run {
    let (tx, status) = watch::channel(TunnelStatus::stopped(Off::Disabled));
    let served = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let (counter, live) = (served.clone(), active.clone());
    let serve: ServeConnection = Arc::new(
        move |mut io: Box<dyn TunnelIo>, drain: CancellationToken| -> BoxFuture<'static, ()> {
            counter.fetch_add(1, Ordering::SeqCst);
            live.fetch_add(1, Ordering::SeqCst);
            let guard = Live(live.clone());
            Box::pin(async move {
                let _guard = guard;
                let mut buf = [0u8; 1];
                tokio::select! {
                    _ = io.read(&mut buf) => {}
                    _ = drain.cancelled() => {}
                }
            })
        },
    );
    let stop = CancellationToken::new();
    let supervisor = Supervisor {
        provider,
        request: OpenRequest {
            authtoken: Secret::new(TOKEN),
            domain: domain.map(String::from),
        },
        status: Arc::new(tx),
        serve,
        stop: stop.clone(),
    };
    Run {
        status,
        stop,
        join: tokio::spawn(supervisor.run()),
        served,
        active,
    }
}

async fn until(r: &Run, pred: impl Fn(&TunnelStatus) -> bool) -> TunnelStatus {
    wait_status(r.status.clone(), LONG, pred).await
}

fn online(s: &TunnelStatus) -> bool {
    s.state == TunnelState::Online
}

/// The count `n` is reached (polled on the paused clock, bounded).
async fn served(r: &Run, n: usize) {
    for _ in 0..10_000 {
        if r.served.load(Ordering::SeqCst) >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("served {} of {}", r.served.load(Ordering::SeqCst), n);
}

#[test]
fn the_backoff_doubles_from_one_second_and_stops_at_thirty() {
    let secs: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
    assert_eq!(backoff(0), Duration::from_secs(1));
    assert_eq!(backoff(u32::MAX), MAX_RETRY);
}

/// The SDK's own reconnect waits 50 ms, 2.5 s, 125 s and then hours; ours
/// is never longer than 30 s between attempts.
#[tokio::test(start_paused = true)]
async fn an_unreachable_ngrok_is_tried_again_at_least_every_thirty_seconds() {
    let fake = FakeProvider::new();
    fake.fail_next(OpenError::Unreachable, 7);
    let r = run(fake.clone(), None);
    let s = until(&r, |s| s.state == TunnelState::Error).await;
    assert!(s.message.contains("Cannot reach ngrok"), "{}", s.message);
    assert!(s.message.contains("keeps trying"), "{}", s.message);
    let s = until(&r, online).await;
    assert_eq!(s.url.as_deref(), Some("https://fake-8.ngrok-free.app"));
    assert_eq!(s.message, "Online at https://fake-8.ngrok-free.app");
    let times = fake.open_times();
    let gaps: Vec<u64> = times.windows(2).map(|w| (w[1] - w[0]).as_secs()).collect();
    assert_eq!(gaps, [1, 2, 4, 8, 16, 30, 30]);
    r.stop.cancel();
    r.join.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_lost_tunnel_says_so_and_opens_again_with_the_same_settings() {
    let fake = FakeProvider::new();
    let r = run(fake.clone(), Some("desk.ngrok-free.app"));
    until(&r, online).await;
    fake.lose();
    let s = until(&r, |s| s.state == TunnelState::Connecting).await;
    assert!(s.message.contains("lost"), "{}", s.message);
    assert_eq!(s.url, None, "no address is offered while it is down");
    let s = until(&r, online).await;
    assert_eq!(s.url.as_deref(), Some("https://desk.ngrok-free.app"));
    assert_eq!(fake.closed(), 1, "the lost tunnel was closed");
    let opens = fake.opens();
    assert_eq!(opens.len(), 2);
    assert_eq!(opens[0], opens[1]);
    let times = fake.open_times();
    assert_eq!((times[1] - times[0]).as_secs(), 1);
    r.stop.cancel();
    r.join.await.unwrap();
    assert_eq!(fake.closed(), 2);
}

/// A wrong authtoken or a domain the account does not own is told to the
/// trader and not tried again: only a settings change can fix it.
#[tokio::test(start_paused = true)]
async fn a_refused_authtoken_or_domain_is_not_tried_again() {
    for (problem, words) in [
        (OpenError::Authtoken, "did not accept the authtoken"),
        (
            OpenError::Domain,
            "cannot use the domain desk.ngrok-free.app",
        ),
        (
            OpenError::Account,
            "will not start a tunnel for this account",
        ),
    ] {
        let fake = FakeProvider::new();
        fake.fail_next(problem, 1);
        let r = run(fake.clone(), Some("desk.ngrok-free.app"));
        let s = until(&r, |s| s.state == TunnelState::Error).await;
        assert!(s.message.contains(words), "{}", s.message);
        assert!(!s.message.contains(TOKEN));
        tokio::time::sleep(LONG).await;
        assert_eq!(fake.opens().len(), 1, "{:?} was tried again", problem);
        r.stop.cancel();
        r.join.await.unwrap();
    }
}

/// Another agent holding the account's tunnel may stop: tried again.
#[tokio::test(start_paused = true)]
async fn a_tunnel_in_use_elsewhere_is_tried_again() {
    let fake = FakeProvider::new();
    fake.fail_next(OpenError::InUse, 2);
    let r = run(fake.clone(), None);
    let s = until(&r, |s| s.state == TunnelState::Error).await;
    assert!(s.message.contains("another ngrok agent"), "{}", s.message);
    until(&r, online).await;
    assert_eq!(fake.opens().len(), 3);
    r.stop.cancel();
    r.join.await.unwrap();
}

/// Stopping lets the connections in flight finish (here they hang up when
/// told to drain) and closes the tunnel; the task ends.
#[tokio::test(start_paused = true)]
async fn a_stop_drains_the_connections_and_closes_the_tunnel() {
    let fake = FakeProvider::new();
    let r = run(fake.clone(), None);
    until(&r, online).await;
    let _a = fake.call().await;
    let _b = fake.call().await;
    served(&r, 2).await;
    r.stop.cancel();
    r.join.await.unwrap();
    assert_eq!(fake.closed(), 1);
    assert_eq!(fake.opens().len(), 1);
}

/// At most MAX_CONNECTIONS callers are served at once. The next is turned
/// away at once, never queued (a queue a stranger could fill would leave
/// the trader's alerts waiting); a caller after a place frees is served.
#[tokio::test(start_paused = true)]
async fn a_caller_past_the_cap_is_turned_away_at_once() {
    let fake = FakeProvider::new();
    let r = run(fake.clone(), None);
    until(&r, online).await;
    let mut callers = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        callers.push(fake.call().await);
    }
    served(&r, MAX_CONNECTIONS).await;
    let mut late = fake.call().await;
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(1), late.read(&mut buf))
        .await
        .expect("turned away at once, not queued");
    assert_eq!(read.unwrap(), 0, "its connection was closed");
    assert_eq!(r.served.load(Ordering::SeqCst), MAX_CONNECTIONS);
    drop(callers.pop());
    count_reaches(&r.active, MAX_CONNECTIONS - 1).await;
    let _next = fake.call().await;
    served(&r, MAX_CONNECTIONS + 1).await;
    r.stop.cancel();
    r.join.await.unwrap();
}

/// `counter` reaches `n` (polled on the test's clock, bounded).
async fn count_reaches(counter: &AtomicUsize, n: usize) {
    for _ in 0..2_000 {
        if counter.load(Ordering::SeqCst) == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{} instead of {}", counter.load(Ordering::SeqCst), n);
}

/// Counts the connections being served; the guard leaves the count on
/// every way a connection's task ends (finished, drained or aborted).
struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// No connection is being served any more.
async fn none_left(live: &AtomicUsize) {
    count_reaches(live, 0).await;
}

/// fd-audit, measured: the app's real connection serving through the
/// tunnel leaves nothing behind after 100 callers, a lost tunnel with
/// callers connected, and a stop with callers connected.
#[tokio::test]
async fn no_connection_outlives_its_caller_a_loss_or_a_stop() {
    use tokio::io::AsyncWriteExt;
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::with(vec![]),
        chrono::Utc::now(),
    );
    let inner = crate::server::tunnel::connections(&t.ctx);
    let live = Arc::new(AtomicUsize::new(0));
    let count = live.clone();
    let serve: ServeConnection = Arc::new(
        move |io: Box<dyn TunnelIo>, drain: CancellationToken| -> BoxFuture<'static, ()> {
            count.fetch_add(1, Ordering::SeqCst);
            let guard = Live(count.clone());
            let served = inner(io, drain);
            Box::pin(async move {
                let _guard = guard;
                served.await
            })
        },
    );
    let fake = FakeProvider::new();
    let (tx, status) = watch::channel(TunnelStatus::stopped(Off::Disabled));
    let stop = CancellationToken::new();
    let join = tokio::spawn(
        Supervisor {
            provider: fake.clone(),
            request: OpenRequest {
                authtoken: Secret::new(TOKEN),
                domain: None,
            },
            status: Arc::new(tx),
            serve,
            stop: stop.clone(),
        }
        .run(),
    );
    let within = Duration::from_secs(10);
    wait_status(status.clone(), within, online).await;
    for _ in 0..100 {
        let mut caller = fake.call().await;
        caller
            .write_all(b"GET /dashboard HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut answer = Vec::new();
        caller.read_to_end(&mut answer).await.unwrap();
        assert!(answer.starts_with(b"HTTP/1.1 404"));
    }
    none_left(&live).await;

    // Callers connected, saying nothing, when the tunnel is lost.
    let mut idle = Vec::new();
    for _ in 0..10 {
        idle.push(fake.call().await);
    }
    fake.lose();
    none_left(&live).await;
    wait_status(status.clone(), within, |s| s.message.contains("lost")).await;
    wait_status(status.clone(), within, online).await;

    // And when it is stopped.
    for _ in 0..10 {
        idle.push(fake.call().await);
    }
    stop.cancel();
    join.await.unwrap();
    none_left(&live).await;
    assert_eq!(fake.closed(), 2);
}

/// One caller's whole exchange through the open tunnel: `raw` sent, the
/// answer read until the connection closes.
async fn exchange(fake: &FakeProvider, raw: &str) -> String {
    use tokio::io::AsyncWriteExt;
    let mut caller = fake.call().await;
    caller.write_all(raw.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    caller.read_to_end(&mut answer).await.unwrap();
    String::from_utf8_lossy(&answer).into_owned()
}

/// Strangers holding places with bodies they never finish do not stop a
/// normal alert while places remain; past the cap the next caller is
/// turned away at once; at the body's time limit each slow caller is
/// answered and its place freed (security review of the tunnel).
#[tokio::test]
async fn a_normal_alert_is_served_while_slow_callers_hold_places() {
    use crate::server::tunnel::{serving, App, Limits};
    use tokio::io::AsyncWriteExt;
    use tower::Layer;
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::with(vec![]),
        chrono::Utc::now(),
    );
    let router = tower_http::normalize_path::NormalizePathLayer::trim_trailing_slash()
        .layer(crate::server::router(t.ctx.clone()).0);
    let limits = Limits {
        header: Duration::from_secs(10),
        body: Duration::from_secs(2),
        answer: Duration::from_secs(10),
        lifetime: Duration::from_secs(30),
    };
    let inner = serving(&t.ctx, App::new(router), limits, t.ctx.tunnel.work());
    let live = Arc::new(AtomicUsize::new(0));
    let count = live.clone();
    let serve: ServeConnection = Arc::new(
        move |io: Box<dyn TunnelIo>, drain: CancellationToken| -> BoxFuture<'static, ()> {
            count.fetch_add(1, Ordering::SeqCst);
            let guard = Live(count.clone());
            let served = inner(io, drain);
            Box::pin(async move {
                let _guard = guard;
                served.await
            })
        },
    );
    let fake = FakeProvider::new();
    let (tx, status) = watch::channel(TunnelStatus::stopped(Off::Disabled));
    let stop = CancellationToken::new();
    let join = tokio::spawn(
        Supervisor {
            provider: fake.clone(),
            request: OpenRequest {
                authtoken: Secret::new(TOKEN),
                domain: None,
            },
            status: Arc::new(tx),
            serve,
            stop: stop.clone(),
        }
        .run(),
    );
    wait_status(status, Duration::from_secs(10), online).await;
    let host = format!("127.0.0.1:{}", t.ctx.server_config().http_port);
    let slow = format!(
        "POST /api/v1/placeorder HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Content-Length: 100\r\n\r\n{{\"apikey\"",
        host
    );
    let alert = format!(
        "POST /strategy/webhook/not-a-token HTTP/1.1\r\nHost: {}\r\n\
         Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
        host
    );
    let mut holding = Vec::new();
    for _ in 0..MAX_CONNECTIONS - 1 {
        let mut c = fake.call().await;
        c.write_all(slow.as_bytes()).await.unwrap();
        holding.push(c);
    }
    count_reaches(&live, MAX_CONNECTIONS - 1).await;

    let answer = exchange(&fake, &alert).await;
    assert!(answer.starts_with("HTTP/1.1 404"), "{}", answer);
    assert!(
        !answer.contains("alerts and API calls"),
        "served by the app: {}",
        answer
    );

    let mut last = fake.call().await;
    last.write_all(slow.as_bytes()).await.unwrap();
    holding.push(last);
    count_reaches(&live, MAX_CONNECTIONS).await;
    let mut late = fake.call().await;
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(1), late.read(&mut buf))
        .await
        .expect("turned away at once, not queued");
    assert_eq!(read.unwrap(), 0);

    // The slow bodies reach their limit: answered, and their places freed.
    let mut first = holding.remove(0);
    let mut answer = Vec::new();
    first.read_to_end(&mut answer).await.unwrap();
    assert!(
        answer.starts_with(b"HTTP/1.1 408"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    none_left(&live).await;
    let answer = exchange(&fake, &alert).await;
    assert!(answer.starts_with("HTTP/1.1 404"), "{}", answer);
    stop.cancel();
    join.await.unwrap();
}
