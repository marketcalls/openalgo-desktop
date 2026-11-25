//! Pocketful's market and order feeds on the rebuilt web adapters (web
//! a1d539764, marketcalls/openalgo#2467): the packets built field by field
//! at the SDK's offsets with the docs' sample values, as
//! `test/test_pocketful_streaming.py` and `test/test_pocketful_order_updates.py`
//! build them.

use super::order_feed::{PocketfulOrderFeed, MODE_ORDER, MODE_TRADE};
use super::*;
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::common::symbols::tests::row;
use serde_json::json;

// ---------------------------------------------------------------------------
// Packets (packet_decoder.py's layouts)
// ---------------------------------------------------------------------------

/// detailed-marketdata.md's sample for NSE token 1333.
const DETAILED: &[(usize, u32)] = &[
    (2, 1333),        // instrument_token
    (6, 144_895),     // last_traded_price
    (10, 1710489044), // last_traded_time
    (14, 1),          // last_traded_quantity
    (18, 8_014_246),  // trade_volume
    (22, 144_890),    // best_bid_price
    (26, 25),         // best_bid_quantity
    (30, 144_895),    // best_ask_price
    (34, 628),        // best_ask_quantity
    (54, 145_018),    // average_trade_price
    (58, 1710489044), // exchange_timestamp
    (62, 145_000),    // open_price
    (66, 145_930),    // high_price
    (70, 144_415),    // low_price
    (74, 145_545),    // close_price
    (78, 175_750),    // yearly_high_price
    (82, 136_355),    // yearly_low_price
    (86, 130_990),    // lowDPR
    (90, 160_095),    // highDPR
    (94, 0),          // currentOpenInterest
    (98, 0),          // initialOpenInterest
];

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// The docs' detailed packet, with `changes` at their offsets.
fn detailed(changes: &[(usize, u32)]) -> Vec<u8> {
    let mut b = vec![0u8; 102];
    b[0] = MODE_DETAILED;
    b[1] = 1;
    for &(at, v) in DETAILED {
        put32(&mut b, at, v);
    }
    put64(&mut b, 38, 624_983); // total_buy_quantity
    put64(&mut b, 46, 899_665); // total_sell_quantity
    for &(at, v) in changes {
        put32(&mut b, at, v);
    }
    b
}

/// compact-marketdata.md's sample: LTP 145065, change 4294966816 (-480).
fn compact(code: u8, token: u32, ltp: u32) -> Vec<u8> {
    let mut b = vec![0u8; 42];
    b[0] = MODE_COMPACT;
    b[1] = code;
    put32(&mut b, 2, token);
    put32(&mut b, 6, ltp);
    put32(&mut b, 10, 4_294_966_816);
    put32(&mut b, 14, 1710491906);
    put32(&mut b, 18, 130_990);
    put32(&mut b, 22, 160_095);
    put32(&mut b, 34, 145_060);
    put32(&mut b, 38, 145_070);
    b
}

/// snapquote-data.md's sample.
fn snapquote() -> Vec<u8> {
    let mut b = vec![0u8; 166];
    b[0] = MODE_SNAPQUOTE;
    b[1] = 1;
    put32(&mut b, 2, 1333);
    let arrays: [(usize, [u32; 5]); 6] = [
        (6, [3, 3, 1, 2, 6]),
        (26, [145_100, 145_095, 145_090, 145_085, 145_080]),
        (46, [121, 53, 64, 76, 130]),
        (66, [1, 4, 1, 3, 4]),
        (86, [145_110, 145_135, 145_140, 145_145, 145_150]),
        (106, [33, 441, 68, 113, 324]),
    ];
    for (start, values) in arrays {
        for (i, v) in values.iter().enumerate() {
            put32(&mut b, start + 4 * i, *v);
        }
    }
    put32(&mut b, 126, 145_026);
    put32(&mut b, 130, 145_000);
    put32(&mut b, 134, 145_930);
    put32(&mut b, 138, 144_415);
    put32(&mut b, 142, 145_545);
    put64(&mut b, 146, 653_906);
    put64(&mut b, 154, 870_361);
    put32(&mut b, 162, 9_438_681);
    b
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: exchange.trim_end_matches("_INDEX").into(),
        mode,
        depth: 5,
    }
}

