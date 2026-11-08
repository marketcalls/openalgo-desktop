//! Sahi feed: protobuf codec and tick normalisation, no network. Ports
//! the cases of the web's `test/test_sahi_streaming_codec.py`; frames are
//! built with the small independent helpers below, not with the codec.

use super::proto::{self, ServerMessage, TickKind};
use super::*;

fn varint(n: i64) -> Vec<u8> {
    let mut n = n as u64;
    let mut out = Vec::new();
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn vfield(field: i64, value: i64) -> Vec<u8> {
    [varint(field << 3), varint(value)].concat()
}

fn lfield(field: i64, payload: &[u8]) -> Vec<u8> {
    [
        varint((field << 3) | 2),
        varint(payload.len() as i64),
        payload.to_vec(),
    ]
    .concat()
}

fn packed(field: i64, values: &[i64]) -> Vec<u8> {
    let body: Vec<u8> = values.iter().flat_map(|v| varint(*v)).collect();
    lfield(field, &body)
}

fn decode(frame: &[u8]) -> ServerMessage {
    proto::decode_server_message(frame).unwrap()
}

fn tick(frame: &[u8]) -> (TickKind, proto::Tick) {
    match decode(frame) {
        ServerMessage::Tick(k, t) => (k, t),
        other => panic!("not a tick: {:?}", other),
    }
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: exchange.into(),
        mode,
        depth: 5,
    }
}

fn feed() -> SahiFeed {
    SahiFeed::new(
        "tok.sig".into(),
        SymbolResolver::new(),
        "wss://direct.sahi.com/v1/stream/marketdata".into(),
    )
}

fn binary(m: &Message) -> &[u8] {
    match m {
        Message::Binary(b) => b,
        other => panic!("not binary: {:?}", other),
    }
}

// ---- encoder ----

#[test]
fn encode_subscribe_bytes() {
    // Subscribe{mode=QUOTE, ids=[1, 300]}: 08 02 | 12 03 01 ac 02.
    assert_eq!(
        proto::encode_subscribe(proto::MODE_QUOTE, &[1, 300]),
        vec![0x0A, 0x07, 0x08, 0x02, 0x12, 0x03, 0x01, 0xAC, 0x02]
    );
}

#[test]
fn encode_unsubscribe_and_mode_change_bytes() {
    assert_eq!(
        proto::encode_unsubscribe(&[5]),
        vec![0x12, 0x03, 0x0A, 0x01, 0x05]
    );
    assert_eq!(
        proto::encode_mode_change(proto::MODE_DEPTH, &[5]),
        vec![0x1A, 0x05, 0x08, 0x03, 0x12, 0x01, 0x05]
    );
}

#[test]
fn encode_large_id() {
    let big = (1i64 << 40) + 7;
    let body = [vfield(1, 1), packed(2, &[big])].concat();
    assert_eq!(
        proto::encode_subscribe(proto::MODE_LTP, &[big]),
        lfield(1, &body)
    );
}

// ---- decoder ----

#[test]
fn decode_ack_packed_and_unpacked() {
    let body = [
        vfield(1, proto::ACK_SUBSCRIBE as i64),
        packed(2, &[11, 22]),
        vfield(2, 33),
    ]
    .concat();
    assert_eq!(
        decode(&lfield(1, &body)),
        ServerMessage::Ack(proto::Ack {
            action: proto::ACK_SUBSCRIBE,
            instrument_ids: vec![11, 22, 33],
        })
    );
}

#[test]
fn decode_ltp_index_without_last_trade_time() {
    let body = [
        vfield(1, 26000),
        vfield(2, proto::SEGMENT_NSE as i64),
        vfield(3, 1),
        vfield(4, 2456735),
    ]
    .concat();
    let (kind, t) = tick(&lfield(2, &body));
    assert_eq!(kind, TickKind::Ltp);
    assert_eq!(t.instrument_id, 26000);
    assert_eq!(t.exchange_segment, proto::SEGMENT_NSE);
    assert_eq!(t.last_trade_price, 2456735);
    assert_eq!(t.last_trade_time, None);
}

