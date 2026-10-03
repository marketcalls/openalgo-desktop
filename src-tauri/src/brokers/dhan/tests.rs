//! Dhan mapping tests against payloads built from the web plugin and the
//! DhanHQ v2 docs (`src-tauri/tests/fixtures/brokers/dhan/`). No account data.

use super::data::{
    adjust_dates, daily_timestamp, intraday_chunks, multiquote_bodies, parse_chart, quote_entry,
    to_depth, to_quote,
};
use super::funds::{funds_from_limit, parse_basket_margin, parse_single_margin};
use super::gtt::{map_gtt_book, modify_gtt_body, place_gtt_body};
use super::mapping::*;
use super::master_contract::{
    assign_values, extract_underlying, format_strike, parse_scrip_master, qualify_equity_symbol,
};
use super::streaming::{Dhan20DepthFeed, DhanFeed, DhanOrderFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/dhan/", $name))
    };
}

fn j(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_scrip_master(fixture!("api-scrip-master.csv"), Variant::Live).unwrap());
    r
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_maps_segments_and_symbols() {
    let rows = parse_scrip_master(fixture!("api-scrip-master.csv"), Variant::Live).unwrap();
    // 25 data rows; the bond (segment X) is dropped.
    assert_eq!(rows.len(), 24);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (sbin.token.as_str(), sbin.brexchange.as_str()),
        ("3045", "NSE_EQ")
    );
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.expiry, "");
    assert_eq!(sbin.name, "STATE BANK OF INDIA");
    // Non-EQ NSE series carries a suffix, so the warrant cannot shadow the
    // equity.
    assert_eq!(r.by_symbol("NSE", "ELECTCAST").unwrap().token, "21290");
    assert_eq!(r.by_symbol("NSE", "ELECTCAST-W1").unwrap().token, "780011");
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().brexchange, "BSE_EQ");

    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY-Oct2026-FUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.tick_size, 0.1);
    assert_eq!(fut.name, "NIFTY");
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (25000.0, "CE"));
    // The underlying comes from the symbol, not Dhan's mnemonic (RELOPT).
    assert_eq!(
        r.by_symbol("NFO", "RELIANCE27OCT261400CE").unwrap().name,
        "RELIANCE"
    );
    assert_eq!(
        r.by_symbol("BFO", "SENSEX29OCT2682000CE").unwrap().name,
        "SENSEX"
    );
    assert!(r.by_symbol("NFO", "VEDL27OCT26FUT").is_some());
    // Currency strike keeps its fraction from SEM_STRIKE_PRICE.
    let usd = r.by_symbol("CDS", "USDINR28OCT2687.5CE").unwrap();
    assert_eq!(usd.tick_size, 0.0025);
    assert!(r.by_symbol("CDS", "USDINR28OCT26FUT").is_some());
    let crude = r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!((crude.lot_size, crude.tick_size), (100, 1.0));
    assert!(r.by_symbol("MCX", "CRUDEOIL15OCT268650CE").is_some());
}

#[test]
fn master_contract_keeps_nse_commodity_apart_from_nfo() {
    // Security id 153964 exists in NSE segment D (TCS option) and NSE
    // segment M (SILVERM option): the segment decides the exchange.
    let r = master();
    let nco = r.by_token("NCO", "153964").unwrap();
    assert_eq!(nco.symbol, "SILVERM26NOV2690000CE");
    assert_eq!(nco.brexchange, "NSE_COMM");
    let nfo = r.by_token("NFO", "153964").unwrap();
    assert_eq!(nfo.symbol, "TCS27OCT263500CE");
}

#[test]
fn master_contract_indices() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(
        (nifty.brexchange.as_str(), nifty.instrument_type.as_str()),
        ("IDX_I", "INDEX")
    );
    // INDEX tick sizes are already rupees.
    assert_eq!(nifty.tick_size, 0.05);
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    // Spaces removed and renamed to the documented symbol.
    assert_eq!(
        r.by_symbol("NSE_INDEX", "NIFTYNXT50").unwrap().brsymbol,
        "NIFTY NEXT 50"
    );
    // Not in the documented set: Dhan's name is kept.
    assert!(r.by_symbol("NSE_INDEX", "NIFTY SOMETHING NEW").is_some());
    assert_eq!(r.by_symbol("BSE_INDEX", "SENSEX").unwrap().tick_size, 0.01);
    assert_eq!(r.by_symbol("BSE_INDEX", "BSEAUTO").unwrap().token, "52");
}

#[test]
fn sandbox_master_keeps_tick_unscaled() {
    let rows = parse_scrip_master(fixture!("api-scrip-master.csv"), Variant::Sandbox).unwrap();
    let sbin = rows
        .iter()
        .find(|r| r.symbol == "SBIN" && r.exchange == "NSE")
        .unwrap();
    assert_eq!(sbin.tick_size, 5.0);
}

