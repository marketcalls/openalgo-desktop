//! Kotak mapping tests against payloads built from the web plugin and the
//! Kotak Neo API docs (`src-tauri/tests/fixtures/brokers/kotak/`). No account
//! data.

use super::auth::normalize_mobile;
use super::data::{
    depth_from_quote, earliest_start, history_chunk_days, history_timestamp, index_candidates,
    is_no_data_fault, kotak_segment, match_multiquotes, normalize_candles, quote_from_row,
    repair_candles,
};
use super::funds::{funds_from_limits, parse_margin, LIMITS_BODY};
use super::hsm::{self, HsmDecoder, HsmEvent, KotakHsmFeed};
use super::mapping::*;
use super::master_contract::{
    combine_details, expiry_from_epoch, fallback_urls, file_key, parse_file, parse_file_paths,
};
use super::streaming::{
    decode_packet, feed_url_from_config, parse_dividers, realtime_url, split_batch, to_wss,
    KotakFeed, KotakOrderFeed, Sfeed, DEFAULT_SFEED_URL,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/kotak/", $name))
    };
}

fn j(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn rows_of(key: &str) -> Vec<SymToken> {
    let csv = match key {
        "NSE_CM" => fixture!("nse_cm.csv"),
        "BSE_CM" => fixture!("bse_cm.csv"),
        "NSE_FO" => fixture!("nse_fo.csv"),
        "CDE_FO" => fixture!("cde_fo.csv"),
        "MCX_FO" => fixture!("mcx_fo.csv"),
        _ => fixture!("bse_fo.csv"),
    };
    parse_file(key, csv).unwrap()
}

use crate::brokers::common::symbols::SymToken;

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut all = Vec::new();
    for k in master_contract::SEGMENT_FILES {
        all.extend(rows_of(k));
    }
    r.load(all);
    r
}

fn session() -> AuthToken {
    AuthToken::new("tok:::sid-1:::https://cis.kotaksecurities.com/:::acc-1:::E43")
}

// ---------------------------------------------------------------------------
// Session and login
// ---------------------------------------------------------------------------

#[test]
fn session_token_is_the_web_composite() {
    let s = KotakSession::parse(&session()).unwrap();
    assert_eq!(
        (s.token.as_str(), s.sid.as_str(), s.base_url.as_str()),
        ("tok", "sid-1", "https://cis.kotaksecurities.com")
    );
    assert_eq!(
        (s.access_token.as_str(), s.data_center.as_str()),
        ("acc-1", "E43")
    );
    // Four-part tokens issued before the data centre was stored still parse.
    let four = KotakSession::parse(&AuthToken::new("t:::s:::https://h:::a")).unwrap();
    assert_eq!(four.data_center, "");
    // A missing base URL means log in again.
    assert_eq!(
        KotakSession::parse(&AuthToken::new("t:::s::::::a"))
            .unwrap_err()
            .code(),
        "AUTH_ERROR"
    );
    assert!(KotakSession::parse(&AuthToken::new("t:::s")).is_err());
    assert_eq!(
        s.compose(),
        "tok:::sid-1:::https://cis.kotaksecurities.com:::acc-1:::E43"
    );
    assert!(!format!("{:?}", s).contains("tok"));
}

#[test]
fn mobile_numbers_normalise_like_web() {
    assert_eq!(normalize_mobile("9876543210"), "+919876543210");
    assert_eq!(normalize_mobile("+91 98765 43210"), "+919876543210");
    assert_eq!(normalize_mobile("919876543210"), "+919876543210");
    assert_eq!(normalize_mobile(" +919876543210 "), "+919876543210");
}

#[test]
fn identity_and_login_kind() {
    let b = KotakBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "kotak");
    assert_eq!(
        b.login_kind(),
        LoginKind::TwoStep {
            step1: &["mobile", "totp"],
            step2: &["mpin"]
        }
    );
    assert!(b.requires_totp());
    assert!(b.capabilities().history);
    assert!(!b.capabilities().gtt);
    assert!(!b.supported_exchanges().contains(&Exchange::Bcd));
    assert_eq!(b.timeframe_map().len(), 10);
}

// ---------------------------------------------------------------------------
// Wire encoding
// ---------------------------------------------------------------------------

#[test]
fn jdata_is_percent_encoded_json() {
    assert_eq!(
        py_quote("nse_cm|11536,nse_cm|1594", "|,"),
        "nse_cm|11536,nse_cm|1594"
    );
    assert_eq!(py_quote("nse_cm|Nifty 50", "|,"), "nse_cm|Nifty%2050");
    assert_eq!(py_quote("a/b c", "/"), "a/b%20c");
    assert_eq!(
        jdata_body(&json!({"on": "1", "am": "NO"})),
        "jData=%7B%22am%22%3A%22NO%22%2C%22on%22%3A%221%22%7D"
    );
    // The funds body is the web's literal.
    assert_eq!(
        urlencoding::decode(LIMITS_BODY.trim_start_matches("jData=")).unwrap(),
        r#"{"seg":"ALL","exch":"ALL","prod":"ALL"}"#
    );
    assert_eq!(fmt_price(0.0), "0");
    assert_eq!(fmt_price(100.0), "100.0");
    assert_eq!(fmt_price(117.6), "117.6");
    assert_eq!(py_float(0.05), "0.05");
}

