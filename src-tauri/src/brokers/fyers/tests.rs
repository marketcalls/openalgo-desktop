//! Fyers mapping tests against payloads built from the web code's field
//! usage and the Fyers API v3 docs (`src-tauri/tests/fixtures/brokers/fyers/`,
//! no account data).

use super::data::{
    chunk_days, depth_to_book, depth_to_quote, effective_range, fyers_resolution, parse_candles,
    parse_quotes, values_to_quote, wants_oi, FyersDepth,
};
use super::funds::{funds_from_limits, m2m, FundLimit};
use super::gtt::{apply_mpp, gtt_ok, map_gtt_book, modify_body, order_info, place_body};
use super::mapping::*;
use super::master_contract::*;
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use base64::Engine;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/fyers/", $name))
    };
}

fn json_fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn index_names() -> HashMap<String, String> {
    serde_json::from_str(fixture!("index_hsm_mapping.json")).unwrap()
}

fn rows() -> Vec<SymToken> {
    let mut files: HashMap<&str, String> = HashMap::new();
    files.insert("NSE_CM", fixture!("NSE_CM.csv").into());
    files.insert("BSE_CM", fixture!("BSE_CM.csv").into());
    files.insert("NSE_FO", fixture!("NSE_FO.csv").into());
    files.insert("BSE_FO", fixture!("BSE_FO.csv").into());
    files.insert("NSE_CD", fixture!("NSE_CD_sym_master.json").into());
    files.insert("MCX_COM", fixture!("MCX_COM_sym_master.json").into());
    parse_all(&files, &index_names()).unwrap()
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
fn master_nse_cash_equities_bonds_and_indices() {
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(sbin.brsymbol, "NSE:SBIN-EQ");
    assert_eq!(sbin.token, "10100000003045");
    assert_eq!(sbin.brexchange, "NSE");
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.name, "STATE BANK OF INDIA");
    assert_eq!((sbin.lot_size, sbin.tick_size), (1, 0.05));
    assert_eq!(sbin.expiry, "");
    // Quoted name with a comma keeps the columns aligned.
    let tata = r.by_symbol("NSE", "TATASTEEL").unwrap();
    assert_eq!(tata.name, "TATA STEEL, LTD");
    assert_eq!(tata.tick_size, 0.01);
    // Type 9 (SME) and -GB bonds of type 2 are NSE EQ; other type 2 is not.
    assert_eq!(r.by_symbol("NSE", "SMECO").unwrap().lot_size, 1600);
    assert_eq!(
        r.by_symbol("NSE", "718GS2033").unwrap().instrument_type,
        "EQ"
    );
    assert!(r.by_symbol("NSE", "SOMEDEBT").is_none());
    // Indices: symbol from the underlying, spaces/hyphens removed, renames.
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "NSE:NIFTY50-INDEX");
    assert_eq!(nifty.brexchange, "NSE");
    assert_eq!(nifty.instrument_type, "EQ");
    // HSM display name from index_hsm_mapping.json ...
    assert_eq!(nifty.name, "Nifty 50");
    assert_eq!(
        r.by_symbol("NSE_INDEX", "BANKNIFTY").unwrap().name,
        "Nifty Bank"
    );
    assert!(r.by_symbol("NSE_INDEX", "NIFTY500").is_some());
    assert!(r.by_symbol("NSE_INDEX", "BHARATBONDAPR30").is_some());
    let mid = r.by_symbol("NSE_INDEX", "NIFTYMIDCAP50").unwrap();
    // ... else the ticker stem.
    assert_eq!(mid.name, "NIFTYMIDCAP50");
    assert!(r.by_symbol("NSE_INDEX", "NIFTYMID50").is_none());
}

#[test]
fn master_bse_cash_and_bse_index_renames() {
    let r = master();
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().brsymbol, "BSE:SBIN-A");
    assert!(r.by_symbol("BSE", "SOMET").is_some());
    assert!(r.by_symbol("BSE", "SOMEMF").is_some());
    assert!(r.by_symbol("BSE", "IGNORED").is_none());
    let sensex = r.by_symbol("BSE_INDEX", "SENSEX").unwrap();
    assert_eq!(sensex.name, "SENSEX");
    assert_eq!(sensex.brexchange, "BSE");
    assert_eq!(
        r.by_symbol("BSE_INDEX", "BSE100").unwrap().brsymbol,
        "BSE:100-INDEX"
    );
    assert_eq!(r.by_symbol("BSE_INDEX", "BSEAUTO").unwrap().name, "AUTO");
    // Unmapped: upper case, no spaces.
    assert!(r.by_symbol("BSE_INDEX", "NEWIDX").is_some());
    assert_eq!(bse_index_symbol("SNXT50"), "BSESENSEXNEXT50");
    assert_eq!(bse_index_symbol("OILGAS"), "BSEOIL&GAS");
    assert_eq!(nse_index_symbol("NIFTY ALPHA 50"), "NIFTYALPHA50");
}

#[test]
fn master_derivatives_are_ddmmmyy_with_master_lots() {
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NSE:NIFTY26OCTFUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 65);
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.brexchange, "NFO");
    assert_eq!(fut.name, "NIFTY 27 Oct 26 FUT");
    let ce = r.by_symbol("NFO", "BANKNIFTY27OCT2671900CE").unwrap();
    assert_eq!((ce.strike, ce.lot_size), (71900.0, 30));
    assert_eq!(ce.instrument_type, "CE");
    let pe = r.by_symbol("NFO", "NIFTY06OCT2622400PE").unwrap();
    assert_eq!(pe.expiry, "06-OCT-26");
    assert_eq!(pe.brsymbol, "NSE:NIFTY26O0622400PE");
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    // A row whose details are not five words is skipped, not mangled.
    assert!(r.by_brsymbol("NFO", "NSE:BROKEN").is_none());
    // BFO: a blank option type is a future.
    let sx = r.by_symbol("BFO", "SENSEX03NOV26FUT").unwrap();
    assert_eq!((sx.instrument_type.as_str(), sx.lot_size), ("FUT", 20));
    assert_eq!(sx.expiry, "03-NOV-26");
    assert!(r.by_symbol("BFO", "SENSEX03NOV2682000CE").is_some());
}