#[test]
fn master_contract_helpers() {
    assert_eq!(format_strike(25000.0), "25000");
    assert_eq!(format_strike(87.5), "87.5");
    assert_eq!(format_strike(1.015), "1.015");
    assert_eq!(format_strike(100.0), "100");
    assert_eq!(qualify_equity_symbol("ELECTCAST", "W1"), "ELECTCAST-W1");
    assert_eq!(qualify_equity_symbol("SBIN", "EQ"), "SBIN");
    assert_eq!(qualify_equity_symbol("SBIN", ""), "SBIN");
    assert_eq!(
        extract_underlying("NIFTY28MAR2420800CE").as_deref(),
        Some("NIFTY")
    );
    assert_eq!(
        extract_underlying("BANKNIFTY24APR24FUT").as_deref(),
        Some("BANKNIFTY")
    );
    assert_eq!(
        extract_underlying("USDINR28OCT2687.5CE").as_deref(),
        Some("USDINR")
    );
    assert_eq!(
        extract_underlying("726GS203225APR2497PE").as_deref(),
        Some("726GS2032")
    );
    assert_eq!(extract_underlying("SBIN"), None);
    assert_eq!(
        assign_values("NSE", "M", "OPTFUT", "CE"),
        Some(("NCO", "NSE_COMM", "CE".into()))
    );
    assert_eq!(
        assign_values("NSE", "D", "FUTSTK", "XX"),
        Some(("NFO", "NSE_FNO", "FUT".into()))
    );
    assert_eq!(assign_values("NSE", "D", "FUTCOM", "XX"), None);
    assert_eq!(assign_values("BSE", "C", "OPTCUR", "PE").unwrap().0, "BCD");
    assert!(parse_scrip_master("a,b\n1,2\n", Variant::Live).is_err());
}

// ---------------------------------------------------------------------------
// Session and errors
// ---------------------------------------------------------------------------

#[test]
fn session_token_carries_client_id() {
    let s = DhanSession::parse(&AuthToken::new("1100012345:::eyJ.tok.en")).unwrap();
    assert_eq!(s.client_id.as_deref(), Some("1100012345"));
    assert_eq!(s.access_token, "eyJ.tok.en");
    let bare = DhanSession::parse(&AuthToken::new("eyJ.tok.en")).unwrap();
    assert!(bare.client_id.is_none());
    assert_eq!(
        bare.require_client_id().unwrap_err().code(),
        "VALIDATION_ERROR"
    );
    assert!(DhanSession::parse(&AuthToken::new("123:::")).is_err());
    assert!(!format!("{:?}", s).contains("tok.en"));
    assert_eq!(
        split_api_key("1100012345:::app-key"),
        (Some("1100012345".into()), Some("app-key".into()))
    );
    assert_eq!(
        split_api_key("1100012345"),
        (Some("1100012345".into()), None)
    );
}

