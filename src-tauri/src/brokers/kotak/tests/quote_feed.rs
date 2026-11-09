//! Web `test/test_kotak_quote_feed.py` (openalgo#2376): SFeed bytes in, the
//! 8765 frames out (`feed::testkit`), with the web test's values.
//!
//! Each packet's tick says what the packet carries and the bridge merges
//! it (`feed::bridge::snapshot`), as the web merges a packet over the
//! instrument's last state; these cases read the frames, built the same
//! way (`feed::testkit`).

use super::*;
use crate::brokers::kotak::streaming::exchange_name;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const NSE_CM: i8 = 1;
const TOKEN: u32 = 11536; // TCS in the fixture master
const LAST_TRADE_TIME: i64 = 1_700_000_001; // Unix seconds on the wire
const MINI_TOUCH_LINE: u8 = 1;
const TOUCH_LINE: u8 = 4;
const DEPTH: u8 = 8;

/// The web test's `market_picture()`; prices in paise (divider 100).
struct Picture {
    level: u8,
    ltp: u32,
    last_trade_time: i64,
    total_buy: i64,
    total_sell: i64,
    buy: Vec<(i64, i32, i32)>,
    sell: Vec<(i64, i32, i32)>,
}

impl Default for Picture {
    fn default() -> Self {
        Self {
            level: TOUCH_LINE,
            ltp: 213_500,
            last_trade_time: LAST_TRADE_TIME,
            total_buy: 900,
            total_sell: 800,
            buy: vec![(100, 213_400, 3)],
            sell: vec![(150, 213_600, 4)],
        }
    }
}

impl Picture {
    fn packet(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(TOKEN.to_le_bytes()); // @0
        b.extend(self.total_buy.to_le_bytes()); // @4
        b.extend(self.total_sell.to_le_bytes()); // @12
        b.extend(1_500_000i64.to_le_bytes()); // volume @20
        b.extend(self.last_trade_time.to_le_bytes()); // ltt @28
        b.extend(self.last_trade_time.to_le_bytes()); // last update @36
        b.extend(210_000u32.to_le_bytes()); // open @44
        b.extend(211_000u32.to_le_bytes()); // close @48
        b.extend(215_000u32.to_le_bytes()); // high @52
        b.extend(209_000u32.to_le_bytes()); // low @56
        b.extend(self.ltp.to_le_bytes()); // ltp @60
        b.extend(50i64.to_le_bytes()); // ltq @64
        b.extend(212_000u32.to_le_bytes()); // atp @72
        b.extend(0u32.to_le_bytes()); // indicative close @76
        b.extend((self.buy.len() as u32).to_le_bytes()); // @80
        b.extend((self.sell.len() as u32).to_le_bytes()); // @84
        b.extend(0i16.to_le_bytes()); // status @88
        b.extend(150i32.to_le_bytes()); // change % @90
        b.extend(4_200u32.to_le_bytes()); // oi @94
        b.extend(99_999f64.to_le_bytes()); // turnover @98
        b.extend(2_500i32.to_le_bytes()); // change @106
        for v in [230_000u32, 190_000, 240_000, 180_000, 1] {
            b.extend(v.to_le_bytes()); // circuits, yearly, lot @110..@130
        }
        b.push(2); // precision @130
        b.extend(1u32.to_le_bytes()); // multiplier @131
        for (q, p, o) in self.buy.iter().chain(self.sell.iter()) {
            b.extend(q.to_le_bytes());
            b.extend(p.to_le_bytes());
            b.extend(o.to_le_bytes());
        }
        packet(0, NSE_CM, self.level, &b)
    }
}