fn texts(frames: &[Message]) -> Vec<Value> {
    frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            other => panic!("{:?}", other),
        })
        .collect()
}

fn feed_with(subs: &[FeedSubscription]) -> PocketfulFeed {
    let mut f = PocketfulFeed::new("wss://algo.pocketful.in", "c", "t", SymbolResolver::new());
    f.subscribe_frames(subs);
    f
}

fn tick(events: &[FeedEvent]) -> &NormalizedTick {
    match events {
        [FeedEvent::Tick(t), ..] => t,
        other => panic!("{:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Decoder and messages
// ---------------------------------------------------------------------------

/// Web test/test_pocketful_streaming.py::test_detailed_packet_decodes_to_the_docs_sample,
/// ::test_compact_packet_decodes_to_the_docs_sample,
/// ::test_snapquote_packet_decodes_to_the_docs_sample
#[test]
fn packets_decode_to_the_docs_samples() {
    let d = decode_detailed(&detailed(&[])).unwrap();
    assert_eq!(
        (d.exchange_code, d.token, d.ltp, d.ltt),
        (1, 1333, 1448.95, 1710489044)
    );
    assert_eq!((d.ltq, d.volume), (1, 8_014_246));
    assert_eq!(
        (d.bid, d.bid_qty, d.ask, d.ask_qty),
        (1448.9, 25, 1448.95, 628)
    );
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (624_983, 899_665));
    assert_eq!(d.average_price, 1450.18);
    assert_eq!(
        (d.open, d.high, d.low, d.close),
        (1450.0, 1459.3, 1444.15, 1455.45)
    );
    assert_eq!((d.year_high, d.year_low, d.oi), (1757.5, 1363.55, 0));

    let c = decode_compact(&compact(1, 1333, 145_065)).unwrap();
    assert_eq!(
        (c.exchange_code, c.token, c.ltp, c.ltt),
        (1, 1333, 1450.65, 1710491906)
    );
    assert_eq!((c.bid, c.ask), (1450.6, 1450.7));

    let s = decode_snapquote(&snapquote()).unwrap();
    let level = |l: &DepthLevel| (l.price, l.quantity, l.orders);
    assert_eq!(level(&s.bids[0]), (1451.0, 121, 3));
    assert_eq!(level(&s.bids[4]), (1450.8, 130, 6));
    assert_eq!(level(&s.asks[0]), (1451.1, 33, 1));
    assert_eq!(level(&s.asks[4]), (1451.5, 324, 4));
    assert_eq!(s.average_price, 1450.26);
    assert_eq!(
        (s.open, s.high, s.low, s.close),
        (1450.0, 1459.3, 1444.15, 1455.45)
    );
    assert_eq!(
        (s.total_buy_qty, s.total_sell_qty, s.volume),
        (653_906, 870_361, 9_438_681)
    );
}

/// Web test/test_pocketful_streaming.py::test_short_packets_decode_to_nothing_and_extra_bytes_are_ignored:
/// a packet shorter than its layout is nothing, bytes past it are ignored,
/// and a text frame has no mode byte and carries no market data.
#[test]
fn short_packets_and_text_frames_carry_nothing() {
    assert!(decode_detailed(&detailed(&[])[..101]).is_none());
    assert!(decode_compact(&compact(1, 1333, 1)[..41]).is_none());
    assert!(decode_snapquote(&snapquote()[..165]).is_none());
    let mut longer = compact(1, 1333, 145_065);
    longer.extend_from_slice(&[0, 1]);
    assert_eq!(decode_compact(&longer).map(|c| c.ltp), Some(1450.65));
    assert!(decode_frame(&Message::Binary(Vec::new())).is_none());
    assert!(decode_frame(&Message::Text(r#"{"mode": 2}"#.into())).is_none());
    let json_packet = json!({"mode": 1, "exchange_code": 1, "instrument_token": 1333,
                             "last_traded_price": 144895});
    assert!(decode_frame(&Message::Text(json_packet.to_string())).is_none());
}

/// Web test/test_pocketful_streaming.py::test_the_feed_address_and_messages_are_the_sdks
#[test]
fn the_feed_address_and_messages_are_the_sdks() {
    assert_eq!(
        feed_url("wss://algo.pocketful.in", "CSG981", "tok.en-1"),
        "wss://algo.pocketful.in/ws/v1/feeds?login_id=CSG981&access_token=tok.en-1"
    );
    let f = feed_with(&[]);
    let (every, frame) = f.heartbeat().unwrap();
    assert_eq!(every, Duration::from_secs(8));
    assert_eq!(texts(&[frame]), [json!({"a": "h", "v": [], "m": ""})]);
    assert_eq!(
        texts(&[sub_frame("subscribe", 1, 1333, MODE_SNAPQUOTE)]),
        [json!({"a": "subscribe", "v": [[1, 1333]], "m": "full_snapquote"})]
    );
    assert_eq!(
        [MODE_DETAILED, MODE_COMPACT, MODE_SNAPQUOTE].map(market_type),
        ["marketdata", "compact_marketdata", "full_snapquote"]
    );
}

/// Web test/test_pocketful_streaming.py::test_exchange_codes_are_the_docs_and_an_unknown_exchange_is_refused,
/// ::test_an_exchange_the_feed_does_not_have_is_refused_not_sent_as_nse:
/// an exchange the feed has no code for is never sent, as NSE or at all.
#[test]
fn exchange_codes_are_the_docs_and_an_unknown_exchange_is_not_sent() {
    let docs = [
        ("NSE", 1),
        ("NFO", 2),
        ("CDS", 3),
        ("MCX", 4),
        ("BSE", 6),
        ("BFO", 7),
    ];
    for (exchange, code) in docs {
        assert_eq!(feed_exchange_code(exchange), Some(code), "{}", exchange);
    }
    assert_eq!(feed_exchange_code("NSE_INDEX"), Some(1));
    assert_eq!(feed_exchange_code("BSE_INDEX"), Some(6));
    assert_eq!(feed_exchange_code("NCDEX"), None);
    let mut f = feed_with(&[]);
    assert!(f
        .subscribe_frames(&[sub("SOYBEAN", "NCDEX", "555", FeedMode::Ltp)])
        .is_empty());
}

/// Web test/test_pocketful_streaming.py::test_an_index_streams_on_its_cash_segment:
/// NIFTY streams on NSE's code, and its compact packet is its LTP.
#[test]
fn an_index_streams_on_its_cash_segment() {
    let mut f = feed_with(&[]);
    let frames = f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp)]);
    assert_eq!(
        texts(&frames),
        [json!({"a": "subscribe", "v": [[1, 26000]], "m": "compact_marketdata"})]
    );
    let ev = f.parse(&Message::Binary(compact(1, 26000, 2_400_050)));
    let t = tick(&ev);
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str()),
        ("NIFTY", "NSE_INDEX")
    );
    assert_eq!((t.mode, t.ltp), (1, 24000.5));
}