#[test]
fn decode_quote_stock_absent_oi() {
    let body = [
        vfield(1, 2885),
        vfield(2, proto::SEGMENT_NSE as i64),
        vfield(3, 2),
        vfield(4, 129050),
        vfield(5, 1761700000123),
        vfield(6, 987654),
        vfield(7, 141950),
        vfield(8, 116150),
    ]
    .concat();
    let (kind, t) = tick(&lfield(3, &body));
    assert_eq!(kind, TickKind::Quote);
    assert_eq!(t.last_trade_time, Some(1761700000123));
    assert_eq!(t.volume_today, 987654);
    assert_eq!(t.open_interest, None);
}

#[test]
fn decode_quote_derivative_oi_zero_present() {
    let body = [
        vfield(1, 50001),
        vfield(2, proto::SEGMENT_NFO as i64),
        vfield(4, 2470000),
        vfield(9, 0),
        vfield(10, 0),
    ]
    .concat();
    assert_eq!(tick(&lfield(3, &body)).1.open_interest, Some(0));
}

fn level(price: i64, qty: i64, orders: i64) -> Vec<u8> {
    [vfield(1, price), vfield(2, qty), vfield(3, orders)].concat()
}

#[test]
fn decode_depth_two_buys_no_sells() {
    let body = [
        vfield(1, 2885),
        vfield(4, 129050),
        lfield(11, &level(129000, 50, 3)),
        lfield(11, &level(128995, 10, 1)),
    ]
    .concat();
    let (kind, t) = tick(&lfield(4, &body));
    assert_eq!(kind, TickKind::Depth);
    let buys: Vec<_> = t
        .best_buys
        .iter()
        .map(|l| (l.price, l.qty, l.orders))
        .collect();
    assert_eq!(buys, vec![(129000, 50, 3), (128995, 10, 1)]);
    assert!(t.best_sells.is_empty());
}

#[test]
fn decode_error() {
    let body = [
        lfield(1, b"UNKNOWN_INSTRUMENT"),
        lfield(2, b"id 9 not found"),
    ]
    .concat();
    assert_eq!(
        decode(&lfield(5, &body)),
        ServerMessage::Error(proto::StreamError {
            code: "UNKNOWN_INSTRUMENT".into(),
            message: "id 9 not found".into(),
        })
    );
}

#[test]
fn unknown_fields_of_every_wire_type_are_skipped() {
    let unknown = [
        vfield(99, 12345),
        varint((98 << 3) | 1),
        vec![1u8; 8],
        lfield(97, b"\xff\xfe junk"),
        varint((96 << 3) | 5),
        vec![2u8; 4],
    ]
    .concat();
    let body = [vfield(1, 7), unknown.clone(), vfield(4, 100)].concat();
    let frame = [unknown, lfield(2, &body), vfield(50, 1)].concat();
    let (kind, t) = tick(&frame);
    assert_eq!(kind, TickKind::Ltp);
    assert_eq!((t.instrument_id, t.last_trade_price), (7, 100));
}

#[test]
fn unknown_oneof_and_empty_frame() {
    assert_eq!(decode(&lfield(9, b"\x08\x01")), ServerMessage::Empty);
    assert_eq!(decode(b""), ServerMessage::Empty);
}

#[test]
fn negative_int64() {
    let body = [vfield(1, 1), vfield(4, -5)].concat();
    assert_eq!(tick(&lfield(2, &body)).1.last_trade_price, -5);
}

#[test]
fn malformed_frames_are_errors() {
    for frame in [
        &b"\x12"[..],
        &b"\x12\x05\x08"[..],
        &b"\x12\x02\x08\xff"[..],
        &b"\x0b"[..],
    ] {
        assert!(proto::decode_server_message(frame).is_err(), "{:?}", frame);
    }
}

// ---- normalisation ----

fn depth_frame() -> Vec<u8> {
    let body = [
        vfield(1, 2885),
        vfield(2, proto::SEGMENT_NSE as i64),
        vfield(4, 129050),
        vfield(5, 1761700000123),
        vfield(6, 4000),
        vfield(7, 141950),
        vfield(8, 116150),
        lfield(11, &level(129000, 50, 3)),
        lfield(11, &level(128995, 10, 1)),
    ]
    .concat();
    lfield(4, &body)
}

