//! IIFL Capital unit tests: mappings on payloads built from the web code
//! (`mapping/*.py`) and IIFL's documented field names, master-contract
//! parsing, `MWBOCombined` decoding at the web ctypes offsets, and the MQTT
//! relay end to end against an in-process MQTT 3.1.1 broker.

use super::auth::{self, checksum, split_code};
use super::data;
use super::funds;
use super::mapping::{self, OrderFields};
use super::master_contract::{self, parse_segment, SEGMENTS};
use super::mqtt_relay::{self, MqttEndpoint};
use super::streaming::{self, decode_market, decode_oi, IiflFeed, IiflOrderFeed};
use super::IiflCapitalBroker;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{BrokerFeed, FeedEvent, FeedMode, FeedSubscription};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{Broker, BrokerCredentials};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../tests/fixtures/brokers/iiflcapital/",
            $name
        ))
    };
}

fn fx(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// `{"preferred_username":"CL1","exp":1}` as a JWT.
pub const JWT: &str =
    "eyJhbGciOiJSUzI1NiJ9.eyJwcmVmZXJyZWRfdXNlcm5hbWUiOiAiQ0wxIiwgImV4cCI6IDF9.c2ln";

fn master_rows() -> Vec<SymbolData> {
    let files: [(&str, &str); 10] = [
        ("NSEEQ", fixture!("NSEEQ.csv")),
        ("BSEEQ", fixture!("BSEEQ.csv")),
        ("NSEFO", fixture!("NSEFO.csv")),
        ("BSEFO", fixture!("BSEFO.csv")),
        ("NSECURR", fixture!("NSECURR.csv")),
        ("BSECURR", fixture!("BSECURR.csv")),
        ("NSECOMM", fixture!("NSECOMM.csv")),
        ("MCXCOMM", fixture!("MCXCOMM.csv")),
        ("NCDEXCOMM", fixture!("NCDEXCOMM.csv")),
        ("INDICES", fixture!("INDICES.csv")),
    ];
    let mut rows = Vec::new();
    for (seg, text) in files {
        let ex = SEGMENTS.iter().find(|(s, _)| *s == seg).unwrap().1;
        rows.extend(parse_segment(seg, ex, text));
    }
    rows
}

fn resolver() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(master_rows());
    r
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn checksum_is_sha256_of_client_code_secret() {
    assert_eq!(
        checksum("CL1", "AC1", "SEC"),
        "8db2cf540885ef68ed0540bdc18576edd8675edc5467ebc8ba614c71f1925e21"
    );
}

#[test]
fn code_and_client_id_fallbacks_follow_the_web() {
    let creds = |key: &str, client: Option<&str>| BrokerCredentials {
        api_key: key.into(),
        client_id: client.map(str::to_string),
        ..Default::default()
    };
    assert_eq!(
        split_code("778:::ac9", &creds("K", None)),
        ("778".into(), "ac9".into())
    );
    assert_eq!(
        split_code("ac9", &creds("CL7:::APP", None)),
        ("CL7".into(), "ac9".into())
    );
    assert_eq!(
        split_code("ac9", &creds("APPKEY", None)),
        ("APPKEY".into(), "ac9".into())
    );
    assert_eq!(
        split_code("ac9", &creds("CL7:::APP", Some("CL9"))),
        ("CL9".into(), "ac9".into())
    );
    assert_eq!(auth::app_key("CL7:::APP"), "APP");
    assert_eq!(auth::app_key("APP"), "APP");
}

#[test]
fn identity_and_capabilities() {
    let b = IiflCapitalBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "iiflcapital");
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "authCode" });
    let c = b.capabilities();
    assert!(c.history && c.margin && c.streaming && c.multiquotes_batch && !c.gtt);
    assert_eq!(b.supported_exchanges().len(), 9);
    let keys: Vec<&str> = b.timeframe_map().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        ["1m", "5m", "10m", "15m", "30m", "60m", "1h", "D", "W", "M"]
    );
}

// ---------------------------------------------------------------------------
// Mappings
// ---------------------------------------------------------------------------

