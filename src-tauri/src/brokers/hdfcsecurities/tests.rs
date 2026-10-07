//! HDFC Securities mapping tests against payloads built from the web plugin
//! and the InvestRight docs (`src-tauri/tests/fixtures/brokers/hdfcsecurities/`).

use super::auth::{access_token, jwt_subject};
use super::data::{compose_depth, compose_quote, parse_ltp};
use super::funds::{funds_from, mtm};
use super::mapping::*;
use super::master_contract::{cash_rank, classify_index_symbol, parse_security_master, tick_size};
use super::proto::*;
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use prost::Message as _;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../tests/fixtures/brokers/hdfcsecurities/",
            $name
        ))
    };
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_security_master(fixture!("security_master.csv")).unwrap());
    r
}

fn data(json: &str) -> Value {
    let v: Value = serde_json::from_str(json).unwrap();
    v["data"].clone()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_rows_and_symbols() {
    let rows = parse_security_master(fixture!("security_master.csv")).unwrap();
    // 19 rows: bond duplicate, COM underlying and DUMMY token dropped.
    assert_eq!(rows.len(), 16);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    // The equity wins over the bond listed under the same name.
    assert_eq!(sbin.brsymbol, "STABANEQNR");
    assert_eq!(sbin.token, "3045");
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.brexchange, "NSE");
    assert_eq!(sbin.strike, 0.0);

    let ce = r.by_symbol("NFO", "NIFTY25AUG2624500CE").unwrap();
    assert_eq!(ce.expiry, "25-AUG-26");
    assert_eq!(ce.lot_size, 75);
    assert_eq!(ce.name, "NIFTY");
    assert_eq!(ce.brexchange, "NSE");
    assert_eq!(ce.instrument_type, "CE");
    let fut = r.by_symbol("NFO", "NIFTY25AUG26FUT").unwrap();
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.strike, 0.0);
    assert_eq!(fut.tick_size, 0.1);
    assert!(r.by_symbol("NFO", "CIPLA25AUG261262.5PE").is_some());
    let bfo = r.by_symbol("BFO", "SENSEX27AUG2680000CE").unwrap();
    assert_eq!(
        (bfo.brsymbol.as_str(), bfo.token.as_str()),
        ("B826625", "826625")
    );
    let usd = r.by_symbol("CDS", "USDINR27AUG26FUT").unwrap();
    assert_eq!(usd.tick_size, 0.0025);
    assert_eq!(usd.lot_size, 1000);
    let gold = r.by_symbol("MCX", "GOLD05OCT26FUT").unwrap();
    assert_eq!(gold.tick_size, 1.0);
    assert!(r.by_symbol("NSE", "SOMEMF").is_none());
    // 27008 sits above the NSE index band.
    assert_eq!(r.by_symbol("NSE", "SWIGGY").unwrap().exchange, "NSE");
}

#[test]
fn master_contract_indices_and_blank_names() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.token, "26000");
    assert_eq!(nifty.brsymbol, "NIFTYEQEQNR");
    assert_eq!(nifty.instrument_type, "EQ");
    assert!(r.by_symbol("NSE_INDEX", "NIFTYMIDCAP50").is_some());
    // Blank display name: named from the security_id, then overridden.
    assert!(r.by_symbol("NSE_INDEX", "MINIFTY").is_some());
    assert_eq!(r.by_symbol("BSE_INDEX", "SENSEX").unwrap().token, "1");
    // BSE blank name borrows the NSE row's name for the same security_id.
    let mm = r.by_symbol("BSE", "M&M").unwrap();
    assert_eq!(mm.token, "500520");
    // No NSE twin: the security_id itself.
    assert!(r.by_symbol("BSE", "B999XYZ").is_some());
}

#[test]
fn master_contract_helpers() {
    assert_eq!(classify_index_symbol("Nifty Mid 50", "X"), "NIFTYMIDCAP50");
    assert_eq!(classify_index_symbol("", "CNX100EQ"), "NIFTY100");
    assert_eq!(classify_index_symbol("NIFTY BANK", "X"), "NIFTYBANK");
    assert_eq!(cash_rank("STABANEQNR"), 0);
    assert_eq!(cash_rank("IDFCFIREQNR"), 1);
    assert_eq!(cash_rank("SWIGGY"), 2);
    assert_eq!(cash_rank("STABANN6NR"), 3);
    assert_eq!(tick_size("5"), 0.05);
    assert_eq!(tick_size("100"), 1.0);
    assert_eq!(tick_size(".0025"), 0.0025);
    assert!(parse_security_master("a,b\n1,2\n").is_err());
    assert!(parse_security_master("").is_err());
}