// ---------------------------------------------------------------------------
// Packets to events
// ---------------------------------------------------------------------------

/// Web test/test_pocketful_streaming.py::test_ltp_mode_publishes_only_the_documented_ltp_frame:
/// a compact packet is an LTP tick with its price and trade time; the
/// packet's change, OI and best prices are not carried.
#[test]
fn a_compact_packet_is_the_ltp_tick() {
    let mut f = feed_with(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Ltp)]);
    let ev = f.parse(&Message::Binary(compact(1, 1333, 145_065)));
    let t = tick(&ev);
    assert_eq!((t.mode, t.ltp), (1, 1450.65));
    assert_eq!(t.last_trade_time_ms, 1_710_491_906_000);
    assert!(t.carried.is_empty(), "{:?}", t);
    assert!(t.extras.is_empty(), "{:?}", t);
    // A price of 0 is no price: nothing to publish.
    let ev = f.parse(&Message::Binary(compact(1, 1333, 0)));
    assert!(!tick(&ev).knows(QuoteField::Ltp));
}

/// Web test/test_pocketful_streaming.py::test_quote_mode_publishes_the_documented_quote_frame
#[test]
fn a_detailed_packet_is_the_quote_tick() {
    let mut f = feed_with(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Quote)]);
    let ev = f.parse(&Message::Binary(detailed(&[])));
    let t = tick(&ev);
    assert_eq!(t.mode, 2);
    assert_eq!((t.ltp, t.change, t.change_percent), (1448.95, -6.5, -0.45));
    assert_eq!(t.volume, 8_014_246);
    assert_eq!(
        (t.open, t.high, t.low, t.close),
        (1450.0, 1459.3, 1444.15, 1455.45)
    );
    assert_eq!((t.last_quantity, t.average_price), (1, 1450.18));
    let x = &t.extras;
    assert_eq!((x.bid_price, x.bid_quantity), (Some(1448.9), Some(25)));
    assert_eq!((x.ask_price, x.ask_quantity), (Some(1448.95), Some(628)));
    assert!(t.knows(QuoteField::OpenInterest) && t.knows(QuoteField::Change));
}