#[test]
fn master_cds_and_mcx_lots_come_from_qty_multiplier() {
    let r = master();
    let usd = r.by_symbol("CDS", "USDINR27OCT26FUT").unwrap();
    assert_eq!((usd.lot_size, usd.tick_size), (1000, 0.0025));
    assert_eq!(usd.brsymbol, "NSE:USDINR26OCTFUT");
    let opt = r.by_symbol("CDS", "USDINR27OCT2683.25CE").unwrap();
    assert_eq!(opt.strike, 83.25);
    assert_eq!(
        r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap().lot_size,
        100
    );
    assert_eq!(
        r.by_symbol("MCX", "NATURALGAS27OCT26FUT").unwrap().lot_size,
        1250
    );
    let c = r.by_symbol("MCX", "CRUDEOIL15OCT268650CE").unwrap();
    assert_eq!((c.lot_size, c.expiry.as_str()), (100, "15-OCT-26"));
    assert!(parse_json_master("not json", "MCX").is_err());
    assert_eq!(rows().len(), 28);
}

#[test]
fn symbol_detail_and_expiry_helpers() {
    assert_eq!(
        reformat_symbol_detail("BANKNIFTY 27 Oct 26 FUT").as_deref(),
        Some("BANKNIFTY27OCT26FUT")
    );
    assert_eq!(
        reformat_symbol_detail("NIFTY 02 Mar 26 30600 CE").as_deref(),
        Some("NIFTY02MAR2630600")
    );
    assert_eq!(reformat_symbol_detail("NIFTY FUT"), None);
    assert_eq!(expiry_from_epoch(1_793_095_200), "27-OCT-26");
    assert_eq!(expiry_from_epoch(0), "");
    let names = HashMap::new();
    assert_eq!(index_feed_name("NSE:NIFTYIT-INDEX", &names), "NIFTYIT");
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let v = json_fixture(fixture!("orders.json"));
    let o = map_orders(rows_of(&v, "orderBook"), &r);
    assert_eq!(o.len(), 7);
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(o[0].exchange, "NSE");
    assert_eq!(o[0].status, "complete");
    assert_eq!(o[0].product, "MIS");
    assert_eq!(o[0].order_type, "MARKET");
    assert_eq!(o[0].average_price, 954.1);
    assert_eq!(o[0].exchange_order_id.as_deref(), Some("1100000012345678"));
    let sl = &o[1];
    assert_eq!(sl.symbol, "NIFTY06OCT2622400PE");
    assert_eq!(sl.exchange, "NFO");
    assert_eq!(sl.status, "trigger pending");
    assert_eq!(sl.order_type, "SL");
    assert_eq!(sl.product, "NRML");
    assert_eq!((sl.price, sl.trigger_price), (120.5, 120.0));
    assert_eq!(sl.pending_quantity, 65);
    assert_eq!(o[2].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(o[2].side, "SELL");
    assert_eq!(o[2].status, "open");
    assert_eq!(o[2].exchange_order_id, None);
    assert_eq!(o[3].status, "rejected");
    assert!(o[3]
        .rejection_reason
        .as_deref()
        .unwrap()
        .contains("Margin Shortfall"));
    assert_eq!(o[4].status, "cancelled");
    assert_eq!(
        (o[4].symbol.as_str(), o[4].exchange.as_str()),
        ("SBIN", "BSE")
    );
    // Not in the master: the Fyers symbol is kept (web behaviour).
    assert_eq!(o[5].symbol, "NSE:NOTINMASTER-EQ");
    assert_eq!(o[5].price, 15.5);
    assert_eq!(o[5].quantity, 2);
    assert_eq!(o[6].status, "unknown");
}

fn rows_of<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Vec<T> {
    super::mapping::rows(v, key)
}

#[test]
fn trades_positions_holdings() {
    let r = master();
    let t = map_trades(
        rows_of(&json_fixture(fixture!("trades.json")), "tradeBook"),
        &r,
    );
    assert_eq!(t[0].symbol, "SBIN");
    assert_eq!(t[0].trade_id, "52605203");
    assert_eq!(t[0].order_id, "26100300000001");
    assert_eq!(t[0].trade_value, 9541.0);
    assert_eq!(t[0].timestamp, "03-Oct-2026 09:15:32");
    assert_eq!(
        (t[1].symbol.as_str(), t[1].side.as_str()),
        ("CRUDEOIL19OCT26FUT", "SELL")
    );
    assert_eq!(t[1].product, "NRML");

    let pv = json_fixture(fixture!("positions.json"));
    let raw: Vec<FyersPosition> = rows_of(&pv, "netPositions");
    let p = map_positions(raw.clone(), &r);
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].average_price, 954.12);
    assert_eq!(p[0].ltp, 955.39);
    assert_eq!(p[0].product, "MIS");
    assert_eq!(p[1].quantity, -65);
    assert_eq!(p[1].exchange, "NFO");
    assert_eq!(p[2].realized_pnl, 12.5);
    let (realised, unrealised) = m2m(&raw);
    assert_eq!(realised, 12.5);
    assert!((unrealised - 451.41).abs() < 1e-9);

    let h = map_holdings(
        rows_of(&json_fixture(fixture!("holdings.json")), "holdings"),
        &r,
    );
    assert_eq!(h.len(), 3);
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].pnl_percentage, 19.26);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    // Zero cost: 0%, never a division error.
    assert_eq!(h[1].pnl_percentage, 0.0);
    assert_eq!(h[1].t1_quantity, 2);
    assert_eq!(
        (h[2].exchange.as_str(), h[2].pnl_percentage),
        ("BSE", -10.0)
    );
}