// ---------------------------------------------------------------------------
// Exchange and order mappings
// ---------------------------------------------------------------------------

#[test]
fn segment_table_and_exchange_codes() {
    let cases = [
        ("NSE", "EQUITY", "NSE"),
        ("NSE", "OPTIDX", "NFO"),
        ("NSE", "FUTSTK", "NFO"),
        ("NSE", "FUTCUR", "CDS"),
        ("NSE", "UNDCUR", "CDS"),
        ("BSE", "EQUITY", "BSE"),
        ("BSE", "OPTSTK", "BFO"),
        ("MCX", "OPTFUT", "MCX"),
        ("MCX", "COM", "MCX"),
        ("nse", "optidx", "NFO"),
        ("NSE", "", "NSE"),
        ("BSE", "WEIRD", "BSE"),
    ];
    for (e, s, want) in cases {
        assert_eq!(to_oa_exchange(e, s), want, "{} {}", e, s);
    }
    assert_eq!(SEGMENT_TO_OA.len(), 18);
    for (oa, rest) in [
        ("NFO", "NSE"),
        ("BFO", "BSE"),
        ("CDS", "NSE"),
        ("NSE_INDEX", "NSE"),
        ("BSE_INDEX", "BSE"),
        ("MCX", "MCX"),
    ] {
        assert_eq!(to_rest_exchange(oa), rest);
    }
    assert_eq!(ws_scrip_id("CDS", "1200"), "NCD_1200");
    assert_eq!(ws_scrip_id("NSE_INDEX", "26000"), "NSE_INDEX_26000");
    assert_eq!(
        from_ws_scrip_id("NSE_INDEX_26000"),
        Some(("NSE_INDEX", "26000"))
    );
    assert_eq!(from_ws_scrip_id("NCD_1200"), Some(("CDS", "1200")));
    assert_eq!(from_ws_scrip_id("XX_1"), None);
}

#[test]
fn product_order_type_and_option_maps() {
    assert_eq!(map_product(Product::Cnc, "NSE"), "DELIVERY");
    assert_eq!(map_product(Product::Nrml, "BSE"), "DELIVERY");
    assert_eq!(map_product(Product::Nrml, "NFO"), "OVERNIGHT");
    assert_eq!(map_product(Product::Cnc, "MCX"), "OVERNIGHT");
    assert_eq!(map_product(Product::Mis, "NFO"), "INTRADAY");
    for (b, oa) in [
        ("DELIVERY", Some("CNC")),
        ("MTF", Some("CNC")),
        ("COLL-SELL", Some("CNC")),
        ("ENCASH", Some("CNC")),
        ("OVERNIGHT", Some("NRML")),
        ("intraday", Some("MIS")),
        ("COVER", Some("MIS")),
        ("BO", None),
    ] {
        assert_eq!(reverse_product(b), oa);
    }
    assert_eq!(map_order_type(PriceType::SlM), "SL-M");
    assert_eq!(reverse_order_type("SL-L"), "SL");
    assert_eq!(reverse_order_type("SLM"), "SL-M");
    assert_eq!(reverse_order_type("market"), "MARKET");
    assert_eq!(reverse_option_type("Call"), "CE");
    assert_eq!(reverse_option_type("put"), "PE");
    assert_eq!(reverse_option_type(""), "");
    assert_eq!(to_order_expiry("25-AUG-26"), "20260825");
    assert_eq!(to_order_expiry(""), "");
    assert_eq!(compact_expiry("30 APR 2024"), "30APR24");
    assert_eq!(compact_expiry("27-JUN-24"), "27JUN24");
    assert_eq!(compact_expiry("2024-06-27"), "");
}

