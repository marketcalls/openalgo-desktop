//! Tradejini mapping, master-contract, feed-decoding and form tests against
//! payloads built from the web code (`broker/tradejini/**`) and the
//! Tradejini API v2 docs (`tests/fixtures/brokers/tradejini/`).

use super::auth::{api_key, twofa_type};
use super::data::{history_window, parse_bars, to_depth};
use super::mapping::*;
use super::master_contract::{
    first_per_token, group_rows, index_symbol, parse_expiry, parse_group, parse_groups, sent_rows,
    Group,
};
use super::orders::{modify_order_form, place_order_form};
use super::streaming::build::{self, V};
use super::streaming::{decode_message, segment, sub_frame, unsub_frame, ws_key, Packet};
use super::streaming::{TradejiniFeed, L1};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use serde_json::Value;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/tradejini/", $name))
    };
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn group(name: &str, fmt: &str) -> Group {
    Group {
        name: name.into(),
        id_format: fmt.into(),
    }
}

fn master_rows() -> Vec<SymbolData> {
    let groups = parse_groups(&json(fixture!("symbol_store.json")));
    let mut rows = Vec::new();
    for g in &groups {
        let csv = match g.name.as_str() {
            "Securities" => fixture!("Securities.csv"),
            "FutureContracts" => fixture!("FutureContracts.csv"),
            "NSEOptions" => fixture!("NSEOptions.csv"),
            "Index" => fixture!("Index.csv"),
            _ => "",
        };
        rows.extend(parse_group(csv, g));
    }
    rows
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(master_rows());
    r
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn symbol_store_groups() {
    let g = parse_groups(&json(fixture!("symbol_store.json")));
    assert_eq!(g.len(), 5);
    assert_eq!(
        g[0],
        group("Securities", "instrument_symbol_series_exchange")
    );
    assert!(parse_groups(&json(r#"{"s":"error","msg":"x"}"#)).is_empty());
}

#[test]
fn securities_rows() {
    let rows = parse_group(
        fixture!("Securities.csv"),
        &group("Securities", "instrument_symbol_series_exchange"),
    );
    // Short row (field count mismatch) and the spot row are dropped.
    assert_eq!(rows.len(), 3);
    let r = &rows[0];
    assert_eq!(r.symbol, "RELIANCE");
    assert_eq!(r.brsymbol, "EQT_RELIANCE_EQ_NSE");
    assert_eq!(r.token, "2885");
    assert_eq!(r.exchange, "NSE");
    assert_eq!(r.brexchange, "NSE");
    assert_eq!(r.name, "RELIANCE INDUSTRIES LTD");
    assert_eq!(r.instrument_type, "EQ");
    assert_eq!(r.tick_size, 0.1);
    assert_eq!(r.expiry, "");
    assert_eq!(rows[2].exchange, "BSE");
}

#[test]
fn derivative_symbols_follow_openalgo_format() {
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.name, "NIFTY");
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.brsymbol, "FUTIDX_NIFTY_NFO_2026-10-27");
    assert_eq!(fut.token, "52168");
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!(ce.strike, 25000.0);
    assert_eq!(ce.instrument_type, "CE");
    assert_eq!(ce.expiry, "27-OCT-26");
    let vedl = r.by_symbol("NFO", "VEDL27OCT26292.5CE").unwrap();
    assert_eq!(vedl.lot_size, 1150);
    // Unparseable expiry is skipped.
    assert!(r
        .contracts(&crate::brokers::common::symbols::ContractQuery {
            exchange: "NFO",
            underlying: "BAD",
            ..Default::default()
        })
        .is_empty());
}

#[test]
fn index_rows_and_renames() {
    let r = master();
    let n = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(n.brsymbol, "IDX_NIFTY_NSE");
    assert_eq!(n.brexchange, "NSE");
    assert_eq!(n.token, "26000");
    assert_eq!(n.instrument_type, "INDEX");
    assert_eq!(n.name, "NIFTY 50");
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    // Unlisted: uppercased, spaces removed.
    assert!(r.by_symbol("NSE_INDEX", "NIFTYCPSE").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "BSESENSEXNEXT50").is_some());
    assert_eq!(index_symbol("BSE_INDEX", "BSE HC"), "BSEHEALTHCARE");
    assert_eq!(index_symbol("NSE_INDEX", "Nifty Pvt Bank"), "NIFTYPVTBANK");
    // The maps are per exchange.
    assert_eq!(index_symbol("NSE_INDEX", "AUTO"), "AUTO");
}

/// Web #2198: a group that sent rows none of which parse fails the
/// download (replacing the master would drop that group); a group that
/// sent nothing is skipped; rows with the header's field count are what
/// "sent" means (web `get_scrip_data`).
#[test]
fn unusable_groups_refuse_the_download() {
    let g = group("Securities", "instrument_symbol_series_exchange");
    let header = "id,dispName,excToken,lot,tick,symbol,desc,asset\n";
    assert_eq!(sent_rows(header), 0);
    assert_eq!(sent_rows(""), 0);
    assert!(group_rows(header, &g).unwrap().is_empty());
    assert!(group_rows("", &g).unwrap().is_empty());
    // A short row was never sent as far as the web is concerned.
    let short = format!("{}EQT_X_EQ_NSE,X\n", header);
    assert_eq!(sent_rows(&short), 0);
    assert!(group_rows(&short, &g).unwrap().is_empty());
    // Rows sent, none usable (no id): refused, naming the group.
    let unusable = format!("{},X,1,1,0.05,X,X,equity\n", header);
    assert_eq!(sent_rows(&unusable), 1);
    let e = group_rows(&unusable, &g).unwrap_err();
    assert!(e
        .client_message()
        .contains("no usable symbols for Securities"));
    assert_eq!(group_rows(fixture!("Securities.csv"), &g).unwrap().len(), 3);
}

/// Web #2198 keeps the first row per token across groups; keyed by
/// exchange too, so NSE and NFO rows sharing a token number both stay.
#[test]
fn first_row_per_exchange_and_token_wins() {
    let row = |sym: &str, ex: &str, tok: &str| {
        crate::brokers::common::symbols::tests::row(sym, sym, ex, tok)
    };
    let rows = first_per_token(vec![
        row("A", "NSE", "1"),
        row("B", "NSE", "1"),
        row("C", "NFO", "1"),
        row("D", "NSE", "2"),
    ]);
    let syms: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["A", "C", "D"]);
}