/// Web test/test_pocketful_streaming.py::test_a_price_the_feed_does_not_have_is_left_out_not_sent_as_zero
#[test]
fn a_price_the_feed_does_not_have_is_left_out() {
    let mut f = feed_with(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Quote)]);
    let ev = f.parse(&Message::Binary(detailed(&[(6, 0), (62, 0), (74, 0)])));
    let t = tick(&ev);
    for field in [
        QuoteField::Ltp,
        QuoteField::Open,
        QuoteField::Close,
        QuoteField::Change,
    ] {
        assert!(!t.knows(field), "{:?}: {:?}", field, t);
    }
    assert!(t.knows(QuoteField::High));
    assert_eq!(t.high, 1459.3);
}

/// Web test/test_pocketful_streaming.py::test_another_exchanges_packet_with_the_same_token_is_not_published
#[test]
fn another_exchanges_packet_with_the_same_token_is_nobodys() {
    let mut f = feed_with(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Ltp)]);
    assert!(f
        .parse(&Message::Binary(compact(2, 1333, 145_065)))
        .is_empty());
}

/// Web test/test_pocketful_streaming.py::test_depth_never_publishes_the_average_price_as_the_last_traded_price:
/// the snapquote's quote has no trade price (its average stays the
/// average), and its book is the packet's five levels a side.
#[test]
fn a_snapquote_never_carries_its_average_as_a_trade_price() {
    let mut f = feed_with(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Depth)]);
    let ev = f.parse(&Message::Binary(snapquote()));
    match &ev[..] {
        [FeedEvent::Tick(t), FeedEvent::Depth(d)] => {
            assert_eq!(t.mode, 3);
            assert!(!t.knows(QuoteField::Ltp), "{:?}", t);
            assert_eq!(t.average_price, 1450.26);
            assert_eq!(
                (t.total_buy_quantity, t.total_sell_quantity),
                (653_906, 870_361)
            );
            assert_eq!(t.volume, 9_438_681);
            assert!(d.absent.contains(QuoteField::Ltp));
            assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
            assert_eq!(
                (d.buy[0].price, d.buy[0].quantity, d.buy[0].orders),
                (1451.0, 121, 3)
            );
            assert_eq!(
                (d.sell[4].price, d.sell[4].quantity, d.sell[4].orders),
                (1451.5, 324, 4)
            );
        }
        other => panic!("{:?}", other),
    }
}

/// Web test/test_pocketful_streaming.py::test_a_depth_subscription_takes_the_book_and_the_last_traded_price:
/// a Depth subscription takes the book and a price channel. The web's is
/// the compact channel; the desktop's the detailed one, which carries the
/// same price and the quote its other holders need (R107-07, one stream
/// per instrument at the highest mode held).
#[test]
fn a_depth_subscription_takes_the_book_and_a_price_channel() {
    let mut f = feed_with(&[]);
    let frames = f.subscribe_frames(&[sub("HDFCBANK", "NSE", "1333", FeedMode::Depth)]);
    assert_eq!(
        texts(&frames),
        [
            json!({"a": "subscribe", "v": [[1, 1333]], "m": "marketdata"}),
            json!({"a": "subscribe", "v": [[1, 1333]], "m": "full_snapquote"}),
        ]
    );
}

// ---------------------------------------------------------------------------
// The order feed
// ---------------------------------------------------------------------------

const CLIENT_ID: &str = "SB3179";
/// A fake access token (no real credential, nothing gitleaks matches).
const ACCESS: &str = "Au-C4o8pSEKRETtoken_part.YoJRluHBJophmZuYTN";

