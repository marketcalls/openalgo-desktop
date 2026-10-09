//! Angel mapping tests against SmartAPI payloads built from the web code's
//! field usage and the SmartAPI documentation
//! (`src-tauri/tests/fixtures/brokers/angel/`; placeholders only).

use super::data::{
    angel_epoch, angel_interval, api_exchange, chunk_days, chunk_window, merge_oi, parse_candles,
    parse_oi, quote_body, to_depth, to_quote, AngelOi, AngelQuoteData,
};
use super::funds::{funds_from_rms, m2m, margin_positions, parse_margin, AngelRms};
use super::gtt::{
    apply_mpp, create_bodies, decode_trigger_id, encode_trigger_id, gtt_product, legs,
    map_gtt_book, modify_bodies, reverse_gtt_product, AngelRule,
};
use super::mapping::*;
use super::master_contract::{convert_expiry, index_symbol, parse_master, strike_text};
use super::streaming::{
    exchange_type, order_status_from_code, order_status_from_text, price_divisor, AngelFeed,
    AngelOrderFeed, LTP_LEN, QUOTE_LEN, SNAP_LEN,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/angel/", $name))
    };
}

fn data<T: serde::de::DeserializeOwned>(json: &str) -> T {
    let env: Envelope<T> = serde_json::from_str(json).unwrap();
    assert!(env.status);
    env.data.unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_master(fixture!("master.json").as_bytes()).unwrap());
    r
}

fn broker() -> AngelBroker {
    AngelBroker::new(master())
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_keeps_every_row_and_strips_series() {
    let rows = parse_master(fixture!("master.json").as_bytes()).unwrap();
    assert_eq!(rows.len(), 28);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (sbin.brsymbol.as_str(), sbin.token.as_str()),
        ("SBIN-EQ", "3045")
    );
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(sbin.expiry, "");
    assert_eq!(sbin.strike, -0.01);
    assert_eq!(sbin.instrument_type, "");
    assert_eq!(r.by_symbol("NSE", "IDEA").unwrap().brsymbol, "IDEA-BE");
    assert_eq!(r.by_symbol("BSE", "SBIN").unwrap().token, "500112");
    // No exchange filter, as on the web.
    assert_eq!(r.by_symbol("NCDEX", "GUARSEED").unwrap().lot_size, 5);
}

#[test]
fn master_index_symbols_from_name_with_overrides() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(
        (nifty.brsymbol.as_str(), nifty.brexchange.as_str()),
        ("Nifty 50", "NSE")
    );
    assert_eq!(nifty.instrument_type, "AMXIDX");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "FINNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "MIDCPNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "NIFTYIT").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "BSE500").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX50").is_some());
    let mcx = r.by_symbol("MCX_INDEX", "MCXCOMPDEX").unwrap();
    assert_eq!(mcx.brexchange, "MCX");
    assert_eq!(index_symbol("S&P BSE SENSEX", "BSE_INDEX"), "BSESENSEX");
    assert_eq!(index_symbol("Nifty Pvt Bank", "NSE_INDEX"), "NIFTYPVTBANK");
}

#[test]
fn master_derivatives_per_exchange() {
    let r = master();
    // NFO keeps Angel's own symbol.
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(
        (fut.expiry.as_str(), fut.instrument_type.as_str()),
        ("27-OCT-26", "FUT")
    );
    assert_eq!(fut.lot_size, 75);
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (25000.0, "CE"));
    assert_eq!(
        r.by_symbol("NFO", "NIFTY27OCT2625000PE")
            .unwrap()
            .instrument_type,
        "PE"
    );
    assert_eq!(
        r.by_symbol("NFO", "VEDL27OCT26292.5CE").unwrap().strike,
        292.5
    );
    // BFO rebuilt from name + DDMMMYY.
    let bf = r.by_symbol("BFO", "SENSEX29OCT26FUT").unwrap();
    assert_eq!(bf.brsymbol, "SENSEX26OCTFUT");
    let bo = r.by_symbol("BFO", "SENSEX08OCT2682000PE").unwrap();
    assert_eq!(
        (bo.brsymbol.as_str(), bo.instrument_type.as_str()),
        ("SENSEX2610082000PE", "PE")
    );
    assert!(r.by_symbol("BFO", "RELIANCE29OCT26FUT").is_some());
    assert!(r.by_symbol("BFO", "RELIANCE29OCT261330CE").is_some());
    // MCX
    let crude = r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!((crude.lot_size, crude.tick_size), (100, 1.0));
    let copt = r.by_symbol("MCX", "CRUDEOIL15OCT268650CE").unwrap();
    assert_eq!((copt.strike, copt.instrument_type.as_str()), (8650.0, "CE"));
    // CDS: options strike /100 then /100000.
    assert!(r.by_symbol("CDS", "USDINR27OCT26FUT").is_some());
    let usd = r.by_symbol("CDS", "USDINR27OCT2683.25CE").unwrap();
    assert_eq!(usd.strike, 83.25);
    assert_eq!(usd.tick_size, 0.0025);
    assert_eq!(
        r.by_symbol("CDS", "726GS203227OCT26FUT")
            .unwrap()
            .instrument_type,
        "FUT"
    );
}

