//! Pocketful mapping tests against payloads built from the web code
//! (`src-tauri/tests/fixtures/brokers/pocketful/`, no account data).

use super::data::{batch_wait, instrument, to_depth, to_quote};
use super::mapping::*;
use super::master_contract::{
    leading_letters, parse_archive, parse_bse, parse_derivatives, parse_expiry, parse_nse,
    parse_nse_indices, Segment,
};
use super::orders::market_protection;
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::{json, Value};
use std::time::Duration;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/pocketful/", $name))
    };
}

fn files() -> Vec<(String, Vec<u8>)> {
    [
        ("NSECompactScrip.csv", fixture!("NSECompactScrip.csv")),
        ("BSECompactScrip.csv", fixture!("BSECompactScrip.csv")),
        ("NFOCompactScrip.csv", fixture!("NFOCompactScrip.csv")),
        ("BFOCompactScrip.csv", fixture!("BFOCompactScrip.csv")),
        ("MCXCompactScrip.csv", fixture!("MCXCompactScrip.csv")),
    ]
    .iter()
    .map(|(n, t)| (n.to_string(), t.as_bytes().to_vec()))
    .collect()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_archive(&files()).unwrap());
    r
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_nse_equities_and_indices() {
    let eq = parse_nse(fixture!("NSECompactScrip.csv")).unwrap();
    // Only instrument_name == EQ (NIFTYBEES is BE).
    assert_eq!(eq.len(), 3);
    let sbin = &eq[0];
    assert_eq!(
        (sbin.symbol.as_str(), sbin.brsymbol.as_str()),
        ("SBIN", "SBIN-EQ")
    );
    assert_eq!(
        (sbin.token.as_str(), sbin.exchange.as_str()),
        ("3045", "NSE")
    );
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(eq[2].name, "TATA CONSULTANCY SERV, LT");
    let idx = parse_nse_indices(fixture!("NSECompactScrip.csv")).unwrap();
    let names: Vec<&str> = idx.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(
        names,
        [
            "NIFTY",
            "BANKNIFTY",
            "INDIAVIX",
            "FINNIFTY",
            "MIDCPNIFTY",
            "NIFTYNXT50",
            "Nifty IT"
        ]
    );
    assert!(idx.iter().all(|r| r.exchange == "NSE_INDEX"));
    assert_eq!(idx[0].brexchange, "NSE");
    assert_eq!(idx[0].brsymbol, "Nifty 50");
}

#[test]
fn master_bse_strips_series_and_maps_indices() {
    let rows = parse_bse(fixture!("BSECompactScrip.csv")).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].symbol, "SBIN");
    assert_eq!(rows[0].brsymbol, "SBIN-A");
    assert_eq!(rows[0].exchange, "BSE");
    assert_eq!(rows[0].instrument_type, "A");
    assert_eq!(rows[2].exchange, "BSE_INDEX");
    assert_eq!(rows[2].brexchange, "BSE");
    assert_eq!(rows[4].symbol, "SENSEX50");
}