fn order_feed() -> PocketfulOrderFeed {
    let symbols = SymbolResolver::new();
    symbols.load(vec![row("PNB", "PNB-EQ", "NSE", "10666")]);
    PocketfulOrderFeed::new("wss://algo.pocketful.in", CLIENT_ID, ACCESS, symbols)
}

/// order-updates.md's "Order Updates Response" (its trailing comma dropped).
fn doc_order() -> Value {
    json!({
        "average_price": 0, "average_trade_price": 0, "client_id": "SB3179",
        "deposit": 0, "device": "", "disclosed_quantity": 0, "exchange": "NSE",
        "exchange_order_id": "1200000008791186", "exchange_time": 1641191673,
        "execution_type": "REGULAR", "filled_quantity": 0, "instrument_token": 10666,
        "last_activity_reference": 1325678673994464800_u64, "leg_order_indicator": "",
        "login_id": "SB3179", "lot_size": 0, "lotsize": 1,
        "market_protection_percentage": 0, "mode": "MODIFY", "nest_request_id": "",
        "nnfid": 111111111111100_u64, "oms_order_id": "20220103-1157",
        "order_entry_time": 1641191331, "order_side": "BUY",
        "order_status": "MODCONFIRMED", "order_type": "LIMIT", "price": 36.95,
        "pro_cli": "CLIENT", "product": "MIS", "quantity": 1, "rejection_code": "NONE",
        "rejection_reason": "NONE", "remaining_quantity": 1, "series": "",
        "spread_token": "", "trade_price": 0, "trading_symbol": "PNB-EQ",
        "transaction_type": "LIMIT", "trigger_price": 0, "user_order_id": "10002",
        "validity": "DAY"
    })
}

/// trade-updates.md's "Trade Updates Response".
fn doc_trade() -> Value {
    json!({"error": {"code": 0, "message": ""}, "result": [{
        "client_id": "SB3179", "exchange": "NSE", "exchange_order_id": "1200000014631276",
        "exchange_time": 1641201487, "filled_quantity": 1, "instrument_token": 10666,
        "login_id": "SB3179", "oms_order_id": "20220103-1891", "order_side": "BUY",
        "pro_cli": "CLIENT", "product": "CNC", "trade_id": "53768151", "trade_price": 38,
        "trading_symbol": "PNB-EQ", "transaction_type": null, "user_order_id": "10002",
        "v_login_id": null}]})
}

const ASCII_HEADER: [u8; 4] = [0x00, 0x00, 0x04, 0x4c];
/// 0xE8 is not UTF-8 there: the SDK's whole-frame decode fails on it.
const HIGH_HEADER: [u8; 4] = [0x00, 0x00, 0x03, 0xe8];

fn frame(mode: u8, payload: &Value, header: [u8; 4]) -> Vec<u8> {
    let mut b = vec![mode];
    b.extend_from_slice(&header);
    b.extend_from_slice(payload.to_string().as_bytes());
    b
}

fn order_frame(records: Vec<Value>) -> Vec<u8> {
    frame(
        MODE_ORDER,
        &json!({"error": {"code": 0, "message": ""}, "result": records}),
        ASCII_HEADER,
    )
}

fn order(changes: Value) -> Value {
    let mut record = doc_order();
    for (k, v) in changes.as_object().unwrap() {
        record[k] = v.clone();
    }
    record
}

fn published(f: &mut PocketfulOrderFeed, msg: Message) -> Vec<OrderUpdate> {
    f.parse(&msg)
        .into_iter()
        .map(|e| match e {
            FeedEvent::OrderUpdate(u) => u,
            other => panic!("{:?}", other),
        })
        .collect()
}

/// Web test/test_pocketful_order_updates.py::test_the_feed_url_is_the_sdk_path_with_both_parts_of_the_stored_token,
/// ::test_the_open_socket_subscribes_to_the_updates_channel_as_the_sdk_does,
/// ::test_the_heartbeat_is_the_sdk_heartbeat_every_eight_seconds
#[test]
fn the_order_socket_connects_and_subscribes_as_the_sdk_does() {
    let mut f = order_feed();
    assert_eq!(
        f.ws_request().unwrap().uri().to_string(),
        format!(
            "wss://algo.pocketful.in/ws/v1/feeds?login_id={}&access_token={}",
            CLIENT_ID, ACCESS
        )
    );
    assert_eq!(
        texts(&f.on_connected()),
        [json!({"a": "subscribe", "v": ["SB3179", "web"], "m": "updates"})]
    );
    let (every, beat) = f.heartbeat().unwrap();
    assert_eq!(every, Duration::from_secs(8));
    assert_eq!(texts(&[beat]), [json!({"a": "h", "v": [], "m": ""})]);
    assert!(f
        .subscribe_frames(&[sub("PNB", "NSE", "10666", FeedMode::Ltp)])
        .is_empty());
}