#[test]
fn funds_use_clear_balance_not_available_balance() {
    let v = json_fixture(fixture!("funds.json"));
    let limits: Vec<FundLimit> = rows_of(&v, "fund_limit");
    let positions: Vec<FyersPosition> =
        rows_of(&json_fixture(fixture!("positions.json")), "netPositions");
    let f = funds_from_limits(&limits, &positions);
    assert!((f.available_cash - 209_293.45).abs() < 1e-6);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.utilised_debits, 45_706.55);
    assert_eq!(f.m2m_realized, 12.5);
    assert!((f.m2m_unrealized - 451.41).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Quotes, depth, history
// ---------------------------------------------------------------------------

fn depth_of(text: &str, br: &str) -> FyersDepth {
    let v = json_fixture(text);
    serde_json::from_value(v["d"][br].clone()).unwrap()
}

#[test]
fn quote_and_depth_from_depth_endpoint() {
    let dsb = depth_of(fixture!("depth.json"), "NSE:SBIN-EQ");
    let q = depth_to_quote(&QuoteKey::new("NSE", "SBIN"), &dsb);
    assert_eq!((q.ltp, q.bid, q.ask), (954.1, 954.05, 954.1));
    assert_eq!((q.bid_qty, q.ask_qty), (120, 77));
    assert_eq!(
        (q.open, q.high, q.low, q.close),
        (951.0, 957.4, 948.2, 950.65)
    );
    assert_eq!(q.volume, 4_823_170);
    assert_eq!(q.change, 3.45);
    let book = depth_to_book(&QuoteKey::new("NSE", "SBIN"), &dsb);
    assert_eq!(book.bids.len(), 5);
    assert_eq!(book.asks.len(), 5);
    assert_eq!(
        book.bids[2],
        DepthLevel {
            price: 953.95,
            quantity: 845,
            orders: 9
        }
    );
    assert_eq!(book.bids[3], DepthLevel::default());
    assert_eq!(
        (book.total_buy_qty, book.total_sell_qty),
        (512_633, 689_415)
    );
    assert_eq!((book.ltq, book.prev_close), (5, 950.65));
    let fno = depth_of(fixture!("depth_fno.json"), "NSE:NIFTY26OCTFUT");
    let q = depth_to_quote(&QuoteKey::new("NFO", "NIFTY27OCT26FUT"), &fno);
    assert_eq!(q.oi, 13_031_500);
}

#[test]
fn bulk_quotes_parse_ok_entries_only() {
    let m = parse_quotes(&json_fixture(fixture!("quotes.json")));
    assert_eq!(m.len(), 3);
    assert!(!m.contains_key("NSE:BAD-EQ"));
    let q = values_to_quote(&QuoteKey::new("NSE", "SBIN"), &m["NSE:SBIN-EQ"], 0);
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close),
        (954.1, 954.05, 954.1, 950.65)
    );
    assert_eq!(q.volume, 4_823_170);
    assert_eq!(q.change, 3.45);
    let f = values_to_quote(
        &QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        &m["NSE:NIFTY26OCTFUT"],
        7,
    );
    assert_eq!(f.oi, 7);
}

#[test]
fn history_resolution_chunks_and_candles() {
    assert_eq!(fyers_resolution("5s").unwrap(), "5S");
    assert_eq!(fyers_resolution("1h").unwrap(), "60");
    assert_eq!(fyers_resolution("D").unwrap(), "1D");
    assert!(fyers_resolution("W").is_err());
    assert!(fyers_resolution("1d").is_err());
    assert_eq!(chunk_days("1D"), 300);
    assert_eq!(chunk_days("15S"), 25);
    assert_eq!(chunk_days("5"), 60);
    assert!(wants_oi("NFO") && wants_oi("CDS") && !wants_oi("NSE") && !wants_oi("BCD"));
    let today = d(2026, 10, 3);
    // End clamped to today; seconds data to the last 30 days.
    assert_eq!(
        effective_range("1", d(2026, 1, 1), d(2027, 1, 1), today).unwrap(),
        (d(2026, 1, 1), today)
    );
    assert_eq!(
        effective_range("5S", d(2026, 1, 1), today, today).unwrap(),
        (d(2026, 9, 3), today)
    );
    assert!(effective_range("1", d(2026, 10, 5), d(2026, 10, 9), today).is_err());

    let day = json_fixture(fixture!("history_day.json"));
    let rows: Vec<Vec<Value>> = serde_json::from_value(day["candles"].clone()).unwrap();
    let c = crate::brokers::common::history::sort_dedupe(parse_candles(&rows, false));
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1_790_726_400);
    assert_eq!(c[1].close, 950.65);
    let oi = json_fixture(fixture!("history_oi.json"));
    let rows: Vec<Vec<Value>> = serde_json::from_value(oi["candles"].clone()).unwrap();
    let c = parse_candles(&rows, true);
    assert_eq!(c[1].oi, 13_031_500);
    assert_eq!(parse_candles(&rows, false)[1].oi, 0);
}

// ---------------------------------------------------------------------------
// Orders and margin
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: &str, product: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "SELL".into(),
        quantity: 65,
        price: 120.5,
        order_type: pricetype.into(),
        product: product.into(),
        validity: "IOC".into(),
        trigger_price: Some(120.0),
        disclosed_quantity: Some(0),
        amo: true,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_order_body_matches_web_transform() {
    let b = place_order_body(&resolved("NIFTY06OCT2622400PE", "NFO", "SL", "NRML"));
    assert_eq!(
        b,
        json!({
            "symbol": "NSE:NIFTY26O0622400PE", "qty": 65, "type": 4, "side": -1,
            "productType": "MARGIN", "limitPrice": 120.5, "stopPrice": 120.0,
            // web hardcodes DAY and never sends AMO
            "validity": "DAY", "disclosedQty": 0, "offlineOrder": false,
            "stopLoss": 0, "takeProfit": 0, "orderTag": "openalgo"
        })
    );
    let m = place_order_body(&resolved("SBIN", "NSE", "SL-M", "MIS"));
    assert_eq!(
        (m["type"].as_i64(), m["productType"].as_str()),
        (Some(3), Some("INTRADAY"))
    );
}