#[test]
fn master_derivatives_build_openalgo_symbols() {
    let nfo = parse_derivatives(fixture!("NFOCompactScrip.csv"), Segment::Nfo).unwrap();
    let fut = &nfo[0];
    assert_eq!(fut.symbol, "NIFTY27OCT26FUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(nfo[1].symbol, "NIFTY06OCT2625000CE");
    assert_eq!(nfo[1].strike, 25000.0);
    assert_eq!(nfo[2].instrument_type, "PE");
    assert_eq!(nfo[3].symbol, "VEDL27OCT26292.5CE");
    // Unknown option type keeps the broker symbol (web fallback).
    assert_eq!(nfo[4].symbol, "ODDROW");
    assert_eq!(nfo[4].instrument_type, "");

    let bfo = parse_derivatives(fixture!("BFOCompactScrip.csv"), Segment::Bfo).unwrap();
    // IF / SF instruments are futures whatever option_type says.
    assert_eq!(bfo[0].symbol, "SENSEX29OCT26FUT");
    assert_eq!(bfo[0].expiry, "29-OCT-26");
    assert_eq!(bfo[1].symbol, "SENSEX08OCT2681500CE");
    assert_eq!(bfo[2].symbol, "SBIN29OCT26FUT");

    let mcx = parse_derivatives(fixture!("MCXCompactScrip.csv"), Segment::Mcx).unwrap();
    // COM rows dropped; base from the trading symbol's leading letters.
    assert_eq!(mcx.len(), 3);
    assert_eq!(mcx[0].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(mcx[0].name, "CRUDEOIL");
    assert_eq!(mcx[0].lot_size, 100);
    assert_eq!(mcx[1].symbol, "SILVERM26NOV26131500CE");
    assert_eq!(mcx[2].symbol, "MCXBULLDEX27OCT26FUT");
    assert_eq!(leading_letters("silverm26mar131500ce"), "SILVERM");
}

#[test]
fn master_archive_order_and_dedupe() {
    let rows = parse_archive(&files()).unwrap();
    assert_eq!(rows.len(), 26);
    let r = master();
    assert!(r.by_symbol("NSE_INDEX", "NIFTY").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert_eq!(r.by_symbol("NSE", "SBIN").unwrap().token, "3045");
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().token, "500112");
    // A duplicate (exchange, token) keeps the first row.
    let mut dup = files();
    dup[0]
        .1
        .extend_from_slice(b"NSE,3045,SBIN2-EQ,DUP,EQ,,0,,1,0.05,EQUITY\n");
    assert_eq!(parse_archive(&dup).unwrap().len(), 26);
    // A missing NSE file, or a header without the columns, is refused.
    assert!(parse_archive(&files()[1..]).is_err());
    assert!(parse_nse("a,b\n1,2\n").is_err());
}

#[test]
fn expiry_encodings() {
    for s in [
        "2026-10-27",
        "27-Oct-2026",
        "27OCT2026",
        "27 Oct 2026",
        "27-10-2026",
    ] {
        assert_eq!(
            parse_expiry(s).map(crate::brokers::common::master_contract::format_expiry),
            Some("27-OCT-26".to_string()),
            "{}",
            s
        );
    }
    assert!(parse_expiry("").is_none());
}

// ---------------------------------------------------------------------------
// Constants and payloads
// ---------------------------------------------------------------------------

#[test]
fn enum_maps_match_web() {
    assert_eq!(order_type(PriceType::SlM), "SLM");
    assert_eq!(order_type(PriceType::Market), "MARKET");
    assert_eq!(oa_pricetype("SLM"), "SL-M");
    assert_eq!(oa_pricetype("LIMIT"), "LIMIT");
    assert_eq!(oa_product("nrml"), "NRML");
    for (ex, code) in [
        ("NSE", 1),
        ("NFO", 2),
        ("CDS", 3),
        ("MCX", 4),
        ("BSE", 6),
        ("BFO", 7),
        ("NSE_INDEX", 1),
        ("BSE_INDEX", 6),
        ("XYZ", 1),
    ] {
        assert_eq!(exchange_code(ex), code, "{}", ex);
    }
}

#[test]
fn status_map_follows_web_order() {
    assert_eq!(map_status("CANCEL_CONFIRMED", ""), "cancelled");
    assert_eq!(map_status("COMPLETE", ""), "complete");
    assert_eq!(map_status("REJECTED", ""), "rejected");
    assert_eq!(map_status("TRIGGER PENDING", ""), "trigger pending");
    assert_eq!(map_status("OPEN", ""), "open");
    assert_eq!(map_status("PENDING", ""), "open");
    assert_eq!(map_status("AMO_SUBMIT", ""), "open");
    assert_eq!(map_status("MODIFY_PENDING", ""), "open");
    assert_eq!(map_status("whatever", "new"), "open");
    assert_eq!(map_status("CANCELLED", ""), "cancelled");
    assert_eq!(map_status("", ""), "unknown");
    assert_eq!(map_status("EXPIRED", ""), "expired");
}

fn order(symbol: &str, exchange: &str, pricetype: &str, side: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 75,
        price: 0.0,
        order_type: pricetype.into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: Some(0.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_payload_matches_transform_data() {
    let o = order("NIFTY06OCT2625000CE", "NFO", "SL-M", "SELL");
    let v = place_payload(&o, "<USER_ID>", order_type(o.pricetype), o.price);
    assert_eq!(
        v,
        json!({
            "exchange": "NFO",
            "instrument_token": 41918,
            "client_id": "<USER_ID>",
            "order_type": "SLM",
            "amo": false,
            "price": 0.0,
            "quantity": 75,
            "disclosed_quantity": 0,
            "validity": "DAY",
            "product": "NRML",
            "order_side": "SELL",
            "device": "WEB",
            "user_order_id": 1,
            "trigger_price": 0.0,
            "execution_type": "REGULAR",
        })
    );
}

#[test]
fn modify_payload_carries_order_id() {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 811.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let r = ResolvedModify::resolve("260930000000104", &m, &master()).unwrap();
    let v = modify_payload(&r, "<USER_ID>");
    assert_eq!(v["oms_order_id"], "260930000000104");
    assert_eq!(v["instrument_token"], 3045);
    assert_eq!(v["order_type"], "LIMIT");
    assert_eq!(v["price"], 811.0);
    assert_eq!(v["execution_type"], "REGULAR");
}

#[test]
fn market_orders_become_protected_limits() {
    let buy = order("NIFTY06OCT2625000CE", "NFO", "MARKET", "BUY");
    // Option at 120: 2% slab, tick 0.05.
    assert_eq!(market_protection(&buy, Some(120.0)), ("LIMIT", 122.4));
    let sell = order("NIFTY27OCT26FUT", "NFO", "MARKET", "SELL");
    // Future above 500: 0.5% slab, tick 0.1.
    assert_eq!(market_protection(&sell, Some(25000.0)), ("LIMIT", 24875.0));
    assert_eq!(market_protection(&buy, None), ("MARKET", 0.0));
    assert_eq!(market_protection(&buy, Some(0.0)), ("MARKET", 0.0));
    let limit = order("SBIN", "NSE", "LIMIT", "BUY");
    assert_eq!(market_protection(&limit, Some(800.0)), ("LIMIT", 0.0));
    assert_eq!(buy.action, Action::Buy);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_merges_and_normalises() {
    let r = master();
    let mut rows = list_at(&json(fixture!("orders_completed.json")), "orders").to_vec();
    rows.extend(list_at(&json(fixture!("orders_pending.json")), "orders").to_vec());
    let orders = map_orders(&rows, &r);
    assert_eq!(orders.len(), 6);
    let o = &orders[0];
    assert_eq!(o.symbol, "SBIN");
    assert_eq!(o.status, "complete");
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345678"));
    assert_eq!(o.average_price, 812.35);
    assert_eq!(o.order_timestamp, "2026-09-30 09:21:04");
    let rej = &orders[1];
    assert_eq!(rej.symbol, "NIFTY06OCT2625000CE");
    assert_eq!(rej.status, "rejected");
    assert_eq!(rej.quantity, 75);
    assert_eq!(rej.rejection_reason.as_deref(), Some("RMS: Margin Exceeds"));
    assert_eq!(orders[2].status, "cancelled");
    assert_eq!(orders[3].status, "trigger pending");
    assert_eq!(orders[3].order_type, "SL-M");
    assert_eq!(orders[3].trigger_price, 805.0);
    assert_eq!(orders[4].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(orders[4].pending_quantity, 100);
    assert_eq!(orders[5].status, "open");
}

#[test]
fn cancellable_rows_and_ids() {
    let rows = list_at(&json(fixture!("orders_pending.json")), "orders").to_vec();
    assert!(rows.iter().all(is_cancellable));
    assert_eq!(order_id_of(&rows[0]), "260930000000104");
    assert!(!is_cancellable(&json!({"status": "COMPLETE"})));
    assert!(is_cancellable(&json!({"status": "PENDING_1"})));
    assert!(is_cancellable(&json!({"order_status": "VALIDATED"})));
    assert_eq!(order_id_of(&json!({"nnf_id": 7})), "7");
}

#[test]
fn trade_book_reads_both_field_sets() {
    let r = master();
    let trades = map_trades(list_at(&json(fixture!("trades.json")), "trades"), &r);
    assert_eq!(trades.len(), 2);
    assert_eq!(trades[0].symbol, "SBIN");
    assert!((trades[0].trade_value - 8123.5).abs() < 1e-6);
    let t = &trades[1];
    assert_eq!(t.symbol, "NIFTY27OCT26FUT");
    assert_eq!(t.side, "SELL");
    assert_eq!(t.quantity, 75);
    assert_eq!(t.average_price, 25110.5);
    assert_eq!(t.trade_id, "50001299");
    assert_eq!(t.order_id, "260930000000110");
    assert_eq!(t.timestamp, "2026-09-30 11:00:00");
}

#[test]
fn positions_compute_pnl_like_the_web() {
    let r = master();
    let p = map_positions(list_at(&json(fixture!("positions.json")), "positions"), &r);
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].average_price, 812.35);
    assert!((p[0].pnl - 26.5).abs() < 1e-6);
    assert_eq!(p[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].average_price, 25110.5);
    assert!((p[1].pnl - 750.0).abs() < 1e-6);
    assert_eq!(p[2].quantity, 0);
    // `data.positions` shape reads the same.
    let nested = json!({"status": "success", "data": {"positions": [{"trading_symbol": "SBIN-EQ", "exchange": "NSE", "quantity": "3", "product": "CNC"}]}});
    let p = map_positions(list_at(&nested, "positions"), &r);
    assert_eq!((p[0].quantity, p[0].product.as_str()), (3, "CNC"));
}

#[test]
fn holdings_use_fallback_fields() {
    let r = master();
    let h = map_holdings(list_at(&json(fixture!("holdings.json")), "holdings"), &r);
    // The row without any symbol is skipped (web `continue`).
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].exchange, "NSE");
    assert_eq!(h[0].pnl, 2300.0);
    assert_eq!(h[0].pnl_percentage, 16.43);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!(h[0].close_price, 810.0);
    assert_eq!(h[1].symbol, "RELIANCE");
    assert_eq!(h[1].exchange, "BSE");
    assert_eq!(h[1].quantity, 4);
    assert_eq!(h[1].average_price, 1300.5);
    assert_eq!(h[1].pnl, 419.0);
    assert_eq!(h[1].pnl_percentage, 8.05);
    assert!(h.iter().all(|x| x.product == "CNC"));
}

#[test]
fn funds_read_label_value_pairs() {
    let f = map_funds(&json(fixture!("funds.json")));
    assert_eq!(f.available_cash, 85234.57);
    assert_eq!(f.utilised_debits, 14765.43);
    assert_eq!(f.collateral, 25000.0);
    assert_eq!(f.m2m_unrealized, -120.5);
    assert_eq!(f.m2m_realized, 340.25);
    assert_eq!(map_funds(&json!({"data": {}})).available_cash, 0.0);
}

#[test]
fn loose_readers() {
    let v = json!({"a": "1.5", "b": null, "c": 7});
    assert_eq!(num(&v, "a"), 1.5);
    assert_eq!(text(&v, "b"), "");
    assert_eq!(text(&v, "c"), "7");
    assert_eq!(int(&v, "a"), 1);
    assert_eq!(list_at(&json!({"data": {"x": [1, 2]}}), "orders").len(), 2);
    assert!(list_at(&json!({"data": null}), "orders").is_empty());
}

// ---------------------------------------------------------------------------
// Feed packets (offsets: `api/packet_decoder.py`)
// ---------------------------------------------------------------------------

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_be_bytes());
}