#[test]
fn expiry_and_strike_text_match_python() {
    assert_eq!(convert_expiry("19MAR2024"), "19-MAR-24");
    assert_eq!(convert_expiry("27oct2026"), "27-OCT-26");
    assert_eq!(convert_expiry(""), "");
    assert_eq!(convert_expiry("garbage"), "GARBAGE");
    assert_eq!(strike_text(25000.0), "25000");
    assert_eq!(strike_text(83.25), "83.25");
    assert_eq!(strike_text(292.5), "292.5");
    // The web's regex also drops an interior ".0".
    assert_eq!(strike_text(100.05), "1005");
}

// ---------------------------------------------------------------------------
// Orders and books
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: &str, product: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 75,
        price: 151.5,
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: if pricetype.starts_with("SL") {
            Some(150.0)
        } else {
            None
        },
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn enum_maps_match_transform_data() {
    assert_eq!(map_variety("SL-M"), "STOPLOSS");
    assert_eq!(map_variety("LIMIT"), "NORMAL");
    assert_eq!(map_order_type("SL"), "STOPLOSS_LIMIT");
    assert_eq!(map_order_type("SL-M"), "STOPLOSS_MARKET");
    assert_eq!(map_order_type("X"), "MARKET");
    assert_eq!(map_product_type("CNC"), "DELIVERY");
    assert_eq!(map_product_type("NRML"), "CARRYFORWARD");
    assert_eq!(map_product_type("MIS"), "INTRADAY");
    assert_eq!(reverse_map_product_type("CARRYFORWARD"), Some("NRML"));
    assert_eq!(reverse_map_product_type("MARGIN"), None);
    assert_eq!(oa_product("NSE", "DELIVERY"), "CNC");
    assert_eq!(oa_product("NFO", "CARRYFORWARD"), "NRML");
    assert_eq!(oa_product("NFO", "INTRADAY"), "MIS");
    assert_eq!(oa_product("NSE", "MARGIN"), "MARGIN");
    assert_eq!(oa_pricetype("STOPLOSS_MARKET"), "SL-M");
    assert_eq!(oa_pricetype("LIMIT"), "LIMIT");
}

#[test]
fn place_body_matches_web_payload() {
    let b = place_order_body(
        &resolved("NIFTY27OCT2625000CE", "NFO", "SL", "NRML"),
        "oa0123456789abcdef",
    );
    assert_eq!(b["ordertag"], "oa0123456789abcdef");
    assert_eq!(b["variety"], "STOPLOSS");
    assert_eq!(b["tradingsymbol"], "NIFTY27OCT2625000CE");
    assert_eq!(b["symboltoken"], "43210");
    assert_eq!(b["transactiontype"], "BUY");
    assert_eq!(b["exchange"], "NFO");
    assert_eq!(b["ordertype"], "STOPLOSS_LIMIT");
    assert_eq!(b["producttype"], "CARRYFORWARD");
    assert_eq!(b["duration"], "DAY");
    assert_eq!(b["price"], "151.5");
    assert_eq!(b["triggerprice"], "150");
    assert_eq!(b["stoploss"], "150");
    assert_eq!(b["squareoff"], "0");
    assert_eq!(b["quantity"], "75");
    // MARKET: trigger is "0", never null.
    let m = place_order_body(&resolved("SBIN", "NSE", "MARKET", "CNC"), "oa0");
    assert_eq!(m["triggerprice"], "0");
    assert_eq!(m["variety"], "NORMAL");
    assert_eq!(m["tradingsymbol"], "SBIN-EQ");
    assert_eq!(m["producttype"], "DELIVERY");
}

#[test]
fn modify_and_cancel_bodies_are_complete() {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "SL".into(),
        quantity: 5,
        price: 905.0,
        trigger_price: 900.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("261003000000001", &m, &master()).unwrap();
    let b = modify_order_body(&rm);
    for k in [
        "variety",
        "orderid",
        "ordertype",
        "producttype",
        "duration",
        "price",
        "quantity",
        "tradingsymbol",
        "symboltoken",
        "exchange",
        "disclosedquantity",
        "stoploss",
    ] {
        assert!(!b[k].is_null(), "{} missing", k);
    }
    assert_eq!(b["variety"], "STOPLOSS");
    assert_eq!(b["ordertype"], "STOPLOSS_LIMIT");
    assert_eq!(b["producttype"], "INTRADAY");
    assert_eq!(b["tradingsymbol"], "SBIN-EQ");
    assert_eq!(b["symboltoken"], "3045");
    assert_eq!(b["stoploss"], "900");
    assert_eq!(b["duration"], "DAY");
    let c = cancel_order_body("42");
    assert_eq!(c, serde_json::json!({"variety": "NORMAL", "orderid": "42"}));
}

