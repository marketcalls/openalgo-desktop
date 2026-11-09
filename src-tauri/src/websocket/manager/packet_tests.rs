//! Audit R107-04 and R107-17 at the manager: each session of a connection
//! is announced with its own number, and one broker message's market
//! events go out as one packet of that session, besides one by one on the
//! tick channel.

use super::tests::{fast, server, sub};
use super::*;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, Message, WsRequest};
use crate::brokers::mock::MockFeed;
use futures_util::StreamExt;
use tokio::sync::{broadcast, Notify};

/// `MockFeed`, reading a JSON array as one message of several events.
struct ManyFeed(MockFeed);

impl BrokerFeed for ManyFeed {
    fn broker(&self) -> &'static str {
        self.0.broker()
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.0.ws_request()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.0.subscribe_frames(subs)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.0.unsubscribe_frames(subs)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let Message::Text(t) = msg else {
            return Vec::new();
        };
        match serde_json::from_str::<Vec<serde_json::Value>>(t) {
            Ok(items) => items
                .iter()
                .flat_map(|v| self.0.parse(&Message::Text(v.to_string())))
                .collect(),
            Err(_) => self.0.parse(msg),
        }
    }
}

fn sbin() -> FeedSubscription {
    FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "1".into(),
        brsymbol: "SBIN".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

#[tokio::test]
async fn a_message_is_one_packet_of_its_numbered_session() {
    let message = r#"[{"t":"SBIN","x":"NSE","p":1.5},{"t":"TCS","x":"NSE","p":2.5}]"#;
    // The first socket sends the message and closes: the manager connects
    // again, a new session of the same connection, which sends it too.
    let (url, _seen, srv) = server(move |i| (vec![message.to_string()], i == 0)).await;
    let m = WebSocketManager::with_config(fast());
    let mut packets = m.subscribe_packets();
    let mut ticks = m.subscribe_ticks();
    m.subscribe(vec![sbin()]).await.unwrap();
    m.connect(Box::new(ManyFeed(MockFeed::new(url))))
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 5 {
        let p = tokio::time::timeout(Duration::from_secs(5), packets.recv())
            .await
            .unwrap()
            .unwrap();
        let events = match &p.kind {
            PacketKind::Opened => "opened".to_string(),
            PacketKind::Closed => "closed".to_string(),
            PacketKind::Events(e) => e.len().to_string(),
        };
        seen.push((p.from, events));
    }
    let (first, second) = (seen[0].0, seen[3].0);
    let of = |from, what: &str| (from, what.to_string());
    assert_eq!(seen[0], of(first, "opened"), "{:?}", seen);
    assert_eq!(seen[1], of(first, "2"), "both events, one packet");
    assert_eq!(seen[2], of(first, "closed"), "the session's end");
    assert_eq!(seen[3], of(second, "opened"), "the reconnect's opening");
    assert_eq!(seen[4], of(second, "2"));
    assert_eq!(first.conn, second.conn);
    assert!(second.session > first.session, "{:?}", seen);
    for symbol in ["SBIN", "TCS"] {
        let ev = tokio::time::timeout(Duration::from_secs(5), ticks.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&*ev, FeedEvent::Tick(t) if t.symbol == symbol),
            "{:?}",
            ev
        );
    }
    m.disconnect().await.unwrap();
    srv.abort();
}

/// A broker whose connection number `held` waits to finish its handshake
/// until `go`, saying when it has arrived.
async fn holds_a_handshake(
    held: usize,
    arrived: Arc<Notify>,
    go: Arc<Notify>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut sockets = tokio::task::JoinSet::new();
        let mut n = 0;
        while let Ok((tcp, _)) = listener.accept().await {
            let wait = (n == held).then(|| (arrived.clone(), go.clone()));
            n += 1;
            sockets.spawn(async move {
                if let Some((arrived, go)) = wait {
                    arrived.notify_one();
                    go.notified().await;
                }
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    while let Some(Ok(_)) = ws.next().await {}
                }
            });
        }
    });
    (url, task)
}

async fn opened_on(packets: &mut broadcast::Receiver<MarketPacket>, conn: usize) -> SessionId {
    loop {
        let p = tokio::time::timeout(Duration::from_secs(5), packets.recv())
            .await
            .expect("a session opens")
            .unwrap();
        if matches!(p.kind, PacketKind::Opened) && p.from.conn == conn {
            return p.from;
        }
    }
}

/// Review F2: a pool connection whose last instrument leaves while it is
/// still connecting is told to stop, and its slot opens again for a new
/// task. If the old task then finishes connecting after the new one has
/// opened, its session must not outrank the new one's: the bridge would
/// drop every packet of the healthy socket until it next reconnected.
#[tokio::test]
async fn a_stopping_connection_never_outranks_its_replacement() {
    let (arrived, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (url, srv) = holds_a_handshake(1, arrived.clone(), go.clone()).await;
    let m = Arc::new(WebSocketManager::with_config(fast()));
    let mut packets = m.subscribe_packets();
    let more: FeedFactory = {
        let url = url.clone();
        Arc::new(move || Some(Box::new(MockFeed::new(url.clone())) as Box<dyn BrokerFeed>))
    };
    let limits = PoolLimits {
        per_connection: 1,
        connections: 2,
    };
    m.connect_pool(Box::new(MockFeed::new(url)), more, limits)
        .await
        .unwrap();
    opened_on(&mut packets, 0).await;
    let quote = |s: &str| sub(s, FeedMode::Quote);
    m.subscribe(vec![quote("A")]).await.unwrap();
    m.subscribe(vec![quote("B")]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .expect("the extra connection's handshake arrives");

    let stopping = {
        let m = m.clone();
        tokio::spawn(async move { m.unsubscribe(vec![quote("B")]).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while m.connection_states().len() != 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the idle connection's slot empties");
    m.subscribe(vec![quote("C")]).await.unwrap();
    let replacement = opened_on(&mut packets, 1).await;
    go.notify_one();
    let stopped = opened_on(&mut packets, 1).await;
    assert!(
        replacement.session > stopped.session,
        "the replacement {:?}, the task being stopped {:?}",
        replacement,
        stopped
    );
    stopping.await.unwrap().unwrap();
    m.disconnect().await.unwrap();
    srv.abort();
}