/// Detailed packet (`packet_decoder.py:160-186`).
pub(crate) fn detailed_packet(code: u8, token: u32, ltp_paise: u32) -> Vec<u8> {
    let mut b = vec![0u8; 102];
    b[0] = 1;
    b[1] = code;
    put32(&mut b, 2, token);
    put32(&mut b, 6, ltp_paise);
    put32(&mut b, 10, 1_790_000_000); // last_traded_time
    put32(&mut b, 14, 25); // ltq
    put32(&mut b, 18, 1_234_567); // volume
    put32(&mut b, 22, ltp_paise - 5); // best bid
    put32(&mut b, 26, 300);
    put32(&mut b, 30, ltp_paise + 5); // best ask
    put32(&mut b, 34, 400);
    put64(&mut b, 38, 5_000_000_000); // total buy (u64)
    put64(&mut b, 46, 6_000);
    put32(&mut b, 54, ltp_paise - 100); // average
    put32(&mut b, 62, 80_000); // open
    put32(&mut b, 66, 82_000); // high
    put32(&mut b, 70, 79_500); // low
    put32(&mut b, 74, 80_500); // close
    put32(&mut b, 94, 98_765); // oi
    b
}

/// Snapquote packet (`packet_decoder.py:83-137`).
pub(crate) fn snapquote_packet(code: u8, token: u32) -> Vec<u8> {
    let mut b = vec![0u8; 166];
    b[0] = 4;
    b[1] = code;
    put32(&mut b, 2, token);
    for i in 0..5 {
        put32(&mut b, 6 + 4 * i, 10 + i as u32); // buyers
        put32(&mut b, 26 + 4 * i, 81_200 - 5 * i as u32); // bid prices
        put32(&mut b, 46 + 4 * i, 100 * (i as u32 + 1)); // bid qtys
        put32(&mut b, 66 + 4 * i, 20 + i as u32); // sellers
        put32(&mut b, 86 + 4 * i, 81_210 + 5 * i as u32); // ask prices
        put32(&mut b, 106 + 4 * i, 50 * (i as u32 + 1)); // ask qtys
    }
    put32(&mut b, 126, 81_150); // averageTradePrice
    put32(&mut b, 130, 80_000);
    put32(&mut b, 134, 82_000);
    put32(&mut b, 138, 79_500);
    put32(&mut b, 142, 80_500);
    put64(&mut b, 146, 111_111);
    put64(&mut b, 154, 222_222);
    put32(&mut b, 162, 3_333_333);
    b
}