#[test]
fn expiry_formats() {
    let d = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
    for s in [
        "2026-10-27",
        "27OCT2026",
        "27Oct26",
        "27-Oct-2026",
        "27-OCT-26",
    ] {
        assert_eq!(parse_expiry(s), Some(d), "{}", s);
    }
    assert_eq!(parse_expiry(""), None);
    assert_eq!(parse_expiry("x"), None);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_rows() {
    let r = master();
    let rows: Vec<Order> = rows(&json(fixture!("orders.json")))
        .iter()
        .map(|o| order_row(o, &r))
        .collect();
    assert_eq!(rows.len(), 5);
    let a = &rows[0];
    assert_eq!(a.symbol, "SBIN");
    assert_eq!(a.exchange, "NSE");
    assert_eq!(a.side, "BUY");
    assert_eq!(a.order_type, "LIMIT");
    assert_eq!(a.product, "MIS");
    assert_eq!(a.status, "open");
    assert_eq!(a.price, 801.5);
    assert_eq!(a.quantity, 10);
    assert_eq!(a.pending_quantity, 10);
    assert_eq!(a.validity, "DAY");
    let b = &rows[1];
    // Short `sym` field names (`exch`, `trdSym`) are read too.
    assert_eq!(b.symbol, "NIFTY27OCT2625000CE");
    assert_eq!(b.exchange, "NFO");
    assert_eq!(b.order_type, "SL");
    assert_eq!(b.product, "NRML");
    assert_eq!(b.status, "trigger pending");
    assert_eq!(b.trigger_price, 121.0);
    assert_eq!(rows[2].status, "complete");
    assert_eq!(rows[2].product, "CNC");
    assert_eq!(rows[2].average_price, 1424.6);
    assert_eq!(rows[3].status, "rejected");
    assert_eq!(rows[3].order_type, "SL-M");
    assert_eq!(
        rows[3].rejection_reason.as_deref(),
        Some("RMS: margin exceeds")
    );
    assert_eq!(rows[3].validity, "IOC");
    // Unknown instrument: broker trading symbol; unknown status lowercased.
    assert_eq!(rows[4].symbol, "UNKNOWN-EQ");
    assert_eq!(rows[4].status, "modified");
    let cancellable: Vec<_> = rows
        .iter()
        .filter(|o| is_cancellable(&o.status))
        .map(|o| o.order_id.as_str())
        .collect();
    assert_eq!(cancellable, ["26100300001", "26100300002", "26100300005"]);
}

#[test]
fn status_table() {
    for (raw, oa) in [
        ("COMPLETED", "complete"),
        ("traded", "complete"),
        ("Filled", "complete"),
        ("pending", "open"),
        ("Trigger Pending", "trigger pending"),
        ("canceled", "cancelled"),
        ("CANCELLED", "cancelled"),
        ("Rejected", "rejected"),
        ("AMO", "amo"),
    ] {
        assert_eq!(order_status(raw), oa);
    }
}

#[test]
fn trade_book_rows() {
    let r = master();
    let t: Vec<Trade> = rows(&json(fixture!("trades.json")))
        .iter()
        .map(|x| trade_row(x, &r))
        .collect();
    assert_eq!(t[0].symbol, "RELIANCE");
    assert_eq!(t[0].exchange, "NSE");
    assert_eq!(t[0].product, "CNC");
    assert_eq!(t[0].side, "BUY");
    assert_eq!(t[0].average_price, 1424.6);
    assert_eq!(t[0].trade_value, 1424.6);
    assert_eq!(t[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(t[1].side, "SELL");
    assert_eq!(t[1].quantity, 75);
    assert_eq!(t[1].product, "NRML");
}

#[test]
fn position_rows() {
    let r = master();
    let p: Vec<Position> = rows(&json(fixture!("positions.json")))
        .iter()
        .map(|x| position_row(x, &r))
        .collect();
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].average_price, 801.26);
    assert_eq!(p[0].product, "MIS");
    assert_eq!(p[0].realized_pnl, 12.5);
    assert_eq!(p[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].product, "NRML");
    assert_eq!(p[2].quantity, 0);
}

#[test]
fn holding_rows() {
    let r = master();
    let v = json(fixture!("holdings.json"));
    let list = v["d"]["holdings"].as_array().unwrap();
    let h: Vec<Holding> = list.iter().filter_map(|x| holding_row(x, &r)).collect();
    // The row without a `sym` object is dropped.
    assert_eq!(h.len(), 2);
    // No LTP in the payload: valued at the average price, pnl = realized.
    assert_eq!(h[0].symbol, "RELIANCE");
    assert_eq!(h[0].quantity, 4);
    assert_eq!(h[0].ltp, 1300.0);
    assert_eq!(h[0].pnl, 10.0);
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].isin.as_deref(), Some("INE002A01018"));
    // `saleableQty` when `qty` is absent; `lastPrice` from the symbol.
    assert_eq!(h[1].quantity, 10);
    assert_eq!(h[1].ltp, 810.0);
    assert_eq!(h[1].pnl, 100.0);
    assert_eq!(h[1].pnl_percentage, 1.25);
    assert_eq!(h[1].current_value, 8100.0);
}

