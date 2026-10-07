//! INDmoney mapping tests against payload shapes built from the web adapter
//! and the INDstocks docs it cites (`tests/fixtures/brokers/indmoney/`).

use super::auth::{classify_profile, token_from, totp_error, TokenCheck};
use super::data::{
    chunk_days, day_ms, depth_from, extract_market_depth, parse_candles, quote_from_full,
};
use super::funds::{funds_from, margin_leg, parse_margin};
use super::mapping::*;
use super::master_contract::{assign, parse_csv};
use super::orders::{
    cancel_body, extract_order_id, is_smart, modify_body, place_body, place_outcome,
    says_no_positions,
};
use super::streaming::{
    decode, map_stream_status, normalize_order, IndmoneyFeed, IndmoneyOrderFeed,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/indmoney/", $name))
    };
}

fn responses() -> Value {
    serde_json::from_str(fixture!("responses.json")).unwrap()
}

fn rows() -> Vec<SymbolData> {
    let mut r = parse_csv("equity", fixture!("equity.csv"));
    r.extend(parse_csv("fno", fixture!("fno.csv")));
    r.extend(parse_csv("index", fixture!("index.csv")));
    r
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(rows());
    r
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_symbols_and_columns() {
    let all = rows();
    // 5 equity + 5 F&O (MCX row dropped) + 5 indices.
    assert_eq!(all.len(), 15);
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY-Oct2026-FUT");
    assert_eq!(
        (fut.expiry.as_str(), fut.lot_size, fut.tick_size),
        ("27-OCT-26", 75, 0.1)
    );
    assert_eq!(
        (fut.name.as_str(), fut.instrument_type.as_str()),
        ("NIFTY", "FUT")
    );
    assert_eq!(
        (fut.exchange.as_str(), fut.brexchange.as_str()),
        ("NFO", "NSE")
    );
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.token.as_str(), ce.strike), ("51012", 25000.0));
    let pe = r.by_symbol("NFO", "VEDL27OCT26292.5PE").unwrap();
    assert_eq!((pe.name.as_str(), pe.lot_size), ("VEDL", 1150));
    let bfo = r.by_symbol("BFO", "SENSEX30OCT2680000CE").unwrap();
    assert_eq!(bfo.brexchange, "BSE");
    // The same security id on NSE cash and NSE F&O stays two instruments.
    assert_eq!(r.by_token("NSE", "2885").unwrap().symbol, "RELIANCE");
    assert_eq!(
        r.by_token("NFO", "2885").unwrap().symbol,
        "RELIANCE27OCT26FUT"
    );
    let eq = r.by_symbol("NSE", "TCS").unwrap();
    assert_eq!(
        (eq.expiry.as_str(), eq.instrument_type.as_str()),
        ("", "EQ")
    );
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().token, "500112");
    // Indices: SEGMENT carries the name, renamed to OpenAlgo symbols.
    let n = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!((n.brsymbol.as_str(), n.token.as_str()), ("NIFTY 50", "13"));
    assert_eq!(n.instrument_type, "INDEX");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX50").is_some());
    assert!(r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").is_none());
}

#[test]
fn venue_assignment_matches_web() {
    assert_eq!(assign("NSE", "E", "EQUITY", "", false).unwrap().0, "NSE");
    assert_eq!(assign("BSE", "D", "OPTIDX", "PE", false).unwrap().2, "PE");
    assert_eq!(assign("NSE", "FNO", "FUTSTK", "", false).unwrap().2, "FUT");
    assert_eq!(
        assign("NSE", "X", "INDEX", "", false).unwrap().0,
        "NSE_INDEX"
    );
    assert_eq!(assign("BSE", "", "", "", true).unwrap().0, "BSE_INDEX");
    assert!(assign("MCX", "D", "FUTCOM", "", false).is_none());
}

// ---------------------------------------------------------------------------
// Transport rules
// ---------------------------------------------------------------------------