#[test]
fn decodes_detailed_packet() {
    let d = decode_detailed(&detailed_packet(1, 3045, 81_225)).unwrap();
    assert_eq!((d.exchange_code, d.token), (1, 3045));
    assert_eq!(d.ltp, 812.25);
    assert_eq!(d.ltq, 25);
    assert_eq!(d.volume, 1_234_567);
    assert_eq!(
        (d.bid, d.bid_qty, d.ask, d.ask_qty),
        (812.2, 300, 812.3, 400)
    );
    assert_eq!(d.total_buy_qty, 5_000_000_000);
    assert_eq!(d.total_sell_qty, 6_000);
    assert_eq!(d.average_price, 811.25);
    assert_eq!(
        (d.open, d.high, d.low, d.close),
        (800.0, 820.0, 795.0, 805.0)
    );
    assert_eq!(d.oi, 98_765);
    assert!(decode_detailed(&[1u8; 101]).is_none());
}

#[test]
fn decodes_compact_packet_with_signed_change() {
    let mut b = vec![0u8; 42];
    b[0] = 2;
    b[1] = 2;
    put32(&mut b, 2, 41918);
    put32(&mut b, 6, 12_050);
    b[10..14].copy_from_slice(&(-150i32).to_be_bytes());
    put32(&mut b, 26, 5_000);
    put32(&mut b, 34, 12_045);
    put32(&mut b, 38, 12_055);
    let c = decode_compact(&b).unwrap();
    assert_eq!((c.exchange_code, c.token), (2, 41918));
    assert_eq!(c.ltp, 120.5);
    assert_eq!(c.change, -1.5);
    assert_eq!(c.oi, 5_000);
    assert_eq!((c.bid, c.ask), (120.45, 120.55));
    assert!(decode_compact(&b[..41]).is_none());
}