#[test]
fn error_envelopes_are_trader_facing() {
    let errs = j(fixture!("errors.json"));
    let e = dhan_error(&errs["invalid_token"]).unwrap();
    assert_eq!(e.code(), "AUTH_ERROR");
    let e = dhan_error(&errs["order_error"]).unwrap();
    assert_eq!(
        e.client_message(),
        "Dhan: Trigger Price should be greater than Price"
    );
    let e = dhan_error(&errs["data_not_subscribed"]).unwrap();
    assert!(e.client_message().contains("Data APIs"));
    let e = dhan_error(&errs["data_rate_limit"]).unwrap();
    assert!(e.client_message().contains("limiting requests"));
    let e = dhan_error(&errs["funds_error"]).unwrap();
    assert_eq!(e.client_message(), "Dhan: Internal Server Error");
    assert!(dhan_error(&j(fixture!("fundlimit.json"))).is_none());
    assert!(dhan_error(&json!([1, 2])).is_none());
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn order(symbol: &str, exchange: &str, pricetype: PriceType, action: Action) -> ResolvedOrder {
    let r = master();
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: action.as_str().into(),
        quantity: 75,
        price: 118.5,
        order_type: pricetype.as_str().into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: Some(120.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &r).unwrap()
}

#[test]
fn place_body_matches_web_transform() {
    let mut o = order("NIFTY27OCT2625000CE", "NFO", PriceType::Market, Action::Buy);
    let b = place_order_body(&o, "1100012345", true).unwrap();
    assert_eq!(
        b,
        json!({
            "dhanClientId": "1100012345",
            "transactionType": "BUY",
            "exchangeSegment": "NSE_FNO",
            "productType": "MARGIN",
            "orderType": "MARKET",
            "validity": "DAY",
            "securityId": "35003",
            "quantity": 75
        })
    );
    o.pricetype = PriceType::Limit;
    o.validity = Validity::Ioc;
    o.disclosed_quantity = 25;
    o.amo = true;
    let b = place_order_body(&o, "1", true).unwrap();
    assert_eq!(b["price"], json!(118.5));
    assert_eq!(b["validity"], "IOC");
    assert_eq!(b["disclosedQuantity"], 25);
    assert_eq!(b["afterMarketOrder"], true);
    assert!(b.get("triggerPrice").is_none());
    o.pricetype = PriceType::Sl;
    let b = place_order_body(&o, "1", true).unwrap();
    assert_eq!(
        (b["orderType"].as_str(), b["triggerPrice"].as_f64()),
        (Some("STOP_LOSS"), Some(120.0))
    );
    o.trigger_price = 0.0;
    assert_eq!(
        place_order_body(&o, "1", true)
            .unwrap_err()
            .client_message(),
        "Trigger price is required for Stop Loss orders"
    );
}

#[test]
fn slm_becomes_protected_stop_loss_on_live_only() {
    // SELL CE at trigger 120: options under 500 protect 2% -> 117.6.
    let o = order("NIFTY27OCT2625000CE", "NFO", PriceType::SlM, Action::Sell);
    let b = place_order_body(&o, "1", true).unwrap();
    assert_eq!(b["orderType"], "STOP_LOSS");
    assert_eq!(b["price"], json!(117.6));
    assert_eq!(b["triggerPrice"], json!(120.0));
    // The sandbox sends the bare STOP_LOSS_MARKET.
    let b = place_order_body(&o, "1", false).unwrap();
    assert_eq!(b["orderType"], "STOP_LOSS_MARKET");
    assert!(b.get("price").is_none());
    // BUY equity at 815.5: 0.5% -> 819.5775, ceiled to the 0.05 tick.
    assert_eq!(
        slm_protected_price("SBIN", Action::Buy, 815.5, 0.05).unwrap(),
        819.6
    );
    // At least one tick away even when the percentage is smaller.
    assert_eq!(
        slm_protected_price("NIFTY27OCT2625000CE", Action::Sell, 1.0, 0.05).unwrap(),
        0.95
    );
    assert!(slm_protected_price("X", Action::Sell, 0.05, 0.05).is_err());
    assert!(slm_protected_price("X", Action::Buy, 10.0, 0.0).is_err());
    assert_eq!(snap_to_tick(87.50249, 0.0025, true), 87.5);
    assert_eq!(snap_to_tick(87.5001, 0.0025, false), 87.5025);
    assert_eq!(tick_decimals(0.0025), 4);
    assert_eq!(tick_decimals(1.0), 0);
}

#[test]
fn modify_body_matches_web_transform() {
    let r = master();
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "SL-M".into(),
        quantity: 10,
        price: 0.0,
        trigger_price: 815.5,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("52261003002", &m, &r).unwrap();
    let b = modify_order_body(&rm, "1100012345", true).unwrap();
    assert_eq!(
        b,
        json!({
            "dhanClientId": "1100012345",
            "orderId": "52261003002",
            "orderType": "STOP_LOSS",
            "legName": "ENTRY_LEG",
            "quantity": 10,
            "validity": "DAY",
            "triggerPrice": 815.5,
            "price": 819.6
        })
    );
}

#[test]
fn enum_maps_match_web() {
    assert_eq!(exchange_segment("NCO"), Some("NSE_COMM"));
    assert_eq!(exchange_segment("NSE_INDEX"), None);
    assert_eq!(data_segment("BSE_INDEX"), Some("IDX_I"));
    assert_eq!(map_exchange("BSE_CURRENCY"), "BCD");
    assert_eq!(map_exchange("WEIRD_SEG"), "WEIRD_SEG");
    assert_eq!(product_type(Product::Nrml), "MARGIN");
    assert_eq!(reverse_product("INTRADAY"), Some("MIS"));
    assert_eq!(reverse_product("MTF"), None);
    assert_eq!(book_product("MARGIN", "NSE"), "MARGIN");
    assert_eq!(book_product("MARGIN", "NFO"), "NRML");
    assert_eq!(reverse_order_type("STOP_LOSS_MARKET"), "SL-M");
    assert_eq!(map_status("TRADED"), "complete");
    assert_eq!(map_status("PART_TRADED"), "part_traded");
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let orders = map_orders(rows(j(fixture!("orders.json"))).unwrap(), &r);
    assert_eq!(orders.len(), 6);
    let o = &orders[0];
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str(), o.status.as_str()),
        ("SBIN", "NSE", "complete")
    );
    assert_eq!((o.average_price, o.filled_quantity), (812.35, 10));
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345678"));
    let sl = &orders[1];
    assert_eq!(sl.symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        (
            sl.order_type.as_str(),
            sl.product.as_str(),
            sl.status.as_str()
        ),
        ("SL", "NRML", "open")
    );
    assert_eq!((sl.price, sl.trigger_price), (118.5, 120.0));
    let rej = &orders[2];
    assert_eq!(
        (rej.status.as_str(), rej.product.as_str()),
        ("rejected", "MIS")
    );
    assert_eq!(rej.rejection_reason.as_deref(), Some("RMS:Margin Exceeds"));
    assert_eq!(orders[3].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(orders[3].status, "cancelled");
    assert_eq!(orders[4].order_type, "SL-M");
    assert_eq!(orders[4].exchange, "BSE");
    assert_eq!(orders[4].status, "transit");
    // Not in the master: the broker symbol is kept, never blank.
    assert_eq!(orders[5].symbol, "NOTINMASTER");
    assert_eq!(orders[5].filled_quantity, 0);
}

#[test]
fn trade_book_values_trades() {
    let r = master();
    let t = map_trades(rows(j(fixture!("trades.json"))).unwrap(), &r);
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].symbol.as_str(), t[0].trade_value), ("SBIN", 8123.5));
    assert_eq!(t[1].symbol, "NIFTY27OCT2625000PE");
    assert_eq!(t[1].product, "MIS");
    assert_eq!(t[1].trade_id, "15532700");
}

#[test]
fn positions_take_ltp_from_quotes() {
    let r = master();
    let mut ltp = HashMap::new();
    ltp.insert(ltp_key("NSE", "SBIN"), 814.1049);
    let p = map_positions(rows(j(fixture!("positions.json"))).unwrap(), &r, &ltp);
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].product.as_str()),
        ("SBIN", "MIS")
    );
    assert_eq!((p[0].quantity, p[0].ltp, p[0].pnl), (10, 814.1, 25.5));
    assert_eq!(p[1].symbol, "NIFTY27OCT2625000PE");
    assert_eq!((p[1].quantity, p[1].product.as_str()), (-150, "NRML"));
    assert_eq!(p[1].pnl, -217.5);
    assert_eq!(p[1].overnight_quantity, -75);
    assert_eq!(p[1].ltp, 0.0);
    assert_eq!((p[2].quantity, p[2].pnl), (0, -450.0));
}