/// The web test's `mini()`: a price, the previous close, ltq 25.
fn mini(ltp: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(TOKEN.to_le_bytes());
    b.extend((LAST_TRADE_TIME + 5).to_le_bytes());
    b.extend(ltp.to_le_bytes());
    b.extend(25i64.to_le_bytes());
    b.extend(211_000u32.to_le_bytes());
    b.extend(140i32.to_le_bytes());
    b.extend(3_000i32.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    b.push(2);
    b.extend(1u32.to_le_bytes());
    packet(0, NSE_CM, MINI_TOUCH_LINE, &b)
}

/// A feed past its login, with `subs` subscribed.
fn feed_with(subs: &[FeedSubscription]) -> KotakFeed {
    let mut f = KotakFeed::new(DEFAULT_SFEED_URL, "sid", "U".into(), master());
    f.on_connected();
    let ack = json!({"message_code": 1119, "exchanges": {"nse_cm": {"value": 1, "divider": 100}}});
    f.parse(&Message::Text(ack.to_string()));
    f.subscribe_frames(subs);
    f
}

fn tcs(mode: FeedMode) -> KotakFeed {
    feed_with(&[fsub("TCS", "NSE", mode)])
}

fn frames_of(f: &mut KotakFeed, frames: &mut Frames, packet: Vec<u8>, mode: Mode) -> Vec<Value> {
    frames.at(&f.parse(&Message::Binary(packet)), mode)
}

fn last(v: Vec<Value>) -> Value {
    v.last().cloned().expect("a frame")
}

/// An index subscription the fixture master does not hold.
fn index_sub(symbol: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: "NSE_INDEX".into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: "NSE_INDEX".into(),
        mode,
        depth: 5,
    }
}

fn index_packet() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(4_247_863_880u32.to_le_bytes()); // a token of Kotak's own
    for v in [2_100_000i32, 2_110_000, 2_150_000, 2_090_000, 2_135_000] {
        b.extend(v.to_le_bytes()); // open, close, high, low, value
    }
    b.extend((LAST_TRADE_TIME as u64).to_le_bytes()); // ltt @24
    b.extend([0u8; 12]); // yearly high and low, change %
    b.extend(0f64.to_le_bytes()); // market cap @44
    b.push(2);
    b.extend(100i32.to_le_bytes());
    let mut n = b"Nifty 50".to_vec();
    n.resize(21, 0);
    b.extend(n);
    packet(7207, NSE_CM, 0, &b)
}

/// Web `test_a_quote_carries_every_field_the_packet_decoded`.
#[test]
fn a_quote_carries_every_field_the_packet_decoded() {
    let mut f = tcs(FeedMode::Quote);
    let q = last(frames_of(
        &mut f,
        &mut Frames::new("kotak"),
        Picture::default().packet(),
        Mode::Quote,
    ));
    let expected = json!({
        "ltp": 2135.0,
        "open": 2100.0,
        "high": 2150.0,
        "low": 2090.0,
        "close": 2110.0,
        "volume": 1_500_000,
        "average_price": 2120.0,
        "last_quantity": 50,
        "total_buy_quantity": 900,
        "total_sell_quantity": 800,
        "oi": 4_200,
        // The packet's own change percent (nc 150), not one worked out
        // from the price and close (1.18).
        "percent_change": 1.5,
    });
    for (k, v) in expected.as_object().unwrap() {
        assert_eq!(q.get(k), Some(v), "{}: {}", k, q);
    }
}

/// A market picture's book totals are the exchange's figure now, a 0
/// included: with every bid gone the total buy quantity is 0, not the last
/// one (web `kotak_adapter._on_data_received`, where a complete packet's
/// fields win; audit P-04).
#[test]
fn a_book_total_of_zero_is_zero() {
    let mut f = tcs(FeedMode::Depth);
    let mut frames = Frames::new("kotak");
    let full = Picture {
        level: DEPTH,
        ..Default::default()
    };
    frames_of(&mut f, &mut frames, full.packet(), Mode::Depth);
    let no_bids = Picture {
        level: DEPTH,
        total_buy: 0,
        buy: vec![],
        ..Default::default()
    };
    let d = last(frames_of(
        &mut f,
        &mut frames,
        no_bids.packet(),
        Mode::Depth,
    ));
    assert_eq!(d["total_buy_quantity"], 0, "{}", d);
    assert_eq!(d["total_sell_quantity"], 800, "{}", d);
}

/// Web `test_ltt_is_the_exchange_last_trade_time_in_milliseconds`, with
/// `last_trade_time` beside it in Quote and Depth (the web's mode 1 publish
/// is `ltp` and `ltt` only).
#[test]
fn ltt_is_the_exchange_trade_time_in_milliseconds() {
    for (mode, level) in [
        (FeedMode::Ltp, TOUCH_LINE),
        (FeedMode::Quote, TOUCH_LINE),
        (FeedMode::Depth, DEPTH),
    ] {
        let mut f = tcs(mode);
        let at = Mode::from_u8(mode.code()).unwrap();
        let picture = Picture {
            level,
            ..Default::default()
        };
        let d = last(frames_of(
            &mut f,
            &mut Frames::new("kotak"),
            picture.packet(),
            at,
        ));
        assert_eq!(d["ltt"], LAST_TRADE_TIME * 1000, "{:?}", mode);
        if mode == FeedMode::Ltp {
            assert!(d.get("last_trade_time").is_none(), "{}", d);
        } else {
            assert_eq!(d["last_trade_time"], d["ltt"], "{:?}", mode);
        }
    }
}