#[test]
fn decodes_snapquote_packet() {
    let s = decode_snapquote(&snapquote_packet(1, 3045)).unwrap();
    assert_eq!(s.bids[0].price, 812.0);
    assert_eq!(s.bids[4].price, 811.8);
    assert_eq!((s.bids[1].quantity, s.bids[1].orders), (200, 11));
    assert_eq!(
        (s.asks[0].price, s.asks[0].quantity, s.asks[0].orders),
        (812.1, 50, 20)
    );
    assert_eq!(s.average_price, 811.5);
    assert_eq!(s.total_buy_qty, 111_111);
    assert_eq!(s.total_sell_qty, 222_222);
    assert_eq!(s.volume, 3_333_333);
    let depth = to_depth(&QuoteKey::new("NSE", "SBIN"), &s);
    assert_eq!(depth.ltp, 811.5);
    assert_eq!(depth.prev_close, 805.0);
    assert_eq!((depth.ltq, depth.oi), (0, 0));
    assert_eq!(depth.bids.len(), 5);
    assert!(decode_snapquote(&[4u8; 165]).is_none());
}

#[test]
fn quote_from_detailed_packet() {
    let d = decode_detailed(&detailed_packet(1, 3045, 81_225)).unwrap();
    let q = to_quote(&QuoteKey::new("NSE", "SBIN"), &d);
    assert_eq!(
        (q.ltp, q.close, q.bid, q.ask),
        (812.25, 805.0, 812.2, 812.3)
    );
    assert_eq!(q.oi, 98_765);
    assert_eq!(q.change, 7.25);
    assert_eq!(q.change_percent, 0.9);
    assert_eq!(batch_wait(1), Duration::from_secs(3));
    assert_eq!(batch_wait(10), Duration::from_secs(5));
    assert_eq!(batch_wait(50), Duration::from_secs(15));
    let r = master();
    assert_eq!(
        instrument(&r.by_symbol("NSE_INDEX", "NIFTY").unwrap()),
        Some((1, 26000))
    );
    assert_eq!(
        instrument(&r.by_symbol("BSE_INDEX", "SENSEX").unwrap()),
        Some((6, 1))
    );
    assert_eq!(
        instrument(&r.by_symbol("BFO", "SBIN29OCT26FUT").unwrap()),
        Some((7, 1136791))
    );
}

