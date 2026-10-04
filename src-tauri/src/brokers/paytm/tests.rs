//! Paytm Money mapping tests against payloads built from the web plugin
//! and Paytm's published API shapes (`tests/fixtures/brokers/paytm/`).

use super::auth::{session_from, TokenResponse};
use super::data::{live_path, pref, to_depth, to_quote, PaytmLive};
use super::funds::{funds_from, m2m, summary};
use super::mapping::*;
use super::master_contract::{index_symbol, option_kind, parse_expiry, parse_security_master};
use super::orders::{cancel_order_body, close_position_body, modify_order_body, place_order_body};
use super::streaming::{decode_frame, decode_packet, packet_len, PaytmFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/paytm/", $name))
    };
}

fn env(json: &str) -> PaytmEnvelope {
    let e: PaytmEnvelope = serde_json::from_str(json).unwrap();
    assert!(e.is_success());
    e
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_security_master(fixture!("security_master.csv")).unwrap());
    r
}

fn order_req(symbol: &str, exchange: &str, pricetype: &str, product: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "SELL".into(),
        quantity: 75,
        price: 120.5,
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: Some(119.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_types_and_exchanges() {
    let rows = parse_security_master(fixture!("security_master.csv")).unwrap();
    // 19 rows; the gold bond (GB) and the MCX currency row are dropped.
    assert_eq!(rows.len(), 17);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (
            sbin.token.as_str(),
            sbin.brexchange.as_str(),
            sbin.instrument_type.as_str()
        ),
        ("3045", "NSE", "EQ")
    );
    assert_eq!(sbin.expiry, "");
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().token, "500112");
    // Quoted name with a comma keeps the columns aligned.
    let rel = r.by_symbol("NSE", "RELIANCE").unwrap();
    assert_eq!(rel.name, "RELIANCE INDUSTRIES, LTD");
    assert_eq!(rel.tick_size, 0.1);
    let etf = r.by_symbol("NSE", "NIFTYBEES").unwrap();
    assert_eq!(etf.instrument_type, "EQ");
    // BSE equity named like an index stays an equity.
    assert_eq!(r.by_symbol("BSE", "AUTO").unwrap().token, "532977");
}

#[test]
fn master_contract_indices_are_renamed_per_exchange() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "NIFTY50");
    assert_eq!(nifty.brexchange, "NSE");
    assert_eq!(nifty.instrument_type, "INDEX");
    assert_eq!(r.by_symbol("NSE_INDEX", "BANKNIFTY").unwrap().token, "25");
    assert_eq!(r.by_symbol("NSE_INDEX", "NIFTYNXT50").unwrap().token, "37");
    assert_eq!(r.by_symbol("NSE_INDEX", "MIDCPNIFTY").unwrap().token, "442");
    assert_eq!(r.by_symbol("BSE_INDEX", "SENSEX").unwrap().token, "51");
    assert_eq!(r.by_symbol("BSE_INDEX", "BSEAUTO").unwrap().token, "85");
    assert_eq!(
        index_symbol("NSE_INDEX", "Nifty Alpha Quality"),
        "NIFTYALPHAQUALITY"
    );
    assert_eq!(
        index_symbol("NSE_INDEX", "NIFTY SMALLCAP250"),
        "NIFTYSMLCAP250"
    );
    assert_eq!(index_symbol("BSE_INDEX", "SNSX50"), "SENSEX50");
}

#[test]
fn master_contract_derivative_symbols() {
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY26OCTFUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.name, "NIFTY");
    assert_eq!(fut.brexchange, "NSE");
    let ce = r.by_symbol("NFO", "NIFTY06OCT2624000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (24000.0, "CE"));
    assert_eq!(ce.token, "43210");
    let pe = r.by_symbol("NFO", "NIFTY06OCT2624000PE").unwrap();
    assert_eq!(pe.instrument_type, "PE");
    // Fractional strikes keep their decimals.
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    let sx = r.by_symbol("BFO", "SENSEX29OCT26FUT").unwrap();
    assert_eq!(sx.brexchange, "BSE");
    assert!(r.by_symbol("BFO", "SENSEX01OCT2681000PE").is_some());
}