#[test]
fn modify_body_always_carries_five_fields() {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 3,
        price: 950.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("26100300000003", &m, &master()).unwrap();
    assert_eq!(
        modify_order_body(&rm),
        json!({"id": "26100300000003", "qty": 3, "type": 1, "limitPrice": 950.0, "stopPrice": 0.0})
    );
}

#[test]
fn margin_payload_and_response() {
    let r = master();
    let leg = |sym: &str, ex: &str| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action: Action::Buy,
        quantity: 65,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 22000.0,
        trigger_price: 0.0,
    };
    let v = margin_leg(&leg("NIFTY27OCT26FUT", "NFO"), &r).unwrap();
    assert_eq!(
        v,
        json!({"symbol": "NSE:NIFTY26OCTFUT", "qty": 65, "side": 1, "type": 1,
               "productType": "MARGIN", "limitPrice": 22000.0, "stopLoss": 0.0,
               "stopPrice": 0.0, "takeProfit": 0.0})
    );
    assert!(margin_leg(&leg("NOPE", "NFO"), &r).is_none());
    let m = parse_margin(&json_fixture(fixture!("margin.json")));
    assert_eq!(m.total_margin_required, 147_738.056_3);
}

#[test]
fn fyers_errors_are_trader_facing() {
    let v = json_fixture(fixture!("errors.json"));
    let err = |k: &str| {
        let (c, m) = code_message(&v[k]);
        fyers_error(400, c, &m)
    };
    assert_eq!(
        err("token_expired").client_message(),
        "Your Fyers session has expired. Log in to Fyers again."
    );
    assert_eq!(err("invalid_token").code(), "AUTH_ERROR");
    assert!(err("order_rejected")
        .client_message()
        .starts_with("Insufficient fund"));
    assert_eq!(fyers_error(401, 0, "").code(), "AUTH_ERROR");
    assert!(fyers_error(500, 0, "").client_message().contains("refused"));
    let mut h = reqwest::header::HeaderMap::new();
    let base = std::time::Duration::from_secs(1);
    assert_eq!(retry_delay(&h, 2, base), std::time::Duration::from_secs(4));
    h.insert("retry-after", "2".parse().unwrap());
    assert_eq!(retry_delay(&h, 0, base), std::time::Duration::from_secs(2));
    h.insert("x-retry-after-ms", "250".parse().unwrap());
    assert_eq!(
        retry_delay(&h, 0, base),
        std::time::Duration::from_millis(250)
    );
    h.insert("x-retry-after-ms", "600000".parse().unwrap());
    assert_eq!(retry_delay(&h, 0, base), std::time::Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// GTT
// ---------------------------------------------------------------------------

fn gtt(kind: GttTriggerType, pricetype: PriceType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type: kind,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 1,
        pricetype,
        price: 901.0,
        trigger_price: 900.5,
        triggerprice_sl: 880.0,
        stoploss: 879.0,
        triggerprice_tg: 990.0,
        target: 991.0,
        last_price: None,
    }
}

#[test]
fn gtt_bodies_match_web_transform() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let b = place_body(&gtt(GttTriggerType::Single, PriceType::Limit), &row);
    assert_eq!(
        b,
        json!({"side": 1, "symbol": "NSE:SBIN-EQ", "productType": "CNC",
               "orderInfo": {"leg1": {"price": 901.0, "triggerPrice": 900.5, "qty": 1}},
               "orderTag": "openalgo"})
    );
    // OCO: target (above LTP) is leg1, stop-loss leg2.
    let oco = order_info(&gtt(GttTriggerType::Oco, PriceType::Limit));
    assert_eq!(
        oco["leg1"],
        json!({"price": 991.0, "triggerPrice": 990.0, "qty": 1})
    );
    assert_eq!(
        oco["leg2"],
        json!({"price": 879.0, "triggerPrice": 880.0, "qty": 1})
    );
    // Wrong way round: swapped.
    let mut bad = gtt(GttTriggerType::Oco, PriceType::Limit);
    std::mem::swap(&mut bad.triggerprice_sl, &mut bad.triggerprice_tg);
    std::mem::swap(&mut bad.stoploss, &mut bad.target);
    assert_eq!(order_info(&bad)["leg1"]["triggerPrice"], 990.0);
    // SINGLE without trigger_price falls back to the sl, then tg trigger.
    let mut s = gtt(GttTriggerType::Single, PriceType::Limit);
    s.trigger_price = 0.0;
    assert_eq!(order_info(&s)["leg1"]["triggerPrice"], 880.0);
    let m = modify_body(
        "25100300000001",
        &gtt(GttTriggerType::Single, PriceType::Limit),
    );
    assert_eq!(m["id"], "25100300000001");
    assert!(m.get("symbol").is_none());
}

#[test]
fn gtt_market_is_price_protected() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let r = apply_mpp(
        &gtt(GttTriggerType::Single, PriceType::Market),
        &row,
        Some(954.1),
    );
    assert_eq!(r.pricetype, PriceType::Limit);
    assert_eq!(r.price, 958.85);
    // No LTP: the price is sent as a LIMIT unchanged (web logs and goes on).
    let r = apply_mpp(&gtt(GttTriggerType::Single, PriceType::Market), &row, None);
    assert_eq!((r.pricetype, r.price), (PriceType::Limit, 901.0));
    let r = apply_mpp(&gtt(GttTriggerType::Oco, PriceType::Market), &row, None);
    assert_eq!((r.stoploss, r.target), (884.4, 994.95));
}

