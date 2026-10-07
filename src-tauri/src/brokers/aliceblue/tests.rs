//! Unit tests for the AliceBlue mappings, payloads, master contract,
//! history maths and socket frame decoding. Payloads are built from the
//! web code (`broker/aliceblue/**`) and AliceBlue's published V2 docs; no
//! account data.

use super::data::{self, candles_from, history_exchange, history_window, resample};
use super::mapping::{self, *};
use super::master_contract::{self, index_symbol, parse_all, parse_exchange_csv, FILES};
use super::orders::{means_empty, Book};
use super::streaming::{self, *};
use super::{auth, funds, WindowLimiter};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::upstox::relay::{Session, READY};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::time::Duration;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/aliceblue/", $name))
    };
}

fn j(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn sym(symbol: &str, brsymbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn resolver() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        sym("SBIN", "SBIN-EQ", "NSE", "3045"),
        sym("INFY", "INFY-EQ", "NSE", "1594"),
        sym("NIFTYBEES", "NIFTYBEES-EQ", "NSE", "10576"),
        sym("TATAMOTORS", "TATAMOTORS", "BSE", "500570"),
        sym("NIFTY28OCT2625000CE", "NIFTY28OCT2625000CE", "NFO", "54957"),
        sym("NIFTY", "NIFTY 50", "NSE_INDEX", "26000"),
    ]);
    r
}

fn resolved(pricetype: PriceType, price: f64, trigger: f64) -> ResolvedOrder {
    ResolvedOrder {
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        quantity: 10,
        price,
        trigger_price: trigger,
        pricetype,
        product: Product::Cnc,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: SymToken {
            token: "3045.0".into(),
            ..sym("SBIN", "SBIN-EQ", "NSE", "3045")
        },
    }
}

// ---------------------------------------------------------------- auth

#[test]
fn checksum_is_sha256_of_user_code_secret() {
    // sha256("AB123" + "ac9" + "sec") per web auth_api.py:46-49.
    let want = auth::sha256_hex("AB123ac9sec");
    assert_eq!(auth::checksum("AB123", "ac9", "sec"), want);
    assert_eq!(want.len(), 64);
    assert_eq!(
        auth::sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        auth::split_code("AB123:ac:9"),
        Some(("AB123".into(), "ac:9".into()))
    );
    assert_eq!(auth::split_code("nocolon"), None);
    assert_eq!(auth::split_code(":x"), None);
}

#[test]
fn ucc_comes_from_user_id_then_jwt_claim() {
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"ucc": "<USER_ID>", "sub": "x"}).to_string());
    let jwt = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", payload);
    assert_eq!(auth::ucc_from_jwt(&jwt).as_deref(), Some("<USER_ID>"));
    assert_eq!(
        auth::ucc(&AuthToken::new(&jwt)).as_deref(),
        Some("<USER_ID>")
    );
    assert_eq!(
        auth::ucc(&AuthToken::new(&jwt).with_user_id("AB1")).as_deref(),
        Some("AB1")
    );
    assert_eq!(auth::ucc_from_jwt("not-a-jwt"), None);
}

// ------------------------------------------------------------ mappings

#[test]
fn product_and_order_type_maps_match_web() {
    assert_eq!(map_product(Product::Cnc), "LONGTERM");
    assert_eq!(map_product(Product::Nrml), "NRML");
    assert_eq!(map_product(Product::Mis), "INTRADAY");
    for (b, oa) in [
        ("LONGTERM", "CNC"),
        ("MTF", "CNC"),
        ("DELIVERY", "CNC"),
        ("CNC", "CNC"),
        ("NRML", "NRML"),
        ("INTRADAY", "MIS"),
        ("MIS", "MIS"),
        ("WHATEVER", "MIS"),
    ] {
        assert_eq!(reverse_product(b), oa, "{}", b);
    }
    assert_eq!(map_order_type(PriceType::SlM), "SLM");
    assert_eq!(map_order_type(PriceType::Sl), "SL");
    assert_eq!(reverse_order_type("SLM"), "SL-M");
    assert_eq!(reverse_order_type("MARKET"), "MARKET");
    assert_eq!(reverse_order_type("LIMIT"), "LIMIT");
    assert_eq!(reverse_order_type("BO"), "UNKNOWN");
}