#[test]
fn master_contract_expiry_and_option_kind() {
    for s in [
        "2026-10-27",
        "2026-10-27 14:30:00",
        "27-10-2026",
        "27-Oct-2026",
    ] {
        assert_eq!(
            parse_expiry(s).map(crate::brokers::common::master_contract::format_expiry),
            Some("27-OCT-26".to_string()),
            "{}",
            s
        );
    }
    assert_eq!(parse_expiry("-1"), None);
    assert_eq!(parse_expiry(""), None);
    assert_eq!(option_kind("NIFTY 12 MAY 17850 CALL"), "CE");
    assert_eq!(option_kind("NIFTY 12 MAY 17850 put"), "PE");
    assert_eq!(option_kind("NIFTY 12 MAY 17850 CE"), "CE");
    assert_eq!(option_kind("NIFTY 12 MAY 17850"), "OPT");
}

#[test]
fn master_contract_rejects_unknown_format() {
    let e = parse_security_master("a,b\n1,2\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_security_master("").is_err());
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[test]
fn enum_maps() {
    assert_eq!(paytm_exchange(Exchange::Nfo), "NSE");
    assert_eq!(paytm_exchange(Exchange::Bfo), "BSE");
    assert_eq!(paytm_exchange(Exchange::NseIndex), "NSE");
    assert_eq!(segment(Exchange::Nse), "E");
    assert_eq!(segment(Exchange::Bfo), "D");
    assert_eq!(paytm_order_type(PriceType::Market), "MKT");
    assert_eq!(paytm_order_type(PriceType::Limit), "LMT");
    assert_eq!(paytm_order_type(PriceType::Sl), "SL");
    assert_eq!(paytm_order_type(PriceType::SlM), "SLM");
    assert_eq!(oa_order_type("SLM"), Some("SL-M"));
    assert_eq!(oa_order_type("lmt"), Some("LIMIT"));
    assert_eq!(oa_order_type("XYZ"), None);
    assert_eq!(paytm_product(Product::Cnc), "C");
    assert_eq!(paytm_product(Product::Nrml), "M");
    assert_eq!(paytm_product(Product::Mis), "I");
    assert_eq!(oa_product("C"), "CNC");
    assert_eq!(oa_product("M"), "NRML");
    assert_eq!(oa_product("I"), "MIS");
    assert_eq!(oa_side("B"), "BUY");
    assert_eq!(paytm_side(Action::Sell), "S");
    assert_eq!(map_status("Successful"), "complete");
    assert_eq!(map_status("Pending"), "trigger pending");
    assert_eq!(map_status("Open"), "open");
    assert_eq!(map_status("Rejected"), "rejected");
    assert_eq!(map_status("Cancelled"), "cancelled");
    assert_eq!(map_status("Expired"), "expired");
    assert_eq!(oa_exchange("NSE", "OPTSTK"), "NFO");
    assert_eq!(oa_exchange("BSE", "FUTIDX"), "BFO");
    assert_eq!(oa_exchange("NSE", "EQUITY"), "NSE");
}

#[test]
fn envelope_errors() {
    let e: PaytmEnvelope =
        serde_json::from_str(r#"{"status":"error","message":"x","errors":[{"message":"Invalid price"},{"message":"Bad qty"}]}"#)
            .unwrap();
    assert!(!e.is_success());
    assert_eq!(e.error_message(), "Invalid price; Bad qty");
    let e: PaytmEnvelope = serde_json::from_str(r#"{"status":"error","message":"Nope"}"#).unwrap();
    assert_eq!(e.error_message(), "Nope");
    assert!(matches!(paytm_error(401, "x"), AppError::Auth(_)));
    assert_eq!(paytm_error(400, "Bad").client_message(), "Bad");
    assert_eq!(
        paytm_error(400, " ").client_message(),
        "Paytm Money refused the request."
    );
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn token_answer_to_session() {
    let full: TokenResponse = serde_json::from_value(json!({
        "access_token": "acc", "public_access_token": "pub", "read_access_token": "rd"
    }))
    .unwrap();
    assert_eq!(session_from(&full), Some(("acc".into(), "pub".into())));
    let only: TokenResponse = serde_json::from_value(json!({"access_token": "acc"})).unwrap();
    assert_eq!(session_from(&only), Some(("acc".into(), "acc".into())));
    let none: TokenResponse = serde_json::from_value(json!({"message": "x"})).unwrap();
    assert_eq!(session_from(&none), None);
}

// ---------------------------------------------------------------------------
// Order bodies
// ---------------------------------------------------------------------------

#[test]
fn place_body_matches_transform_data() {
    let o = order_req("NIFTY06OCT2624000CE", "NFO", "LIMIT", "NRML");
    let b = place_order_body(&o);
    assert_eq!(
        b,
        json!({
            "security_id": "43210", "exchange": "NSE", "txn_type": "S", "order_type": "LMT",
            "quantity": 75, "product": "M", "price": 120.5, "validity": "DAY",
            "segment": "D", "source": "M"
        })
    );
    let sl = place_order_body(&order_req("SBIN", "NSE", "SL-M", "MIS"));
    assert_eq!(sl["order_type"], "SLM");
    assert_eq!(sl["trigger_price"], 119.0);
    assert_eq!(sl["segment"], "E");
    assert_eq!(sl["product"], "I");
}

#[test]
fn modify_and_cancel_bodies_echo_the_book_row() {
    let book = parse_orders(&env(fixture!("orders.json")).data);
    let row = &book[0];
    let m = ResolvedModify {
        order_id: row.order_no.clone(),
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        product: Product::Mis,
        pricetype: PriceType::Limit,
        quantity: 20,
        price: 801.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
        instrument: master().by_symbol("NSE", "SBIN").unwrap(),
    };
    let b = modify_order_body(&m, row);
    assert_eq!(b["order_no"], "261003000000101");
    assert_eq!(b["serial_no"], 1);
    assert_eq!(b["group_id"], 8);
    assert_eq!(b["security_id"], "3045");
    assert_eq!(b["txn_type"], "B");
    assert_eq!(b["source"], "N");
    assert_eq!(b["off_mkt_flag"], "false");
    assert_eq!(b["quantity"], 20);
    assert_eq!(b["price"], 801.0);
    assert_eq!(b["order_type"], "LMT");
    let mut mkt = m.clone();
    mkt.pricetype = PriceType::Market;
    let b = modify_order_body(&mkt, row);
    assert_eq!(
        (b["price"].clone(), b["order_type"].clone()),
        (json!(0.0), json!("MKT"))
    );

    let c = cancel_order_body(row);
    for k in [
        "exchange",
        "segment",
        "product",
        "security_id",
        "quantity",
        "validity",
        "order_type",
        "price",
        "mkt_type",
        "serial_no",
        "group_id",
    ] {
        assert_eq!(c[k], row.raw[k], "{}", k);
    }
    assert_eq!(c["source"], "N");
}

#[test]
fn close_position_body_uses_the_row() {
    let rows: Vec<PaytmPosition> = env(fixture!("positions.json")).rows();
    let long = close_position_body(&rows[0]).unwrap();
    assert_eq!(long["txn_type"], "S");
    assert_eq!(long["quantity"], 10);
    assert_eq!(long["segment"], "E");
    assert_eq!(long["product"], "I");
    let short = close_position_body(&rows[1]).unwrap();
    assert_eq!(short["txn_type"], "B");
    assert_eq!(short["quantity"], 75);
    assert_eq!(short["segment"], "D");
    assert!(close_position_body(&rows[2]).is_none());
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let orders = map_orders(&parse_orders(&env(fixture!("orders.json")).data), &master());
    assert_eq!(orders.len(), 5);
    let o = &orders[0];
    assert_eq!((o.symbol.as_str(), o.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!(o.status, "trigger pending");
    assert_eq!(
        (o.side.as_str(), o.order_type.as_str(), o.product.as_str()),
        ("BUY", "LIMIT", "MIS")
    );
    assert_eq!(o.pending_quantity, 10);
    let opt = &orders[1];
    assert_eq!(opt.symbol, "NIFTY06OCT2624000CE");
    assert_eq!(opt.exchange, "NFO");
    assert_eq!(opt.status, "complete");
    assert_eq!(opt.product, "NRML");
    assert_eq!(opt.filled_quantity, 75);
    assert_eq!(opt.average_price, 120.5);
    let rej = &orders[2];
    assert_eq!(
        (rej.exchange.as_str(), rej.status.as_str()),
        ("BSE", "rejected")
    );
    assert_eq!(
        rej.rejection_reason.as_deref(),
        Some("Insufficient holdings to sell")
    );
    assert_eq!(rej.exchange_order_id, None);
    assert_eq!(orders[3].status, "cancelled");
    assert_eq!(orders[3].order_type, "SL");
    assert_eq!(orders[3].symbol, "NIFTY27OCT26FUT");
    let bfo = &orders[4];
    assert_eq!(
        (bfo.exchange.as_str(), bfo.symbol.as_str()),
        ("BFO", "SENSEX01OCT2681000PE")
    );
    assert_eq!(
        (bfo.status.as_str(), bfo.order_type.as_str()),
        ("open", "SL-M")
    );
}

#[test]
fn trade_book_keeps_traded_orders_only() {
    let trades = map_trades(&parse_orders(&env(fixture!("orders.json")).data), &master());
    assert_eq!(trades.len(), 1);
    let t = &trades[0];
    assert_eq!(t.symbol, "NIFTY06OCT2624000CE");
    assert_eq!(t.exchange, "NFO");
    assert_eq!(t.quantity, 75);
    assert_eq!(t.trade_value, 9037.5);
    assert_eq!(t.timestamp, "03-10-2026 09:21:01");
}

#[test]
fn positions_are_normalised() {
    let rows: Vec<PaytmPosition> = env(fixture!("positions.json")).rows();
    let p = map_positions(&rows, &master());
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].product.as_str(), p[0].quantity),
        ("SBIN", "MIS", 10)
    );
    assert_eq!(p[0].average_price, 800.5);
    assert_eq!(p[0].pnl, 97.5);
    assert_eq!(p[1].symbol, "NIFTY06OCT2624000CE");
    assert_eq!((p[1].exchange.as_str(), p[1].quantity), ("NFO", -75));
    assert_eq!(p[1].average_price, 120.5);
    assert_eq!(p[1].realized_pnl, 250.0);
    assert_eq!(p[1].overnight_quantity, -75);
    assert_eq!(
        (p[2].exchange.as_str(), p[2].symbol.as_str()),
        ("BFO", "SENSEX29OCT26FUT")
    );
    assert_eq!(m2m(&rows), (1250.0, 285.0));
}

#[test]
fn holdings_pick_the_exchange_like_the_web() {
    let h = map_holdings(
        &holding_rows(&env(fixture!("holdings.json")).data),
        &master(),
    );
    assert_eq!(h.len(), 3);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(h[0].pnl, 1002.0);
    assert_eq!(h[0].pnl_percentage, 10.02);
    assert_eq!(h[0].close_price, 2740.0);
    assert_eq!(h[0].isin.as_deref(), Some("INE002A01018"));
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("SBIN", "BSE")
    );
    assert_eq!(h[1].pnl, -100.0);
    assert_eq!(h[1].t1_quantity, 2);
    // Unknown id: the broker symbol, and no division by a zero cost.
    assert_eq!(h[2].symbol, "DELISTCO");
    assert_eq!(h[2].pnl_percentage, 0.0);
    // A plain list works too.
    assert_eq!(holding_rows(&json!([{"nse_security_id": "3045"}])).len(), 1);
}

#[test]
fn funds_from_summary_and_positions() {
    let s = summary(&env(fixture!("funds.json")).data);
    let f = funds_from(&s, 1250.0, 285.0);
    assert_eq!(f.available_cash, 98500.76);
    assert_eq!(f.collateral, 2500.0);
    assert_eq!(f.utilised_debits, 6499.24);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (1250.0, 285.0));
}

// ---------------------------------------------------------------------------
// Quotes
// ---------------------------------------------------------------------------

#[test]
fn pref_strings_follow_prepare_symbol_for_api() {
    let r = master();
    let p = |ex: &str, s: &str| pref(&r.by_symbol(ex, s).unwrap());
    assert_eq!(p("NSE", "SBIN"), "NSE:3045:EQUITY");
    assert_eq!(p("NSE_INDEX", "NIFTY"), "NSE:13:INDEX");
    assert_eq!(p("BSE_INDEX", "SENSEX"), "BSE:51:INDEX");
    assert_eq!(p("NFO", "NIFTY06OCT2624000CE"), "NSE:43210:OPTION");
    assert_eq!(p("BFO", "SENSEX29OCT26FUT"), "BSE:880001:FUTURE");
    assert_eq!(p("NSE", "NIFTYBEES"), "NSE:19913:EQUITY");
    assert_eq!(
        live_path(&["NSE:3045:EQUITY".into(), "NSE:13:INDEX".into()]),
        "/data/v1/price/live?mode=FULL&pref=NSE%3A3045%3AEQUITY%2CNSE%3A13%3AINDEX"
    );
}

#[test]
fn quote_and_depth_from_live_price() {
    let rows: Vec<PaytmLive> = env(fixture!("live_price.json")).rows();
    let k = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&k, &rows[0]);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (810.25, 802.0, 812.4, 799.5, 804.0)
    );
    assert_eq!(q.volume, 1234567);
    assert_eq!((q.bid, q.ask), (0.0, 0.0));
    assert_eq!((q.change, q.change_percent), (6.25, 0.78));
    let d = to_depth(&k, &rows[0]);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.bids[0].price, 810.2);
    assert_eq!(d.bids[0].orders, 3);
    assert_eq!(d.asks[4].price, 0.0);
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (570, 150));
    assert_eq!(d.ltq, 25);
    assert_eq!(d.prev_close, 804.0);
    let o = to_quote(&QuoteKey::new("NFO", "X"), &rows[1]);
    assert_eq!(o.oi, 4520000);
    assert_eq!(rows[1].security_id, "43210");
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