#[test]
fn order_book_maps_symbols_by_token_and_keeps_lowercase_status() {
    let o = map_orders(data(fixture!("order_book.json")), &master());
    assert_eq!(o.len(), 6);
    assert_eq!(o[0].symbol, "SBIN");
    assert_eq!(
        (o[0].status.as_str(), o[0].product.as_str()),
        ("open", "CNC")
    );
    assert_eq!(o[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(o[1].status, "trigger pending");
    assert_eq!(o[1].order_type, "SL");
    assert_eq!(o[1].product, "NRML");
    assert_eq!(o[1].trigger_price, 150.5);
    assert_eq!(o[2].status, "complete");
    assert_eq!((o[2].product.as_str(), o[2].average_price), ("MIS", 9008.0));
    assert_eq!(o[3].status, "rejected");
    assert!(o[3]
        .rejection_reason
        .as_deref()
        .unwrap()
        .starts_with("Insufficient funds"));
    assert_eq!(o[3].exchange_timestamp, None);
    assert_eq!(o[4].status, "cancelled");
    // Unknown token keeps Angel's symbol.
    assert_eq!(o[5].symbol, "UNKNOWN-EQ");
    assert_eq!(o[5].status, "open pending");
}

#[test]
fn trade_book_uses_fill_fields() {
    let t = map_trades(data(fixture!("trade_book.json")), &master());
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!((t[0].quantity, t[0].average_price), (100, 9008.0));
    assert_eq!(t[0].trade_value, 900_800.0);
    assert_eq!(t[0].timestamp, "09:42:37");
    assert_eq!(t[0].trade_id, "50001");
    assert_eq!(
        (t[1].symbol.as_str(), t[1].product.as_str()),
        ("SBIN", "CNC")
    );
}

#[test]
fn positions_and_funds_m2m() {
    let p = map_positions(data(fixture!("positions.json")), &master());
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].quantity),
        ("CRUDEOIL19OCT26FUT", 100)
    );
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].product, "NRML");
    assert_eq!(p[2].symbol, "SBIN");
    assert_eq!(p[2].product, "MIS");
    let (realised, unrealised) = m2m(&p);
    assert_eq!(realised, 12.5);
    assert_eq!(unrealised, 2200.0 + 506.25);
    let f = funds_from_rms(&data::<AngelRms>(fixture!("rms.json")));
    // web comment: 2198702.25 - 2042102.03 = 156600.22 collateral;
    // 2198702.25 + 1697.17 - 156600.22 = 2043799.20 free cash.
    assert!((f.collateral - 156_600.22).abs() < 1e-6);
    assert!((f.available_cash - 2_043_799.20).abs() < 1e-6);
    assert_eq!(f.utilised_debits, 1697.17);
}

#[test]
fn holdings_tolerate_nulls_force_cnc_and_carry_totals() {
    let p: AngelPortfolio = data(fixture!("holdings.json"));
    let stats = p.stats();
    assert_eq!(stats.totalholdingvalue, 9541.0);
    assert_eq!(stats.totalpnlpercentage, 14.95);
    let h = map_holdings(p, &master());
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].t1_quantity, 2);
    assert_eq!(h[0].pnl_percentage, 19.26);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    assert_eq!((h[1].ltp, h[1].pnl), (0.0, 0.0));
    assert!(h.iter().all(|x| x.product == "CNC"));
    // An empty demat answers holdings: null.
    let empty: AngelPortfolio =
        serde_json::from_str(r#"{"holdings":null,"totalholding":null}"#).unwrap();
    assert_eq!(empty.stats().totalinvvalue, 0.0);
    assert!(map_holdings(empty, &master()).is_empty());
}