#[test]
fn place_payload_matches_web_transform_data() {
    let p = place_payload(&resolved(PriceType::Limit, 812.5, 0.0));
    assert_eq!(
        p,
        json!({
            "exchange": "NSE",
            "instrumentId": "3045",
            "transactionType": "BUY",
            "quantity": 10,
            "product": "LONGTERM",
            "orderComplexity": "REGULAR",
            "orderType": "LIMIT",
            "validity": "DAY",
            "price": "812.5",
            "slLegPrice": "",
            "targetLegPrice": "",
            "slTriggerPrice": "0.0",
            "disclosedQuantity": "0",
            "marketProtectionPercent": "",
            "deviceId": "",
            "trailingSlAmount": "",
            "apiOrderSource": "",
            "algoId": "",
            "orderTag": "openalgo",
        })
    );
    let p = place_payload(&resolved(PriceType::SlM, 0.0, 800.0));
    assert_eq!(p["orderType"], "SLM");
    assert_eq!(p["price"], "0.0");
    assert_eq!(p["slTriggerPrice"], "800.0");
}

#[test]
fn modify_payload_matches_web() {
    let m = ResolvedModify {
        order_id: "25100300000103".into(),
        symbol: "INFY".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        product: Product::Cnc,
        pricetype: PriceType::Sl,
        quantity: 5,
        price: 1501.25,
        trigger_price: 1500.0,
        disclosed_quantity: 0,
        instrument: sym("INFY", "INFY-EQ", "NSE", "1594"),
    };
    assert_eq!(
        modify_payload(&m),
        json!({
            "brokerOrderId": "25100300000103",
            "quantity": 5,
            "orderType": "SL",
            "slTriggerPrice": "1500.0",
            "price": "1501.25",
            "slLegPrice": "",
            "trailingSlAmount": "",
            "targetLegPrice": "",
            "validity": "DAY",
            "disclosedQuantity": "0",
            "marketProtection": "",
            "deviceId": "",
        })
    );
}

#[test]
fn python_number_text() {
    assert_eq!(py_float_str(0.0), "0.0");
    assert_eq!(py_float_str(100.0), "100.0");
    assert_eq!(py_float_str(100.55), "100.55");
    assert_eq!(normalize_token("2885.0"), "2885");
    assert_eq!(normalize_token(" 54957 "), "54957");
    assert_eq!(normalize_token("abc"), "abc");
    assert_eq!(num(Some(&json!("1,234.5"))), 1234.5);
    assert_eq!(int(Some(&json!("75.9"))), 75);
    assert_eq!(num(None), 0.0);
}