#[test]
fn error_bodies() {
    let r = j(fixture!("responses.json"));
    let e = kotak_error(reqwest::StatusCode::OK, &r["session_error"], "x");
    assert_eq!(e.code(), "AUTH_ERROR");
    let e = kotak_error(reqwest::StatusCode::OK, &r["order_error"], "x");
    assert_eq!(
        e.client_message(),
        "Kotak: Market order with Algo Id not allowed"
    );
    let e = kotak_error(reqwest::StatusCode::FORBIDDEN, &json!({}), "x");
    assert_eq!(e.code(), "AUTH_ERROR");
    let e = kotak_error(reqwest::StatusCode::BAD_REQUEST, &json!({}), "fallback");
    assert_eq!(e.client_message(), "fallback");
    assert!(stat_ok(&json!({"stat": "ok"})));
    assert!(!stat_ok(&json!({"stat": "Not_Ok"})));
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn order(symbol: &str, exchange: &str, pricetype: PriceType, action: Action) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: action.as_str().into(),
        quantity: 75,
        price: 0.0,
        order_type: pricetype.as_str().into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: Some(120.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_jdata_matches_web_transform() {
    let o = order("NIFTY27OCT2625000CE", "NFO", PriceType::Market, Action::Buy);
    let mut placed = place_order_jdata(&o).unwrap();
    let ig = placed["ig"].as_str().unwrap().to_string();
    assert!(ig.starts_with("openalgo-"), "{ig}");
    placed["ig"] = json!("openalgo");
    assert_eq!(
        placed,
        json!({
            "am": "NO", "dq": "0", "es": "nse_fo", "mp": "0", "pc": "NRML", "pf": "N",
            "pr": "0", "pt": "MKT", "qt": "75", "rt": "DAY", "tp": "120.0",
            "ts": "NIFTY26O2725000CE", "tt": "B", "ig": "openalgo"
        })
    );
    let mut l = order("SBIN", "NSE", PriceType::Limit, Action::Sell);
    l.price = 812.5;
    l.trigger_price = 0.0;
    let v = place_order_jdata(&l).unwrap();
    assert_eq!(
        (
            v["pt"].as_str(),
            v["pr"].as_str(),
            v["tp"].as_str(),
            v["tt"].as_str()
        ),
        (Some("L"), Some("812.5"), Some("0"), Some("S"))
    );
    assert_eq!(v["ts"], "SBIN-EQ");
    assert_eq!(v["es"], "nse_cm");
}

#[test]
fn slm_is_sent_as_protected_sl() {
    // SELL CE at 120: Kotak options grid gives 2% under 500 -> 117.6.
    let o = order("NIFTY27OCT2625000CE", "NFO", PriceType::SlM, Action::Sell);
    let v = place_order_jdata(&o).unwrap();
    assert_eq!(
        (v["pt"].as_str(), v["pr"].as_str()),
        (Some("SL"), Some("117.6"))
    );
    // Kotak's own grid: 10% under 5, an absolute 0.10 under 1.
    assert_eq!(mpp_offset(4.0, "CE"), 0.4);
    assert_eq!(mpp_offset(0.5, "PE"), 0.10);
    assert_eq!(mpp_offset(600.0, "EQ"), 3.0);
    assert_eq!(
        slm_protected_price("NIFTY27OCT2625000CE", Action::Buy, 0.5, 0.05).unwrap(),
        0.6
    );
    assert_eq!(
        slm_protected_price("SBIN", Action::Buy, 815.5, 0.05).unwrap(),
        819.6
    );
    let mut z = o.clone();
    z.trigger_price = 0.0;
    assert_eq!(
        place_order_jdata(&z).unwrap_err().client_message(),
        "Trigger price is required and must be positive for SL-M orders"
    );
    let mut no_tick = o.clone();
    no_tick.instrument.tick_size = 0.0;
    assert!(place_order_jdata(&no_tick).is_err());
}

#[test]
fn modify_jdata_matches_web_transform() {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 811.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("261003000104", &m, &master()).unwrap();
    // Exact equality also pins that modify sends no `ig` (web #2177).
    assert_eq!(
        modify_order_jdata(&rm).unwrap(),
        json!({
            "tk": "3045", "dq": "0", "es": "nse_cm", "mp": "0", "dd": "NA", "vd": "DAY",
            "pc": "CNC", "pr": "811.0", "pt": "L", "qt": "10", "tp": "0",
            "ts": "SBIN-EQ", "no": "261003000104", "tt": "B"
        })
    );
}

#[test]
fn static_maps_match_web() {
    assert_eq!(reverse_map_exchange("CDS"), Some("cde_fo"));
    assert_eq!(reverse_map_exchange("BCD"), Some("bcs_fo"));
    assert_eq!(reverse_map_exchange("NSE_INDEX"), None);
    assert_eq!(map_exchange("mcx_fo"), Some("MCX"));
    assert_eq!(row_exchange("nse_com"), "nse_com");
    assert_eq!(order_type(PriceType::SlM), "SL-M");
    assert_eq!(reverse_order_type("MKT"), "MARKET");
    assert_eq!(reverse_order_type("LMT"), "LIMIT");
    assert_eq!(map_status("trigger pending"), "open");
    assert_eq!(map_status("put order req received"), "open");
    assert_eq!(map_status("Canceled"), "cancelled");
    assert_eq!(map_status("some new state"), "some new state");
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let o = map_orders(&data_rows(&j(fixture!("orders.json"))), &r);
    assert_eq!(o.len(), 6);
    assert_eq!(
        (
            o[0].symbol.as_str(),
            o[0].exchange.as_str(),
            o[0].side.as_str()
        ),
        ("SBIN", "NSE", "BUY")
    );
    assert_eq!(
        (o[0].status.as_str(), o[0].order_type.as_str(), o[0].price),
        ("complete", "MARKET", 812.35)
    );
    assert_eq!(o[0].exchange_order_id.as_deref(), Some("1100000045678901"));
    // A working SL shows its limit, and trigger pending reads as open.
    assert_eq!(o[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        (o[1].status.as_str(), o[1].price, o[1].trigger_price),
        ("open", 117.6, 120.0)
    );
    assert_eq!((o[1].quantity, o[1].pending_quantity), (75, 75));
    assert_eq!(o[2].rejection_reason.as_deref(), Some("RMS:Margin Exceeds"));
    assert_eq!((o[3].status.as_str(), o[3].price), ("open", 3490.0));
    assert_eq!(o[4].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(o[4].status, "cancelled");
    // No token: resolved through the broker symbol.
    assert_eq!(o[5].symbol, "IDEA");
    assert_eq!(o[5].status, "open");
    // Not_Ok and null data are an empty book.
    assert!(data_rows(&json!({"stat": "Not_Ok", "emsg": "No Data"})).is_empty());
    assert!(data_rows(&json!({"stat": "Ok", "data": null})).is_empty());
}

#[test]
fn trade_book_resolves_symbol_without_token() {
    let r = master();
    let t = map_trades(&data_rows(&j(fixture!("trades.json"))), &r);
    assert_eq!(t.len(), 2);
    assert_eq!(
        (
            t[0].symbol.as_str(),
            t[0].trade_id.as_str(),
            t[0].trade_value
        ),
        ("SBIN", "50000123", 8123.5)
    );
    assert_eq!(
        (t[1].symbol.as_str(), t[1].quantity, t[1].side.as_str()),
        ("NIFTY27OCT2625000PE", 75, "SELL")
    );
}

#[test]
fn positions_follow_kotak_pnl_formula() {
    let r = master();
    let rows = data_rows(&j(fixture!("positions.json")));
    let p0 = map_position(&rows[0], &r, 814.1);
    assert_eq!(
        (p0.symbol.as_str(), p0.quantity, p0.average_price),
        ("SBIN", 10, 812.35)
    );
    assert_eq!((p0.ltp, p0.pnl), (814.1, 17.5));
    // Carried short at upldPrc 110 plus today's sale at 98.4.
    let p1 = map_position(&rows[1], &r, 100.0);
    assert_eq!(
        (p1.quantity, p1.average_price, p1.pnl),
        (-150, 104.2, 630.0)
    );
    assert_eq!(p1.overnight_quantity, -75);
    // Carried long without upldPrc: Kotak's carry valuation; no LTP -> 0.
    let p2 = map_position(&rows[2], &r, 0.0);
    assert_eq!((p2.quantity, p2.average_price, p2.pnl), (75, 120.5, 0.0));
    assert_eq!(carry_forward_amounts(&rows[2], 1.0), (9037.5, 0.0, true));
    assert_eq!(price_factor(&rows[2]), 1.0);
    let p3 = map_position(&rows[3], &r, 9000.0);
    assert_eq!((p3.quantity, p3.pnl, p3.average_price), (0, -450.0, 0.0));
    assert_eq!(net_quantity(&rows[1]), -150);
    assert!(position_matches(&rows[0], "SBIN-EQ", "nse_cm", "MIS"));
    assert!(!position_matches(&rows[0], "SBIN-EQ", "nse_cm", "CNC"));
}

#[test]
fn holdings_use_master_symbols() {
    let r = master();
    let v = j(fixture!("holdings.json"));
    let rows = v["data"].as_array().unwrap();
    let h0 = map_holding(&rows[0], &r);
    assert_eq!(
        (
            h0.symbol.as_str(),
            h0.exchange.as_str(),
            h0.product.as_str()
        ),
        ("SBIN", "NSE", "CNC")
    );
    assert_eq!(
        (h0.average_price, h0.ltp, h0.pnl, h0.pnl_percentage),
        (700.12, 814.1, 2279.54, 16.28)
    );
    assert_eq!(h0.isin.as_deref(), Some("INE062A01020"));
    let h1 = map_holding(&rows[1], &r);
    assert_eq!(
        (h1.symbol.as_str(), h1.exchange.as_str(), h1.pnl_percentage),
        ("UNLISTEDCO", "BSE", 0.0)
    );
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_read_collateral_value_as_cash() {
    let f = funds_from_limits(&j(fixture!("limits.json")));
    assert_eq!(
        (f.available_cash, f.collateral, f.utilised_debits),
        (179542.8, 222565.5, 0.2)
    );
    assert_eq!((f.m2m_unrealized, f.m2m_realized), (-1250.75, 320.0));
    assert_eq!(f.total_margin, 402108.1);
}

#[test]
fn margin_body_and_response() {
    let b = KotakBroker::new(master());
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: Action::Buy,
        quantity: 75,
        product: crate::brokers::common::mapping::Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    assert_eq!(
        super::funds::margin_body(&b, &leg).unwrap(),
        json!({"brkName": "KOTAK", "brnchId": "ONLINE", "exSeg": "nse_fo", "prc": "0.0",
               "prcTp": "MKT", "prod": "NRML", "qty": "75", "tok": "52011", "trnsTp": "B"})
    );
    let mut bad = leg.clone();
    bad.key = QuoteKey::new("NSE", "NOPE");
    assert!(super::funds::margin_body(&b, &bad).is_none());
    let r = j(fixture!("responses.json"));
    assert_eq!(parse_margin(&r["margin_ok"]).unwrap(), 98234.55);
    assert_eq!(
        parse_margin(&r["margin_error"]).unwrap_err(),
        "Scrip not allowed for margin calculation"
    );
    assert!(parse_margin(&json!([])).is_err());
}

// ---------------------------------------------------------------------------
// Quotes and history
// ---------------------------------------------------------------------------

#[test]
fn quotes_and_depth_from_neo_rows() {
    let rows: Vec<Value> = serde_json::from_str(fixture!("quotes.json")).unwrap();
    let key = QuoteKey::new("NSE", "SBIN");
    let q = quote_from_row(&key, &rows[0]);
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close, q.volume),
        (814.1, 814.05, 814.1, 812.35, 4823170)
    );
    let d = depth_from_quote(&key, &rows[0]);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks[1], DepthLevel::default());
    // Cash rows carry Neo's whole-book totals.
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (634710, 689415));
    let fo = depth_from_quote(&QuoteKey::new("NFO", "NIFTY27OCT2625000CE"), &rows[1]);
    // F&O leaves them 0: the visible levels are summed instead.
    assert_eq!(
        (fo.total_buy_qty, fo.total_sell_qty, fo.oi, fo.ltq),
        (1500, 1050, 5234175, 75)
    );
    // An index with an empty book: bid and ask fall back to the LTP.
    let idx = quote_from_row(&QuoteKey::new("NSE_INDEX", "NIFTY"), &rows[2]);
    assert_eq!((idx.bid, idx.ask), (25012.35, 25012.35));
    assert_eq!(index_candidates("midcpnifty").len(), 4);
    assert_eq!(index_candidates("NIFTY"), vec!["Nifty 50".to_string()]);
    assert_eq!(index_candidates("NIFTYIT"), vec!["NIFTYIT".to_string()]);
    assert_eq!(kotak_segment("BSE_INDEX"), Some("bse_cm"));
}

#[test]
fn multiquote_rows_match_queries() {
    let rows: Vec<Value> = serde_json::from_str(fixture!("quotes.json")).unwrap();
    let queries = vec![
        "nse_fo|52011".to_string(),
        "NSE_CM|3045".to_string(),
        "nse_cm|Nifty 50".to_string(),
        "nse_cm|404".to_string(),
    ];
    let m = match_multiquotes(&queries, &rows);
    assert_eq!(m[0].unwrap()["exchange_token"], "52011");
    // Case-insensitive fallback.
    assert_eq!(m[1].unwrap()["exchange_token"], "3045");
    // By display symbol without -IN.
    assert_eq!(m[2].unwrap()["exchange_token"], "26000");
    assert!(m[3].is_none());
}

#[test]
fn history_helpers_follow_web() {
    assert_eq!(history_chunk_days("1min"), 30);
    assert_eq!(history_chunk_days("15min"), 60);
    assert_eq!(history_chunk_days("60min"), 90);
    assert_eq!(history_chunk_days("W"), 180);
    assert!(is_no_data_fault("No data found for the given duration"));
    assert!(is_no_data_fault("Market has not yet opened"));
    assert!(!is_no_data_fault("Invalid neosymbol"));
    // ISO with +0530 is the true epoch; daily lands on 00:00 UTC of the IST
    // date, also for today's still-open bar stamped 09:15.
    let ts = json!("2026-10-01T09:15:00+0530");
    assert_eq!(history_timestamp(&ts, false), Some(1790826300));
    assert_eq!(history_timestamp(&ts, true), Some(1790812800));
    assert_eq!(
        history_timestamp(&json!("2026-10-01T00:00:00+0530"), true),
        Some(1790812800)
    );
    let h = j(fixture!("history.json"));
    let rows = normalize_candles(h["intraday"]["data"]["candles"].as_array().unwrap());
    // The 4-column row is dropped, short rows padded to 7.
    assert_eq!(rows.len(), 3);
    let c = repair_candles(&rows, false);
    // The open outside its own bar widens the low; negative volume is zero.
    assert_eq!((c[0].open, c[0].low, c[0].high), (1304.1, 1304.1, 1310.0));
    assert_eq!((c[1].volume, c[1].close), (0, 1308.8));
    assert_eq!(
        earliest_start(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()),
        NaiveDate::from_ymd_opt(2021, 10, 4).unwrap()
    );
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_per_segment_rules() {
    assert_eq!(rows_of("NSE_CM").len(), 4);
    assert_eq!(rows_of("BSE_CM").len(), 2);
    assert_eq!(rows_of("NSE_FO").len(), 4);
    assert_eq!(rows_of("CDE_FO").len(), 3);
    assert_eq!(rows_of("MCX_FO").len(), 3);
    assert_eq!(rows_of("BSE_FO").len(), 2);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (
            sbin.brsymbol.as_str(),
            sbin.brexchange.as_str(),
            sbin.token.as_str()
        ),
        ("SBIN-EQ", "NSE", "3045")
    );
    assert_eq!(
        (sbin.tick_size, sbin.instrument_type.as_str()),
        (0.05, "EQ")
    );
    assert!(r.by_symbol("NSE", "IDEA").is_some());
    // Warrant group is not kept.
    assert!(r.by_symbol("NSE", "ELECTCAST").is_none());
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(
        (nifty.brsymbol.as_str(), nifty.instrument_type.as_str()),
        ("Nifty 50", "EQ")
    );
    assert_eq!(r.by_symbol("BSE_INDEX", "SENSEX").unwrap().token, "1");
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().brexchange, "BSE");
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(
        (fut.expiry.as_str(), fut.brexchange.as_str(), fut.lot_size),
        ("27-OCT-26", "nse_fo", 75)
    );
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!(
        (ce.strike, ce.brsymbol.as_str()),
        (25000.0, "NIFTY26O2725000CE")
    );
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    assert!(r.by_symbol("CDS", "USDINR28OCT2687.5CE").is_some());
    assert_eq!(
        r.by_symbol("CDS", "USDINR28OCT26FUT").unwrap().tick_size,
        0.0025
    );
    // A reference row with no expiry is no contract.
    let eur = r.by_symbol("CDS", "EURINR").unwrap();
    assert_eq!(
        (eur.expiry.as_str(), eur.instrument_type.as_str()),
        ("", "")
    );
    // MCX has no epoch offset.
    assert!(r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").is_some());
    assert!(r.by_symbol("MCX", "CRUDEOIL19OCT268650CE").is_some());
    assert!(r.by_symbol("MCX", "GOLD").is_some());
    assert!(r.by_symbol("MCX", "SILVER").is_none());
    assert!(r.by_symbol("BFO", "SENSEX29OCT2682000CE").is_some());
    assert!(r.by_symbol("BFO", "SENSEX29OCT26FUT").is_some());
}

#[test]
fn master_contract_helpers() {
    assert_eq!(
        combine_details("NIFTY", "27-OCT-26", 25000.0, "CE"),
        "NIFTY27OCT2625000CE"
    );
    assert_eq!(
        combine_details("VEDL", "27-OCT-26", 292.5, "PE"),
        "VEDL27OCT26292.5PE"
    );
    assert_eq!(combine_details("GOLD", "", 0.0, ""), "GOLD");
    assert_eq!(
        expiry_from_epoch(1477578600, true).as_deref(),
        Some("27-OCT-26")
    );
    assert_eq!(
        expiry_from_epoch(1792434540, false).as_deref(),
        Some("19-OCT-26")
    );
    assert_eq!(expiry_from_epoch(0, false), None);
    assert_eq!(expiry_from_epoch(-1, true), None);
    assert_eq!(
        file_key("https://x/y/transformed-v1/nse_cm-v1.csv"),
        Some("NSE_CM")
    );
    assert_eq!(
        file_key("https://x/y/transformed/nse_com.csv"),
        Some("NSE_COM")
    );
    assert_eq!(file_key("https://x/y/bcs_fo.csv"), None);
    let paths = parse_file_paths(&j(fixture!("responses.json"))["file_paths"]).unwrap();
    assert_eq!(paths.len(), 7);
    assert!(parse_file_paths(&json!({"data": {}})).is_none());
    let fb = fallback_urls("https://cdn", "2026-10-03");
    assert_eq!(fb.len(), 7);
    assert!(fb.contains(&(
        "NSE_CM",
        "https://cdn/2026-10-03/transformed-v1/nse_cm-v1.csv".into()
    )));
    assert!(parse_file("NSE_CM", "a,b\n1,2\n").is_err());
    assert!(parse_file("NSE_COM", "anything").unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// SFeed
// ---------------------------------------------------------------------------

/// A packet: the 9-byte header (`u16 length, u16 code, i8 exchange, u8
/// level, u8 auction, u8 seq, u8 bitmask length`, LE) and `body`.
fn packet(code: u16, exchange: i8, level: u8, body: &[u8]) -> Vec<u8> {
    let mut v = ((9 + body.len()) as u16).to_le_bytes().to_vec();
    v.extend(code.to_le_bytes());
    v.push(exchange as u8);
    v.extend([level, 0, 0, 0]);
    v.extend(body);
    v
}

/// Market picture body (135 bytes) plus depth rows, offsets per
/// `sfeed_protocol._MP_BODY`.
fn market_picture(
    token: u32,
    ltp: u32,
    buy: &[(i64, i32, i32)],
    sell: &[(i64, i32, i32)],
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(token.to_le_bytes()); // @0
    b.extend(634_710i64.to_le_bytes()); // total buy @4
    b.extend(689_415i64.to_le_bytes()); // total sell @12
    b.extend(4_823_170i64.to_le_bytes()); // volume @20
    b.extend(1_790_999_702i64.to_le_bytes()); // ltt @28
    b.extend((-1i64).to_le_bytes()); // last update @36
    b.extend(81_000u32.to_le_bytes()); // open @44
    b.extend(81_235u32.to_le_bytes()); // close @48
    b.extend(81_600u32.to_le_bytes()); // high @52
    b.extend(80_850u32.to_le_bytes()); // low @56
    b.extend(ltp.to_le_bytes()); // ltp @60
    b.extend(5i64.to_le_bytes()); // ltq @64
    b.extend(81_312u32.to_le_bytes()); // atp @72
    b.extend(0u32.to_le_bytes()); // indicative close @76
    b.extend((buy.len() as u32).to_le_bytes()); // buy rows @80
    b.extend((sell.len() as u32).to_le_bytes()); // sell rows @84
    b.extend(0i16.to_le_bytes()); // status @88
    b.extend(22i32.to_le_bytes()); // change % @90
    b.extend(0u32.to_le_bytes()); // oi @94
    b.extend(0f64.to_le_bytes()); // turnover @98
    b.extend(175i32.to_le_bytes()); // change @106
    for _ in 0..4 {
        b.extend(0u32.to_le_bytes()); // circuits, yearly @110..@126
    }
    b.extend(1u32.to_le_bytes()); // lot @126
    b.push(2); // precision @130
    b.extend(1u32.to_le_bytes()); // multiplier @131
    assert_eq!(b.len(), 135);
    for (q, p, o) in buy.iter().chain(sell.iter()) {
        b.extend(q.to_le_bytes());
        b.extend(p.to_le_bytes());
        b.extend(o.to_le_bytes());
    }
    b
}

fn index_body(token: u32, value: i32, close: i32, name: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(token.to_le_bytes()); // @0
    b.extend(2_495_010i32.to_le_bytes()); // open @4
    b.extend(close.to_le_bytes()); // close @8
    b.extend(2_504_000i32.to_le_bytes()); // high @12
    b.extend(2_493_055i32.to_le_bytes()); // low @16
    b.extend(value.to_le_bytes()); // value @20
    b.extend(1_790_999_702u64.to_le_bytes()); // ltt @24
    b.extend([0u8; 12]); // yearly high/low, change % @32..@44
    b.extend(0f64.to_le_bytes()); // market cap @44
    b.push(2); // precision @52
    b.extend(1i32.to_le_bytes()); // multiplier @53
    let mut n = name.as_bytes().to_vec();
    n.resize(21, 0);
    b.extend(n); // name @57
    assert_eq!(b.len(), 78);
    b
}

fn mini_body(token: u32, ltp: u32, close: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(token.to_le_bytes());
    b.extend(1_790_999_710i64.to_le_bytes());
    b.extend(ltp.to_le_bytes());
    b.extend(10i64.to_le_bytes());
    b.extend(close.to_le_bytes());
    b.extend(0i32.to_le_bytes());
    b.extend(0i32.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    b.push(2);
    b.extend(1u32.to_le_bytes());
    assert_eq!(b.len(), 45);
    b
}

#[test]
fn sfeed_decoder_reads_le_packets() {
    let mut div = HashMap::new();
    div.insert(1i8, 100.0);
    div.insert(3i8, 10_000_000.0);
    let mp = packet(
        0,
        1,
        8,
        &market_picture(
            3045,
            81_410,
            &[(100, 81_405, 1), (220, 81_400, 3)],
            &[(75, 81_410, 2)],
        ),
    );
    match decode_packet(&mp, &div).unwrap() {
        Sfeed::Scrip {
            exchange,
            token,
            level,
            ltp,
            open,
            close,
            average_price,
            volume,
            total_buy,
            buy,
            sell,
            last_trade_qty,
            ..
        } => {
            assert_eq!(
                (exchange.as_str(), token.as_str(), level),
                ("nse_cm", "3045", 8)
            );
            assert_eq!(
                (ltp, open, close, average_price),
                (814.1, 810.0, 812.35, 813.12)
            );
            assert_eq!((volume, total_buy, last_trade_qty), (4_823_170, 634_710, 5));
            assert_eq!(buy.len(), 2);
            assert_eq!(buy[1].price, 814.0);
            assert_eq!((sell[0].quantity, sell[0].orders), (75, 2));
        }
        other => panic!("unexpected {:?}", other),
    }
    // Touch line (level 4) always carries one row a side.
    let tl = packet(
        0,
        1,
        4,
        &market_picture(3045, 81_410, &[(1, 81_405, 1)], &[(2, 81_410, 1)]),
    );
    match decode_packet(&tl, &div).unwrap() {
        Sfeed::Scrip { buy, sell, .. } => assert_eq!((buy.len(), sell.len()), (1, 1)),
        other => panic!("unexpected {:?}", other),
    }
    let idx = packet(
        7207,
        1,
        0,
        &index_body(4_247_863_880, 2_501_235, 2_498_000, "Nifty 50"),
    );
    match decode_packet(&idx, &div).unwrap() {
        Sfeed::Index {
            name,
            value,
            close,
            token,
            ..
        } => {
            assert_eq!(
                (name.as_str(), value, close),
                ("Nifty 50", 25012.35, 24980.0)
            );
            assert_eq!(token, "4247863880");
        }
        other => panic!("unexpected {:?}", other),
    }
    let mini = packet(0, 1, 1, &mini_body(11536, 350_050, 349_000));
    assert!(matches!(decode_packet(&mini, &div), Some(Sfeed::Lite { ltp, .. }) if ltp == 3500.5));
    // Exchange divider from the auth response (CDS 1e7).
    let usd = packet(0, 3, 1, &mini_body(1002, 875_000_000, 0));
    assert!(matches!(decode_packet(&usd, &div), Some(Sfeed::Lite { ltp, .. }) if ltp == 87.5));
    assert!(matches!(
        decode_packet(&packet(6511, 1, 0, &[]), &div),
        Some(Sfeed::MarketStatus { code: 1, .. })
    ));
    // An all-zero closing-auction packet says nothing.
    assert!(decode_packet(&packet(104, 1, 0, &[0u8; 24]), &div).is_none());
    // Batches: two packets then a truncated tail.
    let mut frame = mp.clone();
    frame.extend(&mini);
    frame.extend(&[50, 0, 1]);
    assert_eq!(split_batch(&frame).len(), 2);
    assert!(split_batch(&[3, 0]).is_empty());
}

fn fsub(symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
    let row = master().by_symbol(exchange, symbol).unwrap();
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: row.token,
        brsymbol: row.brsymbol,
        brexchange: row.brexchange,
        mode,
        depth: 5,
    }
}

fn text_frames(frames: &[Message]) -> Vec<Value> {
    frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            _ => panic!("text frame expected"),
        })
        .collect()
}

#[test]
fn sfeed_handshake_subscribe_and_ticks() {
    let mut f = KotakFeed::new(DEFAULT_SFEED_URL, "sid-1", "<USER_ID>".into(), master());
    assert!(f.awaits_auth_ack());
    let hello = text_frames(&f.on_connected());
    assert_eq!(hello[0]["user"], "<USER_ID>");
    assert_eq!(hello[0]["auth"], "sid-1");
    assert_eq!(hello[0]["format"], "native_batch");
    assert_eq!(hello[0]["source"], "NEOTRADEAPI");
    let ack = json!({"message_code": 1119, "exchanges": {"nse_cm": {"value": 1, "divider": 100}, "nse_fo": {"value": 2, "divider": 100}}});
    assert_eq!(
        f.parse(&Message::Text(ack.to_string())),
        vec![FeedEvent::AuthOk]
    );
    let frames = text_frames(&f.subscribe_frames(&[
        fsub("SBIN", "NSE", FeedMode::Depth),
        fsub("TCS", "NSE", FeedMode::Ltp),
        fsub("NIFTY", "NSE_INDEX", FeedMode::Quote),
    ]));
    assert_eq!(frames.len(), 3);
    assert_eq!(
        frames[0],
        json!({"event": "subscribeScrips", "inputtoken": "nse_cm|3045,nse_cm|11536", "ack_symbol": true})
    );
    assert_eq!(frames[1]["event"], "subscribeDepth");
    assert_eq!(frames[1]["inputtoken"], "nse_cm|3045");
    assert_eq!(frames[2]["inputtoken"], "nse_cm|Nifty 50");
    // Depth packet for SBIN -> a quote tick and a 5-level depth.
    let mp = packet(
        0,
        1,
        8,
        &market_picture(3045, 81_410, &[(100, 81_405, 1)], &[(75, 81_410, 2)]),
    );
    let ev = f.parse(&Message::Binary(mp));
    assert_eq!(ev.len(), 2);
    match (&ev[0], &ev[1]) {
        (FeedEvent::Tick(t), FeedEvent::Depth(d)) => {
            assert_eq!(
                (t.symbol.as_str(), t.mode, t.ltp, t.close),
                ("SBIN", 3, 814.1, 812.35)
            );
            assert_eq!(d.buy.len(), 5);
            assert_eq!(d.buy[0].price, 814.05);
            assert_eq!(d.sell[1], DepthLevel::default());
        }
        other => panic!("unexpected {:?}", other),
    }
    // A mini touch line only moves the LTP; the rest is kept.
    let ev = f.parse(&Message::Binary(packet(
        0,
        1,
        1,
        &mini_body(3045, 81_500, 0),
    )));
    match &ev[..] {
        [FeedEvent::Tick(t)] => assert_eq!((t.ltp, t.open, t.close), (815.0, 810.0, 812.35)),
        other => panic!("unexpected {:?}", other),
    }
    // The index answers with its own token and its name.
    let ev = f.parse(&Message::Binary(packet(
        7207,
        1,
        0,
        &index_body(4_247_863_880, 2_501_235, 2_498_000, "Nifty 50"),
    )));
    match &ev[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.ltp),
                ("NIFTY", "NSE_INDEX", 25012.35)
            );
            assert_eq!(t.change, 32.35);
        }
        other => panic!("unexpected {:?}", other),
    }
    // Unsubscribed tokens are ignored and leave no state behind.
    assert!(f
        .parse(&Message::Binary(packet(0, 1, 1, &mini_body(42, 100, 0))))
        .is_empty());
    assert_eq!(f.state_len(), 2);
    let un = text_frames(&f.unsubscribe_frames(&[fsub("SBIN", "NSE", FeedMode::Depth)]));
    assert_eq!(
        un[0],
        json!({"event": "unsubscribeScrips", "inputtoken": "nse_cm|3045"})
    );
    assert_eq!(un[1]["event"], "unsubscribeDepth");
    assert_eq!((f.subscription_count(), f.state_len()), (2, 1));
    // Mode changes only touch the depth stream.
    let up = text_frames(&f.mode_change_frames(
        &fsub("TCS", "NSE", FeedMode::Ltp),
        &fsub("TCS", "NSE", FeedMode::Depth),
    ));
    assert_eq!(
        up,
        vec![json!({"event": "subscribeDepth", "inputtoken": "nse_cm|11536", "ack_symbol": true})]
    );
    assert!(f
        .mode_change_frames(
            &fsub("TCS", "NSE", FeedMode::Ltp),
            &fsub("TCS", "NSE", FeedMode::Quote)
        )
        .is_empty());
    // native_fallback is a refusal.
    let fb = json!({"message_code": 1117, "format": "native_fallback"});
    assert!(matches!(
        &f.parse(&Message::Text(fb.to_string()))[..],
        [FeedEvent::AuthFailed(_)]
    ));
}