#[test]
fn enum_maps_match_web() {
    for (s, oa) in [
        ("COMPLETE", "complete"),
        ("Executed", "complete"),
        ("FAILED", "rejected"),
        ("canceled", "cancelled"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("PUT ORDER REQ RECEIVED", "open"),
        ("SOMETHING", "open"),
    ] {
        assert_eq!(mapping::map_status(s), oa, "{}", s);
    }
    assert!(mapping::is_open_status("partially_filled"));
    assert!(!mapping::is_open_status("COMPLETE"));
    assert_eq!(mapping::to_segment("CDS"), "NSECURR");
    assert_eq!(mapping::to_segment("MCX"), "MCXCOMM");
    assert_eq!(mapping::data_segment("BSE_INDEX"), "BSEEQ");
    assert_eq!(mapping::from_segment("NCDEXCOMM"), "MCX");
    assert_eq!(mapping::from_segment("BSECURR"), "BCD");
    assert_eq!(mapping::product_from_broker("BNPL"), "CNC");
    assert_eq!(mapping::product_from_broker("x"), "MIS");
    assert_eq!(mapping::order_type_from_broker("SLM"), "SL-M");
    assert_eq!(mapping::action("B"), "BUY");
    assert_eq!(mapping::validity_to_broker(Validity::Ioc), "IOC");
    assert!(mapping::is_rate_limited(429, ""));
    assert!(mapping::is_rate_limited(
        200,
        "EC003 Something went wrong, please try after some time"
    ));
    assert!(!mapping::is_rate_limited(400, "bad"));
}

#[test]
fn slm_protection_snaps_outward_and_fails_closed() {
    // EQ under 500: 1% slab.
    assert_eq!(
        mapping::slm_protected_price("SBIN", Action::Sell, 100.0, 0.05).unwrap(),
        99.0
    );
    assert_eq!(
        mapping::slm_protected_price("SBIN", Action::Buy, 100.0, 0.05).unwrap(),
        101.0
    );
    // Option under 10: 5%, but at least one tick away and tick-snapped.
    assert_eq!(
        mapping::slm_protected_price("NIFTY2643024000CE", Action::Sell, 0.5, 0.05).unwrap(),
        0.45
    );
    assert_eq!(
        mapping::slm_protected_price("NIFTY2643024000CE", Action::Buy, 0.5, 0.05).unwrap(),
        0.55
    );
    // 0.0025 tick (currency future, 1% slab): 101.37 * 0.99 = 100.3563 floors to 100.355.
    assert_eq!(
        mapping::slm_protected_price("USDINR26APRFUT", Action::Sell, 101.37, 0.0025).unwrap(),
        100.355
    );
    assert!(mapping::slm_protected_price("X", Action::Sell, 0.05, 0.05).is_err());
    assert!(mapping::slm_protected_price("X", Action::Sell, 100.0, 0.0).is_err());
}

fn fields(pt: PriceType, price: f64, trig: f64) -> OrderFields<'static> {
    OrderFields {
        symbol: "SBIN",
        action: Action::Buy,
        pricetype: pt,
        price,
        trigger_price: trig,
        tick_size: 0.05,
    }
}

#[test]
fn order_payloads_match_transform_data() {
    let p = mapping::order_payload(
        "3045",
        "NSE",
        &fields(PriceType::Limit, 812.5, 0.0),
        10,
        Product::Mis,
        Validity::Day,
        0,
        None,
    )
    .unwrap();
    assert_eq!(
        p,
        json!({"instrumentId":"3045","exchange":"NSEEQ","transactionType":"BUY","quantity":"10",
               "orderComplexity":"REGULAR","product":"INTRADAY","validity":"DAY",
               "apiOrderSource":"openalgo","orderType":"LIMIT","price":812.5})
    );
    let m = mapping::order_payload(
        "35001",
        "NFO",
        &fields(PriceType::Market, 0.0, 0.0),
        75,
        Product::Nrml,
        Validity::Ioc,
        25,
        Some(&"x".repeat(60)),
    )
    .unwrap();
    assert_eq!(m["orderType"], "MARKET");
    assert!(m.get("price").is_none() && m.get("slTriggerPrice").is_none());
    assert_eq!(m["product"], "NORMAL");
    assert_eq!(m["validity"], "IOC");
    assert_eq!(m["disclosedQuantity"], "25");
    assert_eq!(m["orderTag"].as_str().unwrap().len(), 50);
    let s = mapping::order_payload(
        "3045",
        "NSE",
        &fields(PriceType::SlM, 0.0, 100.0),
        1,
        Product::Cnc,
        Validity::Day,
        0,
        None,
    )
    .unwrap();
    assert_eq!(s["orderType"], "SL");
    assert_eq!(s["slTriggerPrice"], 100.0);
    assert_eq!(s["price"], 101.0);
    assert_eq!(s["product"], "DELIVERY");
    let sl = mapping::order_payload(
        "3045",
        "NSE",
        &fields(PriceType::Sl, 99.0, 98.5),
        1,
        Product::Mis,
        Validity::Day,
        0,
        None,
    )
    .unwrap();
    assert_eq!(
        (sl["price"].clone(), sl["slTriggerPrice"].clone()),
        (json!(99.0), json!(98.5))
    );
}

#[test]
fn modify_payload_sends_only_the_changed_fields() {
    let m = mapping::modify_payload(&fields(PriceType::Limit, 10.5, 0.0), 3, 0).unwrap();
    assert_eq!(m, json!({"quantity":"3","orderType":"LIMIT","price":10.5}));
    let s = mapping::modify_payload(&fields(PriceType::SlM, 0.0, 100.0), 3, 2).unwrap();
    assert_eq!(
        s,
        json!({"quantity":"3","orderType":"SL","price":101.0,"slTriggerPrice":100.0,"disclosedQuantity":"2"})
    );
    assert!(mapping::safe_order_id("26040300_01-A").is_ok());
    assert!(mapping::safe_order_id("../orders").is_err());
    assert!(mapping::safe_order_id("").is_err());
}

#[test]
fn envelope_helpers() {
    assert!(mapping::is_ok(&json!({"status":"Ok"})));
    assert!(mapping::is_ok(
        &json!({"status":"x","result":[{"status":"success"}]})
    ));
    assert!(!mapping::is_ok(&json!({"status":"error"})));
    assert_eq!(
        mapping::message_of(&json!({"result":[{"message":"EC001 bad"}]})).as_deref(),
        Some("EC001 bad")
    );
    // A status wrapper is not a row.
    assert!(mapping::book_rows(&json!([{"status":"ok"}])).is_empty());
    assert!(mapping::book_rows(&json!({"result":{"status":"ok"}})).is_empty());
    assert_eq!(
        mapping::book_rows(&json!({"result":{"positionList":[{"netQuantity":0}]}})).len(),
        1
    );
}