#[test]
fn order_book_rows_are_openalgo_symbols_and_lowercase_statuses() {
    let r = resolver();
    let v = j(fixture!("order_book.json"));
    let rows: Vec<Order> = v["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| order_from(o, &r))
        .collect();
    assert_eq!(rows[0].symbol, "SBIN");
    assert_eq!(rows[0].status, "complete");
    assert_eq!(rows[0].order_type, "MARKET");
    assert_eq!(rows[0].product, "MIS");
    assert_eq!(rows[0].average_price, 812.35);
    assert_eq!(
        rows[0].exchange_order_id.as_deref(),
        Some("1100000012345678")
    );
    // formattedInstrumentName misses the master; tradingSymbol hits.
    assert_eq!(rows[1].symbol, "NIFTY28OCT2625000CE");
    assert_eq!(rows[1].status, "trigger pending");
    assert_eq!(rows[1].side, "SELL");
    assert_eq!(rows[1].trigger_price, 121.0);
    assert_eq!(rows[1].pending_quantity, 75);
    assert_eq!(rows[2].product, "CNC");
    assert_eq!(rows[2].status, "open");
    assert_eq!(rows[3].order_type, "SL-M");
    assert_eq!(rows[3].product, "CNC");
    assert_eq!(rows[3].status, "rejected");
    assert_eq!(
        rows[3].rejection_reason.as_deref(),
        Some("RMS: Margin Exceeds")
    );
}

#[test]
fn unknown_broker_symbol_falls_back_to_token_then_raw() {
    let r = resolver();
    let row = json!({"exchange":"NSE","tradingSymbol":"SBIN-XX","instrumentId":"3045"});
    assert_eq!(order_from(&row, &r).symbol, "SBIN");
    let row = json!({"exchange":"NSE","tradingSymbol":"ZZZ-EQ","instrumentId":"1"});
    assert_eq!(order_from(&row, &r).symbol, "ZZZ-EQ");
}

#[test]
fn trade_book_rows() {
    let r = resolver();
    let v = j(fixture!("trade_book.json"));
    let t: Vec<Trade> = v["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| trade_from(x, &r))
        .collect();
    assert_eq!(t[0].symbol, "SBIN");
    assert_eq!(t[0].timestamp, "2026-10-03 09:16:02");
    assert_eq!(t[0].trade_value, 8123.5);
    assert_eq!(t[0].trade_id, "T5550001");
    assert_eq!(t[1].symbol, "NIFTY28OCT2625000CE");
    assert_eq!(t[1].quantity, 75);
    assert!(t[1].timestamp.ends_with(" 09:31:45"));
    assert_eq!(t[1].timestamp.len(), 19);
    assert_eq!(fill_time("odd"), "odd");
    assert_eq!(fill_time(""), "");
}

#[test]
fn position_rows_follow_web_pnl_rules() {
    let r = resolver();
    let v = j(fixture!("positions.json"));
    let p: Vec<Position> = v["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| position_from(x, &r))
        .collect();
    // Long: (ltp - buy avg) * qty.
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].product, "MIS");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].average_price, 812.35);
    assert_eq!(p[0].pnl, 26.5);
    // Short with a broker unrealised P&L: the broker figure wins.
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].average_price, 118.2);
    assert_eq!(p[1].pnl, 615.0);
    assert_eq!(p[1].product, "NRML");
    // Flat: average 0, realised kept.
    assert_eq!(p[2].quantity, 0);
    assert_eq!(p[2].average_price, 0.0);
    assert_eq!(p[2].realized_pnl, 50.0);
    assert_eq!(p[2].product, "CNC");
}

#[test]
fn holding_rows() {
    let r = resolver();
    let v = j(fixture!("holdings.json"));
    let h: Vec<Holding> = v["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| holding_from(x, &r))
        .collect();
    assert_eq!(h.len(), 2, "row without a symbol is skipped");
    assert_eq!(h[0].symbol, "NIFTYBEES");
    assert_eq!(h[0].exchange, "NSE");
    assert_eq!(h[0].quantity, 100);
    assert_eq!(h[0].pnl, 2050.0);
    assert_eq!(h[0].pnl_percentage, 8.2);
    assert_eq!(h[0].isin.as_deref(), Some("INF204KB14I2"));
    // BSE-only, unsettled quantity, investedPrice fallback.
    assert_eq!(h[1].symbol, "TATAMOTORS");
    assert_eq!(h[1].exchange, "BSE");
    assert_eq!(h[1].quantity, 4);
    assert_eq!(h[1].average_price, 650.0);
    assert_eq!(h[1].pnl, 200.0);
}

#[test]
fn funds_mapping() {
    let v = j(fixture!("limits.json"));
    let f = funds::funds_from_limits(&v["result"][0], 50.0);
    assert_eq!(f.available_cash, 125000.46);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.utilised_debits, 1234.5);
    assert_eq!(f.m2m_realized, 50.0);
    assert_eq!(f.m2m_unrealized, 0.0);
    let p = j(fixture!("positions.json"));
    assert_eq!(funds::realized_from_positions(&p), 50.0);
    assert_eq!(
        funds::realized_from_positions(&json!({"status":"Not_Ok"})),
        0.0
    );
}