#[test]
fn holdings_resolve_real_exchange() {
    let r = master();
    let mut ltp = HashMap::new();
    ltp.insert(ltp_key("NSE", "HDFCBANK"), 1700.25);
    let h = map_holdings(rows(j(fixture!("holdings.json"))).unwrap(), &r, &ltp);
    assert_eq!(h.len(), 2);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("HDFCBANK", "NSE")
    );
    assert_eq!((h[0].pnl, h[0].pnl_percentage), (1000.0, 3.03));
    assert_eq!(h[0].product, "CNC");
    // 500112 only resolves on BSE.
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("SBIN", "BSE")
    );
    assert_eq!((h[1].pnl, h[1].ltp, h[1].t1_quantity), (0.0, 0.0, 2));
    assert_eq!(h[1].current_value, 3500.0);
    let errs = j(fixture!("errors.json"));
    assert!(is_no_holdings(&errs["no_holdings"]));
    assert!(!is_no_holdings(&errs["invalid_token"]));
    assert_eq!(
        rows::<DhanHolding>(errs["invalid_token"].clone())
            .unwrap_err()
            .code(),
        "AUTH_ERROR"
    );
    assert!(rows::<DhanOrder>(Value::Null).unwrap().is_empty());
    assert_eq!(
        rows::<DhanOrder>(json!({"data": [{"orderId": "1"}]}))
            .unwrap()
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_subtract_collateral_from_balance() {
    let pos: Vec<DhanPosition> = rows(j(fixture!("positions.json"))).unwrap();
    let f = funds_from_limit(&j(fixture!("fundlimit.json")), &pos);
    assert_eq!(f.available_cash, 132340.75);
    assert_eq!(f.collateral, 20000.0);
    assert_eq!(f.utilised_debits, 12500.5);
    assert_eq!(f.m2m_realized, -330.0);
    assert_eq!(f.m2m_unrealized, -312.0);
}

#[test]
fn margin_responses_parse_like_web() {
    let m = j(fixture!("margin.json"));
    let s = parse_single_margin(&m["single"]).unwrap();
    assert_eq!(
        (s.total_margin_required, s.span_margin, s.exposure_margin),
        (152340.5, 110250.25, 42090.25)
    );
    let b = parse_basket_margin(&m["multi_snake"]).unwrap();
    assert_eq!(
        (b.total_margin_required, b.span_margin, b.exposure_margin),
        (61250.75, 45000.5, 16250.25)
    );
    assert_eq!(parse_basket_margin(&m["multi_camel"]).unwrap(), b);
    let errs = j(fixture!("errors.json"));
    let e = parse_single_margin(&errs["margin_error_200"]).unwrap_err();
    assert!(e
        .client_message()
        .contains("Invalid securityId for the given exchangeSegment"));
    assert!(parse_basket_margin(&errs["invalid_token"]).is_err());
    assert!(parse_basket_margin(&json!({})).is_err());
    assert!(parse_single_margin(&json!([1])).is_err());
}

// ---------------------------------------------------------------------------
// GTT
// ---------------------------------------------------------------------------

fn gtt(kind: GttTriggerType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        trigger_type: kind,
        action: Action::Sell,
        product: Product::Nrml,
        quantity: 75,
        pricetype: PriceType::Limit,
        price: 25500.0,
        trigger_price: 0.0,
        triggerprice_sl: 24800.0,
        stoploss: 24790.0,
        triggerprice_tg: 25590.0,
        target: 25600.0,
        last_price: None,
    }
}

#[test]
fn forever_order_bodies() {
    let b = place_gtt_body(&gtt(GttTriggerType::Oco), "35001", "1100012345").unwrap();
    assert_eq!(
        b,
        json!({
            "dhanClientId": "1100012345",
            "orderFlag": "OCO",
            "transactionType": "SELL",
            "exchangeSegment": "NSE_FNO",
            "productType": "MARGIN",
            "orderType": "LIMIT",
            "validity": "DAY",
            "securityId": "35001",
            "quantity": 75,
            "price": 24790.0,
            "triggerPrice": 24800.0,
            "price1": 25600.0,
            "triggerPrice1": 25590.0,
            "quantity1": 75
        })
    );
    // SINGLE: trigger falls back to the stop-loss trigger.
    let b = place_gtt_body(&gtt(GttTriggerType::Single), "35001", "1").unwrap();
    assert_eq!(
        (b["price"].as_f64(), b["triggerPrice"].as_f64()),
        (Some(25500.0), Some(24800.0))
    );
    assert!(b.get("price1").is_none());
    let tg = modify_gtt_body(&gtt(GttTriggerType::Oco), "777", "TARGET_LEG", "1");
    assert_eq!(
        (tg["price"].as_f64(), tg["triggerPrice"].as_f64()),
        (Some(25600.0), Some(25590.0))
    );
    assert_eq!(tg["legName"], "TARGET_LEG");
    let sl = modify_gtt_body(&gtt(GttTriggerType::Oco), "777", "STOP_LOSS_LEG", "1");
    assert_eq!(sl["price"], json!(24790.0));
}