fn put_f32(b: &mut [u8], o: usize, v: f32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_i16(b: &mut [u8], o: usize, v: i16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

/// LTP packet per `_parse_ltp_packet` (`paytm_websocket.py:332-345`).
fn ltp_packet(code: u8, id: u32, ltp: f32) -> Vec<u8> {
    let mut b = vec![0u8; 23];
    b[0] = code;
    put_f32(&mut b, 1, ltp);
    put_u32(&mut b, 5, 1_791_014_400);
    put_u32(&mut b, 9, id);
    b[13] = 1;
    b[14] = 1;
    put_f32(&mut b, 15, 6.25);
    put_f32(&mut b, 19, 0.78);
    b
}

/// QUOTE packet per `_parse_quote_packet` (`:363-393`).
fn quote_packet(id: u32) -> Vec<u8> {
    let mut b = vec![0u8; 67];
    b[0] = 62;
    put_f32(&mut b, 1, 810.25);
    put_u32(&mut b, 5, 1_791_014_400);
    put_u32(&mut b, 9, id);
    put_u32(&mut b, 15, 25);
    put_f32(&mut b, 19, 805.5);
    put_u32(&mut b, 23, 1_234_567);
    put_u32(&mut b, 27, 120_000);
    put_u32(&mut b, 31, 110_000);
    put_f32(&mut b, 35, 802.0);
    put_f32(&mut b, 39, 804.0);
    put_f32(&mut b, 43, 812.5);
    put_f32(&mut b, 47, 799.5);
    put_f32(&mut b, 51, 0.78);
    put_f32(&mut b, 55, 6.25);
    put_f32(&mut b, 59, 900.0);
    put_f32(&mut b, 63, 600.0);
    b
}

/// FULL packet per `_parse_full_packet` (`:416-513`).
fn full_packet(id: u32) -> Vec<u8> {
    let mut b = vec![0u8; 175];
    b[0] = 63;
    for i in 0..5 {
        let o = 1 + i * 20;
        put_u32(&mut b, o, 100 + i as u32);
        put_u32(&mut b, o + 4, 200 + i as u32);
        put_i16(&mut b, o + 8, 1 + i as i16);
        put_i16(&mut b, o + 10, 11 + i as i16);
        put_f32(&mut b, o + 12, 118.0 - i as f32 * 0.25);
        put_f32(&mut b, o + 16, 118.5 + i as f32 * 0.25);
    }
    put_f32(&mut b, 101, 118.25);
    put_u32(&mut b, 105, 1_791_014_400);
    put_u32(&mut b, 109, id);
    put_u32(&mut b, 115, 75);
    put_f32(&mut b, 119, 119.5);
    put_u32(&mut b, 123, 9_800_000);
    put_u32(&mut b, 127, 500_000);
    put_u32(&mut b, 131, 400_000);
    put_f32(&mut b, 135, 125.0);
    put_f32(&mut b, 139, 122.0);
    put_f32(&mut b, 143, 131.0);
    put_f32(&mut b, 147, 110.0);
    put_f32(&mut b, 151, -3.07);
    put_f32(&mut b, 155, -3.75);
    put_u32(&mut b, 167, 4_520_000);
    put_u32(&mut b, 171, 12_000);
    b
}

/// INDEX_QUOTE (`:395-414`) and INDEX_FULL (`:515-533`).
fn index_packet(code: u8, id: u32) -> Vec<u8> {
    let mut b = vec![0u8; packet_len(code).unwrap()];
    b[0] = code;
    put_f32(&mut b, 1, 25012.5);
    put_u32(&mut b, 5, id);
    put_f32(&mut b, 11, 24950.0);
    put_f32(&mut b, 15, 24900.0);
    put_f32(&mut b, 19, 25050.0);
    put_f32(&mut b, 23, 24890.0);
    put_f32(&mut b, 27, 112.5);
    put_f32(&mut b, 31, 0.45);
    if code == 66 {
        put_u32(&mut b, 35, 1_791_014_400);
    }
    b
}

#[test]
fn packet_sizes() {
    assert_eq!(packet_len(61), Some(23));
    assert_eq!(packet_len(62), Some(67));
    assert_eq!(packet_len(63), Some(175));
    assert_eq!(packet_len(64), Some(23));
    assert_eq!(packet_len(65), Some(43));
    assert_eq!(packet_len(66), Some(39));
    assert_eq!(packet_len(7), None);
}

#[test]
fn decodes_ltp_and_index_ltp() {
    for code in [61u8, 64] {
        let p = decode_packet(&ltp_packet(code, 3045, 810.25)).unwrap();
        assert_eq!(p.security_id, 3045);
        assert_eq!(p.tick.ltp, 810.25);
        assert_eq!((p.tick.change, p.tick.change_percent), (6.25, 0.78));
        assert_eq!(p.tick.last_trade_time_ms, 1_791_014_400_000);
        assert!(p.depth.is_none());
    }
    // Truncated packets are ignored.
    assert!(decode_packet(&ltp_packet(61, 1, 1.0)[..20]).is_none());
}

#[test]
fn decodes_quote_offsets() {
    let p = decode_packet(&quote_packet(3045)).unwrap();
    let t = &p.tick;
    assert_eq!(t.ltp, 810.25);
    assert_eq!(t.last_quantity, 25);
    assert_eq!(t.average_price, 805.5);
    assert_eq!(t.volume, 1_234_567);
    assert_eq!(
        (t.total_buy_quantity, t.total_sell_quantity),
        (120_000, 110_000)
    );
    assert_eq!(
        (t.open, t.close, t.high, t.low),
        (802.0, 804.0, 812.5, 799.5)
    );
    // change_pct @51, change_abs @55 (note the order differs from FULL).
    assert_eq!((t.change, t.change_percent), (6.25, 0.78));
}

#[test]
fn decodes_full_with_depth_and_oi() {
    let p = decode_packet(&full_packet(43210)).unwrap();
    assert_eq!(p.security_id, 43210);
    let t = &p.tick;
    assert_eq!(t.ltp, 118.25);
    assert_eq!(t.oi, 4_520_000);
    assert_eq!(
        (t.open, t.close, t.high, t.low),
        (125.0, 122.0, 131.0, 110.0)
    );
    assert_eq!((t.change, t.change_percent), (-3.75, -3.07));
    let (buy, sell) = p.depth.unwrap();
    assert_eq!(buy[0].quantity, 100);
    assert_eq!(buy[0].orders, 1);
    assert_eq!(buy[0].price, 118.0);
    assert_eq!(sell[4].quantity, 204);
    assert_eq!(sell[4].orders, 15);
    assert_eq!(sell[4].price, 119.5);
}

#[test]
fn decodes_index_quote_and_full() {
    let q = decode_packet(&index_packet(65, 13)).unwrap();
    assert_eq!(q.security_id, 13);
    assert_eq!(q.tick.ltp, 25012.5);
    assert_eq!(q.tick.close, 24900.0);
    // INDEX_QUOTE: change_abs @27, change_pct @31.
    assert_eq!((q.tick.change, q.tick.change_percent), (112.5, 0.45));
    let f = decode_packet(&index_packet(66, 13)).unwrap();
    // INDEX_FULL: change_pct @27, change_abs @31, ltt @35.
    assert_eq!((f.tick.change_percent, f.tick.change), (112.5, 0.45));
    assert_eq!(f.tick.last_trade_time_ms, 1_791_014_400_000);
}

#[test]
fn frames_carry_several_packets() {
    let mut frame = ltp_packet(61, 3045, 810.25);
    frame.extend(index_packet(65, 13));
    frame.extend(full_packet(43210));
    let packets = decode_frame(&frame);
    assert_eq!(packets.len(), 3);
    assert_eq!(packets[2].security_id, 43210);
    // An unknown code stops the walk without panicking.
    assert!(decode_frame(&[9, 1, 2, 3]).is_empty());
    assert!(decode_frame(&[]).is_empty());
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

fn frame_json(m: &Message) -> Value {
    match m {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn feed_url_and_preference_frames() {
    let mut f = PaytmFeed::new("pub tok", master());
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://developer-ws.paytmmoney.com/broadcast/user/v1/data?x_jwt_token=pub%20tok"
    );
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", "NSE", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", "13", "NSE", FeedMode::Quote),
        sub(
            "NIFTY06OCT2624000CE",
            "NFO",
            "43210",
            "NSE",
            FeedMode::Depth,
        ),
        sub("NIFTYBEES", "NSE", "19913", "NSE", FeedMode::Ltp),
        sub("SENSEX29OCT26FUT", "BFO", "880001", "BSE", FeedMode::Quote),
    ]);
    assert_eq!(frames.len(), 1);
    let v = frame_json(&frames[0]);
    assert_eq!(
        v[0],
        json!({"actionType":"ADD","modeType":"LTP","scripType":"EQUITY","exchangeType":"NSE","scripId":"3045"})
    );
    assert_eq!(v[1]["scripType"], "INDEX");
    assert_eq!(v[1]["modeType"], "QUOTE");
    assert_eq!(v[2]["scripType"], "OPTION");
    assert_eq!(v[2]["modeType"], "FULL");
    // web `_determine_scrip_type`: ETF only when the symbol says so.
    assert_eq!(v[3]["scripType"], "EQUITY");
    assert_eq!(v[4]["scripType"], "FUTURE");
    assert_eq!(v[4]["exchangeType"], "BSE");

    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", "NSE", FeedMode::Ltp)]);
    let v = frame_json(&un[0]);
    assert_eq!(v[0]["actionType"], "REMOVE");
    assert_eq!(v[0]["modeType"], "LTP");

    let ch = f.mode_change_frames(
        &sub("NIFTY", "NSE_INDEX", "13", "NSE", FeedMode::Quote),
        &sub("NIFTY", "NSE_INDEX", "13", "NSE", FeedMode::Depth),
    );
    assert_eq!(ch.len(), 1);
    let v = frame_json(&ch[0]);
    assert_eq!(
        (v[0]["actionType"].clone(), v[0]["modeType"].clone()),
        (json!("REMOVE"), json!("QUOTE"))
    );
    assert_eq!(
        (v[1]["actionType"].clone(), v[1]["modeType"].clone()),
        (json!("ADD"), json!("FULL"))
    );
}

#[test]
fn feed_parses_ticks_and_depth_for_subscribed_ids() {
    let mut f = PaytmFeed::new("t", master());
    f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", "NSE", FeedMode::Quote),
        sub(
            "NIFTY06OCT2624000CE",
            "NFO",
            "43210",
            "NSE",
            FeedMode::Depth,
        ),
    ]);
    let mut frame = quote_packet(3045);
    frame.extend(full_packet(43210));
    frame.extend(ltp_packet(61, 999, 1.0)); // not subscribed
    let ev = f.parse(&Message::Binary(frame));
    assert_eq!(ev.len(), 3);
    match &ev[0] {
        FeedEvent::Tick(t) => {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.mode),
                ("SBIN", "NSE", 2)
            );
            assert_eq!(t.volume, 1_234_567);
        }
        e => panic!("{:?}", e),
    }
    match &ev[2] {
        FeedEvent::Depth(d) => {
            assert_eq!(d.exchange, "NFO");
            assert_eq!(d.buy.len(), 5);
            assert_eq!(d.total_buy_quantity, 500_000);
        }
        e => panic!("{:?}", e),
    }
    assert!(f
        .parse(&Message::Text(r#"{"message":"bad pref"}"#.into()))
        .is_empty());
    assert_eq!(
        f.parse(&Message::Pong(Vec::new())),
        vec![FeedEvent::Heartbeat]
    );
    let (every, ping) = f.heartbeat().unwrap();
    assert_eq!(every, std::time::Duration::from_secs(30));
    assert!(matches!(ping, Message::Ping(_)));
    assert_eq!(f.supported_depth_levels(), &[5]);
}

#[test]
fn broker_identity() {
    let b = PaytmBroker::new(master());
    assert_eq!(b.id(), "paytm");
    assert_eq!(
        b.login_kind(),
        LoginKind::Redirect {
            param: "requestToken"
        }
    );
    let c = b.capabilities();
    assert!(!c.history && !c.margin && !c.gtt && c.streaming && c.multiquotes_batch);
    assert!(b.timeframe_map().is_empty());
    assert_eq!(b.supported_exchanges().len(), 6);
    // The feed prefers the public access token.
    let auth = AuthToken::new("acc").with_feed(Some("pub"));
    assert!(b.create_feed(&auth).is_ok());
    assert!(b.create_feed(&AuthToken::new(" ")).is_err());
}