#[test]
fn gtt_book_active_only_with_oco_legs() {
    let v = json_fixture(fixture!("gtt_book.json"));
    assert!(gtt_ok(&v));
    assert!(!gtt_ok(&json!({"s": "error", "code": 1101})));
    assert!(gtt_ok(&json!({"code": 1103})));
    let book = map_gtt_book(&v, &master());
    assert_eq!(book.len(), 2);
    assert_eq!(book[0].symbol, "SBIN");
    assert_eq!(book[0].trigger_type, "single");
    assert_eq!(book[0].status, "active");
    assert_eq!(book[0].legs[0].product, "CNC");
    assert_eq!(book[0].legs[0].pricetype, "LIMIT");
    let oco = &book[1];
    assert_eq!(oco.symbol, "NIFTY06OCT2622400PE");
    assert_eq!(oco.exchange, "NFO");
    assert_eq!(oco.trigger_type, "two-leg");
    assert_eq!(oco.status, "transit");
    assert_eq!(oco.trigger_prices, vec![111.0, 159.0]);
    assert_eq!(oco.legs.len(), 2);
    assert_eq!(oco.legs[1].price, 110.0);
    assert_eq!(oco.legs[0].action, "SELL");
    assert_eq!(oco.legs[0].product, "NRML");
}

// ---------------------------------------------------------------------------
// Streaming: HSM
// ---------------------------------------------------------------------------

fn jwt(exp: i64) -> String {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        format!(
            r#"{{"hsm_key":"hsmkey123","exp":{},"fy_id":"<USER_ID>"}}"#,
            exp
        )
        .as_bytes(),
    );
    format!("eyJhbGciOiJIUzI1NiJ9.{}.c2ln", payload)
}

fn auth() -> AuthToken {
    AuthToken::new(format!("APPID-100:{}", jwt(4_102_444_800)))
}

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

fn feed() -> HsmFeed {
    HsmFeed::new(HSM_URL, &auth(), master()).unwrap()
}

#[test]
fn hsm_topics_from_fytoken() {
    assert_eq!(
        hsm_topics("10100000003045", "NSE:SBIN-EQ", "", FeedMode::Quote),
        ["sf|nse_cm|3045"]
    );
    assert_eq!(
        hsm_topics("101126102712345", "NSE:NIFTY26OCTFUT", "", FeedMode::Depth),
        ["sf|nse_fo|12345", "dp|nse_fo|12345"]
    );
    assert_eq!(
        hsm_topics(
            "101000000026000",
            "NSE:NIFTY50-INDEX",
            "Nifty 50",
            FeedMode::Depth
        ),
        ["if|nse_cm|Nifty 50"]
    );
    assert_eq!(
        hsm_topics("121000000099", "BSE:NEWIDX-INDEX", "", FeedMode::Ltp),
        ["if|bse_cm|NEWIDX"]
    );
    assert_eq!(
        hsm_topics(
            "1120261019445001",
            "MCX:CRUDEOIL26OCTFUT",
            "",
            FeedMode::Ltp
        ),
        ["sf|mcx_fo|445001"]
    );
    assert!(hsm_topics("9999000000", "X:Y", "", FeedMode::Ltp).is_empty());
    assert!(hsm_topics("1010", "X:Y", "", FeedMode::Ltp).is_empty());
}

#[test]
fn hsm_auth_and_subscribe_frames_are_byte_exact() {
    // fyers_hsm_websocket.py _create_auth_message
    let a = auth_frame("KEY", "SRC");
    let size = 18 + 3 + 3;
    let mut want = ((size - 2) as u16).to_be_bytes().to_vec();
    want.extend([1, 4, 1, 0, 3]);
    want.extend(b"KEY");
    want.extend([2, 0, 1, b'P', 3, 0, 1, 1, 4, 0, 3]);
    want.extend(b"SRC");
    assert_eq!(a, want);
    assert_eq!(a.len(), size);
    // _create_subscription_message
    let s = subscribe_frame(&["sf|nse_cm|3045".to_string()], 11);
    let mut scrips = vec![0u8, 1, 14];
    scrips.extend(b"sf|nse_cm|3045");
    let mut want = ((6 + scrips.len()) as u16).to_be_bytes().to_vec();
    want.extend([4, 2, 1]);
    want.extend((scrips.len() as u16).to_be_bytes());
    want.extend(&scrips);
    want.extend([2, 0, 1, 11]);
    assert_eq!(s, want);
    // _auth_response_ok: [len u16][type 1][count][field id][u16 len]["K"]
    assert!(auth_response_ok(&[0, 6, 1, 1, 1, 0, 1, b'K']));
    assert!(!auth_response_ok(&[0, 6, 1, 1, 1, 0, 1, b'F']));
    assert!(!auth_response_ok(&[0, 6, 1, 1]));
}

#[test]
fn hsm_request_has_no_authorization_header_and_checks_expiry() {
    let f = feed();
    let req = f.ws_request().unwrap();
    assert_eq!(req.uri().to_string(), HSM_URL);
    assert!(req.headers().get("authorization").is_none());
    assert_eq!(req.headers()["user-agent"], "OpenAlgo-HSM/1.0");
    let expired = AuthToken::new(format!("APPID-100:{}", jwt(1_000)));
    assert!(HsmFeed::new(HSM_URL, &expired, master()).is_err());
    assert!(HsmFeed::new(HSM_URL, &AuthToken::new("APPID-100:notajwt"), master()).is_err());
    assert!(HsmFeed::new(HSM_URL, &AuthToken::new("nocolon"), master()).is_err());
}