#[test]
fn books_are_normalised_to_openalgo_symbols() {
    let r = resolver();
    let ob = mapping::book_rows(&fx(fixture!("order_book.json")));
    assert_eq!(ob.len(), 4, "the status wrapper row is dropped");
    let o: Vec<Order> = ob.iter().map(|x| mapping::order_row(x, &r)).collect();
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(o[0].exchange, "NSE");
    assert_eq!(o[0].status, "complete");
    assert_eq!(o[0].product, "MIS");
    assert_eq!(o[0].order_type, "MARKET");
    assert_eq!(o[0].average_price, 812.35);
    assert_eq!(o[0].order_timestamp, "03-Oct-2026 09:15:02");
    assert_eq!(o[1].symbol, "NIFTY30APR2624000CE");
    assert_eq!(o[1].exchange, "NFO");
    assert_eq!(o[1].status, "trigger pending");
    assert_eq!(o[1].quantity, 75, "quantity from filled+pending+cancelled");
    assert_eq!(o[1].trigger_price, 121.0);
    assert_eq!(o[1].product, "NRML");
    assert_eq!(o[2].product, "CNC");
    assert_eq!(o[3].order_type, "SL-M");
    assert_eq!(o[3].status, "rejected");
    assert_eq!(
        o[3].rejection_reason.as_deref(),
        Some("RMS: margin exceeds")
    );

    let t = mapping::trade_row(&mapping::book_rows(&fx(fixture!("trade_book.json")))[0], &r);
    assert_eq!(
        (t.symbol.as_str(), t.quantity, t.average_price),
        ("SBIN", 10, 812.35)
    );
    assert!((t.trade_value - 8123.5).abs() < 1e-9);

    let p: Vec<Position> = mapping::book_rows(&fx(fixture!("positions.json")))
        .iter()
        .map(|x| mapping::position_row(x, &r))
        .collect();
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].ltp, 815.0, "previousDayClose fallback");
    assert_eq!(p[0].pnl, 26.5);
    assert_eq!(p[1].symbol, "NIFTY30APR26FUT");
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].pnl, 150.5, "no LTP: realized only");
    assert_eq!(p[2].product, "CNC");

    let h: Vec<Holding> = mapping::book_rows(&fx(fixture!("holdings.json")))
        .iter()
        .filter_map(|x| mapping::holding_row(x, &r))
        .collect();
    assert_eq!(h.len(), 2, "zero placeholder dropped");
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("SBIN", "NSE")
    );
    assert_eq!(h[0].quantity, 4, "settled DP quantity first");
    assert_eq!(h[0].pnl, 400.0);
    assert_eq!(h[0].pnl_percentage, 14.29);
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("SBIN", "BSE")
    );
    assert_eq!(h[1].quantity, 2);
    assert_eq!(h[1].ltp, 760.0);
}

#[test]
fn funds_prefer_pool_else_sum_segments() {
    let pooled = funds::extract_result(&fx(fixture!("limits.json")));
    let eq = funds::extract_result(&fx(fixture!("limits_equity.json")));
    let fno = funds::extract_result(&fx(fixture!("limits_fno.json")));
    let f = funds::combine(&pooled, &eq, &fno).unwrap();
    assert_eq!(f.available_cash, 105000.5, "tradingLimit '-' falls back");
    assert_eq!(f.collateral, 2000.0);
    assert_eq!(f.utilised_debits, 1500.25);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (0.0, 0.0));
    let pool = json!({"tradingLimit":"5000","utilizedMargin":"10"});
    let g = funds::combine(&pool, &eq, &fno).unwrap();
    assert_eq!(g.available_cash, 5000.0);
    assert!(funds::combine(&Value::Null, &Value::Null, &Value::Null).is_none());
}

#[test]
fn margin_response_parses() {
    let m = funds::parse_margin(
        &json!({"status":"Ok","result":{"span":"1000","exposureMargin":"250.5"}}),
    )
    .unwrap();
    assert_eq!(m.total_margin_required, 1250.5);
    let m = funds::parse_margin(&json!({"result":{"span":1,"exposureMargin":2,"totalMargin":9}}))
        .unwrap();
    assert_eq!(m.total_margin_required, 9.0);
    assert!(funds::parse_margin(&json!({"status":"error","message":"x"})).is_err());
}