#[test]
fn angel_errors_are_trader_facing() {
    let v: serde_json::Value = serde_json::from_str(fixture!("errors.json")).unwrap();
    let env =
        |k: &str| -> Envelope<serde_json::Value> { serde_json::from_value(v[k].clone()).unwrap() };
    let e = env("token_expired");
    assert_eq!(angel_error(&e.errorcode, &e.message).code(), "AUTH_ERROR");
    let e = env("insufficient_funds");
    assert!(angel_error(&e.errorcode, &e.message)
        .client_message()
        .starts_with("Insufficient funds"));
    // modify answers status as the string "true".
    assert!(env("modify_ok").status);
    assert!(!env("login_failed").status);
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quotes_and_depth() {
    let d: AngelQuoteData = data(fixture!("quote.json"));
    let rows = d.fetched.unwrap();
    let sbin = to_quote(&QuoteKey::new("NSE", "SBIN"), &rows[0]);
    assert_eq!((sbin.ltp, sbin.bid, sbin.ask), (954.1, 954.05, 954.1));
    assert_eq!((sbin.bid_qty, sbin.ask_qty), (120, 77));
    assert_eq!((sbin.close, sbin.volume), (950.65, 4_823_170));
    assert_eq!(sbin.change, 3.45);
    let nifty = to_quote(&QuoteKey::new("NSE_INDEX", "NIFTY"), &rows[1]);
    assert_eq!(
        (nifty.symbol.as_str(), nifty.exchange.as_str()),
        ("NIFTY", "NSE_INDEX")
    );
    assert_eq!((nifty.bid, nifty.ask), (0.0, 0.0));
    let opt = to_quote(&QuoteKey::new("NFO", "NIFTY27OCT2625000CE"), &rows[2]);
    assert_eq!(opt.oi, 4_567_800);
    let depth = to_depth(&QuoteKey::new("NSE", "SBIN"), &rows[0]);
    assert_eq!((depth.bids.len(), depth.asks.len()), (5, 5));
    assert_eq!(depth.bids[2].orders, 9);
    assert_eq!(depth.bids[3], DepthLevel::default());
    assert_eq!(
        (depth.total_buy_qty, depth.total_sell_qty),
        (512_633, 689_415)
    );
    assert_eq!((depth.ltq, depth.prev_close), (5, 950.65));
}

#[test]
fn quote_body_groups_tokens_by_api_exchange() {
    assert_eq!(api_exchange("NSE_INDEX"), "NSE");
    assert_eq!(api_exchange("BSE_INDEX"), "BSE");
    assert_eq!(api_exchange("MCX_INDEX"), "MCX");
    assert_eq!(api_exchange("NFO"), "NFO");
    let b = quote_body(&[
        ("NSE".into(), "3045".into()),
        ("NFO".into(), "43210".into()),
        ("NSE".into(), "99926000".into()),
    ]);
    assert_eq!(
        b,
        serde_json::json!({"mode": "FULL", "exchangeTokens": {"NFO": ["43210"], "NSE": ["3045", "99926000"]}})
    );
}

#[test]
fn history_candles_oi_and_chunks() {
    let rows: Vec<Vec<serde_json::Value>> = data(fixture!("candles_minute.json"));
    let c = crate::brokers::common::history::sort_dedupe(parse_candles(&rows, false));
    assert_eq!(c.len(), 2);
    // 2026-10-01T09:15+05:30 = 03:45Z
    assert_eq!(c[0].timestamp, 1_790_826_300);
    assert_eq!(c[1].close, 161.0);
    let oi: Vec<AngelOi> = data(fixture!("oi_data.json"));
    let mut c = c;
    merge_oi(&mut c, &parse_oi(&oi, false));
    assert_eq!((c[0].oi, c[1].oi), (4_500_000, 4_512_750));
    merge_oi(&mut c, &HashMap::new());
    assert_eq!(c[0].oi, 0);
    let day: Vec<Vec<serde_json::Value>> = data(fixture!("candles_day.json"));
    let d = parse_candles(&day, true);
    // 2026-09-29T00:00+05:30 + 5:30 = 2026-09-29T00:00Z
    assert_eq!(d[0].timestamp, 1_790_640_000);
    // A timestamp without an offset is IST wall-clock, not the host's zone
    // (web _angel_timestamps_to_epoch, #2176).
    assert_eq!(angel_epoch("2026-10-01T09:15:00"), Some(1_790_826_300));
    assert_eq!(angel_epoch("2026-10-01 09:15"), Some(1_790_826_300));
    assert_eq!(
        angel_epoch("2026-10-01T09:15:00+05:30"),
        Some(1_790_826_300)
    );
    assert_eq!(angel_epoch("2026-10-01T04:15:00Z"), Some(1_790_828_100));
    assert_eq!(angel_epoch("not a time"), None);
    assert_eq!(d[1].volume, 8_700_000);
    assert_eq!(angel_interval("1h").unwrap(), "ONE_HOUR");
    assert_eq!(angel_interval("D").unwrap(), "ONE_DAY");
    assert!(angel_interval("2h")
        .unwrap_err()
        .client_message()
        .starts_with("Interval 2h is not supported by Angel One"));
    assert_eq!(
        [
            chunk_days("1m"),
            chunk_days("3m"),
            chunk_days("5m"),
            chunk_days("15m"),
            chunk_days("1h"),
            chunk_days("D")
        ],
        [30, 60, 100, 200, 400, 2000]
    );
    let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
    let now = d(2026, 10, 3).and_hms_opt(11, 42, 17).unwrap();
    assert_eq!(
        chunk_window(d(2026, 9, 1), d(2026, 9, 30), now),
        (
            "2026-09-01 00:00".to_string(),
            "2026-09-30 23:59".to_string()
        )
    );
    assert_eq!(
        chunk_window(d(2026, 10, 1), d(2026, 10, 3), now).1,
        "2026-10-03 11:42"
    );
}

// ---------------------------------------------------------------------------
// Margin
// ---------------------------------------------------------------------------

#[test]
fn margin_payload_and_response() {
    let b = broker();
    let leg = |sym: &str, ex: &str| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 150.0,
        trigger_price: 0.0,
    };
    let p = margin_positions(&b, &[leg("NIFTY27OCT2625000CE", "NFO"), leg("NOPE", "NFO")]);
    assert_eq!(p.len(), 1);
    assert_eq!(
        p[0],
        serde_json::json!({"exchange":"NFO","qty":75,"price":150.0,"productType":"CARRYFORWARD","token":"43210","tradeType":"SELL","orderType":"LIMIT"})
    );
    let r = parse_margin(&data::<serde_json::Value>(fixture!("margin_batch.json")));
    assert_eq!(r.total_margin_required, 191_119.2);
    assert_eq!(r.span_margin, 112_760.0);
    assert_eq!(r.exposure_margin, 0.0);
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
        last_price: Some(954.1),
    }
}