#[test]
fn hsm_connect_auth_ack_and_subscriptions() {
    let mut f = feed();
    assert!(f.awaits_auth_ack());
    let hello = f.on_connected();
    assert_eq!(
        hello,
        vec![Message::Binary(auth_frame("hsmkey123", HSM_SOURCE))]
    );
    assert_eq!(
        f.parse(&Message::Binary(vec![0, 6, 1, 1, 1, 0, 1, b'K'])),
        vec![FeedEvent::AuthOk]
    );
    assert!(matches!(
        f.parse(&Message::Binary(vec![0, 6, 1, 1, 1, 0, 1, b'X']))[..],
        [FeedEvent::AuthFailed(_)]
    ));
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", FeedMode::Ltp),
    ]);
    assert_eq!(
        frames,
        vec![Message::Binary(subscribe_frame(
            &["sf|nse_cm|3045".into(), "if|nse_cm|Nifty 50".into()],
            11
        ))]
    );
    // Quote -> depth adds only the depth topic.
    let change = f.mode_change_frames(
        &sub("SBIN", "NSE", FeedMode::Quote),
        &sub("SBIN", "NSE", FeedMode::Depth),
    );
    assert_eq!(
        change,
        vec![Message::Binary(subscribe_frame(
            &["dp|nse_cm|3045".into()],
            11
        ))]
    );
    // HSM has no selective unsubscribe: nothing is sent.
    assert!(f
        .unsubscribe_frames(&[sub("SBIN", "NSE", FeedMode::Depth)])
        .is_empty());
    assert!(matches!(f.heartbeat(), Some((d, Message::Ping(_))) if d.as_secs() == 30));
}

/// Type-6 data frame: bytes [7:9] carry the record count
/// (`_parse_data_feed`).
fn data_frame(records: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8, 0, 6, 0, 0, 0, 0];
    v.extend((records.len() as u16).to_be_bytes());
    for r in records {
        v.extend(r);
    }
    v
}

fn vals(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// Snapshot record (`_parse_snapshot_data`): 83, topic id (host order),
/// name; then for sf/dp the fields, 2 skipped bytes, multiplier, precision
/// and three length-prefixed strings.
fn snapshot(id: u16, name: &str, fields: &[i32], mult_prec: Option<(u16, u8)>) -> Vec<u8> {
    let mut r = vec![83u8];
    r.extend(id.to_le_bytes());
    r.push(name.len() as u8);
    r.extend(name.as_bytes());
    r.push(fields.len() as u8);
    r.extend(vals(fields));
    if let Some((m, p)) = mult_prec {
        r.extend([0, 0]);
        r.extend(m.to_be_bytes());
        r.push(p);
        for s in ["NSE", "3045", "SBIN-EQ"] {
            r.push(s.len() as u8);
            r.extend(s.as_bytes());
        }
    }
    r
}

fn update(id: u16, fields: &[i32]) -> Vec<u8> {
    let mut r = vec![85u8];
    r.extend(id.to_le_bytes());
    r.push(fields.len() as u8);
    r.extend(vals(fields));
    r
}

const NA: i32 = HSM_ABSENT;

#[test]
fn hsm_snapshot_and_update_frames_parse_with_scaling() {
    let mut f = feed();
    f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Depth),
        sub("NIFTY", "NSE_INDEX", FeedMode::Depth),
        sub("CRUDEOIL19OCT26FUT", "MCX", FeedMode::Ltp),
    ]);
    // DATA_FIELDS order: ltp, vol, ltt, feed time, bid size, ask size, bid,
    // ask, ltq, tbq, tsq, atp, OI, low, high, yh, yl, lc, uc, open, close.
    let sf = snapshot(
        7,
        "sf|nse_cm|3045",
        &[
            9_541_000,
            4_823_170,
            1_790_992_800,
            1_790_992_801,
            120,
            77,
            9_540_500,
            9_541_000,
            5,
            512_633,
            689_415,
            9_528_700,
            0,
            9_482_000,
            9_574_000,
            NA,
            NA,
            NA,
            NA,
            9_510_000,
            9_506_500,
            NA,
            NA,
        ],
        Some((100, 2)),
    );
    // INDEX_FIELDS: ltp, prev close, feed time, high, low, open.
    let idx = snapshot(
        8,
        "if|nse_cm|Nifty 50",
        &[
            2_251_235,
            2_248_000,
            1_790_992_800,
            2_254_000,
            2_243_055,
            2_245_010,
            NA,
            NA,
        ],
        None,
    );
    let mut depth_fields = vec![NA; 32];
    for i in 0..5 {
        depth_fields[i] = 9_540_500 - (i as i32) * 500; // bid prices
        depth_fields[5 + i] = 9_541_000 + (i as i32) * 500; // ask prices
        depth_fields[10 + i] = 100 + i as i32; // bid sizes
        depth_fields[15 + i] = 200 + i as i32; // ask sizes
        depth_fields[20 + i] = 1 + i as i32; // bid orders
        depth_fields[25 + i] = 2 + i as i32; // ask orders
    }
    depth_fields[4] = 0; // fifth bid empty
    let dp = snapshot(9, "dp|nse_cm|3045", &depth_fields, Some((100, 2)));
    let crude = snapshot(
        10,
        "sf|mcx_fo|445001",
        &[90_300_000, 1, NA, NA],
        Some((100, 2)),
    );
    // A topic this feed never subscribed is parsed past and ignored.
    let other = snapshot(11, "sf|nse_cm|9999", &[1, 2], Some((100, 2)));
    let ev = f.parse(&Message::Binary(data_frame(&[sf, idx, dp, crude, other])));
    let ticks: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    let depths: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Depth(d) => Some(d.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ticks.len(), 3);
    let s = &ticks[0];
    assert_eq!((s.symbol.as_str(), s.exchange.as_str()), ("SBIN", "NSE"));
    // value / multiplier / 100
    assert_eq!(
        (s.ltp, s.open, s.high, s.low, s.close),
        (954.1, 951.0, 957.4, 948.2, 950.65)
    );
    assert_eq!(
        (s.volume, s.last_quantity, s.average_price),
        (4_823_170, 5, 952.87)
    );
    assert_eq!(
        (s.total_buy_quantity, s.total_sell_quantity),
        (512_633, 689_415)
    );
    assert_eq!(s.last_trade_time_ms, 1_790_992_800_000);
    assert_eq!(s.change, 3.45);
    assert_eq!(s.mode, 3);
    let n = &ticks[1];
    assert_eq!(
        (n.symbol.as_str(), n.exchange.as_str()),
        ("NIFTY", "NSE_INDEX")
    );
    // index: value / 100
    assert_eq!(
        (n.ltp, n.close, n.high, n.low, n.open),
        (22512.35, 22480.0, 22540.0, 22430.55, 22450.1)
    );
    assert_eq!(ticks[2].ltp, 9030.0);
    assert_eq!(ticks[2].symbol, "CRUDEOIL19OCT26FUT");
    // Index in depth mode: synthetic depth; scrip: dp levels with a price.
    assert_eq!(depths.len(), 2);
    let syn = &depths[0];
    assert_eq!(syn.symbol, "NIFTY");
    assert_eq!(syn.buy.len(), 5);
    assert_eq!(syn.buy[0].price, 22501.09);
    assert_eq!(syn.sell[0].price, 22523.61);
    assert_eq!((syn.buy[0].quantity, syn.buy[4].quantity), (6000, 2000));
    let book = &depths[1];
    assert_eq!(book.symbol, "SBIN");
    assert_eq!(book.buy.len(), 4);
    assert_eq!(book.sell.len(), 5);
    assert_eq!(
        book.buy[0],
        DepthLevel {
            price: 954.05,
            quantity: 100,
            orders: 1
        }
    );
    assert_eq!(
        book.sell[4],
        DepthLevel {
            price: 954.3,
            quantity: 204,
            orders: 6
        }
    );
    assert_eq!(book.ltp, (954.05 + 954.1) / 2.0);

    // Update (85): positional values on the stored snapshot; absent values
    // keep the old one; unchanged updates emit nothing.
    let ev = f.parse(&Message::Binary(data_frame(&[update(
        7,
        &[9_542_000, 4_823_200],
    )])));
    match &ev[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!((t.ltp, t.volume, t.open), (954.2, 4_823_200, 951.0));
        }
        other => panic!("unexpected {:?}", other),
    }
    assert!(f
        .parse(&Message::Binary(data_frame(&[update(7, &[9_542_000, NA])])))
        .is_empty());
    // Unknown topic id: skipped by its field count, the next record parses.
    let ev = f.parse(&Message::Binary(data_frame(&[
        update(99, &[1, 2, 3]),
        update(8, &[2_252_000]),
    ])));
    assert!(ev
        .iter()
        .any(|e| matches!(e, FeedEvent::Tick(t) if t.symbol == "NIFTY" && t.ltp == 22520.0)));
    // A reconnect forgets topic ids until fresh snapshots arrive.
    f.on_connected();
    assert!(f
        .parse(&Message::Binary(data_frame(&[update(7, &[1])])))
        .is_empty());
    // Truncated frames never panic.
    let full = data_frame(&[snapshot(7, "sf|nse_cm|3045", &[1; 23], Some((100, 2)))]);
    for cut in 0..full.len() {
        let _ = f.parse(&Message::Binary(full[..cut].to_vec()));
    }
}