#[test]
fn feed_config_and_dividers() {
    let cfg = j(fixture!("responses.json"))["feed_config"]["data"]["configs"].clone();
    assert_eq!(
        feed_url_from_config(&cfg, "E43"),
        (
            Some("sh".into()),
            "wss://sfeed-e43.kotaksecurities.com/apifeed".into()
        )
    );
    assert_eq!(
        feed_url_from_config(&cfg, "E21").1,
        "wss://cdtstream-e21.kotaksecurities.com/feed"
    );
    // HSM routes fall back to the SFeed default, as on the web.
    assert_eq!(
        feed_url_from_config(&cfg, "E10"),
        (Some("hs".into()), DEFAULT_SFEED_URL.into())
    );
    assert_eq!(
        feed_url_from_config(&cfg, "E99"),
        (None, DEFAULT_SFEED_URL.into())
    );
    assert_eq!(to_wss("http://h/x"), "ws://h/x");
    let hsm = KotakHsmFeed::new(hsm::DEFAULT_HSM_URL, "t", "s");
    assert_eq!(
        crate::brokers::common::streaming::normalize_request(hsm.ws_request().unwrap())
            .uri()
            .to_string(),
        "wss://mlhsm.kotaksecurities.com/"
    );
    assert_eq!(to_wss("wss://h"), "wss://h");
    let d = parse_dividers(
        &json!({"exchanges": {"cde_fo": {"divider": 10000000}, "nse_cm": {"value": 1, "divider": 100}}}),
    );
    assert_eq!(d.get(&3), Some(&10_000_000.0));
    assert_eq!(d.get(&1), Some(&100.0));
    let b = KotakBroker::new(SymbolResolver::new());
    assert_eq!(streaming::cached_feed_url(&b, "E43"), DEFAULT_SFEED_URL);
}