#[test]
fn depth_tick_publishes_tick_and_padded_depth_in_rupees() {
    let mut f = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Depth)]);
    let events = f.parse(&Message::Binary(depth_frame()));
    assert_eq!(events.len(), 2);
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("first event is not a tick")
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(t.mode, 3);
    assert_eq!(t.ltp, 1290.5);
    assert_eq!(t.volume, 4000);
    assert_eq!(t.last_trade_time_ms, 1761700000123);
    // Not carried by the stream: never derived, and marked so the frames
    // leave it out.
    assert_eq!((t.open, t.high, t.low, t.close), (0.0, 0.0, 0.0, 0.0));
    assert_eq!((t.change, t.change_percent), (0.0, 0.0));
    assert_eq!(t.absent, NOT_ON_THE_STREAM);
    let FeedEvent::Depth(d) = &events[1] else {
        panic!("second event is not depth")
    };
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(
        d.buy[0],
        DepthLevel {
            price: 1290.0,
            quantity: 50,
            orders: 3
        }
    );
    assert_eq!(d.buy[1].price, 1289.95);
    assert!(d.buy[2..].iter().all(|l| *l == DepthLevel::default()));
    assert!(d.sell.iter().all(|l| *l == DepthLevel::default()));
}

#[test]
fn depth_is_truncated_to_five_levels() {
    let mut f = feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Depth)]);
    let mut body = vec![vfield(1, 3045), vfield(4, 100)];
    for i in 0..7 {
        body.push(lfield(12, &level(100 + i, 1, 1)));
    }
    let events = f.parse(&Message::Binary(lfield(4, &body.concat())));
    let FeedEvent::Depth(d) = &events[1] else {
        panic!("no depth")
    };
    let prices: Vec<f64> = d.sell.iter().map(|l| l.price).collect();
    assert_eq!(prices, vec![1.0, 1.01, 1.02, 1.03, 1.04]);
}

#[test]
fn a_lower_tick_does_not_serve_a_higher_mode() {
    let mut f = feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Depth)]);
    let body = [vfield(1, 3045), vfield(4, 80000), vfield(6, 9)].concat();
    let events = f.parse(&Message::Binary(lfield(3, &body)));
    assert_eq!(events.len(), 1);
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("not a tick")
    };
    assert_eq!((t.mode, t.ltp, t.volume), (2, 800.0, 9));
}

#[test]
fn ltp_mode_leaves_quote_fields_empty() {
    let mut f = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Ltp)]);
    let events = f.parse(&Message::Binary(depth_frame()));
    assert_eq!(events.len(), 1);
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("not a tick")
    };
    assert_eq!((t.mode, t.ltp, t.volume), (1, 1290.5, 0));
}

#[test]
fn index_tick_and_derivative_oi() {
    let mut f = feed();
    f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("NIFTY27OCT26FUT", "NFO", "50001", FeedMode::Quote),
    ]);
    let idx = [
        vfield(1, 26000),
        vfield(2, proto::SEGMENT_NSE as i64),
        vfield(4, 2456735),
    ]
    .concat();
    let events = f.parse(&Message::Binary(lfield(2, &idx)));
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("not a tick")
    };
    assert_eq!(
        (
            t.symbol.as_str(),
            t.exchange.as_str(),
            t.ltp,
            t.last_trade_time_ms
        ),
        ("NIFTY", "NSE_INDEX", 24567.35, 0)
    );
    let fut = [
        vfield(1, 50001),
        vfield(2, proto::SEGMENT_NFO as i64),
        vfield(4, 2470000),
        vfield(10, 1500),
    ]
    .concat();
    let events = f.parse(&Message::Binary(lfield(3, &fut)));
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("not a tick")
    };
    assert_eq!((t.oi, t.ltp), (1500, 24700.0));
}

