//! The connection pool against a local broker socket (audit C07; web
//! `websocket_proxy/connection_manager.py` `ConnectionPool`): instruments
//! fill the first connection before another opens, past the last one's
//! room they are refused, and an extra connection that carries nothing any
//! more closes.

use super::*;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, Message, OrderUpdate};
use crate::brokers::mock::MockFeed;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;

fn sub(symbol: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    }
}

type Seen = Arc<Mutex<Vec<(usize, String)>>>;

/// A broker that records each connection's text frames by the order it
/// was accepted in, and sends each connection `push` once it is open.
async fn broker(push: Vec<String>) -> (String, Seen, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let url = format!("ws://{}", listener.local_addr().expect("bound"));
    let seen: Seen = Arc::default();
    let record = seen.clone();
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        let mut n = 0usize;
        while let Ok((tcp, _)) = listener.accept().await {
            let (idx, record, push) = (n, record.clone(), push.clone());
            n += 1;
            conns.spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let (mut w, mut r) = ws.split();
                for f in push {
                    let _ = w.send(Message::Text(f)).await;
                }
                while let Some(Ok(m)) = r.next().await {
                    if let Message::Text(t) = m {
                        record.lock().push((idx, t.to_string()));
                    }
                }
            });
        }
    });
    (url, seen, task)
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{what} did not happen");
}

fn on(seen: &Seen, conn: usize, symbol: &str) -> bool {
    let wanted = format!("NSE:{symbol}:1");
    seen.lock()
        .iter()
        .any(|(i, f)| *i == conn && f.contains("sub") && f.contains(&wanted))
}

async fn pooled(url: &str, limits: PoolLimits) -> WebSocketManager {
    let m = WebSocketManager::new();
    let more: FeedFactory = {
        let url = url.to_string();
        Arc::new(move || Some(Box::new(MockFeed::new(url.clone())) as Box<dyn BrokerFeed>))
    };
    m.connect_pool(Box::new(MockFeed::new(url)), more, limits)
        .await
        .expect("starts");
    m
}

#[tokio::test]
async fn a_full_connection_opens_another_and_an_idle_one_closes() {
    let (url, seen, srv) = broker(Vec::new()).await;
    let m = pooled(
        &url,
        PoolLimits {
            per_connection: 2,
            connections: 2,
        },
    )
    .await;
    until("the first connection", || m.is_connected()).await;

    let answers = m.admit(["A", "B", "C", "D", "E"].map(sub).to_vec()).await;
    assert_eq!(
        answers.iter().map(Result::is_ok).collect::<Vec<_>>(),
        [true, true, true, true, false]
    );
    assert!(answers[4]
        .as_ref()
        .is_err_and(|why| why.contains("at most 4 instruments")));
    until("the second connection's subscriptions", || {
        on(&seen, 1, "C") && on(&seen, 1, "D")
    })
    .await;
    assert!(on(&seen, 0, "A") && on(&seen, 0, "B"));
    assert!(!on(&seen, 0, "C") && !on(&seen, 1, "A"));
    assert_eq!(m.connection_states().len(), 2);

    // What the second connection carried goes: it closes; the first stays.
    let released = m.release(vec![sub("C"), sub("D")]).await;
    assert!(released.iter().all(Result::is_ok), "{:?}", released);
    until("the idle connection to close", || {
        m.connection_states().len() == 1
    })
    .await;
    assert!(m.is_connected());
    assert_eq!(m.admit(vec![sub("E")]).await, vec![Ok(())]);
    m.disconnect().await.expect("stops");
    assert!(m.connection_states().is_empty());
    srv.abort();
}

/// A broker that sends order updates on every socket (Kite) is heard once:
/// only the first connection relays them.
#[tokio::test]
async fn order_updates_are_heard_from_the_first_connection_only() {
    let update = serde_json::json!({"order": {"orderid": "1"}}).to_string();
    let (url, _seen, srv) = broker(vec![update]).await;
    let m = pooled(
        &url,
        PoolLimits {
            per_connection: 1,
            connections: 2,
        },
    )
    .await;
    let mut rx = m.subscribe_ticks();
    m.admit(vec![sub("A"), sub("B")]).await;
    until("both connections", || m.connection_states().len() == 2).await;
    let mut orders = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if matches!(&*ev, FeedEvent::OrderUpdate(OrderUpdate { .. })) {
            orders += 1;
        }
    }
    assert_eq!(orders, 1);
    m.disconnect().await.expect("stops");
    srv.abort();
}