#[test]
fn rate_limit_buckets() {
    assert_eq!(classify("/order", &Method::POST), Bucket::Order);
    assert_eq!(
        classify("/smart/order/cancel", &Method::POST),
        Bucket::Order
    );
    assert_eq!(classify("/order-book", &Method::GET), Bucket::NonTrading);
    assert_eq!(classify("/order", &Method::GET), Bucket::NonTrading);
    assert_eq!(classify("/market/quotes/full", &Method::GET), Bucket::Quote);
    assert_eq!(
        classify("/market/historical/1minute", &Method::GET),
        Bucket::Data
    );
    assert_eq!(classify("/market/instruments", &Method::GET), Bucket::Data);
    assert_eq!(classify("/margin", &Method::GET), Bucket::Data);
    assert_eq!(classify("/funds", &Method::GET), Bucket::NonTrading);
    assert_eq!(retry_delay(Some("2"), 0), Duration::from_secs(2));
    assert_eq!(retry_delay(Some("0"), 0), Duration::from_millis(50));
    assert_eq!(retry_delay(Some("999"), 0), Duration::from_secs(30));
    assert_eq!(retry_delay(None, 0), Duration::from_secs(1));
    assert_eq!(retry_delay(None, 2), Duration::from_secs(4));
    assert_eq!(retry_delay(Some("x"), 1), Duration::from_secs(2));
}

#[test]
fn envelopes_unwrap_like_the_web() {
    let r = |status, json: Value| Reply {
        status,
        json,
        text: String::new(),
    };
    assert_eq!(
        unwrap_account("/x", &r(200, json!({"status":"success","data":[1]}))).unwrap(),
        json!([1])
    );
    let e = unwrap_account(
        "/x",
        &r(200, json!({"status":"failure","error":{"msg":"Bad order"}})),
    )
    .unwrap_err();
    assert_eq!(e.client_message(), "Bad order");
    let e = unwrap_account("/x", &r(200, json!({"success":false,"message":"nope"}))).unwrap_err();
    assert_eq!(e.client_message(), "nope");
    assert!(matches!(
        unwrap_account("/x", &r(401, Value::Null)).unwrap_err(),
        AppError::Auth(_)
    ));
    assert!(unwrap_account("/x", &r(500, Value::Null)).is_err());
}