/// The `data` of the 8765 frames the feed's events make at `mode`, as the
/// bridge builds them (`feed::bridge`), for Sahi.
fn frames(events: &[FeedEvent], mode: crate::feed::Mode) -> Vec<serde_json::Value> {
    use crate::feed::bridge::{depth_update, tick_update};
    use crate::feed::InstrumentKey;
    let mut last = None;
    let mut out = Vec::new();
    for e in events {
        let update = match e {
            FeedEvent::Tick(t) => {
                last = Some(t.clone());
                tick_update(InstrumentKey::new(t.symbol.clone(), t.exchange.clone()), t)
            }
            FeedEvent::Depth(d) => depth_update(
                InstrumentKey::new(d.symbol.clone(), d.exchange.clone()),
                d,
                last.as_ref(),
            ),
            _ => continue,
        };
        if update.mode >= mode {
            let frame = crate::feed::protocol::market_data(&update, mode, 5, "sahi");
            out.push(serde_json::from_str::<serde_json::Value>(&frame).unwrap()["data"].clone());
        }
    }
    out
}

fn keys(v: &serde_json::Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

/// Web test/test_sahi_streaming_codec.py::test_depth_tick_fans_out_to_every_mode:
/// a depth tick serves every mode; `last_trade_time` goes beside `ltt`,
/// and what the stream does not carry (open, high, low, close, last
/// quantity, average price, the book totals) is left out, never sent as 0.
/// The desktop's frames carry no circuit limits, and LTP data no OI.
#[test]
fn a_depth_tick_frames_only_what_the_stream_carries() {
    use crate::feed::Mode;
    let mut f = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Depth)]);
    let events = f.parse(&Message::Binary(depth_frame()));
    let ltp = &frames(&events, Mode::Ltp)[0];
    assert_eq!(
        keys(ltp),
        [
            "exchange",
            "last_trade_time",
            "ltp",
            "ltt",
            "mode",
            "symbol",
            "timestamp"
        ]
    );
    assert_eq!(
        (ltp["ltp"].as_f64(), ltp["ltt"].as_i64()),
        (Some(1290.5), Some(1761700000123))
    );
    assert_eq!(ltp["last_trade_time"], ltp["ltt"]);
    let quote_keys = [
        "exchange",
        "last_trade_time",
        "ltp",
        "ltt",
        "mode",
        "oi",
        "open_interest",
        "symbol",
        "timestamp",
        "volume",
    ];
    let quote = &frames(&events, Mode::Quote)[0];
    assert_eq!(keys(quote), quote_keys);
    assert_eq!(
        (quote["volume"].as_i64(), quote["oi"].as_i64()),
        (Some(4000), Some(0))
    );
    let depth = frames(&events, Mode::Depth);
    let [book] = &depth[..] else {
        panic!("one depth frame: {:?}", depth)
    };
    let mut with_book: Vec<String> = quote_keys.map(String::from).to_vec();
    with_book.push("depth".into());
    with_book.sort();
    assert_eq!(keys(book), with_book);
    assert_eq!(book["depth"]["buy"][0]["price"], 1290.0);
}

/// Web test/test_sahi_streaming_codec.py::test_index_ltp_without_time_and_derivative_oi:
/// an index carries no trade time, so its LTP, Quote and Depth frames have
/// no `ltt` or `last_trade_time` (the desktop sent its receive time).
#[test]
fn an_index_tick_frames_no_trade_time() {
    use crate::feed::Mode;
    let mut f = feed();
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Depth)]);
    let idx = [vfield(1, 26000), vfield(4, 2456735)].concat();
    for kind in [2, 4] {
        let events = f.parse(&Message::Binary(lfield(kind, &idx)));
        assert!(!events.is_empty(), "kind {}", kind);
        for mode in [Mode::Ltp, Mode::Quote, Mode::Depth] {
            for d in frames(&events, mode) {
                assert_eq!(d["ltp"], 24567.35);
                assert!(
                    d.get("ltt").is_none() && d.get("last_trade_time").is_none(),
                    "kind {} {:?}: {}",
                    kind,
                    mode,
                    d
                );
            }
        }
    }
}