#[test]
fn status_table_matches_web() {
    for (raw, want) in [
        ("TRADED", "complete"),
        ("fully_executed", "complete"),
        ("Rejected", "rejected"),
        ("CANCEL_REJECTED", "rejected"),
        ("MODIFY-REJECTED", "rejected"),
        ("CANCELED", "cancelled"),
        ("CANCEL_CONFIRMED", "cancelled"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("SL TRIGGER PENDING", "trigger pending"),
        ("Partially-Traded", "open"),
        ("AFTER_MARKET_ORDER_REQ_RECEIVED", "open"),
        ("TRANSIT", "open"),
        ("Something New", "something new"),
    ] {
        assert_eq!(map_status(raw), want, "{}", raw);
    }
    assert_eq!(STATUS_MAP.len(), 29);
    assert!(is_cancellable(&json!({"status": "OPEN"})));
    assert!(is_cancellable(
        &json!({"status": "TRADED", "cancellation_allowed": "YES"})
    ));
    assert!(!is_cancellable(
        &json!({"status": "OPEN", "cancellation_allowed": "no"})
    ));
    assert!(!is_cancellable(&json!({"status": "COMPLETE"})));
    assert!(is_cancellable(&json!({"status": "trigger-pending"})));
}

fn resolved(symbol: &str, exchange: &str, product: &str, pricetype: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 75,
        price: 101.5,
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: Some(100.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_payload_equity_and_derivatives() {
    let r = master();
    let eq = place_payload(&resolved("SBIN", "NSE", "CNC", "LIMIT"), &r);
    assert_eq!(eq["exchange"], "NSE");
    assert_eq!(eq["security_id"], "STABANEQNR");
    assert_eq!(eq["instrument_segment"], "EQUITY");
    assert_eq!(eq["product"], "DELIVERY");
    assert_eq!(eq["order_type"], "LIMIT");
    assert_eq!(eq["transaction_type"], "BUY");
    assert_eq!(eq["quantity"], 75);
    assert_eq!(eq["price"], 101.5);
    assert_eq!(eq["trigger_price"], 100.0);
    assert_eq!(eq["disclosed_quantity"], 0);
    assert_eq!(eq["validity"], "DAY");
    assert_eq!(eq["amo"], false);
    assert!(eq.get("expiry_date").is_none());
    let reference = eq["external_reference_number"].as_str().unwrap();
    assert_eq!(reference.len(), 13);

    let ce = place_payload(&resolved("NIFTY25AUG2624500CE", "NFO", "NRML", "SL-M"), &r);
    assert_eq!(ce["exchange"], "NSE");
    assert_eq!(ce["security_id"], "45001");
    assert_eq!(ce["instrument_segment"], "OPTIDX");
    assert_eq!(ce["product"], "OVERNIGHT");
    assert_eq!(ce["order_type"], "SL-M");
    assert_eq!(ce["expiry_date"], "20260825");
    // The underlying's own security_id, not its name.
    assert_eq!(ce["underlying_symbol"], "NIFTYEQEQNR");
    assert_eq!(ce["option_type"], "CE");
    assert_eq!(ce["strike_price"], 24500.0);

    let fut = place_payload(&resolved("NIFTY25AUG26FUT", "NFO", "MIS", "MARKET"), &r);
    assert_eq!(fut["instrument_segment"], "FUTIDX");
    assert_eq!(fut["product"], "INTRADAY");
    assert!(fut.get("option_type").is_none());
    let stk = place_payload(
        &resolved("CIPLA25AUG261262.5PE", "NFO", "NRML", "LIMIT"),
        &r,
    );
    assert_eq!(stk["instrument_segment"], "OPTSTK");
    assert_eq!(stk["underlying_symbol"], "CIPLTDEQNR");
    assert_eq!(stk["strike_price"], 1262.5);
    let bfo = place_payload(
        &resolved("SENSEX27AUG2680000CE", "BFO", "NRML", "LIMIT"),
        &r,
    );
    assert_eq!(bfo["exchange"], "BSE");
    assert_eq!(bfo["security_id"], "B826625");
    assert_eq!(bfo["instrument_segment"], "OPTIDX");
    assert_eq!(bfo["underlying_symbol"], "SENSEXEQ");
    let cds = place_payload(&resolved("USDINR27AUG26FUT", "CDS", "NRML", "LIMIT"), &r);
    assert_eq!(cds["exchange"], "NSE");
    assert_eq!(cds["instrument_segment"], "FUTCUR");
    // No separate broker code for currency underlyings: the plain name.
    assert_eq!(cds["underlying_symbol"], "USDINR");
    let mcx = place_payload(&resolved("GOLD05OCT26FUT", "MCX", "NRML", "LIMIT"), &r);
    assert_eq!(mcx["instrument_segment"], "FUTCOM");
}

#[test]
fn instrument_segment_fallbacks() {
    assert_eq!(instrument_segment("NFO", "CE", "BANKNIFTY", None), "OPTIDX");
    assert_eq!(instrument_segment("NFO", "FUT", "RELIANCE", None), "FUTSTK");
    assert_eq!(instrument_segment("BFO", "PE", "BANKEX", None), "OPTIDX");
    assert_eq!(instrument_segment("MCX", "CE", "GOLD", None), "OPTFUT");
    assert_eq!(
        instrument_segment("MCX", "FUT", "MCXBULLDEX", None),
        "FUTIDX"
    );
    assert_eq!(instrument_segment("CDS", "PE", "USDINR", None), "OPTCUR");
    assert_eq!(
        instrument_segment("NSE_INDEX", "EQ", "NIFTY", None),
        "EQUITY"
    );
}

#[test]
fn reference_numbers_never_repeat() {
    let mut last = 0i64;
    for _ in 0..500 {
        let v: i64 = external_reference_number().parse().unwrap();
        assert!(v > last);
        last = v;
    }
}

#[test]
fn modify_payload_carries_mutable_fields_only() {
    let m = ModifyOrderRequest {
        symbol: "NIFTY25AUG2624500CE".into(),
        exchange: "NFO".into(),
        action: "SELL".into(),
        product: "NRML".into(),
        pricetype: "SL".into(),
        quantity: 150,
        price: 90.0,
        trigger_price: 91.0,
        disclosed_quantity: 0,
    };
    let r = ResolvedModify::resolve("1002", &m, &master()).unwrap();
    let v = modify_payload(&r);
    assert_eq!(
        v,
        json!({"quantity": 150, "order_type": "SL", "validity": "DAY", "disclosed_quantity": 0,
               "product": "OVERNIGHT", "price": 90.0, "trigger_price": 91.0, "amo": false})
    );
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised() {
    let rows = unwrap_rows(&data(fixture!("orders.json")), None);
    let o = map_orders(&rows, &master());
    assert_eq!(o.len(), 7);
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(o[0].side, "BUY");
    assert_eq!(o[0].status, "complete");
    assert_eq!(o[0].product, "CNC");
    assert_eq!(o[0].average_price, 801.5);
    assert_eq!(o[0].exchange_order_id.as_deref(), Some("1100000000001"));
    assert_eq!(o[1].symbol, "NIFTY25AUG2624500CE");
    assert_eq!(o[1].exchange, "NFO");
    assert_eq!(o[1].status, "trigger pending");
    assert_eq!(o[1].order_type, "SL");
    assert_eq!(o[1].product, "NRML");
    assert_eq!(o[1].side, "SELL");
    assert_eq!(o[1].pending_quantity, 75);
    assert_eq!(o[2].status, "rejected");
    assert_eq!(o[2].symbol, "XYZ123");
    assert_eq!(o[2].rejection_reason.as_deref(), Some("Insufficient funds"));
    assert_eq!(o[2].product, "MIS");
    assert_eq!(o[3].symbol, "CIPLA");
    assert_eq!(o[3].status, "open");
    assert_eq!(o[5].exchange, "BSE");
    assert_eq!(o[5].symbol, "M&M");
    assert_eq!(o[5].status, "open");
    assert_eq!(o[5].pending_quantity, 3);
    assert_eq!(o[6].status, "cancelled");
}

#[test]
fn trade_book_reconstructs_symbols() {
    let rows = unwrap_rows(&data(fixture!("trades.json")), None);
    let t = map_trades(&rows, &master());
    assert_eq!(t.len(), 3);
    // Underlying name in security_id, confirmed by the master.
    assert_eq!(t[0].symbol, "NIFTY25AUG2624500CE");
    assert_eq!(t[0].exchange, "NFO");
    assert_eq!(t[0].trade_value, 9037.5);
    assert_eq!(t[0].side, "SELL");
    // An unconfirmed broker code is never spliced into a symbol.
    assert_eq!(t[1].symbol, "CIPLTDEQNR");
    assert_eq!(t[2].symbol, "SBIN");
    assert_eq!(t[2].trade_value, 8015.0);
    assert_eq!(t[2].trade_id, "T3");
}

fn priced_positions() -> Vec<Value> {
    let mut rows = unwrap_rows(&data(fixture!("positions.json")), Some("net"));
    rows[0]["ltp"] = json!(810.0);
    rows[1]["ltp"] = json!(100.0);
    rows
}

#[test]
fn positions_use_merged_ltp() {
    let p = map_positions(&priced_positions(), &master());
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].product, "MIS");
    assert_eq!((p[0].quantity, p[0].ltp, p[0].pnl), (10, 810.0, 100.0));
    assert_eq!(p[0].average_price, 800.0);
    assert_eq!(p[1].symbol, "NIFTY25AUG2624500CE");
    assert_eq!(p[1].exchange, "NFO");
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].average_price, 120.0);
    assert_eq!(p[1].pnl, 1500.0);
    assert_eq!(p[2].quantity, 0);
    assert_eq!(p[2].pnl, 50.0);
    assert_eq!(p[2].average_price, 0.0);
}

