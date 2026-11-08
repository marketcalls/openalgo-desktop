//! Audit C08: what clients holding several modes of one symbol receive for
//! one broker tick, through the production path (the manager's broadcast,
//! the bridge's relay, the registry's fan-out, each client's outbox), and
//! what the Python SDK's callbacks then see. The policy is written in
//! `docs/port/market-data-frames.md`; the recorded transcripts
//! (`tests/fixtures/web/websocket/01_connect_auth_subscribe_flow.jsonl`)
//! show a client holding LTP, Quote and Depth receiving one frame per mode.

use super::bridge::BrokerBridge;
use super::outbox::{Outbox, Outgoing};
use super::registry::Registry;
use super::source::{InstrumentKey, MarketDataSource, Mode};
use crate::brokers::common::streaming::{FeedEvent, NormalizedDepth, NormalizedTick};
use crate::brokers::common::symbols::SymbolResolver;
use crate::websocket::WebSocketManager;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

fn tick(symbol: &str, exchange: &str) -> NormalizedTick {
    NormalizedTick {
        symbol: symbol.into(),
        exchange: exchange.into(),
        mode: 3,
        ltp: 1167.7,
        open: 1180.1,
        high: 1183.9,
        low: 1160.8,
        close: 1187.0,
        volume: 10,
        last_trade_time_ms: 1_791_000_000_000,
        timestamp_ms: 1_791_000_000_100,
        ..Default::default()
    }
}

fn book(symbol: &str, exchange: &str) -> NormalizedDepth {
    NormalizedDepth {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp: 1167.7,
        buy: vec![Default::default(); 5],
        sell: vec![Default::default(); 5],
        timestamp_ms: 1_791_000_000_100,
        ..Default::default()
    }
}

/// The callbacks SDK 2.0.5 (`openalgo/feed.py`, `_on_message`) runs for
/// these frames: LTP for mode 1 with an `ltp`, Quote for mode 2, Depth for
/// mode 3 with a book.
#[derive(Debug, Default, PartialEq, Eq)]
struct Callbacks {
    ltp: usize,
    quote: usize,
    depth: usize,
}

fn sdk(frames: &[Value]) -> Callbacks {
    let mut c = Callbacks::default();
    for f in frames {
        let data = &f["data"];
        match f["mode"].as_u64() {
            Some(1) if data.get("ltp").is_some() => c.ltp += 1,
            Some(2) => c.quote += 1,
            Some(3) if data.get("depth").is_some() => c.depth += 1,
            _ => {}
        }
    }
    c
}

async fn drain(o: &Outbox) -> Vec<Value> {
    let mut out = Vec::new();
    while !o.is_empty() {
        if let Some(Outgoing::Text(t)) = o.next().await {
            out.push(serde_json::from_str(&t).unwrap_or_default());
        }
    }
    out
}

struct Feed {
    manager: Arc<WebSocketManager>,
    bridge: Arc<BrokerBridge>,
    registry: Registry,
    outboxes: BTreeMap<u64, Arc<Outbox>>,
}

impl Feed {
    fn new() -> Self {
        let manager = Arc::new(WebSocketManager::new());
        // The instruments are in the master: the bridge streams only what
        // the broker connection took (audit C02).
        let symbols = SymbolResolver::new();
        symbols.load(
            [
                ("RELIANCE", "NSE", "738561"),
                ("NIFTY", "NSE_INDEX", "256265"),
            ]
            .map(
                |(symbol, exchange, token)| crate::brokers::common::symbols::SymToken {
                    symbol: symbol.into(),
                    brsymbol: symbol.into(),
                    name: symbol.into(),
                    exchange: exchange.into(),
                    brexchange: exchange.into(),
                    token: token.into(),
                    expiry: String::new(),
                    strike: 0.0,
                    lot_size: 1,
                    instrument_type: "EQ".into(),
                    tick_size: 0.05,
                },
            )
            .to_vec(),
        );
        let bridge = BrokerBridge::new(manager.clone(), symbols);
        bridge.start();
        let registry = Registry::new(bridge.clone(), 100);
        Self {
            manager,
            bridge,
            registry,
            outboxes: BTreeMap::new(),
        }
    }

    fn client(&mut self, id: u64, key: &InstrumentKey, modes: &[Mode]) {
        let o = Arc::new(Outbox::new(64, 64));
        self.registry.add_client(id, o.clone());
        self.registry.set_identity(id, "u", "zerodha");
        for m in modes {
            assert!(self.registry.subscribe(id, key, *m, 5).is_ok());
        }
        self.outboxes.insert(id, o);
    }