#[test]
fn gtt_bodies_single_and_oco_legs() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let single = create_bodies(&gtt(GttTriggerType::Single, PriceType::Limit), &row);
    assert_eq!(single.len(), 1);
    assert_eq!(
        single[0].1,
        serde_json::json!({"tradingsymbol":"SBIN-EQ","symboltoken":"3045","exchange":"NSE","transactiontype":"BUY","producttype":"DELIVERY","price":"901","qty":"1","triggerprice":"900.5","disclosedqty":"0","timeperiod":365})
    );
    let oco = create_bodies(&gtt(GttTriggerType::Oco, PriceType::Limit), &row);
    assert_eq!(oco.len(), 2);
    assert_eq!((oco[0].0, oco[1].0), ("SL", "TG"));
    assert_eq!(oco[0].1["triggerprice"], "880");
    assert_eq!(oco[1].1["price"], "991");
    let ids = vec!["1234567".to_string(), "1234568".to_string()];
    let m = modify_bodies(&gtt(GttTriggerType::Oco, PriceType::Limit), "3045", &ids);
    assert_eq!(m[1].1["id"], "1234568");
    assert!(m[0].1.get("tradingsymbol").is_none());
    assert_eq!(encode_trigger_id(&ids), "1234567-1234568");
    assert_eq!(decode_trigger_id("1234567-1234568"), ids);
    assert_eq!(decode_trigger_id("-"), Vec::<String>::new());
    // Single trigger falls back to the SL then TG trigger.
    let mut g = gtt(GttTriggerType::Single, PriceType::Limit);
    g.trigger_price = 0.0;
    assert_eq!(legs(&g)[0].1, 880.0);
    assert_eq!(gtt_product("NRML"), "MARGIN");
    assert_eq!(gtt_product("CNC"), "DELIVERY");
    assert_eq!(reverse_gtt_product("MARGIN"), "NRML");
    assert_eq!(reverse_gtt_product("DELIVERY"), "CNC");
}

#[test]
fn gtt_market_becomes_protected_limit() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let r = apply_mpp(&gtt(GttTriggerType::Single, PriceType::Market), &row, 954.1);
    assert_eq!(r.pricetype, PriceType::Limit);
    // 954.1 * 1.005 = 958.8705 -> tick 0.05 -> 958.85
    assert_eq!(r.price, 958.85);
    let r = apply_mpp(&gtt(GttTriggerType::Oco, PriceType::Market), &row, 0.0);
    assert_eq!((r.stoploss, r.target), (884.4, 994.95));
}