#[test]
fn order_id_map_is_bounded() {
    let mut m = OrderIdMap::default();
    m.remember("EQ-96057848");
    m.remember("nodash");
    assert_eq!(m.canonical("96057848"), "EQ-96057848");
    assert_eq!(m.canonical("DRV-1"), "DRV-1");
    assert_eq!(m.canonical("123"), "123");
    for i in 0..(ORDER_ID_MAX + 50) {
        m.remember(&format!("EQ-{}", i));
    }
    assert_eq!(m.len(), ORDER_ID_MAX);
    assert_eq!(m.canonical("96057848"), "96057848");
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

#[test]
fn statuses_types_products() {
    assert_eq!(map_status("O-PENDING"), "open");
    assert_eq!(map_status("partially filled"), "open");
    assert_eq!(map_status("SL-PENDING"), "trigger pending");
    assert_eq!(map_status("TRADED"), "complete");
    assert_eq!(map_status("ABORTED"), "rejected");
    assert_eq!(map_status("PARTIALLY FILLED - EXPIRED"), "cancelled");
    assert_eq!(map_status("AMO RECEIVED"), "amo received");
    assert!(is_cancellable("queued") && is_cancellable("SL-PENDING"));
    assert!(!is_cancellable("SUCCESS"));
    assert_eq!(map_order_type("GTT_LIMIT"), "SL");
    assert_eq!(map_order_type("GTT_MARKET"), "SL-M");
    assert_eq!(map_order_type("STOP_LOSS"), "SL");
    assert!(is_smart_type("GTT_LIMIT") && is_smart_type("oco") && !is_smart_type("LIMIT"));
    assert_eq!(map_product("MARGIN", "NFO"), "NRML");
    assert_eq!(map_product("DELIVERY", "NSE"), "CNC");
    assert_eq!(map_product("", "NFO"), "NRML");
    assert_eq!(map_product("", "NSE"), "MIS");
    assert_eq!(product(Product::Nrml), "MARGIN");
    assert_eq!((api_exchange("BFO"), segment("BFO")), ("BSE", "DERIVATIVE"));
    assert_eq!(segment_from_order_id("DRV-1"), "DERIVATIVE");
    assert_eq!(segment_from_order_id("GTT-1"), "EQUITY");
    assert_eq!(scrip_segment("NSE_INDEX"), Some("NIDX"));
    assert_eq!(ws_segment("BSE_INDEX"), Some("BIDX"));
    assert_eq!(ws_segment("MCX"), None);
}

#[test]
fn exchange_resolution_uses_pair_then_token() {
    let r = master();
    assert_eq!(resolve_exchange(&r, "1", "NSE", "DERIVATIVE"), "NFO");
    assert_eq!(resolve_exchange(&r, "1", "BSE_FNO", ""), "BFO");
    // Empty exchange on a derivative position: probe NFO/BFO.
    assert_eq!(resolve_exchange(&r, "860001", "", "DERIVATIVE"), "BFO");
    // Nothing stated: the token decides (NFO is probed first).
    assert_eq!(resolve_exchange(&r, "51012", "", ""), "NFO");
    assert_eq!(resolve_exchange(&r, "500112", "", "EQUITY"), "BSE");
    assert_eq!(resolve_exchange(&r, "nope", "", ""), "NSE");
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_rows_are_openalgo() {
    let r = master();
    let book = rows_of(&responses()["order_book"]["data"]);
    let o: Vec<Order> = book.iter().map(|x| map_order(&r, x)).collect();
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(
        (
            o[0].exchange.as_str(),
            o[0].side.as_str(),
            o[0].product.as_str()
        ),
        ("NSE", "BUY", "MIS")
    );
    assert_eq!((o[0].status.as_str(), o[0].price), ("open", 812.5));
    assert_eq!(o[0].exchange_order_id.as_deref(), Some("1100000012345"));
    assert_eq!(o[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        (o[1].exchange.as_str(), o[1].product.as_str()),
        ("NFO", "NRML")
    );
    assert_eq!(
        (o[1].status.as_str(), o[1].filled_quantity),
        ("complete", 75)
    );
    assert_eq!(o[1].average_price, 1250.5);
    // A stop comes back as GTT_LIMIT with prices in the target legs.
    assert_eq!(o[2].order_type, "SL");
    assert_eq!((o[2].price, o[2].trigger_price), (1418.0, 1420.0));
    assert_eq!(o[2].status, "trigger pending");
    // Empty exchange + EQUITY: token probe finds BSE.
    assert_eq!(
        (o[3].exchange.as_str(), o[3].symbol.as_str()),
        ("BSE", "SBIN")
    );
    assert_eq!(o[3].rejection_reason.as_deref(), Some("Insufficient funds"));
    assert!(is_smart("EQ-777", &book) && !is_smart("EQ-96057848", &book));
    assert!(is_smart("GTT-1", &[]));
}

fn rows_of(v: &Value) -> Vec<Value> {
    v.as_array().unwrap().clone()
}

#[test]
fn trades_borrow_side_product_exchange_from_orders() {
    let r = master();
    let all = responses();
    let facts = order_facts(&rows_of(&all["order_book"]["data"]));
    let t = map_trade(&r, &all["trade_book_equity"]["data"][0], "EQUITY", &facts);
    assert_eq!(
        (
            t.symbol.as_str(),
            t.exchange.as_str(),
            t.side.as_str(),
            t.product.as_str()
        ),
        ("SBIN", "NSE", "BUY", "MIS")
    );
    assert_eq!(
        (t.quantity, t.trade_value, t.trade_id.as_str()),
        (4, 3250.0, "F1")
    );
    let t = map_trade(
        &r,
        &all["trade_book_derivative"]["data"][0],
        "DERIVATIVE",
        &facts,
    );
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.side.as_str()),
        ("NIFTY27OCT2625000CE", "NFO", "SELL")
    );
    // No matching order: venue from the token, side unknown.
    let lone = json!({"exch_order_id":"x","scrip_code":"500112","quantity":1,"price":800});
    let t = map_trade(&r, &lone, "EQUITY", &HashMap::new());
    assert_eq!(
        (t.exchange.as_str(), t.symbol.as_str(), t.side.as_str()),
        ("BSE", "SBIN", "")
    );
}

#[test]
fn positions_and_holdings() {
    let r = master();
    let all = responses();
    let mut p = all["positions_derivative_margin"]["data"][0].clone();
    p["query_segment"] = json!("derivative");
    p["query_product"] = json!("margin");
    p["last_traded_price"] = json!("1,240.50");
    let pos = map_position(&r, &p);
    assert_eq!(
        (
            pos.symbol.as_str(),
            pos.exchange.as_str(),
            pos.product.as_str()
        ),
        ("NIFTY27OCT2625000CE", "NFO", "NRML")
    );
    assert_eq!((pos.quantity, pos.ltp), (-75, 1240.5));
    // Short 75 from 1250.5 marked at 1240.5: +750.
    assert_eq!(pos.pnl, 750.0);
    let mut p = all["positions_equity_intraday"]["data"][0].clone();
    p["query_product"] = json!("intraday");
    let pos = map_position(&r, &p);
    // No LTP: realized only.
    assert_eq!(
        (pos.product.as_str(), pos.pnl, pos.realized_pnl),
        ("MIS", 100.0, 100.0)
    );
    let h: Vec<Holding> = rows_of(&all["holdings"]["data"])
        .iter()
        .map(|x| map_holding(&r, x))
        .collect();
    assert_eq!(
        (h[0].symbol.as_str(), h[0].quantity, h[0].t1_quantity),
        ("RELIANCE", 10, 2)
    );
    assert_eq!(
        (h[0].ltp, h[0].pnl, h[0].current_value),
        (1300.0, 0.0, 13000.0)
    );
    assert_eq!(h[0].isin.as_deref(), Some("INE002A01018"));
    assert_eq!(
        (h[1].symbol.as_str(), h[1].isin.as_deref()),
        ("UNKNOWNCO", None)
    );
    assert!(says_no_positions(&all["positions_derivative_intraday"]));
    assert!(says_no_positions(
        &json!({"status":"failure","error":{"msg":"No data available"}})
    ));
    assert!(!says_no_positions(
        &json!({"status":"error","message":"Server busy"})
    ));
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn order(
    symbol: &str,
    exchange: &str,
    side: &str,
    pricetype: &str,
    price: f64,
    trig: f64,
) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(trig),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_bodies_match_web_transform() {
    let (path, b) = place_body(&order("SBIN", "NSE", "BUY", "LIMIT", 812.5, 0.0), None).unwrap();
    assert_eq!(path, "/order");
    assert_eq!(
        b,
        json!({"txn_type":"BUY","exchange":"NSE","segment":"EQUITY","product":"INTRADAY",
               "security_id":"3045","qty":10,"algo_id":"99999","order_type":"LIMIT",
               "validity":"DAY","is_amo":false,"limit_price":812.5})
    );
    // MARKET -> LIMIT at LTP +0.1% / -0.1%.
    let (_, b) = place_body(
        &order("SBIN", "NSE", "BUY", "MARKET", 0.0, 0.0),
        Some(812.4),
    )
    .unwrap();
    assert_eq!(
        (b["order_type"].as_str(), b["limit_price"].as_f64()),
        (Some("LIMIT"), Some(813.21))
    );
    let (_, b) = place_body(
        &order("SBIN", "BSE", "SELL", "MARKET", 0.0, 0.0),
        Some(800.0),
    )
    .unwrap();
    assert_eq!(b["limit_price"], json!(799.2));
    assert_eq!(b["algo_id"], "9999999999999999");
    // No LTP: native MARKET without a limit price.
    let (_, b) = place_body(&order("SBIN", "NSE", "BUY", "MARKET", 0.0, 0.0), Some(0.0)).unwrap();
    assert_eq!(b["order_type"], "MARKET");
    assert!(b.get("limit_price").is_none());
    // SL keeps its limit; SL-M protects off the trigger (0.5% on 1420, 0.1 tick).
    let (path, b) = place_body(
        &order("RELIANCE", "NSE", "SELL", "SL", 1418.0, 1420.0),
        None,
    )
    .unwrap();
    assert_eq!(path, "/smart/order");
    assert_eq!(
        (b["order_type"].as_str(), b["trigger_limit_price"].as_f64()),
        (Some("TRIGGER"), Some(1418.0))
    );
    assert!(b.get("is_amo").is_none() && b.get("limit_price").is_none());
    let (_, b) = place_body(&order("SBIN", "NSE", "SELL", "SL-M", 0.0, 800.0), None).unwrap();
    assert_eq!(b["trigger_limit_price"], json!(796.0));
    let (_, b) = place_body(&order("SBIN", "NSE", "BUY", "SL", 0.0, 800.0), None).unwrap();
    assert_eq!(b["trigger_limit_price"], json!(804.0));
    // The web reads the instrument type from the symbol suffix, so
    // RELIANCE ("...CE") takes the option slab (1%), exactly as there.
    let (_, b) = place_body(&order("RELIANCE", "NSE", "SELL", "SL-M", 0.0, 1420.0), None).unwrap();
    assert_eq!(b["trigger_limit_price"], json!(1405.8));
    // F&O stops ride NSE; BSE stops are refused; a stop needs a trigger.
    assert!(place_body(
        &order("NIFTY27OCT26FUT", "NFO", "BUY", "SL-M", 0.0, 25000.0),
        None
    )
    .is_ok());
    let e = place_body(&order("SBIN", "BSE", "BUY", "SL", 800.0, 801.0), None).unwrap_err();
    assert!(e.client_message().contains("NSE only"));
    let e = place_body(&order("SBIN", "NSE", "BUY", "SL", 800.0, 0.0), None).unwrap_err();
    assert!(e.client_message().contains("trigger price is required"));
    let mut ioc = order("SBIN", "NSE", "BUY", "LIMIT", 812.5, 0.0);
    ioc.validity = crate::brokers::common::mapping::Validity::Ioc;
    ioc.amo = true;
    let (_, b) = place_body(&ioc, None).unwrap();
    assert_eq!(
        (b["validity"].as_str(), b["is_amo"].as_bool()),
        (Some("IOC"), Some(true))
    );
    let mut mcx = order("SBIN", "NSE", "BUY", "LIMIT", 1.0, 0.0);
    mcx.exchange = crate::brokers::common::mapping::Exchange::Mcx;
    assert!(place_body(&mcx, None).is_err());
}

#[test]
fn placement_answers_and_write_bodies() {
    assert_eq!(
        extract_order_id(&json!({"data":{"order_id":"EQ-1"}})).as_deref(),
        Some("EQ-1")
    );
    assert_eq!(
        extract_order_id(&json!({"data":{"order_data":[{"order_id":"GTT-9","child_order_details":{"order_id":"EQ-2"}}]}}))
            .as_deref(),
        Some("GTT-9")
    );
    assert_eq!(
        place_outcome(200, &json!({"status":"success","data":{"order_id":"EQ-1"}})).unwrap(),
        "EQ-1"
    );
    assert_eq!(
        place_outcome(
            200,
            &json!({"status":"failure","error":{"msg":"No order number in RS response"}})
        )
        .unwrap(),
        "ORDER_PLACED"
    );
    let e = place_outcome(
        200,
        &json!({"status":"failure","error":{"msg":"Insufficient funds"}}),
    )
    .unwrap_err();
    assert_eq!(e.client_message(), "Insufficient funds");
    assert!(place_outcome(429, &Value::Null)
        .unwrap_err()
        .client_message()
        .contains("not resent"));
    let m = ResolvedModify::resolve(
        "EQ-777",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "SELL".into(),
            product: "CNC".into(),
            pricetype: "SL".into(),
            quantity: 5,
            price: 1417.0,
            trigger_price: 1419.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    assert_eq!(
        modify_body(&m, true),
        json!({"segment":"EQUITY","order_id":"EQ-777","qty":5,"algo_id":"99999","trigger_price":1419.0,"trigger_limit_price":1417.0})
    );
    assert_eq!(
        modify_body(&m, false),
        json!({"segment":"EQUITY","order_id":"EQ-777","qty":5,"limit_price":1417.0})
    );
    assert_eq!(
        cancel_body("DRV-5"),
        json!({"segment":"DERIVATIVE","order_id":"DRV-5"})
    );
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_formulas() {
    let f = funds_from(&responses()["funds"]["data"]);
    assert_eq!(f.available_cash, 2980.4);
    assert_eq!(f.collateral, 1000.0);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (12.5, -3.25));
    assert_eq!(f.utilised_debits, 2019.6);
    // No breakdown: withdrawal balance; never negative utilisation.
    let f = funds_from(&json!({"sod_balance":100,"withdrawal_balance":150}));
    assert_eq!((f.available_cash, f.utilised_debits), (150.0, 0.0));
}

#[test]
fn margin_legs_and_answers() {
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 101.5,
        trigger_price: 0.0,
    };
    assert_eq!(
        margin_leg(&leg, "51012").unwrap(),
        json!({"segment":"DERIVATIVE","txnType":"SELL","quantity":"75","price":"101.5",
               "product":"MARGIN","securityID":"51012","exchange":"NSE"})
    );
    assert!(margin_leg(&leg, "None").is_none());
    let mut zero = leg.clone();
    zero.price = 0.0;
    assert_eq!(margin_leg(&zero, "51012").unwrap()["price"], "0");
    assert_eq!(
        parse_margin(&responses()["margin_ok"]),
        Some((120000.5, 90000.25, 30000.25))
    );
    assert_eq!(parse_margin(&json!({"status":"error","message":"x"})), None);
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quotes_and_depth() {
    let all = responses();
    let full = &all["quotes_full"]["data"];
    let k = QuoteKey::new("NSE", "SBIN");
    let q = quote_from_full(&k, &full["NSE_3045"], "NSE_3045");
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (812.4, 805.0, 818.9, 801.1, 808.0)
    );
    assert_eq!(
        (q.volume, q.bid, q.ask, q.bid_qty),
        (1234567, 812.35, 812.45, 100)
    );
    let q = quote_from_full(&QuoteKey::new("NFO", "X"), &full["NFO_51012"], "NFO_51012");
    assert_eq!(
        (q.ltp, q.close, q.oi, q.ask),
        (1240.5, 1210.0, 123450, 1241.0)
    );
    // Market depth: flat, scrip-nested, single unknown key.
    let flat = json!({"depth":[]});
    assert!(extract_market_depth(Some(&flat), "S").is_some());
    let one = json!({"weird":{"aggregate":{}}});
    assert!(extract_market_depth(Some(&one), "S").is_some());
    assert!(extract_market_depth(Some(&json!({"a":1,"b":2})), "S").is_none());
    let mkt = &all["quotes_mkt"]["data"]["NSE_3045"];
    let dp = depth_from(&k, "NSE_3045", &full["NSE_3045"], mkt, None);
    assert_eq!(dp.bids.len(), 5);
    assert_eq!((dp.bids[1].price, dp.bids[1].quantity), (812.3, 1200));
    assert_eq!((dp.asks[0].price, dp.asks[4].price), (812.45, 0.0));
    assert_eq!((dp.total_buy_qty, dp.total_sell_qty), (12000, 9000));
    assert_eq!((dp.ltp, dp.prev_close), (812.4, 808.0));
    // Index: no depth, OHLC from the full quote.
    let ix = depth_from(
        &QuoteKey::new("NSE_INDEX", "NIFTY"),
        "NIDX_13",
        &full["NIDX_13"],
        &Value::Null,
        None,
    );
    assert_eq!(
        (ix.ltp, ix.total_buy_qty, ix.bids[0].price),
        (25010.5, 0, 0.0)
    );
    // Nothing but depth: best bid stands in for LTP.
    let only = depth_from(&k, "NSE_3045", &Value::Null, mkt, None);
    assert_eq!(only.ltp, 812.35);
}

#[test]
fn history_parsing_and_windows() {
    let all = responses();
    let c = parse_candles(&all["history"], "NSE_3045");
    assert_eq!(c.len(), 3);
    assert_eq!(
        (c[1].timestamp, c[1].open, c[1].volume),
        (1791000000, 808.0, 1200)
    );
    let c = parse_candles(&all["history_list"], "NSE_3045");
    assert_eq!((c[0].timestamp, c[0].close), (1791000000, 810.0));
    assert_eq!(chunk_days("5minute"), 7);
    assert_eq!(chunk_days("120minute"), 14);
    assert_eq!(chunk_days("1week"), 365);
    let midnight_utc = d(2026, 10, 3)
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    assert_eq!(day_ms(d(2026, 10, 3), false), (midnight_utc - 19800) * 1000);
    assert_eq!(
        day_ms(d(2026, 10, 3), true),
        (midnight_utc - 19800 + 86399) * 1000
    );
}

// ---------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------

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

fn text(m: &Message) -> Value {
    serde_json::from_str(m.to_text().unwrap()).unwrap()
}

#[test]
fn subscribe_frames_split_by_mode_and_segment() {
    let mut f = IndmoneyFeed::new("wss://x", "tok");
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("RELIANCE", "NSE", "2885", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", "13", FeedMode::Quote),
        sub("RELIANCE27OCT26FUT", "NFO", "2885", FeedMode::Depth),
        sub("CRUDE", "MCX", "9", FeedMode::Ltp),
    ]);
    let v: Vec<Value> = frames.iter().map(text).collect();
    assert_eq!(v.len(), 4);
    assert!(v.contains(&json!({"action":"subscribe","mode":"ltp","instruments":["NSE:2885"]})));
    assert!(v.contains(&json!({"action":"subscribe","mode":"quote","instruments":["NSE:3045"]})));
    assert!(v.contains(&json!({"action":"subscribe","mode":"quote","instruments":["NIDX:13"]})));
    assert!(v.contains(&json!({"action":"subscribe","mode":"quote","instruments":["NFO:2885"]})));
    assert_eq!(f.subscription_count(), 4);
    let many: Vec<FeedSubscription> = (0..2500)
        .map(|i| sub(&format!("S{}", i), "NSE", &i.to_string(), FeedMode::Ltp))
        .collect();
    let frames = f.subscribe_frames(&many);
    assert_eq!(frames.len(), 3);
    assert_eq!(
        text(&frames[0])["instruments"].as_array().unwrap().len(),
        1000
    );
    let un = f.unsubscribe_frames(&many);
    assert_eq!(text(&un[0])["action"], "unsubscribe");
    assert_eq!(f.subscription_count(), 4);
}

#[test]
fn ticks_resolve_retain_and_fan_out_depth() {
    let mut f = IndmoneyFeed::new("wss://x", "tok");
    f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("RELIANCE", "NSE", "2885", FeedMode::Ltp),
        sub("RELIANCE27OCT26FUT", "NFO", "2885", FeedMode::Depth),
    ]);
    let q = json!({"mode":"quote","instrument":"3045","timestamp":1750138351089_i64,
        "data":{"ltp":1426,"open":1410,"high":1430,"low":1400,"close":1418,"volume":5000,
                "bid_price":1425.9,"bid_qty":10,"ask_price":1426.1,"ask_qty":20,"average_price":1420.5,"oi":0}});
    let ev = f.parse(&Message::Text(q.to_string()));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!(
        (t.ltp, t.close, t.volume, t.change),
        (1426.0, 1418.0, 5000, 8.0)
    );
    assert_eq!(t.last_trade_time_ms, 1750138351089);
    // Zeros keep the last value; double-encoded frames decode.
    let z = json!({"mode":"quote","instrument":"3045","timestamp":1,"data":{"ltp":0,"high":1431}});
    let wrapped = Value::String(z.to_string()).to_string();
    let ev = f.parse(&Message::Text(wrapped));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.ltp, t.high, t.open), (1426.0, 1431.0, 1410.0));
    // Bare token 2885 is subscribed on NSE and NFO: ambiguous, dropped.
    let amb = json!({"mode":"ltp","instrument":"2885","data":{"ltp":1}});
    assert!(f.parse(&Message::Text(amb.to_string())).is_empty());
    // Qualified instrument resolves; a depth subscription also gets depth.
    let nfo = json!({"mode":"quote","instrument":"NFO:2885","data":{"ltp":1450,"bid_price":1449.5,"bid_qty":500,"ask_price":1450.5,"ask_qty":250}});
    let ev = f.parse(&Message::Text(nfo.to_string()));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(dp) = &ev[1] else {
        panic!()
    };
    assert_eq!(
        (
            dp.symbol.as_str(),
            dp.buy.len(),
            dp.buy[0].price,
            dp.sell[0].quantity
        ),
        ("RELIANCE27OCT26FUT", 5, 1449.5, 250)
    );
    let ltp = json!({"mode":"ltp","instrument":"NSE:2885","data":{"ltp":1427}});
    let ev = f.parse(&Message::Text(ltp.to_string()));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.mode, t.ltp, t.open),
        ("RELIANCE", 1, 1427.0, 0.0)
    );
    assert_eq!(
        f.parse(&Message::Text("pong".into())),
        vec![FeedEvent::Heartbeat]
    );
    assert!(f.parse(&Message::Text("garbage".into())).is_empty());
    assert!(f
        .parse(&Message::Text(
            json!({"mode":"ltp","instrument":"404","data":{}}).to_string()
        ))
        .is_empty());
    // Unsubscribing clears the per-instrument cache.
    assert_eq!(f.cache_len(), 3);
    f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Quote)]);
    assert_eq!(f.cache_len(), 2);
    let req = f.ws_request().unwrap();
    assert_eq!(req.headers()["Authorization"], "tok");
    assert!(matches!(f.heartbeat(), Some((d, Message::Ping(_))) if d.as_secs() == 30));
}