#[test]
fn quote_depth_and_history_parsing() {
    let rows = data::market_rows(&fx(fixture!("marketquotes.json")));
    assert_eq!(rows.len(), 2);
    let q = data::parse_quote_row(&rows[0], &QuoteKey::new("NSE", "SBIN"));
    assert_eq!(
        (q.ltp, q.open, q.close, q.volume),
        (812.35, 805.0, 800.0, 1234567)
    );
    assert_eq!(
        (q.bid, q.ask, q.bid_qty, q.ask_qty),
        (812.3, 812.4, 100, 50)
    );
    assert_eq!(q.change, 12.35);
    let q2 = data::parse_quote_row(&rows[1], &QuoteKey::new("NFO", "X"));
    assert_eq!(
        (q2.ltp, q2.close, q2.bid, q2.ask),
        (120.5, 115.0, 120.45, 120.55)
    );

    let d = data::parse_depth_row(
        &data::market_rows(&fx(fixture!("marketdepth.json")))[0],
        &QuoteKey::new("NSE", "SBIN"),
    );
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.bids[1].quantity, 200);
    assert_eq!(d.asks[0].price, 812.4);
    assert_eq!(d.asks[1].orders, 2);
    assert_eq!(d.total_buy_qty, 9000);
    assert_eq!(d.total_sell_qty, 120, "summed when absent");
    assert_eq!(d.prev_close, 800.0);

    let c = data::parse_history_rows(&data::market_rows(&fx(fixture!("historical.json"))));
    assert_eq!(c.len(), 2, "ms and ISO forms of one minute dedupe");
    assert_eq!(c[0].timestamp, 1775187900);
    assert_eq!(c[1].volume, 1200);
    let pipe = data::parse_history_rows(&[json!("1775211300|1|2|0.5|1.5|10|7")]);
    assert_eq!((pipe[0].close, pipe[0].oi), (1.5, 7));
    let keyed = data::parse_history_rows(&[
        json!({"time":"2026-04-03 03:45:00","o":1,"h":2,"l":1,"c":2,"v":3}),
    ]);
    assert_eq!(keyed[0].timestamp, 1775187900);
    assert_eq!(
        data::iifl_date(chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
        "05-Jan-2026"
    );
    assert!(data::supports_oi("MCX") && !data::supports_oi("NSE"));
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_rows() {
    let rows = master_rows();
    let find = |ex: &str, s: &str| {
        rows.iter()
            .find(|r| r.exchange == ex && r.symbol == s)
            .unwrap_or_else(|| panic!("{} {}", ex, s))
    };
    let sbin = find("NSE", "SBIN");
    assert_eq!(
        (
            sbin.brsymbol.as_str(),
            sbin.token.as_str(),
            sbin.brexchange.as_str()
        ),
        ("SBIN-EQ", "3045", "NSEEQ")
    );
    assert_eq!(
        (sbin.instrument_type.as_str(), sbin.tick_size),
        ("EQ", 0.05)
    );
    assert_eq!(find("NSE", "RELIANCE").name, "RELIANCE INDUSTRIES, LTD");
    assert!(!rows
        .iter()
        .any(|r| r.exchange == "NSE" && r.token == "26000"));
    assert!(!rows.iter().any(|r| r.token == "9999"));
    assert_eq!(find("BSE", "SBIN").tick_size, 0.01, "blank tick defaults");
    let fut = find("NFO", "NIFTY30APR26FUT");
    assert_eq!((fut.expiry.as_str(), fut.lot_size), ("30-APR-26", 75));
    assert_eq!(fut.instrument_type, "FUT");
    let ce = find("NFO", "NIFTY30APR2624000CE");
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (24000.0, "CE"));
    let pe = find("NFO", "NIFTY24APR2624050.5PE");
    assert_eq!(pe.expiry, "24-APR-26", "time of day ignored");
    assert!(
        !rows.iter().any(|r| r.token == "40003"),
        "unknown option type"
    );
    assert_eq!(find("BFO", "SENSEX30APR2680000PE").lot_size, 20);
    assert_eq!(find("CDS", "USDINR28APR26FUT").tick_size, 0.0025);
    assert_eq!(find("MCX", "CRUDEOIL20APR26FUT").brexchange, "MCXCOMM");
    let nifty = find("NSE_INDEX", "NIFTY");
    assert_eq!(
        (nifty.token.as_str(), nifty.brexchange.as_str()),
        ("26000", "NSEEQ")
    );
    assert_eq!(nifty.instrument_type, "INDEX");
    find("NSE_INDEX", "BANKNIFTY");
    find("NSE_INDEX", "INDIAVIX");
    find("BSE_INDEX", "SENSEX");
    find("BSE_INDEX", "BSESENSEXNEXT50");
    assert!(
        !rows.iter().any(|r| r.token == "5"),
        "unknown index exchange"
    );
    assert_eq!(master_contract::expiry("30-Apr-2026"), "30-APR-26");
    assert_eq!(master_contract::expiry("2026-04-30"), "30-APR-26");
    assert_eq!(master_contract::expiry(""), "");
    assert_eq!(master_contract::strike_text(0.0), "");
    assert_eq!(master_contract::strike_text(292.5), "292.5");
}

// ---------------------------------------------------------------------------
// Feed decoding
// ---------------------------------------------------------------------------