// ---------------------------------------------------------------------------
// Order feed
// ---------------------------------------------------------------------------

#[test]
fn order_feed_handshake_alternates_and_parses_orders() {
    let s = KotakSession::parse(&session()).unwrap();
    assert_eq!(
        realtime_url(&s.base_url),
        "wss://cis.kotaksecurities.com/realtime"
    );
    let mut f = KotakOrderFeed::new(&s, master());
    let first = f.connect_frame();
    match &first {
        Message::Text(t) => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"type": "cn", "Authorization": "tok", "Sid": "sid-1", "src": "WEB"})
        ),
        other => panic!("unexpected {:?}", other),
    }
    // Opened but never acknowledged: the next attempt uses the raw form.
    match f.connect_frame() {
        Message::Text(t) => assert_eq!(t, "{type:cn,Authorization:tok,Sid:sid-1,src:WEB}"),
        other => panic!("unexpected {:?}", other),
    }
    assert_eq!(
        f.parse(&Message::Text(
            r#"{"ak":"ok","type":"cn","msg":"connected"}"#.into()
        )),
        vec![FeedEvent::AuthOk]
    );
    // Acknowledged: the encoding sticks.
    assert!(matches!(f.connect_frame(), Message::Text(t) if t.starts_with("{type:cn")));
    let r = j(fixture!("responses.json"));
    match &f.parse(&Message::Text(r["order_feed_order"].to_string()))[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(
                (u.symbol.as_str(), u.exchange.as_str()),
                ("NIFTY27OCT2625000CE", "NFO")
            );
            assert_eq!(
                (
                    u.order_status.as_str(),
                    u.pending_quantity,
                    u.action.as_str()
                ),
                ("trigger pending", 75, "SELL")
            );
            assert_eq!((u.price, u.trigger_price), (117.6, 120.0));
        }
        other => panic!("unexpected {:?}", other),
    }
    match &f.parse(&Message::Text(r["order_feed_rejected"].to_string()))[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(
                (u.symbol.as_str(), u.order_status.as_str()),
                ("TCS", "rejected")
            );
            assert_eq!((u.pricetype.as_str(), u.pending_quantity), ("LIMIT", 1));
            assert_eq!(u.rejection_reason, "RMS:Margin Exceeds");
        }
        other => panic!("unexpected {:?}", other),
    }
    assert!(f
        .parse(&Message::Text(r#"{"type":"position","data":{}}"#.into()))
        .is_empty());
    assert!(f.parse(&Message::Text("keepalive".into())).is_empty());
}

// ---------------------------------------------------------------------------
// Legacy HSM
// ---------------------------------------------------------------------------

#[test]
fn hsm_request_frames_match_hswebsocketlib() {
    // [u16 len][type 1][3 fields][1][u16 len][jwt][2][u16 len][sid][3][u16 6]JS_API
    let c = hsm::connect_frame("ab", "s");
    assert_eq!(
        c,
        vec![
            0, 20, 1, 3, 1, 0, 2, b'a', b'b', 2, 0, 1, b's', 3, 0, 6, b'J', b'S', b'_', b'A', b'P',
            b'I'
        ]
    );
    let s = hsm::subscription_frame(&["nse_cm|1".to_string()], true, "sf", 1).unwrap();
    // [len][4][2][1][u16 data len][u16 count][u8 len]"sf|nse_cm|1"[2][u16 1][channel]
    let mut want = vec![4u8, 2, 1, 0, 14, 0, 1, 11];
    want.extend(b"sf|nse_cm|1");
    want.extend([2, 0, 1, 1]);
    let mut framed = (want.len() as u16).to_be_bytes().to_vec();
    framed.extend(want);
    assert_eq!(s, framed);
    let too_many: Vec<String> = (0..101).map(|i| format!("nse_cm|{}", i)).collect();
    assert!(hsm::subscription_frame(&too_many, true, "sf", 1).is_none());
    assert_eq!(hsm::ack_frame(7), vec![0, 9, 3, 1, 1, 0, 4, 0, 0, 0, 7]);
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

#[test]
fn hsm_decoder_snapshots_updates_and_acks() {
    let mut d = HsmDecoder::new();
    // Connection ok with an ack every 2 messages.
    let conn = vec![0, 11, 1, 2, 1, 0, 1, b'K', 2, 0, 1, 2];
    assert_eq!(d.decode(&conn), Some(HsmEvent::Connected { ok: true }));
    // Data: [len][6][u32 msg num][u16 count][u16 sub len][83 snap][u32 topic]
    // [u8 name len][name][u8 n][n x u32][u8 m][(fid, len, text)...]
    let name = b"sf|nse_cm|3045";
    let mut sub = vec![83u8];
    sub.extend(be32(9));
    sub.push(name.len() as u8);
    sub.extend(name);
    let mut vals = vec![0u32; 25];
    vals[4] = 4_823_170; // volume
    vals[5] = 81_410; // ltp
    vals[20] = 81_000; // open
    vals[21] = 81_235; // close
    vals[23] = 1; // multiplier
    vals[24] = 2; // precision
    vals[13] = 0x8000_0000; // not available
    sub.push(vals.len() as u8);
    for v in &vals {
        sub.extend(be32(*v));
    }
    sub.push(2);
    sub.extend([52, 4]);
    sub.extend(b"3045");
    sub.extend([53, 6]);
    sub.extend(b"nse_cm");
    let mut body = vec![6u8];
    body.extend(be32(101));
    body.extend(1u16.to_be_bytes());
    body.extend((sub.len() as u16).to_be_bytes());
    body.extend(&sub);
    let mut frame = (body.len() as u16).to_be_bytes().to_vec();
    frame.extend(&body);
    assert_eq!(d.decode(&frame), Some(HsmEvent::Data(vec![9])));
    let t = &d.topics[&9];
    assert_eq!(
        (t.price(5), t.price(21), t.long(4)),
        (814.1, 812.35, 4_823_170)
    );
    assert_eq!(t.price(13), 0.0);
    assert!(d.pending_acks.is_empty());
    // Update: [85][u32 topic][u8 n][n x u32]; second message triggers an ack.
    let mut upd = vec![85u8];
    upd.extend(be32(9));
    upd.push(6);
    for v in [0x8000_0000u32, 0, 0, 0, 0, 81_500] {
        upd.extend(be32(v));
    }
    let mut body = vec![6u8];
    body.extend(be32(102));
    body.extend(1u16.to_be_bytes());
    body.extend((upd.len() as u16).to_be_bytes());
    body.extend(&upd);
    let mut frame2 = (body.len() as u16).to_be_bytes().to_vec();
    frame2.extend(&body);
    assert_eq!(d.decode(&frame2), Some(HsmEvent::Data(vec![9])));
    assert_eq!(d.topics[&9].price(5), 815.0);
    assert_eq!(d.topics[&9].long(0), 0);
    assert_eq!(d.pending_acks, vec![102]);
    // Malformed frames are dropped, not panics.
    assert_eq!(d.decode(&[0, 5, 6, 0]), None);
    assert_eq!(d.decode(&[0]), None);
    assert_eq!(
        d.decode(&[0, 8, 4, 1, 1, 0, 1, b'N']),
        Some(HsmEvent::Subscription { ok: false })
    );

    // The feed turns the same frames into ticks for the subscription.
    let mut feed = KotakHsmFeed::new(hsm::DEFAULT_HSM_URL, "tok", "sid");
    assert!(matches!(
        feed.on_connected().as_slice(),
        [Message::Binary(_)]
    ));
    assert_eq!(feed.parse(&Message::Binary(conn)), vec![FeedEvent::AuthOk]);
    let frames = feed.subscribe_frames(&[fsub("SBIN", "NSE", FeedMode::Quote)]);
    assert_eq!(frames.len(), 1);
    match &feed.parse(&Message::Binary(frame))[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!(
                (t.symbol.as_str(), t.ltp, t.open, t.close, t.volume),
                ("SBIN", 814.1, 810.0, 812.35, 4_823_170)
            );
        }
        other => panic!("unexpected {:?}", other),
    }
    assert_eq!(feed.topic_count(), 1);
    // A data frame due an acknowledgement is answered on the same socket.
    let ev = feed.parse(&Message::Binary(frame2));
    assert!(
        ev.contains(&FeedEvent::Reply(Message::Binary(hsm::ack_frame(102)))),
        "{:?}",
        ev
    );
}

#[test]
fn feed_state_is_released_with_its_subscriptions() {
    // Resource hygiene: 300 subscribe / tick / unsubscribe cycles leave
    // nothing behind in the SFeed client's maps.
    let mut f = KotakFeed::new(DEFAULT_SFEED_URL, "sid", "U".into(), master());
    f.on_connected();
    for i in 0..300u32 {
        let s = FeedSubscription {
            token: (500_000 + i).to_string(),
            ..fsub("SBIN", "NSE", FeedMode::Depth)
        };
        f.subscribe_frames(std::slice::from_ref(&s));
        let ack = json!({"message_code": 1109, "trading_symbols": {format!("nse_cm|{}", 500_000 + i): "X"}});
        f.parse(&Message::Text(ack.to_string()));
        f.parse(&Message::Binary(packet(
            0,
            1,
            1,
            &mini_body(500_000 + i, 100, 0),
        )));
        f.unsubscribe_frames(std::slice::from_ref(&s));
    }
    assert_eq!((f.subscription_count(), f.state_len()), (0, 0));
    // An index subscription drops all of its name aliases too.
    let idx = fsub("NIFTY", "NSE_INDEX", FeedMode::Ltp);
    f.subscribe_frames(std::slice::from_ref(&idx));
    f.unsubscribe_frames(std::slice::from_ref(&idx));
    assert!(f
        .parse(&Message::Binary(packet(
            7207,
            1,
            0,
            &index_body(1, 100, 100, "Nifty 50")
        )))
        .is_empty());
    assert_eq!(f.state_len(), 0);
}

#[tokio::test]
async fn resumed_session_restores_the_ucc_and_resolves_the_feed_host() {
    use crate::brokers::common::streaming::{normalize_request, OrderFeed};
    let b = KotakBroker::with_urls(master(), "http://127.0.0.1:9", "http://127.0.0.1:9/cfg");
    // A resumed session has not been through `authenticate` in this run.
    b.restore_session(&BrokerCredentials {
        api_key: "UCC77".into(),
        ..Default::default()
    });
    assert_eq!(b.ucc_hint(), "UCC77");
    let auth = AuthToken::new("tok:::sid:::https://h.example:::acc:::E43");
    let mut f = b.create_feed(&auth).unwrap();
    // The data-centre host is looked up before connecting (here the config
    // service is unreachable, so the default host is kept) and remembered.
    f.prepare().await.unwrap();
    assert_eq!(
        normalize_request(f.ws_request().unwrap()).uri().host(),
        normalize_request(
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                DEFAULT_SFEED_URL
            )
            .unwrap()
        )
        .uri()
        .host()
    );
    assert!(b
        .feed_url
        .lock()
        .as_ref()
        .is_some_and(|(dc, _)| dc == "E43"));
    assert!(matches!(
        b.create_order_feed(&auth).unwrap(),
        OrderFeed::Socket(_)
    ));
    b.on_logout().await;
    assert!(b.feed_url.lock().is_none());
}