/// Web test/test_pocketful_order_updates.py::test_the_documented_order_update_becomes_one_normalized_event
#[test]
fn the_documented_order_update_is_one_event() {
    let mut f = order_feed();
    let doc = json!({"error": {"code": 0, "message": ""}, "result": [doc_order()]});
    let got = published(
        &mut f,
        Message::Binary(frame(MODE_ORDER, &doc, ASCII_HEADER)),
    );
    assert_eq!(
        got,
        [OrderUpdate {
            orderid: "20220103-1157".into(),
            symbol: "PNB".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            quantity: 1,
            price: 36.95,
            trigger_price: 0.0,
            pricetype: "LIMIT".into(),
            product: "MIS".into(),
            order_status: "open".into(),
            filled_quantity: 0,
            pending_quantity: 1,
            average_price: 0.0,
            rejection_reason: String::new(),
        }]
    );
}

/// Web test/test_pocketful_order_updates.py::test_every_order_in_the_result_list_is_published,
/// ::test_an_error_envelope_is_logged_and_its_orders_still_published
#[test]
fn every_order_of_a_frame_is_published() {
    let mut f = order_feed();
    let got = published(
        &mut f,
        Message::Binary(order_frame(vec![
            order(json!({"oms_order_id": "A-1", "order_status": "COMPLETE",
                         "filled_quantity": 1, "remaining_quantity": 0})),
            order(json!({"oms_order_id": "A-2", "order_status": "REJECTED",
                         "rejection_reason": "RMS: margin"})),
        ])),
    );
    let seen: Vec<_> = got
        .iter()
        .map(|u| (u.orderid.as_str(), u.order_status.as_str()))
        .collect();
    assert_eq!(seen, [("A-1", "complete"), ("A-2", "rejected")]);
    let errored = json!({"error": {"code": 1, "message": "partial"}, "result": [doc_order()]});
    let got = published(
        &mut f,
        Message::Binary(frame(MODE_ORDER, &errored, ASCII_HEADER)),
    );
    assert_eq!(got.len(), 1);
}