#[test]
fn json_market_frames_are_accepted() {
    let p = decode_frame(&Message::Text(
        json!({"mode": 1, "exchangeCode": 1, "instrumentToken": 3045, "last_traded_price": 81225, "close_price": 80500})
            .to_string(),
    ))
    .unwrap();
    match p {
        Packet::Detailed(d) => assert_eq!((d.token, d.ltp, d.close), (3045, 812.25, 805.0)),
        other => panic!("{:?}", other),
    }
    assert!(decode_frame(&Message::Text("{\"a\":\"h\"}".into())).is_none());
}

fn sub(symbol: &str, exchange: &str, token: &str, brex: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: brex.into(),
        mode,
        depth: 5,
    }
}

#[test]
fn feed_frames_and_heartbeat() {
    let mut f = PocketfulFeed::new("wss://x", "<USER_ID>", "tok/en", master());
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://x/ws/v1/feeds?login_id=%3CUSER_ID%3E&access_token=tok%2Fen"
    );
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", "NSE", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", "26000", "NSE", FeedMode::Quote),
        sub(
            "NIFTY06OCT2625000CE",
            "NFO",
            "41918",
            "NFO",
            FeedMode::Depth,
        ),
        sub("BAD", "NSE", "x", "NSE", FeedMode::Ltp),
    ]);
    let texts: Vec<Value> = frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => json(t),
            other => panic!("{:?}", other),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            json!({"a":"subscribe","v":[[1,3045]],"m":"compact_marketdata"}),
            json!({"a":"subscribe","v":[[1,26000]],"m":"marketdata"}),
            json!({"a":"subscribe","v":[[2,41918]],"m":"full_snapquote"}),
        ]
    );
    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", "NSE", FeedMode::Ltp)]);
    assert_eq!(
        un,
        vec![Message::Text(
            json!({"a":"unsubscribe","v":[[1,3045]],"m":"compact_marketdata"}).to_string()
        )]
    );
    let (every, hb) = f.heartbeat().unwrap();
    assert_eq!(every, Duration::from_secs(15));
    assert_eq!(hb, Message::Text("{\"a\":\"h\"}".into()));
}