// ---------------------------------------------------------------------------
// Streaming: order updates and TBT
// ---------------------------------------------------------------------------

#[test]
fn order_updates_normalise_like_web() {
    let r = master();
    let u = parse_order_update(fixture!("order_update.json"), &r).unwrap();
    assert_eq!(u.orderid, "26100300000003");
    assert_eq!(
        (u.symbol.as_str(), u.exchange.as_str()),
        ("CRUDEOIL19OCT26FUT", "MCX")
    );
    assert_eq!((u.action.as_str(), u.pricetype.as_str()), ("SELL", "LIMIT"));
    assert_eq!(u.order_status, "complete");
    assert_eq!(
        (u.quantity, u.filled_quantity, u.pending_quantity),
        (100, 100, 0)
    );
    assert_eq!(u.average_price, 9050.0);
    assert_eq!(u.product, "MARGIN");
    assert_eq!(u.rejection_reason, "");
    let rej = parse_order_update(fixture!("order_update_rejected.json"), &r).unwrap();
    assert_eq!(rej.order_status, "rejected");
    assert_eq!(rej.rejection_reason, "RED:Margin Shortfall");
    assert_eq!(rej.symbol, "NIFTY27OCT26FUT");
    // Acks and heartbeats are not order records.
    assert!(parse_order_update(
        r#"{"code":1605,"message":"Successfully subscribed","s":"ok"}"#,
        &r
    )
    .is_none());
    assert!(parse_order_update("pong", &r).is_none());
    assert_eq!(order_feed_status(4), "open");
    assert_eq!(order_feed_status(7), "expired");

    let mut f = super::streaming::OrderFeed::new(ORDER_WS_URL, &auth(), r).unwrap();
    let req = f.ws_request().unwrap();
    assert!(req.headers()["authorization"]
        .to_str()
        .unwrap()
        .starts_with("APPID-100:"));
    match &f.on_connected()[..] {
        [Message::Text(t)] => assert_eq!(
            serde_json::from_str::<Value>(t).unwrap(),
            json!({"T": "SUB_ORD", "SLIST": ["orders"], "SUB_T": 1})
        ),
        other => panic!("unexpected {:?}", other),
    }
    assert!(matches!(
        &f.parse(&Message::Text(fixture!("order_update.json").into()))[..],
        [FeedEvent::OrderUpdate(_)]
    ));
}