#[test]
fn gtt_book_is_active_only() {
    let rules: Vec<AngelRule> = data(fixture!("gtt_rule_list.json"));
    let book = map_gtt_book(rules, &master());
    assert_eq!(book.len(), 2);
    assert_eq!(book[0].trigger_id, "1234567");
    assert_eq!(book[0].symbol, "SBIN");
    assert_eq!(book[0].trigger_prices, vec![900.5]);
    assert_eq!(book[0].legs[0].product, "CNC");
    assert_eq!(book[0].legs[0].pricetype, "LIMIT");
    assert_eq!(book[1].status, "active");
    assert_eq!(book[1].legs[0].product, "NRML");
    assert_eq!(book[1].legs[0].quantity, 2);
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

fn feed() -> AngelFeed {
    AngelFeed::new(
        streaming::WS_URL,
        "jwt.token.sig",
        "apikey",
        "<USER_ID>",
        "feedtok",
        master(),
    )
}

/// A SmartStream packet per `smartWebSocketV2.py:560-636` (little-endian).
fn packet(mode: u8, et: u8, token: &str, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len];
    p[0] = mode;
    p[1] = et;
    p[2..2 + token.len()].copy_from_slice(token.as_bytes());
    p[27..35].copy_from_slice(&77i64.to_le_bytes()); // sequence
    p[35..43].copy_from_slice(&1_790_916_300_000i64.to_le_bytes()); // exch ts ms
    p
}

fn put_i64(p: &mut [u8], o: usize, v: i64) {
    p[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_f64(p: &mut [u8], o: usize, v: f64) {
    p[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn exchange_types_and_divisors() {
    assert_eq!(exchange_type("NSE"), 1);
    assert_eq!(exchange_type("NFO"), 2);
    assert_eq!(exchange_type("BSE"), 3);
    assert_eq!(exchange_type("BFO"), 4);
    assert_eq!(exchange_type("MCX"), 5);
    assert_eq!(exchange_type("NCDEX"), 7);
    assert_eq!(exchange_type("CDS"), 13);
    assert_eq!(exchange_type("???"), 1);
    assert_eq!(price_divisor(1), 100.0);
    assert_eq!(price_divisor(13), 10_000_000.0);
}

#[test]
fn subscribe_frames_per_mode_from_brexchange() {
    let mut f = feed();
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", FeedMode::Quote),
        sub("SENSEX", "BSE_INDEX", FeedMode::Ltp),
        sub("CRUDEOIL19OCT26FUT", "MCX", FeedMode::Depth),
    ]);
    let v: Vec<serde_json::Value> = frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            _ => panic!("text frames"),
        })
        .collect();
    assert_eq!(v.len(), 3);
    assert_eq!(
        v[0],
        serde_json::json!({"correlationID":"openalgo","action":1,"params":{"mode":1,"tokenList":[{"exchangeType":3,"tokens":["99919000"]}]}})
    );
    assert_eq!(
        v[1]["params"]["tokenList"],
        serde_json::json!([{"exchangeType":1,"tokens":["3045","99926000"]}])
    );
    assert_eq!(v[2]["params"]["mode"], 3);
    assert_eq!(v[2]["params"]["tokenList"][0]["exchangeType"], 5);
    assert_eq!(f.registered(), 4);
    // Unsubscribe uses the mode it was subscribed with and frees the entry.
    let un = f.unsubscribe_frames(&[sub("CRUDEOIL19OCT26FUT", "MCX", FeedMode::Depth)]);
    let u: serde_json::Value = match &un[0] {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        _ => panic!(),
    };
    assert_eq!(
        (u["action"].as_i64(), u["params"]["mode"].as_i64()),
        (Some(0), Some(3))
    );
    assert_eq!(f.registered(), 3);
    // Mode change: unsubscribe old, subscribe new; registry stays bounded.
    let ch = f.mode_change_frames(
        &sub("SBIN", "NSE", FeedMode::Quote),
        &sub("SBIN", "NSE", FeedMode::Depth),
    );
    assert_eq!(ch.len(), 2);
    assert_eq!(f.registered(), 3);
}

#[test]
fn feed_handshake_uses_raw_jwt_and_feed_token() {
    let b = broker();
    let auth = AuthToken::new("apikey:jwt.token.sig")
        .with_feed(Some("feedtok"))
        .with_user_id("<USER_ID>");
    let req = b.create_feed(&auth).unwrap().ws_request().unwrap();
    assert_eq!(req.uri().to_string(), streaming::WS_URL);
    let h = req.headers();
    assert_eq!(h["Authorization"], "jwt.token.sig");
    assert_eq!(h["x-api-key"], "apikey");
    assert_eq!(h["x-client-code"], "<USER_ID>");
    assert_eq!(h["x-feed-token"], "feedtok");
    // No feed token or client code: refused before connecting.
    assert!(b.create_feed(&AuthToken::new("apikey:jwt")).is_err());
    let f = feed();
    assert_eq!(
        f.heartbeat(),
        Some((
            std::time::Duration::from_secs(10),
            Message::Text("ping".into())
        ))
    );
    let order = b
        .order_socket(&AuthToken::new("apikey:jwt.token.sig"))
        .unwrap()
        .ws_request()
        .unwrap();
    assert_eq!(order.headers()["Authorization"], "Bearer jwt.token.sig");
}