    /// Emit `events` on the broker feed and fan each resulting update out,
    /// as the feed server's loop does.
    async fn tick(&self, events: Vec<FeedEvent>) {
        let mut rx = self.bridge.updates();
        let n = events.len();
        for e in events {
            self.manager.emit(Arc::new(e));
        }
        for _ in 0..n {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(u)) => self.registry.dispatch(&u),
                other => panic!("no update relayed: {:?}", other.is_ok()),
            }
        }
    }

    async fn frames(&self, id: u64) -> Vec<Value> {
        drain(&self.outboxes[&id]).await
    }
}

/// One equity tick with its book (as Kite sends a full packet: a tick and
/// a depth snapshot) to three clients: all three modes, LTP only, Quote and
/// Depth. Each client receives one frame per mode it holds, each shaped for
/// its mode, so every SDK callback runs once.
#[tokio::test]
async fn each_mode_a_client_holds_gets_one_frame_per_tick() {
    let key = InstrumentKey::new("RELIANCE", "NSE");
    let mut feed = Feed::new();
    feed.client(1, &key, &[Mode::Ltp, Mode::Quote, Mode::Depth]);
    feed.client(2, &key, &[Mode::Ltp]);
    feed.client(3, &key, &[Mode::Quote, Mode::Depth]);
    feed.tick(vec![
        FeedEvent::Tick(tick("RELIANCE", "NSE")),
        FeedEvent::Depth(book("RELIANCE", "NSE")),
    ])
    .await;

    let all = feed.frames(1).await;
    let mut modes: Vec<u64> = all.iter().filter_map(|f| f["mode"].as_u64()).collect();
    modes.sort();
    assert_eq!(modes, vec![1, 2, 3], "{:?}", all);
    assert_eq!(
        sdk(&all),
        Callbacks {
            ltp: 1,
            quote: 1,
            depth: 1
        }
    );
    for f in &all {
        let data = f["data"].as_object().unwrap();
        match f["mode"].as_u64() {
            Some(1) => assert!(!data.contains_key("volume") && !data.contains_key("depth")),
            Some(2) => assert!(data.contains_key("open") && !data.contains_key("depth")),
            _ => assert!(data.contains_key("depth") && data.contains_key("open")),
        }
    }

    let ltp_only = feed.frames(2).await;
    assert_eq!(
        ltp_only.len(),
        1,
        "one LTP frame, not the Depth dict as well"
    );
    assert_eq!(
        sdk(&ltp_only),
        Callbacks {
            ltp: 1,
            ..Default::default()
        }
    );
    assert!(ltp_only[0]["data"].get("depth").is_none());

    let quote_depth = feed.frames(3).await;
    assert_eq!(
        sdk(&quote_depth),
        Callbacks {
            quote: 1,
            depth: 1,
            ..Default::default()
        }
    );
    feed.bridge.stop().await;
}

/// An index tick carries no book and reaches every mode a client holds
/// from the one update: three frames, three callbacks (the Depth callback
/// needs a book, which an index frame has not, as on the web).
#[tokio::test]
async fn an_index_tick_reaches_every_held_mode_once() {
    let key = InstrumentKey::new("NIFTY", "NSE_INDEX");
    let mut feed = Feed::new();
    feed.client(1, &key, &[Mode::Ltp, Mode::Quote, Mode::Depth]);
    feed.tick(vec![FeedEvent::Tick(tick("NIFTY", "NSE_INDEX"))])
        .await;
    let frames = feed.frames(1).await;
    let mut modes: Vec<u64> = frames.iter().filter_map(|f| f["mode"].as_u64()).collect();
    modes.sort();
    assert_eq!(modes, vec![1, 2, 3]);
    assert_eq!(
        sdk(&frames),
        Callbacks {
            ltp: 1,
            quote: 1,
            depth: 0
        }
    );
    feed.bridge.stop().await;
}

/// A Quote packet reaches Quote and LTP holders and no Depth holder (whose
/// frame waits for the book), and an LTP packet the LTP holder alone.
#[tokio::test]
async fn a_lower_mode_packet_reaches_only_the_modes_it_fills() {
    let key = InstrumentKey::new("RELIANCE", "NSE");
    let mut feed = Feed::new();
    feed.client(1, &key, &[Mode::Ltp, Mode::Quote, Mode::Depth]);
    let mut quote = tick("RELIANCE", "NSE");
    quote.mode = 2;
    feed.tick(vec![FeedEvent::Tick(quote)]).await;
    assert_eq!(
        sdk(&feed.frames(1).await),
        Callbacks {
            ltp: 1,
            quote: 1,
            depth: 0
        }
    );
    let mut ltp = tick("RELIANCE", "NSE");
    ltp.mode = 1;
    feed.tick(vec![FeedEvent::Tick(ltp)]).await;
    assert_eq!(
        sdk(&feed.frames(1).await),
        Callbacks {
            ltp: 1,
            ..Default::default()
        }
    );
    feed.bridge.stop().await;
}