/// Web test/test_pocketful_order_updates.py::test_a_trade_update_publishes_nothing,
/// ::test_frames_that_carry_no_order_publish_nothing,
/// ::test_an_unreadable_order_frame_is_logged_and_dropped
#[test]
fn frames_that_carry_no_order_publish_nothing() {
    let mut f = order_feed();
    let market = |mode: u8, len: usize| {
        let mut b = vec![0u8; len];
        b[0] = mode;
        Message::Binary(b)
    };
    let mut unreadable = vec![MODE_ORDER];
    unreadable.extend_from_slice(&ASCII_HEADER);
    unreadable.extend_from_slice(b"{not json");
    for msg in [
        Message::Binary(frame(MODE_TRADE, &doc_trade(), ASCII_HEADER)),
        market(1, 102),
        market(2, 42),
        market(4, 166),
        Message::Binary(Vec::new()),
        Message::Text(r#"{"a": "h", "v": [], "m": ""}"#.into()),
        Message::Binary(order_frame(Vec::new())),
        Message::Binary(unreadable),
    ] {
        assert!(published(&mut f, msg.clone()).is_empty(), "{:?}", msg);
    }
}

/// Web test/test_pocketful_order_updates.py::test_the_header_bytes_are_skipped_without_being_decoded,
/// ::test_a_text_frame_is_read_as_its_bytes_like_the_javascript_sdk
#[test]
fn the_header_is_skipped_unread_and_a_text_frame_is_read_as_bytes() {
    let mut f = order_feed();
    let doc = json!({"error": {"code": 0, "message": ""}, "result": [doc_order()]});
    let raw = frame(MODE_ORDER, &doc, HIGH_HEADER);
    assert!(
        std::str::from_utf8(&raw).is_err(),
        "the SDK's decode fails here"
    );
    let got = published(&mut f, Message::Binary(raw));
    assert_eq!(got.len(), 1);
    let text = String::from_utf8(order_frame(vec![doc_order()])).unwrap();
    let got = published(&mut f, Message::Text(text));
    assert_eq!(got[0].orderid, "20220103-1157");
}

/// Web test/test_pocketful_order_updates.py::test_order_status_vocabulary
#[test]
fn order_status_vocabulary() {
    let f = order_feed();
    let cases = [
        (json!("COMPLETE"), "complete"),
        (json!("REJECTED"), "rejected"),
        (json!("CANCELLED"), "cancelled"),
        (json!("PENDING"), "open"),
        (json!("CONFIRMED"), "open"),
        (json!("MODCONFIRMED"), "open"),
        (json!("MODIFY_CONFIRMED"), "open"),
        (json!("AMO_REQ_RECEIVED"), "open"),
        (json!("AMO_MODIFY_CONFIRMED"), "open"),
        (json!("AMO_CANCEL_CONFIRMED"), "cancelled"),
        (json!("CANCEL_CONFIRMED"), "cancelled"),
        (json!("TRIGGER_PENDING"), "trigger pending"),
        (json!("TRIGGER PENDING"), "trigger pending"),
        (json!("OPEN"), "open"),
        (json!("complete"), "complete"),
        (json!("PARTIALLY_FILLED"), "partially_filled"),
        (json!(""), "open"),
        (Value::Null, "open"),
    ];
    for (raw, want) in cases {
        let u = f
            .normalize(&order(json!({"order_status": raw.clone()})))
            .unwrap();
        assert_eq!(u.order_status, want, "{}", raw);
    }
}

/// Web test/test_pocketful_order_updates.py::test_a_rejection_reason_is_reported_only_for_a_rejected_order,
/// ::test_order_types_map_to_openalgo_price_types,
/// ::test_products_map_to_openalgo_products
#[test]
fn reasons_price_types_and_products() {
    let f = order_feed();
    let n = |changes: Value| f.normalize(&order(changes)).unwrap();
    let reason = |status: &str, text: &str| {
        n(json!({"order_status": status, "rejection_reason": text})).rejection_reason
    };
    assert_eq!(
        reason("REJECTED", "RMS: margin shortfall"),
        "RMS: margin shortfall"
    );
    assert_eq!(reason("CONFIRMED", "RMS: stale text"), "");
    assert_eq!(reason("REJECTED", "NONE"), "");
    for (order_type, want) in [
        ("LIMIT", "LIMIT"),
        ("SL", "SL"),
        ("SLM", "SL-M"),
        ("MARKET", "MARKET"),
        ("SL-M", "SL-M"),
    ] {
        assert_eq!(n(json!({"order_type": order_type})).pricetype, want);
    }
    for (product, want) in [
        ("CNC", "CNC"),
        ("MIS", "MIS"),
        ("NRML", "NRML"),
        ("INTRADAY", "MIS"),
        ("MARGIN", "NRML"),
        ("CO", "CO"),
    ] {
        assert_eq!(n(json!({"product": product})).product, want);
    }
}

/// Web test/test_pocketful_order_updates.py::test_quantities_and_average_price_fall_back_when_a_field_is_missing,
/// ::test_a_record_without_an_order_id_is_not_an_order
#[test]
fn missing_fields_fall_back_and_an_order_needs_its_id() {
    let f = order_feed();
    let mut record = order(
        json!({"quantity": 10, "filled_quantity": 4, "average_price": 0,
                                  "average_trade_price": 37.5}),
    );
    record.as_object_mut().unwrap().remove("remaining_quantity");
    let u = f.normalize(&record).unwrap();
    assert_eq!((u.pending_quantity, u.average_price), (6, 37.5));
    assert!(f.normalize(&order(json!({"oms_order_id": ""}))).is_none());
    assert!(f.normalize(&json!("not a record")).is_none());
    // No master row for the token or the symbol: the broker's own symbol.
    let u = f
        .normalize(&order(
            json!({"instrument_token": 1, "trading_symbol": "XYZ-EQ"}),
        ))
        .unwrap();
    assert_eq!(u.symbol, "XYZ-EQ");
}