#[test]
fn binary_ltp_quote_and_snapquote() {
    let mut f = feed();
    f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Depth),
        sub("NIFTY", "NSE_INDEX", FeedMode::Ltp),
        sub("NIFTY27OCT2625000CE", "NFO", FeedMode::Quote),
        sub("USDINR27OCT26FUT", "CDS", FeedMode::Ltp),
    ]);
    // LTP (51 bytes): index keeps NSE_INDEX from the subscription.
    let mut ltp = packet(1, 1, "99926000", LTP_LEN);
    put_i64(&mut ltp, 43, 2_251_235);
    let ev = f.parse(&Message::Binary(ltp));
    match &ev[..] {
        [FeedEvent::Tick(t)] => {
            assert_eq!(
                (t.exchange.as_str(), t.symbol.as_str()),
                ("NSE_INDEX", "NIFTY")
            );
            assert_eq!((t.ltp, t.mode), (22512.35, 1));
            assert_eq!(t.last_trade_time_ms, 1_790_916_300_000);
        }
        other => panic!("{:?}", other),
    }
    // QUOTE (123 bytes)
    let mut q = packet(2, 2, "43210", QUOTE_LEN);
    put_i64(&mut q, 43, 15_020);
    put_i64(&mut q, 51, 75);
    put_i64(&mut q, 59, 15_288);
    put_i64(&mut q, 67, 1_200_000);
    put_f64(&mut q, 75, 90_000.0);
    put_f64(&mut q, 83, 81_000.0);
    put_i64(&mut q, 91, 16_000);
    put_i64(&mut q, 99, 16_550);
    put_i64(&mut q, 107, 14_800);
    put_i64(&mut q, 115, 15_695);
    let ev = f.parse(&Message::Binary(q));
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        (t.ltp, t.last_quantity, t.average_price),
        (150.2, 75, 152.88)
    );
    assert_eq!(t.volume, 1_200_000);
    assert_eq!(
        (t.total_buy_quantity, t.total_sell_quantity),
        (90_000, 81_000)
    );
    assert_eq!(
        (t.open, t.high, t.low, t.close),
        (160.0, 165.5, 148.0, 156.95)
    );
    assert_eq!(t.change, -6.75);
    // SNAP_QUOTE (379 bytes): best-five routed by flag (non-zero = buy).
    let mut s = packet(3, 1, "3045", SNAP_LEN);
    put_i64(&mut s, 43, 95_410);
    put_i64(&mut s, 115, 95_065);
    put_i64(&mut s, 131, 0);
    for i in 0..10usize {
        let o = 147 + i * 20;
        // Interleave: even packets sell (flag 0), odd packets buy (flag 1).
        let flag: u16 = (i % 2) as u16;
        s[o..o + 2].copy_from_slice(&flag.to_le_bytes());
        put_i64(&mut s, o + 2, 100 + i as i64);
        let px = if flag == 1 {
            95_405 - i as i64
        } else {
            95_410 + i as i64
        };
        put_i64(&mut s, o + 10, px);
        s[o + 18..o + 20].copy_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    let ev = f.parse(&Message::Binary(s));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.mode, t.ltp, t.close), (3, 954.1, 950.65));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(d.buy[0].price, 954.04);
    assert_eq!((d.buy[0].quantity, d.buy[0].orders), (101, 2));
    assert_eq!(d.sell[0].price, 954.1);
    assert_eq!(d.sell[4].quantity, 108);
    // CDS divides by 10^7.
    let mut c = packet(1, 13, "1170", LTP_LEN);
    put_i64(&mut c, 43, 832_500_000);
    let FeedEvent::Tick(t) = &f.parse(&Message::Binary(c))[0] else {
        panic!()
    };
    assert_eq!(t.ltp, 83.25);
    // Unknown token, wrong segment, short frame, pong.
    assert!(f
        .parse(&Message::Binary(packet(1, 1, "42", LTP_LEN)))
        .is_empty());
    assert!(f
        .parse(&Message::Binary(packet(1, 3, "3045", LTP_LEN)))
        .is_empty());
    assert!(f.parse(&Message::Binary(vec![1, 1, 0])).is_empty());
    assert_eq!(
        f.parse(&Message::Text("pong".into())),
        vec![FeedEvent::Heartbeat]
    );
    assert!(f
        .parse(&Message::Text(
            r#"{"correlationID":"openalgo","errorCode":"E1002","errorMessage":"Invalid Request"}"#
                .into()
        ))
        .is_empty());
}