#[test]
fn empty_book_semantics() {
    assert!(means_empty(Book::Orders, "EC916"));
    assert!(means_empty(Book::Orders, "Failed to retrieve order book"));
    assert!(!means_empty(Book::Orders, "EC003"));
    assert!(means_empty(Book::Trades, "No trades found"));
    assert!(means_empty(Book::Positions, "EC919"));
    assert!(means_empty(Book::Positions, "EC920"));
    // The smart-order read: EC920 is empty, EC919 is a failed read.
    assert!(means_empty(Book::PositionsStrict, "EC920"));
    assert!(!means_empty(Book::PositionsStrict, "EC919"));
    assert!(!means_empty(
        Book::PositionsStrict,
        "Failed to retrieve the position book"
    ));
    assert!(means_empty(Book::Holdings, "EC922"));
}

#[test]
fn error_codes_are_expanded() {
    assert_eq!(describe("EC912"), "EC912: Failed to place the order.");
    assert_eq!(
        describe("Rejected: EC904"),
        "Rejected: EC904: 'quantity' should be a positive number."
    );
    assert_eq!(describe("EC000 unknown"), "EC000 unknown");
    assert_eq!(describe("XEC912"), "XEC912");
    assert_eq!(describe("plain"), "plain");
    assert_eq!(ERROR_CODES.len(), 133);
}

#[test]
fn order_feed_frames() {
    let r = resolver();
    let v = json!({"t":"om","norenordno":"25100300000101","tsym":"SBIN-EQ","exch":"NSE",
        "trantype":"B","qty":"10","prc":"0","trgprc":"0","prctyp":"MKT","pcode":"MIS",
        "status":"COMPLETE","fillshares":"4","avgprc":"812.35"});
    let u = order_update_from(&v, &r).unwrap();
    assert_eq!(u.symbol, "SBIN");
    assert_eq!(u.action, "BUY");
    assert_eq!(u.pricetype, "MARKET");
    assert_eq!(u.order_status, "complete");
    assert_eq!(u.pending_quantity, 6);
    assert_eq!(u.average_price, 812.35);
    assert_eq!(u.rejection_reason, "");
    let rej = json!({"t":"om","status":"REJECTED","rejreason":"margin","tsym":"X","exch":"NSE"});
    assert_eq!(
        order_update_from(&rej, &r).unwrap().rejection_reason,
        "margin"
    );
    assert!(order_update_from(&json!({"t":"ok"}), &r).is_none());
    for (raw, want) in [
        ("New", "open"),
        ("REPLACED", "open"),
        ("trigger_pending", "trigger pending"),
        ("Canceled", "cancelled"),
        ("", "open"),
        ("Odd State", "odd state"),
    ] {
        assert_eq!(order_feed_status(raw), want);
    }
}

// ------------------------------------------------------ master contract

#[test]
fn master_equities() {
    let nse = parse_exchange_csv("NSE", fixture!("NSE.csv"));
    let syms: Vec<&str> = nse.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["SBIN", "INFY", "NIFTYBEES", "IDEA"]);
    assert_eq!(nse[0].brsymbol, "SBIN-EQ");
    assert_eq!(nse[0].token, "3045");
    assert_eq!(nse[0].strike, 1.0);
    assert_eq!(nse[0].instrument_type, "EQ");
    assert_eq!(nse[0].brexchange, "NSE");
    assert_eq!(nse[0].name, "STATE BANK OF INDIA");
    let bse = parse_exchange_csv("BSE", fixture!("BSE.csv"));
    assert_eq!(bse.len(), 1);
    assert_eq!(bse[0].name, "TATA MOTORS, LTD");
}

