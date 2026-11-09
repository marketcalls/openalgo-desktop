//! Audit R107-04 and R107-17 at the manager: each session of a connection
//! is announced with its own number, and one broker message's market
//! events go out as one packet of that session, besides one by one on the
//! tick channel.

use super::tests::{fast, server};
use super::*;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, Message, WsRequest};
use crate::brokers::mock::MockFeed;

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
    while seen.len() < 4 {
        let p = tokio::time::timeout(Duration::from_secs(5), packets.recv())
            .await
            .unwrap()
            .unwrap();
        let events = match &p.kind {
            PacketKind::Opened => None,
            PacketKind::Events(e) => Some(e.len()),
        };
        seen.push((p.from, events));
    }
    let (first, second) = (seen[0].0, seen[2].0);
    assert_eq!(seen[0].1, None, "the session's opening: {:?}", seen);
    assert_eq!(seen[1], (first, Some(2)), "both events, one packet");
    assert_eq!(seen[2].1, None, "the reconnect's opening");
    assert_eq!(seen[3], (second, Some(2)));
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