#[test]
fn order_update_normalises_like_web() {
    let mut f = AngelOrderFeed::new(streaming::ORDER_WS_URL, "jwt", master());
    let ev = f.parse(&Message::Text(fixture!("order_update.json").to_string()));
    match &ev[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(u.symbol, "CRUDEOIL19OCT26FUT");
            assert_eq!(u.order_status, "complete");
            assert_eq!((u.quantity, u.filled_quantity), (100, 100));
            assert_eq!(u.product, "MIS");
            assert_eq!(u.pricetype, "MARKET");
            assert_eq!(u.average_price, 9008.0);
        }
        other => panic!("{:?}", other),
    }
    // AB00 acknowledgement frames are ignored.
    assert!(f
        .parse(&Message::Text(
            r#"{"order-status":"AB00","orderData":{}}"#.into()
        ))
        .is_empty());
    assert_eq!(order_status_from_code("AB10"), Some("trigger pending"));
    assert_eq!(order_status_from_code("AB07"), Some("cancelled"));
    assert_eq!(order_status_from_code("ZZ"), None);
    assert_eq!(order_status_from_text("Executed"), "complete");
    assert_eq!(order_status_from_text(""), "open");
}

// ---------------------------------------------------------------------------
// Ambiguous placement replies (web test_angel_ambiguous_order_reconciliation.py)
// ---------------------------------------------------------------------------

mod ambiguous_place {
    use super::super::orders::{classify_place_reply, reconcile_by_tag, PlaceOutcome};
    use super::*;
    use serde_json::Value;

    fn body(tag: &str) -> Value {
        place_order_body(&resolved("SBIN", "NSE", "LIMIT", "MIS"), tag)
    }

    fn row(tag: &str, symbol: &str, qty: i64, id: &str) -> AngelOrder {
        AngelOrder {
            ordertag: tag.into(),
            tradingsymbol: symbol.into(),
            symboltoken: "3045".into(),
            exchange: "NSE".into(),
            transactiontype: "BUY".into(),
            quantity: qty,
            orderid: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ordertag_is_short_and_unique() {
        let a = new_ordertag();
        let b = new_ordertag();
        assert!(a.starts_with("oa") && a.len() == 18, "{a}");
        assert!(a[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn only_status_true_with_orderid_is_placed() {
        assert_eq!(
            classify_place_reply(br#"{"status":true,"data":{"orderid":"2610"}}"#),
            PlaceOutcome::Placed("2610".into())
        );
        // A partial success, an empty body, a non-object and a gateway page
        // are all ambiguous: the order may have been taken.
        for b in [
            &br#"{"status":true}"#[..],
            br#"{"status":true,"data":{"orderid":""}}"#,
            br#"{"status":true,"data":null}"#,
            b"",
            b"null",
            b"[1]",
            b"<html>502 Bad Gateway</html>",
        ] {
            assert_eq!(classify_place_reply(b), PlaceOutcome::Ambiguous, "{b:?}");
        }
    }

    #[test]
    fn explicit_rejection_is_a_refusal_not_ambiguous() {
        assert_eq!(
            classify_place_reply(br#"{"status":false,"message":"rejected","errorcode":"AB1"}"#),
            PlaceOutcome::Refused {
                errorcode: "AB1".into(),
                message: "rejected".into()
            }
        );
    }

    #[test]
    fn reconciliation_matches_only_the_unique_tag_and_fields() {
        let tag = "oa0123456789abcdef";
        let b = body(tag);
        let qty: i64 = b["quantity"].as_str().unwrap().parse().unwrap();
        let rows = vec![row(tag, "SBIN-EQ", qty, "fresh-order")];
        assert_eq!(
            reconcile_by_tag(&rows, tag, &b).map(|r| r.orderid.as_str()),
            Some("fresh-order")
        );
        // An older order with another tag is never claimed.
        let rows = vec![row("openalgo", "SBIN-EQ", qty, "older-order")];
        assert!(reconcile_by_tag(&rows, tag, &b).is_none());
        // Same tag but another symbol, or another quantity: refused.
        let rows = vec![row(tag, "OTHER-EQ", qty, "wrong-symbol")];
        assert!(reconcile_by_tag(&rows, tag, &b).is_none());
        let rows = vec![row(tag, "SBIN-EQ", qty + 1, "wrong-qty")];
        assert!(reconcile_by_tag(&rows, tag, &b).is_none());
        // Two rows with the tag: ambiguous, refused.
        let rows = vec![
            row(tag, "SBIN-EQ", qty, "duplicate-0"),
            row(tag, "SBIN-EQ", qty, "duplicate-1"),
        ];
        assert!(reconcile_by_tag(&rows, tag, &b).is_none());
    }

    #[test]
    fn order_book_rows_carry_the_ordertag() {
        let r: AngelOrder = serde_json::from_str(
            r#"{"orderid":"1","ordertag":"oa0123456789abcdef","quantity":"1"}"#,
        )
        .unwrap();
        assert_eq!(r.ordertag, "oa0123456789abcdef");
        assert_eq!(r.quantity, 1);
    }
}
