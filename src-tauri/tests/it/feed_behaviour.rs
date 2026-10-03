//! Feed server behaviour beyond the recorded transcripts: reference counting
//! across clients, bounded memory for slow consumers, order-update fan-out
//! from the event bus, throttling, connection and message limits, shutdown.

use crate::feed_support;

use feed_support::*;
use futures_util::SinkExt;
use openalgo_desktop_lib::events::{Event, EventBus};
use openalgo_desktop_lib::feed::orders::OrderRelay;
use openalgo_desktop_lib::feed::source::{DepthBook, DepthLevel, QuoteFields};
use openalgo_desktop_lib::feed::{FakeSource, InstrumentKey, MarketUpdate, Mode};
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

fn sym(i: usize) -> InstrumentKey {
    InstrumentKey::new(format!("SYM{}", i), "NSE")
}

fn depth_update(key: InstrumentKey, ltp: f64, levels: usize) -> MarketUpdate {
    let lv = |p: f64| DepthLevel {
        price: p,
        quantity: 100,
        orders: 3,
    };
    MarketUpdate {
        key,
        mode: Mode::Depth,
        ltp,
        ltt: Some(1_791_000_000_000),
        timestamp: 1_791_000_000_000,
        quote: Some(QuoteFields {
            volume: 1000,
            open: Some(1.0),
            high: Some(2.0),
            low: Some(0.5),
            close: Some(1.5),
            ..Default::default()
        }),
        depth: Some(DepthBook {
            buy: (0..levels).map(|i| lv(ltp - i as f64 * 0.05)).collect(),
            sell: (0..levels).map(|i| lv(ltp + i as f64 * 0.05)).collect(),
        }),
        exact_mode: false,
    }
}

fn source_depth(mode: Mode, depth: u8) -> u8 {
    if mode == Mode::Depth {
        depth
    } else {
        5
    }
}

/// Twenty clients subscribe, re-subscribe, unsubscribe, unsubscribe_all and
/// disconnect abruptly in a seeded random order. After every step the
/// source's active set equals the union of what live clients hold, and each
/// source key was subscribed at most once at a time.
#[tokio::test]
async fn refcount_matches_union_of_live_clients() {
    let h = start_with(FakeSource::permissive(), |_| {}).await;
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    const N: usize = 20;
    let mut clients: Vec<Option<Client>> = Vec::new();
    let mut held: Vec<HashMap<(usize, Mode), u8>> = vec![HashMap::new(); N];
    for _ in 0..N {
        let mut c = Client::connect(&h.url).await;
        c.auth().await;
        clients.push(Some(c));
    }
    let modes = [Mode::Ltp, Mode::Quote, Mode::Depth];
    let depths = [5u8, 20, 30, 50];

    for step in 0..600 {
        let i = rng.gen_range(0..N);
        let Some(c) = clients[i].as_mut() else {
            continue;
        };
        match rng.gen_range(0..10) {
            0..=4 => {
                let s = rng.gen_range(0..6);
                let m = modes[rng.gen_range(0..3)];
                let d = depths[rng.gen_range(0..4)];
                let v = c
                    .request(json!({"action": "subscribe", "symbols": [{"symbol": sym(s).symbol, "exchange": "NSE"}], "mode": m as u8, "depth": d}))
                    .await;
                assert_eq!(v["status"], "success", "{}", v);
                held[i].insert((s, m), d);
            }
            5..=6 => {
                let s = rng.gen_range(0..6);
                let m = modes[rng.gen_range(0..3)];
                let v = c
                    .request(json!({"action": "unsubscribe", "symbols": [{"symbol": sym(s).symbol, "exchange": "NSE", "mode": m.label()}]}))
                    .await;
                assert_eq!(v["status"], "success", "{}", v);
                held[i].remove(&(s, m));
            }
            7 => {
                let v = c.request(json!({"action": "unsubscribe_all"})).await;
                assert_eq!(v["type"], "unsubscribe");
                assert_eq!(
                    v["successful"].as_array().map(Vec::len),
                    Some(held[i].len())
                );
                held[i].clear();
            }
            _ => {
                // Abrupt disconnect: drop the socket without a close frame,
                // then reconnect a fresh client in the same slot.
                clients[i] = None;
                held[i].clear();
                let mut c = Client::connect(&h.url).await;
                c.auth().await;
                clients[i] = Some(c);
            }
        }
        let expected: BTreeSet<(InstrumentKey, Mode, u8)> = held
            .iter()
            .flat_map(|m| m.iter())
            .map(|((s, m), d)| (sym(*s), *m, source_depth(*m, *d)))
            .collect();
        let ok = eventually(Duration::from_secs(5), || {
            h.source.active().into_iter().collect::<BTreeSet<_>>() == expected
        })
        .await;
        assert!(
            ok,
            "step {}: source {:?} expected {:?}",
            step,
            h.source.active(),
            expected
        );
    }
    assert!(
        h.source.violations().is_empty(),
        "{:?}",
        h.source.violations()
    );
    clients.clear();
    assert!(
        eventually(Duration::from_secs(5), || h.source.active_count() == 0
            && h.handle.stats().clients == 0)
        .await
    );
    assert_eq!(h.handle.stats().instruments, 0);
    assert_eq!(h.handle.stats().source_keys, 0);
    assert!(h.source.violations().is_empty());
    h.handle.stop().await;
}