#[test]
fn unknown_and_unsubscribed_ids_and_wrong_segments_are_ignored() {
    let mut f = feed();
    let frame = |id: i64, seg: u64| {
        Message::Binary(lfield(
            2,
            &[vfield(1, id), vfield(2, seg as i64), vfield(4, 100)].concat(),
        ))
    };
    assert!(f.parse(&frame(2885, proto::SEGMENT_NSE)).is_empty());
    let s = sub("RELIANCE", "NSE", "2885", FeedMode::Ltp);
    f.subscribe_frames(std::slice::from_ref(&s));
    assert_eq!(f.parse(&frame(2885, proto::SEGMENT_NSE)).len(), 1);
    assert!(f.parse(&frame(2885, proto::SEGMENT_BSE)).is_empty());
    f.unsubscribe_frames(&[s]);
    assert!(f.parse(&frame(2885, proto::SEGMENT_NSE)).is_empty());
    assert!(f.subs.is_empty());
}

#[test]
fn malformed_and_text_frames_publish_nothing() {
    let mut f = feed();
    assert!(f
        .parse(&Message::Binary(b"\x12\x05\x08".to_vec()))
        .is_empty());
    assert!(f.parse(&Message::Text("hello".into())).is_empty());
}

// ---- frames and session ----

#[test]
fn subscribe_groups_by_mode_and_unsubscribe_lists_ids() {
    let mut f = feed();
    let subs = [
        sub("RELIANCE", "NSE", "2885", FeedMode::Quote),
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
    ];
    let frames = f.subscribe_frames(&subs);
    assert_eq!(frames.len(), 2);
    assert_eq!(
        binary(&frames[0]),
        proto::encode_subscribe(proto::MODE_QUOTE, &[2885, 3045])
    );
    assert_eq!(
        binary(&frames[1]),
        proto::encode_subscribe(proto::MODE_LTP, &[26000])
    );
    let frames = f.unsubscribe_frames(&subs[..2]);
    assert_eq!(frames.len(), 1);
    assert_eq!(binary(&frames[0]), proto::encode_unsubscribe(&[2885, 3045]));
    assert_eq!(f.subs.len(), 1);
}

#[test]
fn mode_change_is_one_frame_in_place() {
    let mut f = feed();
    let old = sub("SBIN", "NSE", "3045", FeedMode::Ltp);
    f.subscribe_frames(std::slice::from_ref(&old));
    let new = old.with_mode(FeedMode::Depth);
    let frames = f.mode_change_frames(&old, &new);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        binary(&frames[0]),
        proto::encode_mode_change(proto::MODE_DEPTH, &[3045])
    );
    assert_eq!(f.subs[&3045].mode, FeedMode::Depth);
}

#[test]
fn subscriptions_are_capped_per_connection() {
    let mut f = feed();
    let subs: Vec<_> = (0..MAX_INSTRUMENTS as i64 + 5)
        .map(|i| {
            sub(
                &format!("S{}", i),
                "NSE",
                &(i + 1).to_string(),
                FeedMode::Ltp,
            )
        })
        .collect();
    let frames = f.subscribe_frames(&subs);
    assert_eq!(f.subs.len(), MAX_INSTRUMENTS);
    // 1000 ids go out in two frames of 500.
    assert_eq!(frames.len(), 2);
}

#[test]
fn unparsable_token_without_a_master_row_is_skipped() {
    let mut f = feed();
    assert!(f
        .subscribe_frames(&[sub("X", "NSE", "abc", FeedMode::Ltp)])
        .is_empty());
}

#[test]
fn handshake_carries_bearer_and_user_agent() {
    let req = feed().ws_request().unwrap();
    assert_eq!(req.uri().path(), "/v1/stream/marketdata");
    assert_eq!(req.headers()["Authorization"], "Bearer tok.sig");
    assert_eq!(req.headers()["User-Agent"], "openalgo-sahi/1.0");
    assert!(!format!("{:?}", feed()).contains("tok.sig"));
}

#[test]
fn auth_failures_are_token_closes_and_refusals_only() {
    let f = feed();
    for code in [401, 403, 4401, 4403] {
        assert!(f.is_auth_failure(Some(code)), "{}", code);
    }
    for code in [1000, 4408, 4409, 4429, 500] {
        assert!(!f.is_auth_failure(Some(code)), "{}", code);
    }
    assert!(!f.is_auth_failure(None));
}

#[test]
fn paise_to_rupees() {
    assert_eq!(rupees(249955), 2499.55);
    assert_eq!(rupees(5), 0.05);
    assert_eq!(rupees(0), 0.0);
}