/// A 188-byte `MWBOCombined` built at the literal offsets of the web
/// ctypes struct (`_pack_ = 2`), independent of `streaming::offsets`.
pub fn mwbo(divisor: i32) -> Vec<u8> {
    let mut b = vec![0u8; 188];
    let put_i = |b: &mut Vec<u8>, o: usize, v: i32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put_u = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put_i(&mut b, 0, 81235); // ltp
    put_u(&mut b, 4, 5); // lastTradedQuantity
    put_u(&mut b, 8, 1_234_567); // tradedVolume
    put_i(&mut b, 12, 81550); // high
    put_i(&mut b, 16, 80110); // low
    put_i(&mut b, 20, 80500); // open
    put_i(&mut b, 24, 80000); // close
    put_i(&mut b, 28, 81000); // averageTradedPrice
    b[32..34].copy_from_slice(&7u16.to_le_bytes()); // reserved
    put_u(&mut b, 34, 100); // bestBidQuantity
    put_i(&mut b, 38, 81230); // bestBidPrice
    put_u(&mut b, 42, 50); // bestAskQuantity
    put_i(&mut b, 46, 81240); // bestAskPrice
    put_u(&mut b, 50, 9000); // totalBidQuantity
    put_u(&mut b, 54, 8000); // totalAskQuantity
    put_i(&mut b, 58, divisor); // priceDivisor
    put_i(&mut b, 62, 1_775_211_300); // lastTradedTime
    for i in 0..10usize {
        let o = 66 + i * 12;
        put_u(&mut b, o, 10 * (i as u32 + 1)); // quantity
        let price = if i < 5 {
            81230 - 5 * i as i32
        } else {
            81240 + 5 * (i as i32 - 5)
        };
        put_i(&mut b, o + 4, price);
        b[o + 8..o + 10].copy_from_slice(&(i as i16 + 1).to_le_bytes()); // orders
        b[o + 10..o + 12].copy_from_slice(&(if i < 5 { 1i16 } else { 2 }).to_le_bytes());
    }
    b
}

fn oi_packet(oi: i32) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [oi, oi + 10, oi - 10, oi - 5] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

#[test]
fn mwbo_offsets_match_the_web_struct() {
    use streaming::offsets::*;
    assert_eq!(
        (BEST_BID_QTY, PRICE_DIVISOR, LAST_TRADED_TIME, DEPTH, SIZE),
        (34, 58, 62, 66, 186)
    );
    let p = decode_market(&mwbo(100)).unwrap();
    assert_eq!(
        (p.ltp, p.high, p.low, p.open, p.close),
        (812.35, 815.5, 801.1, 805.0, 800.0)
    );
    assert_eq!((p.last_traded_quantity, p.volume), (5, 1_234_567));
    assert_eq!(p.average_price, 810.0);
    assert_eq!((p.best_bid_quantity, p.best_bid_price), (100, 812.3));
    assert_eq!((p.best_ask_quantity, p.best_ask_price), (50, 812.4));
    assert_eq!((p.total_buy_quantity, p.total_sell_quantity), (9000, 8000));
    assert_eq!(p.ltt, 1_775_211_300);
    assert_eq!(p.buy.len(), 5);
    assert_eq!(
        (p.buy[0].price, p.buy[0].quantity, p.buy[0].orders),
        (812.3, 10, 1)
    );
    assert_eq!(
        (p.buy[4].price, p.sell[0].price, p.sell[4].quantity),
        (812.1, 812.4, 100)
    );
    // priceDivisor 0 -> 100; 10000 divisor scales (currency).
    assert_eq!(decode_market(&mwbo(0)).unwrap().ltp, 812.35);
    assert_eq!(decode_market(&mwbo(10000)).unwrap().ltp, 8.1235);
    assert!(decode_market(&mwbo(100)[..185]).is_none());
    assert_eq!(decode_market(&mwbo(100)[..186]).unwrap().ltp, 812.35);
    assert_eq!(decode_oi(&oi_packet(1_234_500)), Some(1_234_500));
    assert_eq!(decode_oi(&[0; 15]), None);
}

pub fn sub(
    symbol: &str,
    exchange: &str,
    brex: &str,
    token: &str,
    mode: FeedMode,
) -> FeedSubscription {
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
fn topics_follow_the_bridge_layout() {
    let opt = sub(
        "NIFTY30APR2624000CE",
        "NFO",
        "NSEFO",
        "40001",
        FeedMode::Quote,
    );
    assert_eq!(
        streaming::topics_for(&opt),
        [
            "prod/marketfeed/mw/v1/nsefo/40001",
            "prod/marketfeed/oi/v1/nsefo/40001"
        ]
    );
    let ltp = sub(
        "NIFTY30APR2624000CE",
        "NFO",
        "NSEFO",
        "40001",
        FeedMode::Ltp,
    );
    assert_eq!(streaming::topics_for(&ltp).len(), 1);
    let idx = sub("NIFTY", "NSE_INDEX", "NSEEQ", "26000", FeedMode::Depth);
    assert_eq!(
        streaming::topics_for(&idx),
        ["prod/marketfeed/index/v1/nseeq/26000"]
    );
    assert_eq!(
        streaming::order_topics("778"),
        ["prod/updates/order/v1/778", "prod/updates/trade/v1/778"]
    );
}

#[test]
fn ticks_are_sliced_per_mode() {
    let p = decode_market(&mwbo(100)).unwrap();
    let ev = streaming::ticks(
        &sub("SBIN", "NSE", "NSEEQ", "3045", FeedMode::Ltp),
        &p,
        Some(9),
    );
    assert_eq!(ev.len(), 1);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.mode, t.ltp, t.open, t.oi), (1, 812.35, 0.0, 0));
    assert_eq!(t.last_trade_time_ms, 1_775_211_300_000);
    let ev = streaming::ticks(&sub("X", "NFO", "NSEFO", "1", FeedMode::Depth), &p, Some(9));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.mode, t.open, t.close, t.oi, t.volume),
        (3, 805.0, 800.0, 9, 1_234_567)
    );
    assert_eq!((t.change, t.change_percent), (12.35, 1.54));
    assert_eq!((t.average_price, t.last_quantity), (810.0, 5));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(
        (d.buy.len(), d.sell.len(), d.total_buy_quantity),
        (5, 5, 9000)
    );
}