#[test]
fn forever_book_groups_legs_and_keeps_active_only() {
    let r = master();
    let rows: Vec<Value> = serde_json::from_str(fixture!("forever_orders.json")).unwrap();
    let book = map_gtt_book(&rows, &r);
    assert_eq!(book.len(), 2);
    assert_eq!(book[0].trigger_id, "5132208051112");
    assert_eq!(
        (book[0].trigger_type.as_str(), book[0].status.as_str()),
        ("single", "active")
    );
    assert_eq!(book[0].symbol, "SBIN");
    let oco = &book[1];
    assert_eq!(oco.trigger_type, "two-leg");
    assert_eq!(oco.symbol, "NIFTY27OCT26FUT");
    assert_eq!(oco.trigger_prices, vec![24800.0, 25590.0]);
    assert_eq!(oco.legs[0].pricetype, "MARKET");
    assert_eq!(oco.legs[1].pricetype, "LIMIT");
    assert_eq!(oco.legs[1].product, "NRML");
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quotes_and_depth_from_marketfeed() {
    let v = j(fixture!("quote.json"));
    let key = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&key, quote_entry(&v, "NSE_EQ", "3045").unwrap());
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close),
        (814.1, 814.05, 814.1, 812.35)
    );
    assert_eq!(
        (q.volume, q.open, q.high, q.low),
        (4823170, 810.0, 816.0, 808.5)
    );
    assert_eq!(q.change, 1.75);
    let d5 = to_depth(&key, quote_entry(&v, "NSE_EQ", "3045").unwrap());
    assert_eq!(d5.bids.len(), 5);
    assert_eq!(d5.bids[3], DepthLevel::default());
    assert_eq!(d5.asks[4].quantity, 900);
    assert_eq!((d5.total_buy_qty, d5.total_sell_qty), (370, 1290));
    assert_eq!((d5.ltq, d5.prev_close), (5, 812.35));
    // camelCase fallbacks, OI as open_interest, null depth.
    let opt = to_quote(
        &QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        quote_entry(&v, "NSE_FNO", "35003").unwrap(),
    );
    assert_eq!(
        (opt.ltp, opt.oi, opt.volume, opt.bid),
        (121.4, 5234100, 1250775, 0.0)
    );
    // An empty entry is "no quote".
    assert!(quote_entry(&v, "NSE_FNO", "35004").is_none());
    assert!(quote_entry(&v, "BSE_EQ", "1").is_none());
}

#[test]
fn multiquote_bodies_group_by_segment_in_batches() {
    let wanted = vec![("NSE_EQ", 3045), ("NSE_EQ", 1333), ("IDX_I", 13)];
    let b = multiquote_bodies(&wanted);
    assert_eq!(b, vec![json!({"NSE_EQ": [3045, 1333], "IDX_I": [13]})]);
    let many: Vec<(&'static str, i64)> = (0..2500).map(|i| ("NSE_EQ", i)).collect();
    let b = multiquote_bodies(&many);
    assert_eq!(b.len(), 3);
    assert_eq!(b[0]["NSE_EQ"].as_array().unwrap().len(), 1000);
    assert_eq!(b[2]["NSE_EQ"].as_array().unwrap().len(), 500);
}

#[test]
fn history_helpers_follow_web() {
    // Saturday start moves to Monday, Sunday end back to Friday.
    assert_eq!(
        adjust_dates(d(2026, 10, 3), d(2026, 10, 11)),
        (d(2026, 10, 5), d(2026, 10, 9))
    );
    // 90-day chunks share their boundary day.
    let c = intraday_chunks(d(2026, 1, 1), d(2026, 7, 1), 90);
    assert_eq!(
        c,
        vec![
            (d(2026, 1, 1), d(2026, 4, 1)),
            (d(2026, 4, 1), d(2026, 6, 30)),
            (d(2026, 6, 30), d(2026, 7, 1))
        ]
    );
    assert!(intraday_chunks(d(2026, 1, 1), d(2026, 1, 1), 90).is_empty());
    // IST midnight 2026-10-02 (18:30 UTC the day before) -> 00:00 UTC on 2 Oct.
    assert_eq!(daily_timestamp(1790879400), 1790899200);
    let daily = parse_chart(&j(fixture!("charts_historical.json")), true);
    assert_eq!(daily[0].timestamp, 1790899200);
    assert_eq!(
        (daily[0].open, daily[0].close, daily[0].volume),
        (805.0, 810.0, 15233100)
    );
    let intra = parse_chart(&j(fixture!("charts_intraday.json")), false);
    assert_eq!(intra.len(), 3);
    assert_eq!(intra[0].timestamp, 1790999700);
    assert_eq!(
        (intra[1].volume, intra[2].volume, intra[2].oi),
        (8800, 12001, 0)
    );
    assert_eq!(
        history_instrument_type("NFO", "NIFTY27OCT2625000CE").unwrap(),
        "OPTIDX"
    );
    assert_eq!(
        history_instrument_type("NFO", "RELIANCE27OCT261400CE").unwrap(),
        "OPTSTK"
    );
    assert_eq!(
        history_instrument_type("NFO", "VEDL27OCT26FUT").unwrap(),
        "FUTSTK"
    );
    assert_eq!(
        history_instrument_type("MCX", "MCXBULLDEX27OCT26FUT").unwrap(),
        "FUTIDX"
    );
    assert_eq!(
        history_instrument_type("MCX", "CRUDEOIL15OCT268650CE").unwrap(),
        "OPTFUT"
    );
    assert_eq!(
        history_instrument_type("CDS", "USDINR28OCT26FUT").unwrap(),
        "FUTCUR"
    );
    assert_eq!(
        history_instrument_type("NCO", "SILVERM26NOV2690000CE").unwrap(),
        "OPTFUT"
    );
    assert_eq!(
        history_instrument_type("BSE_INDEX", "SENSEX").unwrap(),
        "INDEX"
    );
    assert!(history_instrument_type("CRYPTO", "BTC").is_err());
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
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

fn texts(frames: &[Message]) -> Vec<Value> {
    frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            _ => panic!("text frame expected"),
        })
        .collect()
}