#[test]
fn master_derivatives() {
    let nfo = parse_exchange_csv("NFO", fixture!("NFO.csv"));
    let syms: Vec<&str> = nfo.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(
        syms,
        [
            "NIFTY28OCT2625000CE",
            "NIFTY28OCT2624987.5PE",
            "BANKNIFTY26OCTFUT"
        ]
    );
    assert_eq!(nfo[0].expiry, "28-OCT-26");
    assert_eq!(nfo[0].strike, 25000.0);
    assert_eq!(nfo[0].lot_size, 75);
    assert_eq!(nfo[0].instrument_type, "CE");
    assert_eq!(nfo[2].instrument_type, "FUT");
    assert_eq!(nfo[2].brsymbol, "BANKNIFTY26OCTF");

    let cds = parse_exchange_csv("CDS", fixture!("CDS.csv"));
    assert_eq!(cds[0].symbol, "USDINR26OCTFUT");
    assert_eq!(cds[1].symbol, "USDINR29OCT2683.25CE");

    let mcx = parse_exchange_csv("MCX", fixture!("MCX.csv"));
    let syms: Vec<&str> = mcx.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["CRUDEOIL26NOVFUT", "GOLD25NOV2675000PE"]);
    assert_eq!(mcx[0].lot_size, 100);

    let bfo = parse_exchange_csv("BFO", fixture!("BFO.csv"));
    let syms: Vec<&str> = bfo.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["SENSEX30OCT26FUT", "SENSEX30OCT2682000CE"]);

    let bcd = parse_exchange_csv("BCD", fixture!("BCD.csv"));
    assert_eq!(bcd[0].symbol, "EURINR26OCTFUT");
    assert_eq!(bcd[0].strike, 1.0);
}

#[test]
fn master_indices() {
    let idx = master_contract::parse_indices_csv(fixture!("INDICES.csv"));
    let got: Vec<(&str, &str)> = idx
        .iter()
        .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("NIFTY", "NSE_INDEX"),
            ("BANKNIFTY", "NSE_INDEX"),
            ("NIFTYIT", "NSE_INDEX"),
            ("SENSEX", "BSE_INDEX"),
            ("BSECONSUMERDURABLES", "BSE_INDEX"),
            ("BSEALLCAP", "BSE_INDEX"),
            ("SENSEX50", "BSE_INDEX"),
            ("MCXCOMDEX", "MCX_INDEX"),
        ]
    );
    assert_eq!(idx[0].brsymbol, "NIFTY 50");
    assert_eq!(idx[0].brexchange, "NSE");
    assert_eq!(idx[0].instrument_type, "NSE_INDEX");
    assert_eq!(idx[0].tick_size, 0.01);
    assert_eq!(
        index_symbol("NIFTY MIDCAP SELECT", "NSE_INDEX"),
        "MIDCPNIFTY"
    );
    assert_eq!(index_symbol("BSEIPO", "BSE_INDEX"), "BSEIPO");
}

#[test]
fn master_all_files_in_web_order() {
    let files: Vec<(&str, String)> = FILES
        .iter()
        .map(|f| {
            let body = match *f {
                "NSE" => fixture!("NSE.csv"),
                "BSE" => fixture!("BSE.csv"),
                "NFO" => fixture!("NFO.csv"),
                "CDS" => fixture!("CDS.csv"),
                "MCX" => fixture!("MCX.csv"),
                "BFO" => fixture!("BFO.csv"),
                "BCD" => fixture!("BCD.csv"),
                _ => fixture!("INDICES.csv"),
            };
            (*f, body.to_string())
        })
        .collect();
    let rows = parse_all(&files);
    assert_eq!(rows.len(), 4 + 1 + 3 + 2 + 2 + 2 + 1 + 8);
    assert!(rows
        .iter()
        .all(|r| r.expiry.is_empty() || r.expiry.len() == 9));
    assert_eq!(master_contract::clean_token("2885.0"), "2885");
    assert_eq!(master_contract::clean_token("x"), "");
    assert_eq!(
        master_contract::parse_expiry("10/28/2026"),
        NaiveDate::from_ymd_opt(2026, 10, 28)
    );
}

// --------------------------------------------------------------- history