#[test]
fn funds_sum_segments_and_accept_object() {
    let f = funds(&json(fixture!("limits.json"))["d"]).unwrap();
    assert_eq!(f.available_cash, 105000.75);
    assert_eq!(f.collateral, 2500.0);
    assert_eq!(f.m2m_unrealized, -100.25);
    assert_eq!(f.m2m_realized, 45.0);
    assert_eq!(f.utilised_debits, 16000.0);
    let o = funds(&json(r#"{"availMargin":"10.5","marginUsed":2}"#)).unwrap();
    assert_eq!(o.available_cash, 10.5);
    assert_eq!(o.utilised_debits, 2.0);
    assert!(funds(&json("[]")).is_none());
    assert!(funds(&json(r#""No Data""#)).is_none());
}

#[test]
fn envelope_errors() {
    assert_eq!(
        envelope_error(&json(r#"{"s":"error","msg":"Invalid symbol"}"#)).as_deref(),
        Some("Invalid symbol")
    );
    assert_eq!(
        envelope_error(&json(r#"{"s":"error","d":{"msg":"Order not found"}}"#)).as_deref(),
        Some("Order not found")
    );
    assert_eq!(envelope_error(&json(r#"{"s":"error"}"#)), None);
    assert!(rows(&json(r#"{"s":"no-data","msg":"No Data Available"}"#)).is_empty());
}

// ---------------------------------------------------------------------------
// Order forms
// ---------------------------------------------------------------------------

fn resolved(pricetype: PriceType, price: f64, trigger: f64) -> ResolvedOrder {
    let r = master();
    ResolvedOrder {
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        quantity: 10,
        price,
        trigger_price: trigger,
        pricetype,
        product: Product::Mis,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: r.by_symbol("NSE", "SBIN").unwrap(),
    }
}

fn form_map(f: &[(&'static str, String)]) -> Vec<(String, String)> {
    f.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn place_form_market_has_market_protection() {
    let f = form_map(&place_order_form(&resolved(PriceType::Market, 0.0, 0.0)));
    assert_eq!(
        f,
        vec![
            ("symId".into(), "EQT_SBIN_EQ_NSE".into()),
            ("qty".into(), "10".into()),
            ("side".into(), "buy".into()),
            ("type".into(), "market".into()),
            ("product".into(), "intraday".into()),
            ("validity".into(), "day".into()),
            ("mktProt".into(), "2".into()),
        ]
    );
}

#[test]
fn place_form_stoplimit_and_extras() {
    let mut o = resolved(PriceType::Sl, 800.0, 799.5);
    o.action = Action::Sell;
    o.product = Product::Cnc;
    o.validity = Validity::Ioc;
    o.disclosed_quantity = 5;
    o.amo = true;
    let f = form_map(&place_order_form(&o));
    let get = |k: &str| f.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    assert_eq!(get("side"), Some("sell"));
    assert_eq!(get("type"), Some("stoplimit"));
    assert_eq!(get("product"), Some("delivery"));
    assert_eq!(get("limitPrice"), Some("800.0"));
    assert_eq!(get("trigPrice"), Some("799.5"));
    assert_eq!(get("validity"), Some("ioc"));
    assert_eq!(get("discQty"), Some("5"));
    assert_eq!(get("amo"), Some("true"));
    assert_eq!(get("mktProt"), None);
    let slm = form_map(&place_order_form(&resolved(PriceType::SlM, 0.0, 790.0)));
    assert!(slm.contains(&("type".into(), "stopmarket".into())));
    assert!(slm.contains(&("mktProt".into(), "2".into())));
    assert!(!slm.iter().any(|(k, _)| k == "limitPrice"));
}

#[test]
fn modify_form() {
    let r = master();
    let m = ResolvedModify {
        order_id: "26100300001".into(),
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        product: Product::Mis,
        pricetype: PriceType::Sl,
        quantity: 12,
        price: 802.0,
        trigger_price: 801.0,
        disclosed_quantity: 0,
        instrument: r.by_symbol("NSE", "SBIN").unwrap(),
    };
    let f = form_map(&modify_order_form(&m));
    assert_eq!(
        f,
        vec![
            ("symId".into(), "EQT_SBIN_EQ_NSE".into()),
            ("orderId".into(), "26100300001".into()),
            ("qty".into(), "12".into()),
            ("type".into(), "stoplimit".into()),
            ("validity".into(), "day".into()),
            ("side".into(), "buy".into()),
            ("limitPrice".into(), "802.0".into()),
            ("trigPrice".into(), "801.0".into()),
        ]
    );
}

#[test]
fn enum_maps() {
    assert_eq!(reverse_product("Bracket"), Some("BO"));
    assert_eq!(reverse_product("margin"), Some("NRML"));
    assert_eq!(reverse_product("x"), None);
    assert_eq!(reverse_order_type("weird"), "WEIRD");
    assert_eq!(py_float(100.0), "100.0");
    assert_eq!(py_float(0.05), "0.05");
}

#[test]
fn auth_helpers() {
    assert_eq!(twofa_type(Some("OTP")), "otp");
    assert_eq!(twofa_type(Some("sms")), "totp");
    assert_eq!(twofa_type(None), "totp");
    let mut c = BrokerCredentials {
        api_key: "YOUR_BROKER_API_KEY".into(),
        api_secret: Some("abc123".into()),
        ..Default::default()
    };
    assert_eq!(api_key(&c).as_deref(), Some("abc123"));
    c.api_key = "key1".into();
    assert_eq!(api_key(&c).as_deref(), Some("key1"));
    c.api_key = String::new();
    c.api_secret = None;
    assert_eq!(api_key(&c), None);
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[test]
fn history_window_is_ist_session() {
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    };
    // 2026-10-01 09:15 IST = 03:45 UTC, 2026-10-03 23:59:59 IST = 18:29:59 UTC.
    assert_eq!(history_window(&req), (1790826300, 1791052199));
}

#[test]
fn bars_both_shapes_sorted_and_deduped() {
    let c = parse_bars(&json(fixture!("history.json")));
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1759463100);
    assert_eq!(c[0].volume, 1500);
    assert_eq!(c[1].close, 802.0);
    assert_eq!(c[1].oi, 0);
    let ms = parse_bars(&json(
        r#"{"s":"ok","d":{"bars":[[1759463100000,1,2,0.5,1.5,9]]}}"#,
    ));
    assert_eq!(ms[0].timestamp, 1759463100);
    assert!(parse_bars(&json(r#"{"s":"no-data"}"#)).is_empty());
}

// ---------------------------------------------------------------------------
// NxtradStream decoding
// ---------------------------------------------------------------------------

#[test]
fn segment_divisors() {
    assert_eq!(segment(1), Some(("NSE", 100.0)));
    assert_eq!(segment(5), Some(("CDS", 10_000_000.0)));
    assert_eq!(segment(6), Some(("BCD", 10_000.0)));
    assert_eq!(segment(9), Some(("NCO", 10_000.0)));
    assert_eq!(segment(11), None);
}

#[test]
fn header_and_packet_offsets() {
    // Hand-assembled frame: [0..4] i32 len, [4] version 1, [5] compression
    // 0, then one L1 packet: [0..2] i16 len 13, [2] type 10, then
    // key 26 (u8 seg=1), key 27 (i32 token=2885), key 29 (i32 ltp=142460):
    // 3 + 2 + 5 + 5 = 15 bytes.
    let mut pkt = 15i16.to_le_bytes().to_vec();
    pkt.push(10);
    pkt.push(26);
    pkt.push(1);
    pkt.push(27);
    pkt.extend_from_slice(&2885i32.to_le_bytes());
    pkt.push(29);
    pkt.extend_from_slice(&142460i32.to_le_bytes());
    assert_eq!(pkt.len(), 15);
    let mut frame = ((pkt.len() + 6) as i32).to_le_bytes().to_vec();
    frame.push(1);
    frame.push(0);
    frame.extend(&pkt);
    let p = decode_message(&frame);
    assert_eq!(p.len(), 1);
    let Packet::L1(q) = &p[0] else {
        panic!("not L1")
    };
    assert_eq!(q.key(), "2885_NSE");
    assert_eq!(q.ltp, Some(1424.6));
    // Version other than 1 is ignored.
    frame[4] = 2;
    assert!(decode_message(&frame).is_empty());
}

#[test]
fn l1_full_packet_divides_after_segment() {
    // exchSeg and token come last; prices still use the NSE divisor.
    let f = build::frame(
        &[build::l1_full(
            1, 2885, 142460, 141000, 143000, 140500, 141800, 123456, 142455, 142465, None,
        )],
        false,
    );
    let Packet::L1(q) = &decode_message(&f)[0] else {
        panic!()
    };
    assert_eq!(q.ltp, Some(1424.6));
    assert_eq!(q.open, Some(1410.0));
    assert_eq!(q.close, Some(1418.0));
    assert_eq!(q.vol, Some(123456));
    assert_eq!(q.bid_price, Some(1424.55));
    assert_eq!(q.ask_qty, Some(200));
    assert_eq!(q.chng, Some(6.6));
    assert_eq!(q.chng_per, Some(0.46));
    assert_eq!(q.ltt, Some(1_759_475_100));
    assert!(q.is_complete("NSE"));
    // A derivative needs OI too.
    assert!(!q.is_complete("NFO"));
}

#[test]
fn cds_divisor_and_zlib() {
    // CDS prices are scaled by 1e7: 83.1234 -> 831234000.
    let f = build::frame(
        &[build::l1_full(
            5,
            1234,
            831_234_000,
            830_000_000,
            832_000_000,
            829_000_000,
            830_500_000,
            10,
            831_200_000,
            831_300_000,
            Some(500),
        )],
        true,
    );
    assert_eq!(f[5], 100);
    let Packet::L1(q) = &decode_message(&f)[0] else {
        panic!()
    };
    assert_eq!(q.exch, "CDS");
    assert!((q.ltp.unwrap() - 83.1234).abs() < 1e-9);
    assert_eq!(q.oi, Some(500));
    assert!(q.is_complete("CDS"));
}

#[test]
fn l5_levels_split_bids_then_asks() {
    let f = build::frame(
        &[build::l5(
            3,
            61001,
            &[(12050, 75, 1), (12045, 150, 2)],
            &[(12055, 225, 3), (12060, 300, 4), (12065, 75, 1)],
        )],
        false,
    );
    let Packet::L5(b) = &decode_message(&f)[0] else {
        panic!()
    };
    assert_eq!(b.key(), "61001_NFO");
    assert_eq!(b.bids.len(), 2);
    assert_eq!(b.asks.len(), 3);
    assert_eq!(b.bids[0].price, 120.5);
    assert_eq!(b.bids[1].orders, 2);
    assert_eq!(b.asks[2].price, 120.65);
    assert_eq!(b.tot_buy_qty, 225);
    assert_eq!(b.tot_sell_qty, 600);
    let d = to_depth(&QuoteKey::new("NFO", "NIFTY27OCT2625000CE"), b);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.total_buy_qty, 225);
    assert_eq!(d.bids[4].price, 0.0);
}

#[test]
fn auth_pong_and_unknown_packets() {
    let f = build::frame(
        &[
            build::auth(1),
            build::packet(16, &[(62, V::U8(1))]),
            build::packet(12, &[(26, V::U8(1))]),
            build::packet(99, &[]),
        ],
        false,
    );
    assert_eq!(
        decode_message(&f),
        vec![Packet::Auth(1), Packet::Pong, Packet::Other(12)]
    );
    // A zero packet length stops the walk.
    let mut bad = build::frame(&[build::auth(1)], false);
    bad[6] = 0;
    bad[7] = 0;
    assert!(decode_message(&bad).is_empty());
    assert!(decode_message(&[1, 2]).is_empty());
}

#[test]
fn unknown_field_stops_the_packet_without_panic() {
    let mut p = build::packet(10, &[(26, V::U8(1)), (27, V::I32(5))]);
    p.push(200);
    p.push(1);
    let len = p.len() as i16;
    p[0..2].copy_from_slice(&len.to_le_bytes());
    let Packet::L1(q) = &decode_message(&build::frame(&[p], false))[0] else {
        panic!()
    };
    assert_eq!(q.key(), "5_NSE");
}

#[test]
fn l1_deltas_merge() {
    let mut a = L1 {
        ltp: Some(1.0),
        open: Some(2.0),
        ..Default::default()
    };
    a.merge(&L1 {
        ltp: Some(3.0),
        ..Default::default()
    });
    assert_eq!((a.ltp, a.open), (Some(3.0), Some(2.0)));
}

// ---------------------------------------------------------------------------
// Live feed
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, token: &str, brex: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: brex.into(),
        mode,
        depth: 5,
    }
}

fn text(m: &Message) -> Value {
    let Message::Text(t) = m else {
        panic!("not text")
    };
    assert!(t.ends_with('\n'));
    serde_json::from_str(t.trim_end()).unwrap()
}

#[test]
fn request_frames() {
    let f = text(&sub_frame("L1", &["2885_NSE".into(), "26000_NSE".into()]));
    assert_eq!(
        f,
        serde_json::json!({"type":"L1","action":"sub","tokens":[{"t":"2885_NSE"},{"t":"26000_NSE"}]})
    );
    assert_eq!(
        text(&unsub_frame("L5")),
        serde_json::json!({"type":"L5","action":"unsub"})
    );
    assert_eq!(ws_key("26000", "NSE", "NSE_INDEX"), "26000_NSE");
    assert_eq!(ws_key("26000", "", "NSE_INDEX"), "26000_NSE");
}

#[test]
fn feed_resends_full_lists() {
    let mut f = TradejiniFeed::new(streaming::STREAM_URL, "key", "tok");
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://api.tradejini.com/v2.1/stream?token=key:tok&version=3.1"
    );
    let a = sub("SBIN", "NSE", "3045", "NSE", FeedMode::Quote);
    let b = sub("RELIANCE", "NSE", "2885", "NSE", FeedMode::Depth);
    let frames = f.subscribe_frames(std::slice::from_ref(&a));
    assert_eq!(frames.len(), 1);
    assert_eq!(text(&frames[0])["tokens"].as_array().unwrap().len(), 1);
    // A second subscribe re-sends the whole L1 list (it replaces it), and
    // the depth instrument also goes on L5.
    let frames = f.subscribe_frames(std::slice::from_ref(&b));
    assert_eq!(frames.len(), 2);
    let l1 = text(&frames[0]);
    assert_eq!(l1["type"], "L1");
    assert_eq!(
        l1["tokens"],
        serde_json::json!([{"t":"2885_NSE"},{"t":"3045_NSE"}])
    );
    assert_eq!(
        text(&frames[1])["tokens"],
        serde_json::json!([{"t":"2885_NSE"}])
    );
    // Dropping depth to quote only touches L5, which becomes empty.
    let frames = f.mode_change_frames(&b, &b.with_mode(FeedMode::Quote));
    assert_eq!(frames.len(), 1);
    assert_eq!(
        text(&frames[0]),
        serde_json::json!({"type":"L5","action":"unsub"})
    );
    // Unsubscribe re-sends what is left, then clears the feed.
    let frames = f.unsubscribe_frames(&[a]);
    assert_eq!(
        text(&frames[0])["tokens"],
        serde_json::json!([{"t":"2885_NSE"}])
    );
    let frames = f.unsubscribe_frames(&[b.with_mode(FeedMode::Quote)]);
    assert_eq!(
        text(&frames[0]),
        serde_json::json!({"type":"L1","action":"unsub"})
    );
    assert!(f.subscribe_frames(&[]).is_empty());
    let (period, ping) = f.heartbeat().unwrap();
    assert_eq!(period, streaming::HEARTBEAT);
    assert_eq!(text(&ping), serde_json::json!({"type":"PING"}));
}

#[test]
fn feed_ticks_depth_and_auth() {
    let mut f = TradejiniFeed::new(streaming::STREAM_URL, "key", "tok");
    f.subscribe_frames(&[
        sub("RELIANCE", "NSE", "2885", "NSE", FeedMode::Depth),
        sub("SBIN", "NSE", "3045", "NSE", FeedMode::Ltp),
    ]);
    let frame = build::frame(
        &[
            build::auth(1),
            build::l1_full(
                1, 2885, 142460, 141000, 143000, 140500, 141800, 99, 142455, 142465, None,
            ),
            build::l5(1, 2885, &[(142455, 10, 1)], &[(142465, 20, 2)]),
            // Not subscribed: ignored.
            build::l1_full(1, 1, 100, 100, 100, 100, 100, 1, 100, 100, None),
        ],
        true,
    );
    let ev = f.parse(&Message::Binary(frame));
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[0], FeedEvent::AuthOk);
    let FeedEvent::Tick(t) = &ev[1] else { panic!() };
    assert_eq!(t.symbol, "RELIANCE");
    assert_eq!(t.mode, 3);
    assert_eq!(t.ltp, 1424.6);
    assert_eq!(t.close, 1418.0);
    assert_eq!(t.change, 6.6);
    assert_eq!(t.last_trade_time_ms, 1_759_475_100_000);
    let FeedEvent::Depth(d) = &ev[2] else {
        panic!()
    };
    assert_eq!(d.ltp, 1424.6);
    assert_eq!(d.buy[0].price, 1424.55);
    assert_eq!(d.sell[0].orders, 2);
    assert_eq!(d.buy.len(), 5);
    assert_eq!(d.total_sell_quantity, 20);
    // A delta carrying only ltp keeps the cached open/close.
    let delta = build::frame(
        &[build::packet(
            10,
            &[(26, V::U8(1)), (27, V::I32(3045)), (29, V::I32(80150))],
        )],
        false,
    );
    let ev = f.parse(&Message::Binary(delta));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.symbol.as_str(), t.ltp, t.mode), ("SBIN", 801.5, 1));
    let ev = f.parse(&Message::Binary(build::frame(&[build::auth(0)], false)));
    assert!(matches!(ev[0], FeedEvent::AuthFailed(_)));
    assert!(f.cached() <= 3);
    f.on_connected();
    assert_eq!(f.cached(), 0);
    assert!(f.parse(&Message::Text("x".into())).is_empty());
}

#[test]
fn adapter_identity_and_capabilities() {
    let b = TradejiniBroker::new(master());
    assert_eq!(b.id(), "tradejini");
    assert!(b.requires_totp());
    let c = b.capabilities();
    assert!(c.history && c.streaming && c.multiquotes_batch);
    assert!(!c.margin && !c.gtt && !c.order_feed);
    assert_eq!(b.timeframe_map(), TIMEFRAME_MAP);
    assert!(b.supported_exchanges().contains(&Exchange::Bcd));
    assert!(b.create_feed(&AuthToken::new("nocolon")).is_err());
    assert!(b.create_feed(&AuthToken::new("k:t")).is_ok());
}