#[test]
fn feed_ticks_and_depth_use_subscription_names() {
    let mut f = PocketfulFeed::new("wss://x", "c", "t", master());
    f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", "NSE", FeedMode::Quote),
        sub("SBIN", "NSE", "3045", "NSE", FeedMode::Depth),
    ]);
    let ev = f.parse(&Message::Binary(detailed_packet(1, 26000, 2_510_050)));
    match &ev[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str()),
                ("NIFTY", "NSE_INDEX")
            );
            assert_eq!(t.mode, 2);
            assert_eq!(t.ltp, 25100.5);
            assert_eq!(t.total_buy_quantity, 5_000_000_000);
            assert_eq!(t.last_trade_time_ms, 1_790_000_000_000);
        }
        other => panic!("{:?}", other),
    }
    let ev = f.parse(&Message::Binary(snapquote_packet(1, 3045)));
    match &ev[..] {
        [FeedEvent::Tick(t), FeedEvent::Depth(d)] => {
            assert_eq!(t.ltp, 811.5);
            assert_eq!(t.mode, 3);
            assert_eq!(d.symbol, "SBIN");
            assert_eq!(d.buy[0].price, 812.0);
            assert_eq!(d.sell.len(), 5);
        }
        other => panic!("{:?}", other),
    }
    // Same token on another exchange code is not this subscription.
    assert!(f
        .parse(&Message::Binary(detailed_packet(6, 3045, 100)))
        .is_empty());
    assert!(f.parse(&Message::Binary(vec![9, 9, 9])).is_empty());
}

fn update_frame(mode: u8, body: &str) -> Message {
    let mut b = vec![mode, 0, 0, 0, 0];
    b.extend_from_slice(body.as_bytes());
    Message::Binary(b)
}

#[test]
fn order_and_trade_updates() {
    let mut f = PocketfulFeed::new("wss://x", "c", "t", master());
    let ev = f.parse(&update_frame(50, fixture!("order_update.json")));
    match &ev[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(u.orderid, "260930000000104");
            assert_eq!(u.symbol, "SBIN");
            assert_eq!(u.action, "SELL");
            assert_eq!(u.pricetype, "SL-M");
            assert_eq!(u.order_status, "open");
            assert_eq!(
                (u.quantity, u.filled_quantity, u.pending_quantity),
                (10, 4, 6)
            );
            assert_eq!(u.average_price, 804.9);
        }
        other => panic!("{:?}", other),
    }
    let ev = f.parse(&update_frame(
        51,
        r#"{"order_id":"9","trading_symbol":"NIFTY26OCTFUT","exchange":"NFO","transaction_type":"BUY","trade_quantity":75,"trade_price":25100}"#,
    ));
    match &ev[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(u.symbol, "NIFTY27OCT26FUT");
            assert_eq!(u.order_status, "complete");
            assert_eq!(u.filled_quantity, 75);
        }
        other => panic!("{:?}", other),
    }
    let rejected = f.parse(&update_frame(
        50,
        r#"{"oms_order_id":"5","order_status":"REJECTED","rejection_reason":"No margin"}"#,
    ));
    match &rejected[..] {
        [FeedEvent::OrderUpdate(u)] => assert_eq!(u.rejection_reason, "No margin"),
        other => panic!("{:?}", other),
    }
    // Garbage after the prefix is ignored.
    assert!(f.parse(&update_frame(50, "not json")).is_empty());
}