#[test]
fn order_updates_map_letters_and_canonical_ids() {
    let ids = Arc::new(parking_lot::Mutex::new(OrderIdMap::default()));
    ids.lock().remember("EQ-96057848");
    let mut f = IndmoneyOrderFeed::new("wss://o", "tok", ids.clone());
    assert_eq!(
        text(&f.on_connected()[0]),
        json!({"action":"subscribe","mode":"order_update"})
    );
    let raw = responses()["order_update_live"]
        .as_str()
        .unwrap()
        .to_string();
    // The live stream double-encodes: a JSON string holding JSON.
    let wrapped = Value::String(raw.clone()).to_string();
    for frame in [raw, wrapped] {
        let ev = f.parse(&Message::Text(frame));
        let FeedEvent::OrderUpdate(u) = &ev[0] else {
            panic!("{:?}", ev)
        };
        assert_eq!(u.orderid, "EQ-96057848");
        assert_eq!(
            (u.action.as_str(), u.order_status.as_str()),
            ("SELL", "complete")
        );
        assert_eq!(
            (u.quantity, u.filled_quantity, u.pending_quantity),
            (1, 1, 0)
        );
        assert_eq!((u.average_price, u.rejection_reason.as_str()), (22.89, ""));
    }
    let rej = json!({"order_id":"5","order_status":"J","order_type":"BUY","req_quantity":3,"error_message":"RMS block"});
    let u = normalize_order(&rej, &OrderIdMap::default()).unwrap();
    assert_eq!(
        (
            u.orderid.as_str(),
            u.order_status.as_str(),
            u.pending_quantity
        ),
        ("5", "rejected", 3)
    );
    assert_eq!(u.rejection_reason, "RMS block");
    assert!(normalize_order(&json!({"mode":"order_update"}), &OrderIdMap::default()).is_none());
    assert_eq!(map_stream_status("P"), "open");
    assert_eq!(map_stream_status("X"), "cancelled");
    assert_eq!(map_stream_status("PARTIALLY_EXECUTED"), "open");
    assert_eq!(map_stream_status("weird"), "weird");
    assert!(decode("[1,2]").is_none());
}