/// A client that stops reading never makes the server buffer more than one
/// frame per subscription; when it reads again it gets the latest prices.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_consumer_memory_is_bounded_and_gets_latest() {
    crate::isolated!(slow_consumer_memory_is_bounded_and_gets_latest);
    const SYMBOLS: usize = 50;
    let h = start_with(FakeSource::permissive(), |_| {}).await;
    let mut slow = Client::connect(&h.url).await;
    slow.auth().await;
    let symbols: Vec<Value> = (0..SYMBOLS)
        .map(|i| json!({"symbol": sym(i).symbol, "exchange": "NSE"}))
        .collect();
    let ack = slow
        .request(json!({"action": "subscribe", "symbols": symbols, "mode": 3, "depth": 50}))
        .await;
    assert_eq!(ack["status"], "success");

    let rss_before = rss_kib();
    let mut max_queue = 0usize;
    let mut last = HashMap::new();
    for n in 0..60_000u64 {
        let i = (n as usize) % SYMBOLS;
        let ltp = 100.0 + n as f64 / 100.0;
        last.insert(sym(i).symbol, ltp);
        h.source.publish(depth_update(sym(i), ltp, 50));
        if n % 500 == 0 {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
            max_queue = max_queue.max(h.handle.queue_lengths().into_iter().max().unwrap_or(0));
        }
    }
    // Let the dispatcher drain the broadcast channel.
    tokio::time::sleep(Duration::from_millis(300)).await;
    max_queue = max_queue.max(h.handle.queue_lengths().into_iter().max().unwrap_or(0));
    let rss_after = rss_kib();
    assert!(
        max_queue <= SYMBOLS,
        "queue grew to {} frames for {} subscriptions",
        max_queue,
        SYMBOLS
    );
    eprintln!(
        "slow consumer: max queued frames {}, RSS {} KiB -> {} KiB",
        max_queue, rss_before, rss_after
    );

    // Now read everything; the final frame per symbol carries the final price.
    let mut seen: HashMap<String, f64> = HashMap::new();
    let mut frames = 0;
    while let Some(v) = slow.recv_within(Duration::from_millis(500)).await {
        if v["type"] == "market_data" {
            frames += 1;
            seen.insert(
                v["symbol"].as_str().unwrap().to_string(),
                v["data"]["ltp"].as_f64().unwrap(),
            );
        }
    }
    assert!(frames < 60_000, "slow client got every tick ({})", frames);
    assert_eq!(seen.len(), SYMBOLS);
    for (s, ltp) in &last {
        assert_eq!(seen.get(s), Some(ltp), "latest price for {}", s);
    }
    drop(slow);
    assert!(eventually(Duration::from_secs(5), || h.source.active_count() == 0).await);
    h.handle.stop().await;
}

/// A client that sends requests but never reads the answers is disconnected
/// once its control queue is full, instead of growing without limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_reading_client_is_disconnected_at_control_cap() {
    let h = start_with(FakeSource::permissive(), |c| c.control_queue_cap = 16).await;
    let mut c = Client::connect(&h.url).await;
    let ping = json!({"action": "ping"}).to_string();
    let mut sent = 0;
    let disconnected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if c.ws
                .send(tokio_tungstenite::tungstenite::Message::Text(ping.clone()))
                .await
                .is_err()
            {
                return;
            }
            sent += 1;
            if h.handle.connections() == 0 {
                return;
            }
        }
    })
    .await;
    assert!(disconnected.is_ok(), "server kept a non-reading client");
    assert!(eventually(Duration::from_secs(5), || h.handle.connections() == 0).await);
    eprintln!("non-reading client dropped after {} unanswered pings", sent);
    h.handle.stop().await;
}

/// `order.update` published on the event bus reaches every socket that sent
/// `subscribe_orders`, as the 18-key web frame, and no other socket.
#[tokio::test]
async fn order_update_fans_out_from_the_event_bus() {
    let bus = EventBus::new();
    let relay = OrderRelay::register(&bus);
    let h = start_full(FakeSource::new(known()), relay, Some(BROKER), |_| {}).await;
    let mut a = Client::connect(&h.url).await;
    let mut b = Client::connect(&h.url).await;
    let mut quiet = Client::connect(&h.url).await;
    for c in [&mut a, &mut b, &mut quiet] {
        c.auth().await;
    }
    for c in [&mut a, &mut b] {
        let v = c
            .request(json!({"action": "subscribe_orders", "request_id": "o1"}))
            .await;
        assert_eq!(
            v,
            json!({"type": "subscribe_orders", "status": "success", "message": "Subscribed to order updates", "request_id": "o1"})
        );
    }
    bus.publish(Event::OrderUpdate(order_update("26071590395364")));
    let want = json!({
        "type": "order_update", "user_id": USER_ID, "mode": "analyze", "broker": "sandbox",
        "orderid": "26071590395364", "symbol": "NIFTY25AUG26FUT", "exchange": "NFO",
        "action": "BUY", "quantity": 65, "price": 24077.8, "trigger_price": 0.0,
        "pricetype": "MARKET", "product": "NRML", "order_status": "complete",
        "filled_quantity": 65, "pending_quantity": 0, "average_price": 24077.8,
        "rejection_reason": ""
    });
    for c in [&mut a, &mut b] {
        let v = c.recv().await;
        assert_eq!(v, want);
        assert_eq!(v.as_object().unwrap().len(), 18);
    }
    assert_eq!(quiet.recv_within(Duration::from_millis(200)).await, None);

    let v = a.request(json!({"action": "unsubscribe_orders"})).await;
    assert_eq!(
        v,
        json!({"type": "unsubscribe_orders", "status": "success", "message": "Unsubscribed from order updates"})
    );
    bus.publish(Event::OrderUpdate(order_update("2")));
    assert_eq!(b.recv().await["orderid"], "2");
    assert_eq!(a.recv_within(Duration::from_millis(200)).await, None);
    h.handle.stop().await;
    bus.shutdown(Duration::from_secs(1)).await;
}