#[test]
fn feed_frames_and_parse() {
    let mut f = IiflFeed::new(
        MqttEndpoint::plain("127.0.0.1", 1),
        JWT,
        SymbolResolver::new(),
    )
    .unwrap();
    assert!(f.awaits_auth_ack());
    let s = sub(
        "NIFTY30APR2624000CE",
        "NFO",
        "NSEFO",
        "40001",
        FeedMode::Depth,
    );
    let frames = f.subscribe_frames(std::slice::from_ref(&s));
    assert_eq!(frames.len(), 1);
    let Message::Text(t) = &frames[0] else {
        panic!()
    };
    let v: Value = serde_json::from_str(t).unwrap();
    assert_eq!(v["op"], "sub");
    assert_eq!(v["topics"].as_array().unwrap().len(), 2);
    let oi = mqtt_relay::encode_publish("prod/marketfeed/oi/v1/nsefo/40001", &oi_packet(500));
    assert!(f.parse(&Message::Binary(oi)).is_empty());
    let mw = mqtt_relay::encode_publish("prod/marketfeed/mw/v1/nsefo/40001", &mwbo(100));
    let ev = f.parse(&Message::Binary(mw.clone()));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.oi),
        ("NIFTY30APR2624000CE", "NFO", 500)
    );
    // Unknown topic and unsubscribed instruments are ignored.
    let other = mqtt_relay::encode_publish("prod/marketfeed/mw/v1/nsefo/1", &mwbo(100));
    assert!(f.parse(&Message::Binary(other)).is_empty());
    let un = f.unsubscribe_frames(std::slice::from_ref(&s));
    let Message::Text(u) = &un[0] else { panic!() };
    assert!(u.contains("\"unsub\"") && u.contains("oi/v1/nsefo/40001"));
    assert!(f.parse(&Message::Binary(mw)).is_empty());
    assert_eq!(
        f.parse(&Message::Text(crate::brokers::common::relay::READY.into())),
        vec![FeedEvent::AuthOk]
    );
    assert!(IiflFeed::new(
        MqttEndpoint::plain("h", 1),
        "not-a-jwt",
        SymbolResolver::new()
    )
    .is_err());
}

#[test]
fn credentials_follow_bridgepy() {
    assert_eq!(streaming::jwt_username(JWT).as_deref(), Some("CL1"));
    assert_eq!(streaming::jwt_username("a.b"), None);
    let id = streaming::client_id("openalgo");
    assert!(id.starts_with("openalgo"));
    // %d%m%y%H%M%S%f (6+6+6 digits) + 8 hex.
    assert_eq!(id.len(), "openalgo".len() + 18 + 8);
    assert_ne!(id, streaming::client_id("openalgo"));
}