/// Web `test_no_trade_yet_publishes_no_trade_time_rather_than_now`.
#[test]
fn no_trade_yet_publishes_no_trade_time() {
    for mode in [FeedMode::Ltp, FeedMode::Quote] {
        let mut f = tcs(mode);
        let picture = Picture {
            last_trade_time: 0,
            ..Default::default()
        };
        let at = Mode::from_u8(mode.code()).unwrap();
        let d = last(frames_of(
            &mut f,
            &mut Frames::new("kotak"),
            picture.packet(),
            at,
        ));
        assert!(
            d.get("ltt").is_none() && d.get("last_trade_time").is_none(),
            "{:?}: {}",
            mode,
            d
        );
    }
}

/// Web `test_depth_publishes_its_book_totals_and_every_quote_field`.
#[test]
fn a_depth_carries_its_book_totals_and_every_quote_field() {
    let mut f = tcs(FeedMode::Depth);
    let picture = Picture {
        level: DEPTH,
        total_buy: 12_345,
        total_sell: 6_789,
        ..Default::default()
    };
    let d = last(frames_of(
        &mut f,
        &mut Frames::new("kotak"),
        picture.packet(),
        Mode::Depth,
    ));
    let expected = json!({
        "total_buy_quantity": 12_345,
        "total_sell_quantity": 6_789,
        "open": 2100.0,
        "close": 2110.0,
        "volume": 1_500_000,
        "average_price": 2120.0,
        "last_quantity": 50,
        "oi": 4_200,
    });
    for (k, v) in expected.as_object().unwrap() {
        assert_eq!(d.get(k), Some(v), "{}: {}", k, d);
    }
    assert_eq!(
        d["depth"]["buy"][0],
        json!({"price": 2134.0, "quantity": 100, "orders": 3})
    );
}

/// Web `test_a_mini_tick_keeps_the_last_known_ohlc_and_volume`.
#[test]
fn a_mini_tick_keeps_the_last_known_ohlc_and_volume() {
    let mut f = tcs(FeedMode::Quote);
    let mut frames = Frames::new("kotak");
    frames_of(
        &mut f,
        &mut frames,
        Picture::default().packet(),
        Mode::Quote,
    );
    let q = last(frames_of(&mut f, &mut frames, mini(214_000), Mode::Quote));
    assert_eq!(q["ltp"], 2140.0, "the mini tick's own price");
    assert_eq!(
        (q["open"].as_f64(), q["high"].as_f64(), q["low"].as_f64()),
        (Some(2100.0), Some(2150.0), Some(2090.0))
    );
    assert_eq!(q["volume"], 1_500_000);
    assert_eq!(q["last_quantity"], 25, "a field the mini tick carries wins");
    assert_eq!(q["ltt"], (LAST_TRADE_TIME + 5) * 1000);
}

/// Web `test_a_depth_packet_replaces_the_whole_book`: an empty row is an
/// empty level, not the previous packet's. A side holds its real levels
/// only, where the web pads it to five zero rows (a web defect, lead
/// decision 9 Nov).
#[test]
fn a_depth_packet_replaces_the_whole_book() {
    let mut f = tcs(FeedMode::Depth);
    let mut frames = Frames::new("kotak");
    let first = Picture {
        level: DEPTH,
        buy: vec![(100, 213_400, 3), (200, 213_300, 5)],
        sell: vec![(150, 213_600, 4), (250, 213_700, 6)],
        ..Default::default()
    };
    frames_of(&mut f, &mut frames, first.packet(), Mode::Depth);
    let second = Picture {
        level: DEPTH,
        buy: vec![(90, 213_500, 2)],
        sell: vec![(80, 213_550, 1)],
        ..Default::default()
    };
    let d = last(frames_of(&mut f, &mut frames, second.packet(), Mode::Depth));
    assert_eq!(d["depth"]["buy"][0]["price"], 2135.0);
    assert_eq!(d["depth"]["buy"].as_array().map(Vec::len), Some(1), "{}", d);
    assert_eq!(
        d["depth"]["sell"].as_array().map(Vec::len),
        Some(1),
        "{}",
        d
    );
}