#[test]
fn history_windows() {
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    let now = 4_102_444_800_000; // 2100-01-01
                                 // Daily: 00:00 IST start, end 23:59:59 IST rounded up to the next UTC
                                 // midnight.
    let (f, t) = history_window(d(2026, 10, 1), d(2026, 10, 1), true, now).unwrap();
    assert_eq!(f, 1_790_793_000_000); // 2026-10-01 00:00 IST
    assert_eq!(t, 1_790_899_200_000); // 2026-10-02 00:00 UTC
                                      // Intraday: 09:15 IST start, 23:59:59 IST end.
    let (f, t) = history_window(d(2026, 10, 1), d(2026, 10, 1), false, now).unwrap();
    assert_eq!(f, 1_790_826_300_000);
    assert_eq!(t, 1_790_879_399_000);
    // End capped at now; then the one-hour minimum.
    let (f, t) = history_window(d(2026, 10, 1), d(2026, 10, 5), false, 1_790_826_400_000).unwrap();
    assert_eq!(t - f, 3_600_000);
    // Start in the future: nothing.
    assert!(history_window(d(2026, 10, 1), d(2026, 10, 1), false, 0).is_none());
    assert_eq!(history_exchange("NSE_INDEX"), "NSE::index");
    assert_eq!(history_exchange("BSE_INDEX"), "BSE::index");
    assert_eq!(history_exchange("NFO"), "NFO");
}

#[test]
fn history_candles_and_resampling() {
    let v = j(fixture!("history_1m.json"));
    let c = candles_from(v["result"].as_array().unwrap(), false);
    // Floored to the minute, IST, sorted, duplicates dropped (first wins
    // after a stable sort).
    assert_eq!(c.len(), 4);
    assert_eq!(c[0].timestamp, 1_790_826_300); // 09:15 IST
    assert_eq!(c[1].close, 802.0);
    assert!(c.iter().all(|x| x.oi == 0));
    let r5 = resample(&c, 5);
    assert_eq!(r5.len(), 2);
    assert_eq!(r5[0].timestamp, 1_790_826_300);
    assert_eq!(
        (r5[0].open, r5[0].high, r5[0].low, r5[0].close, r5[0].volume),
        (800.0, 803.0, 798.0, 799.0, 2200)
    );
    assert_eq!(r5[1].timestamp, 1_790_826_600); // 09:20
                                                // 1h buckets start on the IST hour (09:00).
    let h = resample(&c, 60);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].timestamp, 1_790_825_400);
    let d = j(fixture!("history_day.json"));
    let dc = candles_from(d["result"].as_array().unwrap(), true);
    assert_eq!(dc[0].timestamp, 1_790_706_600); // 2026-09-30 00:00 IST
    assert_eq!(dc[1].volume, 98765);
    assert_eq!(data::resample_minutes("1h"), Some(60));
    assert_eq!(data::resample_minutes("1m"), None);
    assert_eq!(data::quote_key("NSE_INDEX", "26000.0"), "NSE|26000");
}

// ---------------------------------------------------------------- feeds

#[test]
fn feed_login_and_subscription_frames() {
    let f = j(&connect_frame("jwt", "AB1"));
    assert_eq!(f["t"], "c");
    assert_eq!(f["actid"], "AB1_API");
    assert_eq!(f["uid"], "AB1_API");
    assert_eq!(f["source"], "API");
    assert_eq!(f["susertoken"], susertoken("jwt"));
    assert_eq!(
        susertoken("jwt"),
        auth::sha256_hex(&auth::sha256_hex("jwt"))
    );
    assert_eq!(
        j(&subscribe_frame(
            &["NSE|2885".to_string(), "NFO|54957".to_string()],
            false
        )),
        json!({"t":"t","k":"NSE|2885#NFO|54957"})
    );
    assert_eq!(
        j(&subscribe_frame(&["NSE|2885".to_string()], true)),
        json!({"t":"d","k":"NSE|2885"})
    );
    assert_eq!(
        j(&unsubscribe_frame(&["NSE|1".to_string()])),
        json!({"t":"u","k":"NSE|1"})
    );
    assert_eq!(j(&heartbeat_frame()), json!({"k":"","t":"h"}));
    assert_eq!(auth_ack(&json!({"t":"ck","s":"OK"})), Some(true));
    assert_eq!(auth_ack(&json!({"t":"ck","s":"NOT_OK"})), Some(false));
    assert_eq!(auth_ack(&json!({"t":"cf","k":"OK"})), Some(true));
    assert_eq!(auth_ack(&json!({"t":"tk"})), None);
    assert_eq!(feed_token("NSE_INDEX", "99926000"), "26000");
    assert_eq!(feed_token("NSE", "99926000"), "99926000");
    assert_eq!(ab_exchange("BSE_INDEX"), "BSE");
}