#[test]
fn holdings_fall_back_to_close_and_isin() {
    let mut rows = unwrap_rows(&data(fixture!("holdings.json")), None);
    rows[0]["ltp"] = json!(810.0);
    let h = map_holdings(&rows, &master());
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].pnl, 550.0);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!(h[1].symbol, "INE999X01010");
    assert_eq!(h[1].exchange, "BSE");
    assert_eq!(h[1].ltp, 12.0);
    assert_eq!(h[1].pnl_percentage, 20.0);
}

#[test]
fn funds_from_margins_and_positions() {
    let m = mtm(&priced_positions());
    assert_eq!(m, (200.0, 1600.0));
    let f = funds_from(&data(fixture!("margins.json")), m).unwrap();
    assert_eq!(f.available_cash, 100000.5);
    assert_eq!(f.utilised_debits, 184.0);
    assert_eq!(f.collateral, 2500.0);
    assert_eq!(f.m2m_realized, 200.0);
    assert_eq!(f.m2m_unrealized, 1600.0);
    assert!(funds_from(&json!({}), (0.0, 0.0)).is_err());
}

#[test]
fn error_bodies() {
    assert!(matches!(
        broker_error(&json!({"error": "invalid credentials"})),
        AppError::Auth(_)
    ));
    assert_eq!(
        broker_error(&json!({"status": "error", "message": "RMS: margin shortfall"}))
            .client_message(),
        "RMS: margin shortfall"
    );
    assert_eq!(
        broker_error(&json!({})).client_message(),
        "HDFC Securities refused the request."
    );
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn token_exchange_shapes() {
    assert_eq!(
        access_token(&json!({"accessToken": "a"})).as_deref(),
        Some("a")
    );
    assert_eq!(
        access_token(&json!({"access_token": "b"})).as_deref(),
        Some("b")
    );
    assert_eq!(
        access_token(&json!({"status": "success", "data": {"accessToken": "c"}})).as_deref(),
        Some("c")
    );
    assert_eq!(access_token(&json!({"message": "bad"})), None);
    // header.{"sub":"<USER_ID>"}.sig
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI8VVNFUl9JRD4ifQ.c2ln";
    assert_eq!(jwt_subject(jwt).as_deref(), Some("<USER_ID>"));
    assert_eq!(jwt_subject("opaque"), None);
}

#[test]
fn identity_and_capabilities() {
    let b = HdfcSecuritiesBroker::new(master());
    assert_eq!(b.id(), "hdfcsecurities");
    assert!(b.timeframe_map().is_empty());
    let c = b.capabilities();
    assert!(!c.history && !c.margin && !c.gtt && !c.order_feed && c.streaming);
    assert_eq!(b.supported_exchanges().len(), 8);
    assert!(b.create_feed(&AuthToken::new("no-colon")).is_err());
    assert!(b.create_feed(&AuthToken::new("key:tok")).is_ok());
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn fetch_ltp_recovers_blank_exchange() {
    let v: Value = serde_json::from_str(fixture!("fetch_ltp.json")).unwrap();
    let asked = vec![
        ("NSE".to_string(), "3045".to_string()),
        ("NFO".to_string(), "45001".to_string()),
        ("BFO".to_string(), "826625".to_string()),
    ];
    let m = parse_ltp(&v, &asked);
    assert_eq!(m[&("NSE".into(), "3045".into())], (810.0, 800.1));
    assert_eq!(m[&("BFO".into(), "826625".into())], (310.5, 300.0));
    // Ambiguous blank exchange is dropped.
    let both = vec![
        ("BFO".to_string(), "826625".to_string()),
        ("CDS".to_string(), "826625".to_string()),
    ];
    assert!(!parse_ltp(&v, &both).contains_key(&("BFO".into(), "826625".into())));
}

fn mbp_packet(ty: i32, token: i64, ltp: f64, oi: i64) -> GenericDto {
    let level = |price: f64, qty: i64, buy: bool| MarketDepthDto {
        quantity: qty,
        price,
        number_of_orders: 3,
        buy_flag: buy,
    };
    GenericDto {
        instrument_id: token,
        packet_type: ty,
        packet_timestamp: 1_790_000_000_123,
        mbp_data: Some(MbpData {
            last_traded_price: ltp,
            last_trade_time: 1_790_000_000,
            open_price: 99.0,
            high_price: 105.0,
            low_price: 98.0,
            closing_price: 110.0,
            volume_traded_today: 12345,
            last_trade_quantity: 75,
            average_trade_price: 101.2,
            total_buy_quantity: 5000,
            total_sell_quantity: 6000,
            lower_circuit_limit: 5.0,
            upper_circuit_limit: 300.0,
            oi,
            market_depth_dto_list: Some(MarketDepthDtoList {
                market_depth_dto: (0..6)
                    .map(|i| level(100.0 - i as f64, 10 + i, true))
                    .chain((0..6).map(|i| level(101.0 + i as f64, 20 + i, false)))
                    .collect(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn protobuf_mbp_index_circuit_oi_packets() {
    let frame = GenericDtoList {
        generic_dto_list: vec![
            mbp_packet(packet_type::NSE_FO_ALL, 45001, 100.5, 900),
            GenericDto {
                instrument_id: 26000,
                packet_type: packet_type::NSE_INDEX,
                index_data: Some(IndexData {
                    index_value: 24510.5,
                    opening_index: 24400.0,
                    high_index_value: 24600.0,
                    low_index_value: 24390.0,
                    closing_index: 24500.0,
                    packet_time_stamp: 1_790_000_000_999,
                    ..Default::default()
                }),
                ..Default::default()
            },
            GenericDto {
                instrument_id: 45001,
                packet_type: packet_type::NSE_FO_CIRC,
                mbp_data: Some(MbpData {
                    lower_circuit_limit: 1.0,
                    upper_circuit_limit: 400.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
            GenericDto {
                instrument_id: 45001,
                packet_type: packet_type::NSE_FO_OI,
                mbp_data: Some(MbpData {
                    oi: 1200,
                    ..Default::default()
                }),
                ..Default::default()
            },
            GenericDto {
                instrument_id: 1,
                packet_type: packet_type::HEARTBEAT,
                ..Default::default()
            },
            GenericDto {
                instrument_id: 45001,
                packet_type: packet_type::NSE_FO_GREEK,
                greek_data: Some(GreekData {
                    delta: 0.5,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
    };
    let p = decode_frame(&frame.encode_to_vec());
    assert_eq!(p.len(), 5);
    assert_eq!(p[0].kind, Kind::Mbp);
    assert_eq!(p[0].exchange, Some("NFO"));
    assert_eq!(
        (p[0].ltp, p[0].close, p[0].oi, p[0].volume),
        (100.5, 110.0, 900, 12345)
    );
    let (buy, sell) = p[0].depth.clone().unwrap();
    assert_eq!((buy.len(), sell.len()), (5, 5));
    assert_eq!(
        (buy[0].price, buy[0].quantity, buy[0].orders),
        (100.0, 10, 3)
    );
    assert_eq!(sell[4].price, 105.0);
    assert_eq!(p[1].kind, Kind::Index);
    assert_eq!(p[1].exchange, Some("NSE_INDEX"));
    assert_eq!(
        (p[1].ltp, p[1].open, p[1].close),
        (24510.5, 24400.0, 24500.0)
    );
    assert_eq!(p[1].timestamp_ms, 1_790_000_000_999);
    assert_eq!(p[2].kind, Kind::Circuit);
    assert_eq!(
        (p[2].lower_limit, p[2].upper_limit, p[2].ltp),
        (1.0, 400.0, 0.0)
    );
    assert_eq!(p[3].kind, Kind::Oi);
    assert_eq!(p[3].oi, 1200);
    assert_eq!(p[4].kind, Kind::Greek);

    // A bare GenericDTO frame decodes too.
    let one = mbp_packet(packet_type::BSE_CM, 500520, 3433.0, 0).encode_to_vec();
    let p = decode_frame(&one);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].exchange, Some("BSE"));
    assert!(decode_frame(&[0xff, 0xff, 0xff]).is_empty());
    for (ty, ex) in [
        (packet_type::NSE_CM_ALL, Some("NSE")),
        (packet_type::NSE_CD_OI, Some("CDS")),
        (packet_type::BSE_FO_ALL, Some("BFO")),
        (packet_type::MCX_PKT, Some("MCX")),
        (packet_type::BSE_INDEX, Some("BSE_INDEX")),
        (packet_type::ORDER, None),
    ] {
        assert_eq!(packet_exchange(ty), ex);
    }
}

#[test]
fn snapshot_accumulation_keeps_earlier_values() {
    let full = decode_frame(&mbp_packet(packet_type::NSE_FO_ALL, 45001, 100.5, 0).encode_to_vec())
        .remove(0);
    let oi = decode_frame(
        &GenericDto {
            instrument_id: 45001,
            packet_type: packet_type::NSE_FO_OI,
            mbp_data: Some(MbpData {
                oi: 1200,
                ..Default::default()
            }),
            ..Default::default()
        }
        .encode_to_vec(),
    )
    .remove(0);
    let mut acc = full.clone();
    acc.accumulate(&oi);
    assert_eq!((acc.ltp, acc.oi, acc.kind), (100.5, 1200, Kind::Mbp));
    assert!(acc.depth.is_some());
    let key = QuoteKey::new("NFO", "NIFTY25AUG2624500CE");
    let q = compose_quote(&key, (101.0, 0.0), Some(&acc));
    assert_eq!(
        (q.ltp, q.close, q.bid, q.ask, q.oi),
        (101.0, 110.0, 100.0, 101.0, 1200)
    );
    assert_eq!(q.change, -9.0);
    let d = compose_depth(&key, (0.0, 0.0), Some(&acc));
    assert_eq!(d.ltp, 100.5);
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.total_buy_qty, d.total_sell_qty, d.ltq), (5000, 6000, 75));
    let empty = compose_depth(&key, (101.0, 100.0), None);
    assert_eq!(empty.bids, vec![DepthLevel::default(); 5]);
    assert_eq!((empty.ltp, empty.prev_close), (101.0, 100.0));
}

// ---------------------------------------------------------------------------
// Streaming adapter
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: String::new(),
        mode,
        depth: 5,
    }
}

fn text(m: &Message) -> Value {
    match m {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        other => panic!("not text: {:?}", other),
    }
}

#[test]
fn feed_frames_and_request() {
    let mut f = HdfcSecuritiesFeed::new(
        "wss://feed.example/wsapi/v1/session",
        "KEY",
        "TOK",
        master(),
    );
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://feed.example/wsapi/v1/session?token=TOK&api_key=KEY"
    );
    assert_eq!(req.headers()["Authorization"], "TOK");
    assert_eq!(req.headers()["User-Agent"], USER_AGENT);
    let frames = f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("USDINR27AUG26FUT", "CDS", "1200", FeedMode::Quote),
        sub("SBIN", "NSE", "3045", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 2);
    assert_eq!(
        text(&frames[0]),
        json!({"heart_beat": false, "subscribe": [{"scripId": "NSE_INDEX_26000", "type": "LTP"}]})
    );
    assert_eq!(
        text(&frames[1]),
        json!({"heart_beat": false, "subscribe": [
            {"scripId": "NCD_1200", "type": "ALL"},
            {"scripId": "NSE_3045", "type": "ALL"}]})
    );
    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Depth)]);
    assert_eq!(
        text(&un[0]),
        json!({"heart_beat": false, "unSubscribe": [{"scripId": "NSE_3045", "type": "ALL"}]})
    );
    let (dur, hb) = f.heartbeat().unwrap();
    assert_eq!(dur.as_secs(), 10);
    assert_eq!(text(&hb), json!({"heart_beat": true}));
    // Batches of 100 scrips.
    let many: Vec<FeedSubscription> = (0..250)
        .map(|i| {
            sub(
                &format!("S{}", i),
                "NSE",
                &format!("{}", 1000 + i),
                FeedMode::Quote,
            )
        })
        .collect();
    assert_eq!(f.subscribe_frames(&many).len(), 3);
}

#[test]
fn feed_ticks_depth_and_partial_merge() {
    let mut f = HdfcSecuritiesFeed::new("wss://x", "KEY", "TOK", master());
    f.subscribe_frames(&[
        sub("NIFTY25AUG2624500CE", "NFO", "45001", FeedMode::Depth),
        // Same token on another exchange: told apart by packet type.
        sub("ABC", "NSE", "45001", FeedMode::Ltp),
    ]);
    // A partial packet before any full one is not published.
    let oi_only = GenericDto {
        instrument_id: 45001,
        packet_type: packet_type::NSE_FO_OI,
        mbp_data: Some(MbpData {
            oi: 1500,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(f
        .parse(&Message::Binary(oi_only.encode_to_vec()))
        .is_empty());
    let ev = f.parse(&Message::Binary(
        mbp_packet(packet_type::NSE_FO_ALL, 45001, 100.5, 900).encode_to_vec(),
    ));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.symbol, "NIFTY25AUG2624500CE");
    assert_eq!(t.exchange, "NFO");
    assert_eq!((t.mode, t.ltp, t.oi, t.volume), (3, 100.5, 900, 12345));
    assert_eq!(t.change, -9.5);
    assert_eq!(t.last_trade_time_ms, 1_790_000_000_000);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    // OI refresh merges into the last full packet: price survives.
    let ev = f.parse(&Message::Binary(oi_only.encode_to_vec()));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.ltp, t.oi), (100.5, 1500));
    // NSE cash packet for the same token goes to the NSE subscription.
    let ev = f.parse(&Message::Binary(
        mbp_packet(packet_type::NSE_CM_ALL, 45001, 7.0, 0).encode_to_vec(),
    ));
    assert_eq!(ev.len(), 1);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.symbol.as_str(), t.mode, t.volume), ("ABC", 1, 0));
    // Unsubscribed instruments are forgotten, snapshots included.
    assert_eq!(f.tracked(), (2, 2));
    f.unsubscribe_frames(&[
        sub("NIFTY25AUG2624500CE", "NFO", "45001", FeedMode::Depth),
        sub("ABC", "NSE", "45001", FeedMode::Ltp),
    ]);
    assert_eq!(f.tracked(), (0, 0));
    assert!(f
        .parse(&Message::Binary(
            mbp_packet(packet_type::NSE_FO_ALL, 45001, 1.0, 0).encode_to_vec()
        ))
        .is_empty());
    assert!(f.parse(&Message::Text("{}".into())).is_empty());
}

#[test]
fn feed_sub_types() {
    assert_eq!(sub_type(FeedMode::Ltp), "LTP");
    assert_eq!(sub_type(FeedMode::Quote), "ALL");
    assert_eq!(sub_type(FeedMode::Depth), "ALL");
    assert_eq!(exit_action(5), Action::Sell);
    assert_eq!(exit_action(-5), Action::Buy);
}