/// With a throttle window, a burst for one symbol is coalesced and the last
/// price is never lost.
#[tokio::test]
async fn throttle_coalesces_and_keeps_the_latest() {
    let h = start_with(FakeSource::new(known()), |c| {
        c.throttle = Duration::from_millis(100)
    })
    .await;
    let mut c = Client::connect(&h.url).await;
    c.auth().await;
    c.request(json!({"action": "subscribe", "symbol": "RELIANCE", "exchange": "NSE", "mode": 1}))
        .await;
    for i in 0..500 {
        h.source.publish(MarketUpdate::ltp(
            InstrumentKey::new("RELIANCE", "NSE"),
            1000.0 + i as f64,
            1_791_000_000_000 + i,
        ));
    }
    let mut frames = Vec::new();
    while let Some(v) = c.recv_within(Duration::from_millis(400)).await {
        frames.push(v);
    }
    assert!(
        !frames.is_empty() && frames.len() <= 3,
        "{} frames",
        frames.len()
    );
    assert_eq!(frames.last().unwrap()["data"]["ltp"], 1499.0);
    h.handle.stop().await;
}

/// Without a throttle every tick is forwarded to a reading client (the web's
/// current behaviour).
#[tokio::test]
async fn no_throttle_forwards_every_tick_to_a_fast_client() {
    let h = start_default().await;
    let mut c = Client::connect(&h.url).await;
    c.auth().await;
    c.request(json!({"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "mode": "LTP"}))
        .await;
    for i in 0..20 {
        h.source.publish(MarketUpdate::ltp(
            InstrumentKey::new("SBIN", "NSE"),
            900.0 + i as f64,
            i,
        ));
        let v = c.recv().await;
        assert_eq!(v["data"]["ltp"], 900.0 + i as f64);
    }
    h.handle.stop().await;
}

#[tokio::test]
async fn connection_cap_rejects_with_try_again_later() {
    let h = start_with(FakeSource::new(known()), |c| c.max_connections = 2).await;
    let a = Client::connect(&h.url).await;
    let _b = Client::connect(&h.url).await;
    assert!(eventually(Duration::from_secs(2), || h.handle.connections() == 2).await);
    let mut c = Client::connect(&h.url).await;
    assert_eq!(
        c.expect_close(Duration::from_secs(5)).await,
        Some((1013, "Too many connections".into()))
    );
    drop(a);
    assert!(eventually(Duration::from_secs(2), || h.handle.connections() == 1).await);
    let mut d = Client::connect(&h.url).await;
    assert_eq!(d.request(json!({"action": "ping"})).await["type"], "pong");
    h.handle.stop().await;
}

#[tokio::test]
async fn oversize_message_closes_with_1009() {
    let h = start_with(FakeSource::new(known()), |c| c.max_message_bytes = 4096).await;
    let mut c = Client::connect(&h.url).await;
    c.send_raw(&"x".repeat(10_000)).await;
    let got = c.expect_close(Duration::from_secs(5)).await;
    assert_eq!(got.map(|g| g.0), Some(1009));
    assert!(eventually(Duration::from_secs(2), || h.handle.connections() == 0).await);
    h.handle.stop().await;
}

/// Stop closes clients with 1001, releases every source key and the port.
#[tokio::test]
async fn stop_closes_clients_and_releases_port_and_subscriptions() {
    let h = start_default().await;
    let addr = h.handle.local_addr();
    let mut c = Client::connect(&h.url).await;
    c.auth().await;
    c.request(json!({"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": 2}))
        .await;
    assert_eq!(h.source.active_count(), 1);
    let source = h.source.clone();
    let stopping = tokio::spawn(h.handle.stop());
    assert_eq!(
        c.expect_close(Duration::from_secs(5)).await.map(|g| g.0),
        Some(1001)
    );
    stopping.await.unwrap();
    assert_eq!(source.active_count(), 0);
    assert!(source.violations().is_empty());
    let rebind = tokio::net::TcpListener::bind(addr).await;
    assert!(rebind.is_ok(), "port released after stop");
}