/// Hand-built protobuf (`msg.proto`): varint key = field << 3 | wire type.
mod pb {
    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    pub fn bytes(field: u32, b: &[u8], out: &mut Vec<u8>) {
        varint(u64::from(field << 3 | 2), out);
        varint(b.len() as u64, out);
        out.extend(b);
    }
    pub fn uint(field: u32, v: u64, out: &mut Vec<u8>) {
        varint(u64::from(field << 3), out);
        varint(v, out);
    }
    pub fn wrapper(field: u32, v: u64, out: &mut Vec<u8>) {
        let mut w = Vec::new();
        if v != 0 {
            uint(1, v, &mut w);
        }
        bytes(field, &w, out);
    }
    /// MarketLevel {price=1, qty=2, nord=3, num=4}
    pub fn level(price: u64, qty: u64, nord: u64, num: u64) -> Vec<u8> {
        let mut l = Vec::new();
        wrapper(1, price, &mut l);
        wrapper(2, qty, &mut l);
        wrapper(3, nord, &mut l);
        wrapper(4, num, &mut l);
        l
    }
}

fn tbt_message(
    ticker: &str,
    snapshot: bool,
    bids: &[Vec<u8>],
    asks: &[Vec<u8>],
    tbq: u64,
) -> Vec<u8> {
    // Depth {tbq=1, tsq=2, asks=3, bids=4}
    let mut depth = Vec::new();
    pb::wrapper(1, tbq, &mut depth);
    pb::wrapper(2, 999, &mut depth);
    for a in asks {
        pb::bytes(3, a, &mut depth);
    }
    for b in bids {
        pb::bytes(4, b, &mut depth);
    }
    // MarketFeed {depth=5, feed_time=6, token=8, snapshot=10, ticker=11}
    let mut feed = Vec::new();
    pb::bytes(5, &depth, &mut feed);
    pb::wrapper(6, 1_790_992_800, &mut feed);
    pb::bytes(8, b"10100000003045", &mut feed);
    pb::bytes(11, ticker.as_bytes(), &mut feed);
    // FeedsEntry {key=1, value=2}
    let mut entry = Vec::new();
    pb::bytes(1, b"10100000003045", &mut entry);
    pb::bytes(2, &feed, &mut entry);
    // SocketMessage {type=1, feeds=2, snapshot=3}
    let mut msg = Vec::new();
    pb::uint(1, 6, &mut msg);
    pb::bytes(2, &entry, &mut msg);
    pb::uint(3, u64::from(snapshot), &mut msg);
    msg
}

#[test]
fn tbt_protobuf_decodes_and_accumulates_50_levels() {
    let snap = tbt_message(
        "NSE:SBIN-EQ",
        true,
        &[pb::level(95405, 100, 3, 0), pb::level(95400, 50, 1, 1)],
        &[pb::level(95410, 70, 2, 0), pb::level(95415, 0, 0, 49)],
        5000,
    );
    let m = decode_tbt(&snap).unwrap();
    assert!(m.snapshot && !m.error);
    assert_eq!(m.feeds.len(), 1);
    assert_eq!(m.feeds[0].ticker, "NSE:SBIN-EQ");
    assert_eq!(m.feeds[0].bids[1].num, Some(1));
    // An empty wrapper is present with value 0 (HasField semantics).
    assert_eq!(m.feeds[0].asks[1].qty, Some(0));

    let mut f = TbtFeed::new(TBT_URL, &auth(), master()).unwrap();
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Depth),
        sub("CRUDEOIL19OCT26FUT", "MCX", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 2);
    let first: Value = match &frames[0] {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        _ => panic!("text"),
    };
    assert_eq!(
        first,
        json!({"type": 1, "data": {"subs": 1, "symbols": ["NSE:SBIN-EQ"], "mode": "depth", "channel": "1"}})
    );
    let ev = f.parse(&Message::Binary(snap));
    let d = match &ev[..] {
        [FeedEvent::Depth(d)] => d.clone(),
        other => panic!("unexpected {:?}", other),
    };
    assert_eq!(d.symbol, "SBIN");
    assert_eq!(d.buy.len(), 2);
    assert_eq!(
        d.buy[0],
        DepthLevel {
            price: 954.05,
            quantity: 100,
            orders: 3
        }
    );
    assert_eq!(d.sell.len(), 2);
    assert_eq!(d.sell[1].price, 954.15);
    assert_eq!(d.ltp, 954.08);
    assert_eq!((d.total_buy_quantity, d.total_sell_quantity), (5000, 999));
    // A diff changes only the levels it names.
    let diff = tbt_message("NSE:SBIN-EQ", false, &[pb::level(0, 150, 0, 0)], &[], 0);
    let ev = f.parse(&Message::Binary(diff));
    let d = match &ev[..] {
        [FeedEvent::Depth(d)] => d.clone(),
        other => panic!("unexpected {:?}", other),
    };
    // price wrapper present with 0 clears the price: the level drops out.
    assert_eq!(d.buy.len(), 1);
    assert_eq!(d.buy[0].price, 954.0);
    // Unsubscribe sends subs -1 and frees the book.
    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", FeedMode::Depth)]);
    assert_eq!(un.len(), 1);
    assert!(f
        .parse(&Message::Binary(tbt_message(
            "NSE:SBIN-EQ",
            true,
            &[],
            &[],
            1
        )))
        .is_empty());
    assert_eq!(
        f.parse(&Message::Text("pong".into())),
        vec![FeedEvent::Heartbeat]
    );
    assert!(decode_tbt(&[0x0a, 0xff]).is_none());
    assert_eq!(f.supported_depth_levels(), &[50]);
    let req = f.ws_request().unwrap();
    assert!(req.headers().get("authorization").is_some());
}