/// An 8-byte market-feed header (web `_parse_regular_message`): u8 code,
/// u16 length (header included), u8 segment, u32 security id, all LE.
fn header(code: u8, payload_len: usize, segment: u8, security_id: u32) -> Vec<u8> {
    let mut v = vec![code];
    v.extend(((payload_len + 8) as u16).to_le_bytes());
    v.push(segment);
    v.extend(security_id.to_le_bytes());
    v
}

fn f32le(v: f32) -> [u8; 4] {
    v.to_le_bytes()
}

#[test]
fn feed_url_and_subscribe_frames() {
    let b = DhanBroker::new(master());
    let feed = b
        .create_feed(&AuthToken::new("1100012345:::tok en"))
        .unwrap();
    // The manager gives the bare host its `/` path (no `GET ?version=2`).
    let req = crate::brokers::common::streaming::normalize_request(feed.ws_request().unwrap());
    assert_eq!(
        req.uri().to_string(),
        "wss://api-feed.dhan.co/?version=2&token=tok%20en&clientId=1100012345&authType=2"
    );
    assert!(req
        .uri()
        .path_and_query()
        .unwrap()
        .as_str()
        .starts_with("/?version=2&"));
    assert!(b.create_feed(&AuthToken::new("tokenonly")).is_err());
    let mut f = DhanFeed::with_url("wss://x", "t", "c");
    let frames = texts(&f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", FeedMode::Quote),
        sub("NIFTY27OCT2625000CE", "NFO", FeedMode::Depth),
        sub("RELIANCE", "NSE", FeedMode::Ltp),
    ]));
    assert_eq!(frames.len(), 3);
    assert_eq!(
        frames[0],
        json!({"RequestCode": 15, "InstrumentCount": 2, "InstrumentList": [
            {"ExchangeSegment": "NSE_EQ", "SecurityId": "3045"},
            {"ExchangeSegment": "NSE_EQ", "SecurityId": "2885"}
        ]})
    );
    assert_eq!(frames[1]["RequestCode"], 17);
    assert_eq!(frames[1]["InstrumentList"][0]["ExchangeSegment"], "IDX_I");
    assert_eq!(frames[2]["RequestCode"], 21);
    let un = texts(&f.unsubscribe_frames(&[sub("SBIN", "NSE", FeedMode::Ltp)]));
    assert_eq!(un[0]["RequestCode"], 16);
    // 100 instruments a frame.
    let many: Vec<FeedSubscription> = (0..150)
        .map(|i| FeedSubscription {
            token: (10_000 + i).to_string(),
            ..sub("SBIN", "NSE", FeedMode::Quote)
        })
        .collect();
    let fr = texts(&f.subscribe_frames(&many));
    assert_eq!(fr.len(), 2);
    assert_eq!(fr[0]["InstrumentCount"], 100);
}