#[test]
fn quote_snapshot_merges_tf_onto_tk() {
    // Keys per web alicebluewebsocket.py:476-556.
    let mut q = QuoteSnap::default();
    q.apply(
        &json!({"t":"tk","e":"NSE","tk":"3045","lp":"812.35","o":"805","h":"815",
        "l":"801","c":"808.1","v":"120000","ltq":"5","ap":"810.2","oi":"0",
        "tbq":"5000","tsq":"6000","bp1":"812.3","sp1":"812.4","bq1":"10","sq1":"12"}),
    );
    assert!(q.full);
    assert_eq!(
        (q.ltp, q.close, q.volume, q.bid, q.ask),
        (812.35, 808.1, 120000, 812.3, 812.4)
    );
    q.apply(&json!({"t":"tf","e":"NSE","tk":"3045","lp":"813.0","v":"120500","o":"1"}));
    assert_eq!(q.ltp, 813.0);
    assert_eq!(q.volume, 120500);
    // tf does not touch open/high/low/close in the REST quote path.
    assert_eq!(q.open, 805.0);
    assert_eq!(q.bid, 812.3);
}

#[test]
fn depth_snapshot_merges_levels() {
    // Keys per web alicebluewebsocket.py:616-741.
    let mut d = DepthSnap::default();
    d.apply(
        &json!({"t":"dk","e":"NSE","tk":"3045","lp":"812","bp1":"811.9","bq1":"100",
        "bo1":"3","bp2":"811.8","bq2":"50","sp1":"812.1","sq1":"70","so1":"2","sp2":"0",
        "tbq":"9000","tsq":"8000","c":"808"}),
    );
    assert_eq!(d.bids.len(), 2);
    assert_eq!(d.asks.len(), 1, "levels without a price are dropped");
    assert_eq!(d.bids[0].orders, 3);
    d.apply(&json!({"t":"df","e":"NSE","tk":"3045","bq1":"150","sp2":"812.2","sq2":"9"}));
    assert_eq!(d.bids[0].price, 811.9);
    assert_eq!(d.bids[0].quantity, 150);
    assert_eq!(d.asks[1].price, 812.2);
    let key = QuoteKey::new("NSE", "SBIN");
    let md = data::depth_from_snap(&key, &d);
    assert_eq!(md.bids.len(), 5);
    assert_eq!(md.asks.len(), 5);
    assert_eq!(md.prev_close, 808.0);
    assert_eq!(md.total_buy_qty, 9000);
    assert_eq!(md.bids[4].price, 0.0);
    let q = data::quote_from_snap(
        &key,
        &QuoteSnap {
            ltp: 1.0,
            close: 2.0,
            ..Default::default()
        },
    );
    assert_eq!((q.ltp, q.close), (1.0, 2.0));
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

fn feed() -> AliceBlueFeed {
    AliceBlueFeed::new(
        crate::brokers::common::http::client(),
        super::Endpoints::default(),
        "jwt",
        "AB1",
    )
}

#[test]
fn live_feed_frames_and_retention() {
    let mut f = feed();
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("INFY", "NSE", "1594", FeedMode::Depth),
    ]);
    let texts: Vec<Value> = frames.iter().map(|m| j(m.to_text().unwrap())).collect();
    assert_eq!(
        texts,
        [
            json!({"t":"t","k":"NSE|3045#NSE|26000"}),
            json!({"t":"d","k":"NSE|1594"})
        ]
    );
    assert_eq!(f.subscription_count(), 3);
    // Control frames from the relay.
    assert_eq!(
        f.parse(&Message::Text(READY.into())),
        vec![FeedEvent::AuthOk]
    );
    // tk snapshot then a tf delta that sends 0 for unchanged prices.
    let ev = f.parse(&Message::Text(
        json!({"t":"tk","e":"NSE","tk":"3045","lp":"812.35","o":"805","h":"815","l":"801",
            "c":"808.1","v":"1000","pc":"0.53","ap":"810","toi":"0","ft":"1759290300"})
        .to_string(),
    ));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!((t.symbol.as_str(), t.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!(t.mode, 2);
    assert_eq!(t.close, 808.1);
    assert_eq!(t.change_percent, 0.53);
    assert_eq!(t.last_trade_time_ms, 1_759_290_300_000);
    let ev = f.parse(&Message::Text(
        json!({"t":"tf","e":"NSE","tk":"3045","lp":"813","o":"0","v":"1200"}).to_string(),
    ));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!((t.ltp, t.open, t.volume), (813.0, 805.0, 1200));
    // LTP subscription carries only the price.
    let ev = f.parse(&Message::Text(
        json!({"t":"tk","e":"NSE","tk":"26000","lp":"25000.5","o":"24900"}).to_string(),
    ));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("tick expected")
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("NIFTY", "NSE_INDEX", 1)
    );
    assert_eq!((t.ltp, t.open), (25000.5, 0.0));
    // Depth.
    let ev = f.parse(&Message::Text(
        json!({"t":"dk","e":"NSE","tk":"1594","lp":"1500","bp1":"1499.9","bq1":"10","bo1":"2",
            "sp1":"1500.1","sq1":"5","tbq":"100","tsq":"90"})
        .to_string(),
    ));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("depth expected")
    };
    assert_eq!(d.buy.len(), 1);
    assert_eq!(d.buy[0].orders, 0);
    assert_eq!(d.total_sell_quantity, 90);
    // Unknown instruments are ignored; unsubscribe cleans state.
    assert!(f
        .parse(&Message::Text(
            json!({"t":"tk","e":"NSE","tk":"1"}).to_string()
        ))
        .is_empty());
    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Quote)]);
    assert_eq!(j(un[0].to_text().unwrap()), json!({"t":"u","k":"NSE|3045"}));
    assert_eq!(f.subscription_count(), 2);
    assert!(f
        .parse(&Message::Text(
            json!({"t":"tf","e":"NSE","tk":"3045","lp":"1"}).to_string()
        ))
        .is_empty());
}