// ---------------------------------------------------------------------------
// Order tag (web test/test_kotak_order_tag.py, #2177)
// ---------------------------------------------------------------------------

#[test]
fn every_order_gets_a_different_tag() {
    let o = order("NIFTY27OCT2625000CE", "NFO", PriceType::Market, Action::Buy);
    let tags: std::collections::HashSet<String> = (0..50)
        .map(|_| {
            place_order_jdata(&o).unwrap()["ig"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(tags.len(), 50);
}

#[test]
fn default_tag_is_openalgo_prefix_plus_uuid_v4() {
    let o = order("NIFTY27OCT2625000CE", "NFO", PriceType::Market, Action::Buy);
    let v = place_order_jdata(&o).unwrap();
    let tag = v["ig"].as_str().unwrap();
    let (prefix, rest) = tag.split_once('-').unwrap();
    assert_eq!(prefix, "openalgo");
    assert_eq!(uuid::Uuid::parse_str(rest).unwrap().get_version_num(), 4);
    assert!(tag.len() <= 52);
}

#[test]
fn caller_tag_becomes_the_prefix_and_is_still_unique() {
    let a = order_tag(Some("ironcondor"));
    let b = order_tag(Some("ironcondor"));
    assert!(a.starts_with("ironcondor-") && b.starts_with("ironcondor-"));
    assert_ne!(a, b);
}

#[test]
fn long_or_blank_caller_tag_is_bounded_and_never_blank() {
    let long = order_tag(Some(&"x".repeat(100)));
    assert!(long.starts_with(&format!("{}-", "x".repeat(15))));
    assert!(long.len() <= 52);
    assert!(order_tag(Some("   ")).starts_with("openalgo-"));
}