#[test]
fn broker_identity_and_capabilities() {
    let b = PocketfulBroker::new(master());
    assert_eq!(b.id(), "pocketful");
    assert!(b.timeframe_map().is_empty());
    let c = b.capabilities();
    assert!(!c.history && !c.margin && !c.gtt);
    assert!(c.streaming && c.order_feed && c.multiquotes_batch);
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "code" });
    assert_eq!(b.supported_exchanges().len(), 7);
    // The feed needs the client id stored with the session.
    assert!(b.create_feed(&AuthToken::new("tok")).is_err());
    assert!(b
        .create_feed(&AuthToken::new("tok").with_user_id("<USER_ID>"))
        .is_ok());
}

#[test]
fn auth_helpers() {
    assert_eq!(auth::basic_auth("id", "sec"), "Basic aWQ6c2Vj");
    let f = auth::token_form("c1", "http://127.0.0.1:5000/pocketful/callback");
    assert_eq!(f[0], ("grant_type", "authorization_code".to_string()));
    assert_eq!(f[1].1, "c1");
}

// ---------------------------------------------------------------------------
// Secrets stay out of logged errors
// ---------------------------------------------------------------------------

const SENTINEL: &str = "SENTINEL-7f3a9c";

/// A loopback port with nothing listening (bound, then released).
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn transport_errors_lose_their_url_before_logging() {
    let url = format!(
        "http://127.0.0.1:{}/oapi/v1/orders?api_key={s}&token={s}",
        closed_port(),
        s = SENTINEL
    );
    let raw = crate::brokers::common::http::client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .unwrap_err();
    // Precondition: the unredacted error does carry the secret.
    assert!(format!("{} {:?}", raw, raw).contains(SENTINEL));
    let e = super::redact(crate::error::AppError::from(raw));
    let shown = format!("{} {:?} {} {}", e, e, e.code(), e.client_message());
    assert!(!shown.contains(SENTINEL), "{}", shown);
    // Non-transport errors pass through untouched.
    let other = super::redact(crate::error::AppError::Broker("kept".into()));
    assert_eq!(other.client_message(), "kept");
}

#[tokio::test]
async fn socket_errors_are_logged_by_kind_only() {
    use tokio_tungstenite::tungstenite::{error::UrlError, http, Error as E};
    let url = format!(
        "ws://127.0.0.1:{}/session?token={s}&api_key={s}",
        closed_port(),
        s = SENTINEL
    );
    let real = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap_err();
    let refused = http::Response::builder()
        .status(401)
        .body(Some(format!("bad token {}", SENTINEL).into_bytes()))
        .unwrap();
    let errors = vec![
        real,
        E::Http(refused),
        E::Io(std::io::Error::other(url.clone())),
        E::Url(UrlError::UnsupportedUrlScheme),
        E::ConnectionClosed,
    ];
    for e in &errors {
        let kind = super::streaming::ws_error_kind(e);
        assert!(!kind.contains(SENTINEL), "{}", kind);
        assert!(!kind.is_empty());
    }
    assert_eq!(
        super::streaming::ws_error_kind(&errors[1]),
        "refused with HTTP 401"
    );
}