#[test]
fn order_and_trade_packets_normalise() {
    let r = resolver();
    let u = streaming::order_packet(&fx(fixture!("order_update.json")), &r);
    assert_eq!(u.orderid, "260403000000102");
    assert_eq!(
        (u.symbol.as_str(), u.exchange.as_str()),
        ("NIFTY30APR2624000CE", "NFO")
    );
    assert_eq!(
        (u.order_status.as_str(), u.action.as_str()),
        ("trigger pending", "SELL")
    );
    assert_eq!((u.pricetype.as_str(), u.product.as_str()), ("SL", "NRML"));
    assert_eq!(
        (u.quantity, u.pending_quantity, u.trigger_price),
        (75, 75, 121.0)
    );
    assert_eq!(u.rejection_reason, "");
    let rj = streaming::order_packet(&fx(fixture!("order_update_rejected.json")), &r);
    assert_eq!(rj.order_status, "rejected");
    assert_eq!(rj.rejection_reason, "RMS: margin exceeds");
    let t = streaming::trade_packet(&fx(fixture!("trade_update.json")), &r);
    assert_eq!((t.symbol.as_str(), t.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!(
        (t.action.as_str(), t.order_status.as_str()),
        ("BUY", "complete")
    );
    assert_eq!(
        (t.filled_quantity, t.average_price, t.product.as_str()),
        (10, 812.35, "MIS")
    );
    // Unknown instrument: raw trading symbol, no exchange.
    let raw = streaming::order_packet(&json!({"instrumentId":"1","tradingSymbol":"ABC"}), &r);
    assert_eq!(
        (
            raw.symbol.as_str(),
            raw.exchange.as_str(),
            raw.order_status.as_str()
        ),
        ("ABC", "", "open")
    );
}

// ---------------------------------------------------------------------------
// In-process MQTT 3.1.1 broker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Connect {
    pub client_id: String,
    pub username: String,
    pub password: String,
    pub keepalive: u16,
    pub clean: bool,
}

#[derive(Default)]
pub struct FakeMqtt {
    pub password: String,
    pub connects: Mutex<Vec<Connect>>,
    pub subscribed: Mutex<Vec<String>>,
    pub unsubscribed: Mutex<Vec<String>>,
    pub pings: Mutex<usize>,
    /// Published (in this order) once their topic is subscribed.
    pub on_subscribe: Mutex<Vec<(String, Vec<u8>)>>,
}

async fn read_packet(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let first = s.read_u8().await.ok()?;
    let mut len = 0usize;
    let mut mul = 1usize;
    loop {
        let b = s.read_u8().await.ok()?;
        len += (b as usize & 0x7f) * mul;
        if b & 0x80 == 0 {
            break;
        }
        mul *= 128;
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

fn mqtt_str(b: &[u8], at: &mut usize) -> String {
    let n = u16::from_be_bytes([b[*at], b[*at + 1]]) as usize;
    let s = String::from_utf8_lossy(&b[*at + 2..*at + 2 + n]).to_string();
    *at += 2 + n;
    s
}

fn remaining(n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut x = n;
    loop {
        let mut b = (x % 128) as u8;
        x /= 128;
        if x > 0 {
            b |= 0x80;
        }
        out.push(b);
        if x == 0 {
            break;
        }
    }
    out
}

fn publish_packet(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload);
    let mut out = vec![0x30];
    out.extend(remaining(body.len()));
    out.extend(body);
    out
}

async fn mqtt_session(mut s: TcpStream, fake: Arc<FakeMqtt>) {
    while let Some((first, body)) = read_packet(&mut s).await {
        match first & 0xF0 {
            0x10 => {
                let mut at = 0;
                let _proto = mqtt_str(&body, &mut at);
                let _level = body[at];
                let flags = body[at + 1];
                let keepalive = u16::from_be_bytes([body[at + 2], body[at + 3]]);
                at += 4;
                let client_id = mqtt_str(&body, &mut at);
                let username = if flags & 0x80 != 0 {
                    mqtt_str(&body, &mut at)
                } else {
                    String::new()
                };
                let password = if flags & 0x40 != 0 {
                    mqtt_str(&body, &mut at)
                } else {
                    String::new()
                };
                let accepted = password == fake.password;
                fake.connects.lock().push(Connect {
                    client_id,
                    username,
                    password,
                    keepalive,
                    clean: flags & 0x02 != 0,
                });
                let _ = s
                    .write_all(&[0x20, 0x02, 0x00, if accepted { 0 } else { 4 }])
                    .await;
                if !accepted {
                    return;
                }
            }
            0x80 => {
                let pid = [body[0], body[1]];
                let mut at = 2;
                let mut topics = Vec::new();
                while at < body.len() {
                    topics.push(mqtt_str(&body, &mut at));
                    at += 1;
                }
                let mut ack = vec![0x90, (2 + topics.len()) as u8, pid[0], pid[1]];
                ack.extend(std::iter::repeat_n(0u8, topics.len()));
                let _ = s.write_all(&ack).await;
                fake.subscribed.lock().extend(topics.iter().cloned());
                let pubs: Vec<(String, Vec<u8>)> = fake
                    .on_subscribe
                    .lock()
                    .iter()
                    .filter(|(t, _)| topics.contains(t))
                    .cloned()
                    .collect();
                for (t, p) in pubs {
                    let _ = s.write_all(&publish_packet(&t, &p)).await;
                }
            }
            0xA0 => {
                let pid = [body[0], body[1]];
                let mut at = 2;
                while at < body.len() {
                    let t = mqtt_str(&body, &mut at);
                    fake.unsubscribed.lock().push(t);
                }
                let _ = s.write_all(&[0xB0, 0x02, pid[0], pid[1]]).await;
            }
            0xC0 => {
                *fake.pings.lock() += 1;
                let _ = s.write_all(&[0xD0, 0x00]).await;
            }
            0xE0 => return,
            _ => {}
        }
    }
}

pub async fn fake_mqtt(fake: Arc<FakeMqtt>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(mqtt_session(s, fake.clone()));
        }
    });
    port
}

async fn next_frame<S>(ws: &mut S) -> Message
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("frame in time")
        .expect("stream open")
        .expect("frame")
}

async fn wait_for(cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not met in time");
}