// ---------------------------------------------------------------------------
// Auth helpers and identity
// ---------------------------------------------------------------------------

#[test]
fn auth_helpers() {
    assert_eq!(classify_profile(200, &Value::Null), TokenCheck::Valid);
    assert_eq!(
        classify_profile(401, &responses()["profile_rejected"]),
        TokenCheck::Rejected("Token expired".into())
    );
    assert_eq!(classify_profile(503, &Value::Null), TokenCheck::Unknown);
    assert_eq!(classify_profile(429, &Value::Null), TokenCheck::Unknown);
    assert_eq!(
        token_from(&responses()["token_ok"]).as_deref(),
        Some("ind-tok-123")
    );
    assert_eq!(
        token_from(&json!({"access_token":"a"})).as_deref(),
        Some("a")
    );
    assert_eq!(token_from(&json!({"data":{}})), None);
    assert!(totp_error(429, &Value::Null).contains("1 request per 60 seconds"));
    assert!(totp_error(401, &json!({"message":"Invalid TOTP"})).starts_with("Invalid TOTP"));
    assert!(totp_error(503, &Value::Null).contains("temporarily unavailable"));
}

#[test]
fn identity_and_capabilities() {
    let b = IndmoneyBroker::new(master());
    assert_eq!((b.id(), b.name()), ("indmoney", "INDmoney"));
    assert_eq!(b.supported_exchanges().len(), 6);
    assert_eq!(b.timeframe_map().len(), 15);
    assert!(!b.requires_totp());
    let c = b.capabilities();
    assert!(c.history && c.margin && c.streaming && c.order_feed && c.multiquotes_batch && !c.gtt);
    assert!(b.create_feed(&AuthToken::new("tok")).is_ok());
    assert!(b.create_order_feed(&AuthToken::new("tok")).is_ok());
    assert!(b.create_feed(&AuthToken::new(" ")).is_err());
}