/// Web `test_bcd_maps_to_the_segment_the_feed_reports`: BSE currency
/// derivatives stream as `bse_cd` (SFeed exchange id 7).
#[test]
fn bcd_subscribes_under_the_segment_the_feed_reports() {
    let mut f = feed_with(&[]);
    let usdinr = FeedSubscription {
        symbol: "USDINR28NOV26FUT".into(),
        exchange: "BCD".into(),
        token: "1002".into(),
        brsymbol: "USDINR26NOVFUT".into(),
        brexchange: "bcs_fo".into(),
        mode: FeedMode::Quote,
        depth: 5,
    };
    let frames = text_frames(&f.subscribe_frames(&[usdinr]));
    assert_eq!(frames[0]["inputtoken"], "bse_cd|1002");
    assert_eq!(exchange_name(7), Some("bse_cd"));
}

/// Web `test_an_hs_data_centre_really_falls_back_to_sfeed`.
#[test]
fn an_hs_data_centre_falls_back_to_sfeed_with_no_source() {
    let cfg = json!({
        "E99_broadcast_source": "hs",
        "E99_hs_broadcast_endpoint": "mlhsm.kotaksecurities.com",
    });
    assert_eq!(
        feed_url_from_config(&cfg, "E99"),
        (None, DEFAULT_SFEED_URL.into())
    );
}

/// Web `test_two_symbols_on_one_index_name_both_receive_the_tick` and
/// `test_the_index_feed_stays_while_another_symbol_still_uses_its_name`
/// (and `test_disconnect_forgets_the_index_aliases`: once both are gone, a
/// late index packet reaches nobody).
#[test]
fn two_symbols_on_one_index_name_share_its_feed() {
    let nifty = index_sub("NIFTY", "26000", FeedMode::Quote);
    let nifty50 = index_sub("NIFTY50", "26999", FeedMode::Quote);
    let mut f = feed_with(&[nifty.clone(), nifty50.clone()]);
    let ticks = |f: &mut KotakFeed| -> Vec<(String, f64, i64)> {
        f.parse(&Message::Binary(index_packet()))
            .into_iter()
            .filter_map(|e| match e {
                FeedEvent::Tick(t) => Some((t.symbol, t.ltp, t.last_trade_time_ms)),
                _ => None,
            })
            .collect()
    };
    let mut both = ticks(&mut f);
    both.sort_by(|a, b| a.0.cmp(&b.0));
    let at = LAST_TRADE_TIME * 1000;
    assert_eq!(
        both,
        [
            ("NIFTY".to_string(), 21350.0, at),
            ("NIFTY50".to_string(), 21350.0, at)
        ]
    );

    assert!(
        f.unsubscribe_frames(&[nifty]).is_empty(),
        "NIFTY50 still needs the Nifty 50 feed"
    );
    assert_eq!(ticks(&mut f), [("NIFTY50".to_string(), 21350.0, at)]);
    let un = text_frames(&f.unsubscribe_frames(&[nifty50]));
    assert_eq!(
        un,
        [json!({"event": "unsubscribeIndices", "inputtoken": "nse_cm|Nifty 50"})]
    );
    assert!(ticks(&mut f).is_empty());
}

/// Web `test_a_first_mini_tick_publishes_no_zero_ohlc_or_volume`: a mini
/// touch line before any market picture has a price and no open, high, low
/// or volume, and none goes out as 0.
#[test]
fn a_first_mini_tick_sends_no_zero_ohlc_or_volume() {
    let mut f = tcs(FeedMode::Depth);
    let events = f.parse(&Message::Binary(mini(214_000)));
    let q = last(Frames::new("kotak").at(&events, Mode::Quote));
    assert_eq!(q["ltp"], 2140.0);
    for k in ["open", "high", "low", "volume"] {
        assert!(q.get(k).is_none(), "{}: {}", k, q);
    }
    assert_eq!(
        q["close"], 2110.0,
        "the mini tick carries the previous close"
    );
}