#[test]
fn relay_market_session_waits_for_login_answer() {
    let mut s = MarketSession::new("jwt", "AB1");
    let open = s.on_open();
    assert_eq!(j(open[0].to_text().unwrap())["t"], "c");
    assert!(!s.ready_on_open());
    let st = s.on_upstream(Message::Text(r#"{"t":"ck","s":"OK"}"#.into()));
    assert!(st.ready && st.down.is_empty());
    let st = s.on_upstream(Message::Text(r#"{"t":"ck","s":"Not_Ok"}"#.into()));
    assert!(st.auth_failed.is_some());
    let st = s.on_upstream(Message::Text(r#"{"t":"tf","e":"NSE","tk":"1"}"#.into()));
    assert_eq!(st.down.len(), 1);
    assert_eq!(s.keepalive().map(|(d, _)| d), Some(Duration::from_secs(30)));
    let o = OrderSession {
        subscribe: order_subscribe_frame("ot", "AB1"),
        heartbeat: order_heartbeat_frame("AB1"),
    };
    assert_eq!(j(&o.subscribe), json!({"orderToken":"ot","userId":"AB1"}));
    assert_eq!(j(&o.heartbeat), json!({"heartbeat":"h","userId":"AB1"}));
    assert_eq!(o.keepalive().map(|(d, _)| d), Some(Duration::from_secs(55)));
}

#[tokio::test(start_paused = true)]
async fn window_limiter_holds_the_budget() {
    let l = WindowLimiter::new(Duration::from_secs(10), 3);
    let start = tokio::time::Instant::now();
    for _ in 0..3 {
        l.acquire().await;
    }
    assert_eq!(start.elapsed(), Duration::ZERO);
    l.acquire().await;
    assert!(start.elapsed() >= Duration::from_secs(10));
    assert!(l.in_window().await <= 3);
}

#[test]
fn identity() {
    use crate::brokers::Broker;
    let b = super::AliceBlueBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "aliceblue");
    assert_eq!(b.timeframe_map().len(), 8);
    assert!(!b.capabilities().margin);
    assert!(b.capabilities().streaming);
    let _ = streaming::HEARTBEAT;
    let _ = mapping::s;
}