#[tokio::test]
async fn relay_drives_the_market_feed_end_to_end() {
    let fake = Arc::new(FakeMqtt {
        password: format!("OPENID~~{}~", JWT),
        ..Default::default()
    });
    fake.on_subscribe.lock().extend([
        (
            "prod/marketfeed/oi/v1/nsefo/40001".to_string(),
            oi_packet(777),
        ),
        ("prod/marketfeed/mw/v1/nsefo/40001".to_string(), mwbo(100)),
    ]);
    let port = fake_mqtt(fake.clone()).await;
    let mut feed = IiflFeed::new(
        MqttEndpoint::plain("127.0.0.1", port),
        JWT,
        SymbolResolver::new(),
    )
    .unwrap();
    let req = feed.ws_request().unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(
        feed.parse(&next_frame(&mut ws).await),
        vec![FeedEvent::AuthOk]
    );
    {
        let c = fake.connects.lock();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].username, "CL1");
        assert_eq!(c[0].password, format!("OPENID~~{}~", JWT));
        assert_eq!(c[0].keepalive, 20);
        assert!(c[0].clean);
        assert!(c[0].client_id.starts_with("openalgo"));
    }
    feed.on_connected();
    let s = sub(
        "NIFTY30APR2624000CE",
        "NFO",
        "NSEFO",
        "40001",
        FeedMode::Depth,
    );
    for f in feed.subscribe_frames(std::slice::from_ref(&s)) {
        ws.send(f).await.unwrap();
    }
    let mut got = Vec::new();
    while got.len() < 2 {
        got.extend(feed.parse(&next_frame(&mut ws).await));
    }
    let FeedEvent::Tick(t) = &got[0] else {
        panic!("{:?}", got)
    };
    assert_eq!(
        (t.symbol.as_str(), t.ltp, t.oi),
        ("NIFTY30APR2624000CE", 812.35, 777)
    );
    assert!(matches!(got[1], FeedEvent::Depth(_)));
    for f in feed.unsubscribe_frames(std::slice::from_ref(&s)) {
        ws.send(f).await.unwrap();
    }
    let f2 = fake.clone();
    wait_for(move || f2.unsubscribed.lock().len() == 2).await;
    // A second manager connection replaces the first session with a fresh
    // client id.
    let (mut ws2, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    assert_eq!(
        feed.parse(&next_frame(&mut ws2).await),
        vec![FeedEvent::AuthOk]
    );
    let c = fake.connects.lock().clone();
    assert_eq!(c.len(), 2);
    assert_ne!(c[0].client_id, c[1].client_id);
}

#[tokio::test]
async fn refused_login_reaches_the_feed_as_auth_failure() {
    let fake = Arc::new(FakeMqtt {
        password: "something else".into(),
        ..Default::default()
    });
    let port = fake_mqtt(fake.clone()).await;
    let mut feed = IiflFeed::new(
        MqttEndpoint::plain("127.0.0.1", port),
        JWT,
        SymbolResolver::new(),
    )
    .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    match feed.parse(&next_frame(&mut ws).await).as_slice() {
        [FeedEvent::AuthFailed(m)] => assert!(m.contains("Log in")),
        other => panic!("{:?}", other),
    }
}

#[tokio::test]
async fn unreachable_bridge_closes_without_ready() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let mut feed = IiflFeed::new(
        MqttEndpoint::plain("127.0.0.1", port),
        JWT,
        SymbolResolver::new(),
    )
    .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap();
    assert!(matches!(
        m,
        None | Some(Ok(Message::Close(_))) | Some(Err(_))
    ));
    let _ = feed.parse(&Message::Close(None));
}

#[tokio::test]
async fn order_feed_subscribes_client_topics_and_emits_updates() {
    // Client id from GET /profile (no user id on the session).
    let app = axum::Router::new().route(
        "/profile",
        axum::routing::get(|| async {
            axum::Json(json!({"status":"Ok","result":{"clientId":"778"}}))
        }),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    let fake = Arc::new(FakeMqtt {
        password: format!("OPENID~~{}~", JWT),
        ..Default::default()
    });
    fake.on_subscribe.lock().extend([
        (
            "prod/updates/order/v1/778".to_string(),
            fixture!("order_update.json").as_bytes().to_vec(),
        ),
        (
            "prod/updates/trade/v1/778".to_string(),
            fixture!("trade_update.json").as_bytes().to_vec(),
        ),
    ]);
    let port = fake_mqtt(fake.clone()).await;
    let mut feed = IiflOrderFeed::new(
        MqttEndpoint::plain("127.0.0.1", port),
        JWT,
        None,
        base,
        resolver(),
    )
    .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    let mut got = Vec::new();
    while got.len() < 3 {
        got.extend(feed.parse(&next_frame(&mut ws).await));
    }
    assert_eq!(got[0], FeedEvent::AuthOk);
    let FeedEvent::OrderUpdate(o) = &got[1] else {
        panic!("{:?}", got)
    };
    assert_eq!(o.order_status, "trigger pending");
    let FeedEvent::OrderUpdate(t) = &got[2] else {
        panic!("{:?}", got)
    };
    assert_eq!(
        (t.symbol.as_str(), t.order_status.as_str()),
        ("SBIN", "complete")
    );
    assert_eq!(
        fake.subscribed.lock().clone(),
        ["prod/updates/order/v1/778", "prod/updates/trade/v1/778"]
    );
    assert!(fake.connects.lock()[0]
        .client_id
        .starts_with("openalgo-orderupdate"));
    assert!(feed.subscribe_frames(&[]).is_empty());
}

#[tokio::test]
async fn dropping_the_feed_stops_the_relay() {
    let fake = Arc::new(FakeMqtt {
        password: format!("OPENID~~{}~", JWT),
        ..Default::default()
    });
    let port = fake_mqtt(fake).await;
    let feed = IiflFeed::new(
        MqttEndpoint::plain("127.0.0.1", port),
        JWT,
        SymbolResolver::new(),
    )
    .unwrap();
    let url = feed.ws_request().unwrap().uri().to_string();
    drop(feed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

#[test]
fn create_feed_requires_a_usable_session() {
    let b = IiflCapitalBroker::new(SymbolResolver::new());
    assert!(b.create_feed(&AuthToken::new("")).is_err());
    assert!(b.create_feed(&AuthToken::new("opaque")).is_err());
    assert!(b
        .order_socket(&AuthToken::new(JWT).with_user_id("778"))
        .is_ok());
}