#[test]
fn binary_packets_decode_little_endian() {
    let mut f = DhanFeed::with_url("wss://x", "t", "c");
    f.subscribe_frames(&[
        sub("RELIANCE", "NSE", FeedMode::Ltp),
        sub("SBIN", "NSE", FeedMode::Quote),
        sub("NIFTY27OCT2625000CE", "NFO", FeedMode::Depth),
        sub("NIFTY", "NSE_INDEX", FeedMode::Ltp),
    ]);
    let mut frame = Vec::new();
    // Code 2 ticker for RELIANCE (segment 1): f32 ltp @0, u32 ltt @4.
    frame.extend(header(2, 8, 1, 2885));
    frame.extend(f32le(1416.7));
    frame.extend(1_790_999_700u32.to_le_bytes());
    // Code 6 prev close for SBIN: f32 @0, u32 prev OI @4.
    frame.extend(header(6, 8, 1, 3045));
    frame.extend(f32le(812.35));
    frame.extend(0u32.to_le_bytes());
    // Code 4 quote for SBIN (42 bytes).
    let mut q = Vec::new();
    q.extend(f32le(814.1)); // ltp @0
    q.extend(5u16.to_le_bytes()); // ltq @4
    q.extend(1_790_999_702u32.to_le_bytes()); // ltt @6
    q.extend(f32le(813.12)); // atp @10
    q.extend(4_823_170u32.to_le_bytes()); // volume @14
    q.extend(689_415u32.to_le_bytes()); // total sell @18
    q.extend(512_633u32.to_le_bytes()); // total buy @22
    q.extend(f32le(810.0)); // open @26
    q.extend(f32le(0.0)); // close @30 (not sent intraday)
    q.extend(f32le(816.0)); // high @34
    q.extend(f32le(808.5)); // low @38
    assert_eq!(q.len(), 42);
    frame.extend(header(4, q.len(), 1, 3045));
    frame.extend(&q);
    // Code 5 OI for the option (segment 2).
    frame.extend(header(5, 4, 2, 35003));
    frame.extend(5_234_100u32.to_le_bytes());
    // Code 8 full for the option (154 bytes).
    let mut p = Vec::new();
    p.extend(f32le(121.4));
    p.extend(75u16.to_le_bytes());
    p.extend(1_790_999_703u32.to_le_bytes());
    p.extend(f32le(119.85));
    p.extend(1_250_775u32.to_le_bytes());
    p.extend(300_000u32.to_le_bytes());
    p.extend(250_000u32.to_le_bytes());
    p.extend(5_234_175u32.to_le_bytes()); // oi @26
    p.extend(5_300_000u32.to_le_bytes()); // oi high @30
    p.extend(5_100_000u32.to_le_bytes()); // oi low @34
    p.extend(f32le(115.0)); // open @38
    p.extend(f32le(118.0)); // close @42
    p.extend(f32le(125.0)); // high @46
    p.extend(f32le(110.5)); // low @50
    let bids: [f32; 5] = [121.35, 121.3, 121.25, 121.2, 121.15];
    let asks: [f32; 5] = [121.45, 121.5, 121.55, 121.6, 121.65];
    for i in 0..5u32 {
        // Level i at 54 + 20*i.
        p.extend((100 + i).to_le_bytes()); // bid qty @+0
        p.extend((200 + i).to_le_bytes()); // ask qty @+4
        p.extend((1 + i as u16).to_le_bytes()); // bid orders @+8
        p.extend((2 + i as u16).to_le_bytes()); // ask orders @+10
        p.extend(f32le(bids[i as usize])); // bid price @+12
        p.extend(f32le(asks[i as usize])); // ask price @+16
    }
    assert_eq!(p.len(), 154);
    frame.extend(header(8, p.len(), 2, 35003));
    frame.extend(&p);
    // Heartbeat, an unsubscribed token and a disconnect notice.
    frame.extend(header(0, 0, 0, 0));
    frame.extend(header(2, 8, 1, 42));
    frame.extend([0u8; 8]);
    frame.extend(header(50, 2, 0, 0));
    frame.extend(805u16.to_le_bytes());

    let events = f.parse(&Message::Binary(frame));
    let ticks: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ticks.len(), 3);
    let r = &ticks[0];
    assert_eq!((r.symbol.as_str(), r.mode, r.ltp), ("RELIANCE", 1, 1416.7));
    assert_eq!(r.last_trade_time_ms, 1_790_999_700_000);
    let s = &ticks[1];
    assert_eq!((s.symbol.as_str(), s.mode), ("SBIN", 2));
    assert_eq!(
        (s.ltp, s.last_quantity, s.average_price),
        (814.1, 5, 813.12)
    );
    assert_eq!(
        (s.volume, s.total_buy_quantity, s.total_sell_quantity),
        (4_823_170, 512_633, 689_415)
    );
    assert_eq!((s.open, s.high, s.low), (810.0, 816.0, 808.5));
    // The quote carried no close; the previous close from code 6 fills it.
    assert_eq!((s.close, s.change), (812.35, 1.75));
    let o = &ticks[2];
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str(), o.mode),
        ("NIFTY27OCT2625000CE", "NFO", 3)
    );
    assert_eq!(
        (o.oi, o.open, o.close, o.high, o.low),
        (5_234_175, 115.0, 118.0, 125.0, 110.5)
    );
    let depth = events
        .iter()
        .find_map(|e| match e {
            FeedEvent::Depth(d) => Some(d.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(depth.buy.len(), 5);
    assert_eq!(
        depth.buy[0],
        DepthLevel {
            price: 121.35,
            quantity: 100,
            orders: 1
        }
    );
    assert_eq!(
        depth.sell[4],
        DepthLevel {
            price: 121.65,
            quantity: 204,
            orders: 6
        }
    );
    assert!(events.contains(&FeedEvent::Heartbeat));
    // A short packet and a truncated tail decode to nothing.
    assert!(f
        .parse(&Message::Binary(
            header(4, 10, 1, 3045)
                .into_iter()
                .chain([0u8; 10])
                .collect()
        ))
        .is_empty());
    assert!(f.parse(&Message::Binary(vec![2, 50, 0])).is_empty());
}

#[test]
fn index_ticks_match_by_token_on_segment_zero() {
    let mut f = DhanFeed::with_url("wss://x", "t", "c");
    f.subscribe_frames(&[sub("SENSEX", "BSE_INDEX", FeedMode::Ltp)]);
    let mut frame = header(2, 8, 0, 51);
    frame.extend(f32le(81234.5));
    frame.extend(0u32.to_le_bytes());
    match &f.parse(&Message::Binary(frame))[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!(
                (t.exchange.as_str(), t.symbol.as_str(), t.ltp),
                ("BSE_INDEX", "SENSEX", 81234.5)
            )
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn twenty_depth_pairs_bid_and_ask_halves() {
    let mut f = Dhan20DepthFeed::with_url("wss://depth", "t", "c");
    assert_eq!(
        f.ws_request().unwrap().uri().to_string(),
        "wss://depth/?token=t&clientId=c&authType=2"
    );
    let frames = texts(&f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Depth),
        sub("SBIN", "BSE", FeedMode::Depth),
    ]));
    // BSE has no 20-level depth.
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["RequestCode"], 23);
    assert_eq!(frames[0]["InstrumentCount"], 1);
    // 12-byte header: u16 length @0, u8 code @2, u8 segment @3, u32 id @4,
    // u32 sequence @8; 20 rows of f64 price, u32 qty, u32 orders.
    let half = |code: u8, base: f64| {
        let mut v = Vec::new();
        v.extend(332u16.to_le_bytes());
        v.push(code);
        v.push(1);
        v.extend(3045u32.to_le_bytes());
        v.extend(7u32.to_le_bytes());
        for i in 0..20u32 {
            v.extend((base + f64::from(i) * 0.05).to_le_bytes());
            v.extend((10 * (i + 1)).to_le_bytes());
            v.extend((i + 1).to_le_bytes());
        }
        v
    };
    assert!(f.parse(&Message::Binary(half(41, 813.0))).is_empty());
    let ev = f.parse(&Message::Binary(half(51, 814.0)));
    match &ev[..] {
        [FeedEvent::Depth(d)] => {
            assert_eq!(d.buy.len(), 20);
            assert_eq!(d.sell.len(), 20);
            assert_eq!(d.buy[0].price, 813.0);
            assert_eq!(d.sell[19].quantity, 200);
            assert_eq!(d.total_buy_quantity, 2100);
        }
        other => panic!("unexpected {:?}", other),
    }
    // Pairs are consumed: a lone half again waits.
    assert!(f.parse(&Message::Binary(half(51, 814.0))).is_empty());
}

#[test]
fn order_update_frame_becomes_order_update() {
    let mut f = DhanOrderFeed::with_url("wss://ou", "tok", "1100012345", master());
    match f.on_connected().as_slice() {
        [Message::Text(t)] => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"LoginReq": {"MsgCode": 42, "ClientId": "1100012345", "Token": "tok"}, "UserType": "SELF"})
        ),
        other => panic!("unexpected {:?}", other),
    }
    let ev = f.parse(&Message::Text(fixture!("order_update.json").into()));
    match &ev[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(u.orderid, "52261003002");
            assert_eq!(
                (u.symbol.as_str(), u.exchange.as_str()),
                ("NIFTY27OCT2625000CE", "NFO")
            );
            assert_eq!(
                (u.action.as_str(), u.pricetype.as_str(), u.product.as_str()),
                ("SELL", "SL", "NRML")
            );
            assert_eq!(
                (
                    u.order_status.as_str(),
                    u.filled_quantity,
                    u.pending_quantity
                ),
                ("open", 50, 25)
            );
            assert_eq!(u.average_price, 118.55);
            assert_eq!(u.rejection_reason, "");
        }
        other => panic!("unexpected {:?}", other),
    }
    // PascalCase fallback and a rejection reason.
    let pascal = json!({"Type": "order_alert", "Data": {
        "OrderNo": "9", "Status": "Rejected", "Quantity": 1, "TradedQty": 0,
        "Exchange": "BSE", "Segment": "E", "SecurityId": "500112", "TxnType": "B",
        "OrderType": "SLM", "Product": "C", "ReasonDescription": "Insufficient funds"}});
    match &f.parse(&Message::Text(pascal.to_string()))[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!((u.symbol.as_str(), u.exchange.as_str()), ("SBIN", "BSE"));
            assert_eq!(
                (u.order_status.as_str(), u.pricetype.as_str()),
                ("rejected", "SL-M")
            );
            assert_eq!(u.rejection_reason, "Insufficient funds");
        }
        other => panic!("unexpected {:?}", other),
    }
    assert!(f
        .parse(&Message::Text(r#"{"Type":"login_ack"}"#.into()))
        .is_empty());
    assert!(f.parse(&Message::Text("not json".into())).is_empty());
}

#[test]
fn identity_and_capabilities() {
    let b = DhanBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "dhan");
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "tokenId" });
    assert_eq!(b.capabilities().depth_levels, &[5, 20]);
    assert!(b.capabilities().gtt);
    assert_eq!(b.timeframe_map().len(), 6);
    assert!(b.supported_exchanges().contains(&Exchange::Nco));
}

#[test]
fn feed_state_is_released_with_its_subscriptions() {
    // Resource hygiene: 300 subscribe / tick / unsubscribe cycles leave
    // nothing behind in the feeds' maps.
    let mut f = DhanFeed::with_url("wss://x", "t", "c");
    let mut d20 = Dhan20DepthFeed::with_url("wss://d", "t", "c");
    for i in 0..300u32 {
        let s = FeedSubscription {
            token: (100_000 + i).to_string(),
            ..sub("SBIN", "NSE", FeedMode::Quote)
        };
        f.subscribe_frames(std::slice::from_ref(&s));
        d20.subscribe_frames(std::slice::from_ref(&s));
        let mut oi = header(5, 4, 1, 100_000 + i);
        oi.extend(7u32.to_le_bytes());
        f.parse(&Message::Binary(oi));
        // A lone bid half waits for its ask.
        let mut half = Vec::new();
        half.extend(332u16.to_le_bytes());
        half.extend([41u8, 1]);
        half.extend((100_000 + i).to_le_bytes());
        half.extend([0u8; 4]);
        half.extend([0u8; 320]);
        d20.parse(&Message::Binary(half));
        f.unsubscribe_frames(std::slice::from_ref(&s));
        d20.unsubscribe_frames(std::slice::from_ref(&s));
    }
    assert_eq!(f.subscription_count(), 0);
    assert_eq!(d20.sizes(), (0, 0));
    // Packets for unknown instruments allocate nothing.
    let mut half = Vec::new();
    half.extend(332u16.to_le_bytes());
    half.extend([51u8, 1]);
    half.extend(999u32.to_le_bytes());
    half.extend([0u8; 324]);
    d20.parse(&Message::Binary(half));
    assert_eq!(d20.sizes(), (0, 0));
}
