//! Nubra mapping, master-contract and feed tests against payloads built
//! from the web code (`broker/nubra/**`) and its protobuf descriptors. No
//! account data.

use super::auth::{mask_phone, normalize_totp, totp_candidates};
use super::data::{self, chunk_days, history_body, history_query_target};
use super::mapping::{self, *};
use super::master_contract::{parse_indexes, parse_refdata};
use super::proto;
use super::streaming::{self, decode_order_update, NubraFeed, OrderSession};
use super::*;
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use crate::brokers::common::relay::Session;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use serde_json::{json, Value};
use std::collections::BTreeMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/nubra/", $name))
    };
}

fn j(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// The master the fixtures resolve against (built by the parser itself).
fn symbols() -> SymbolResolver {
    let mut rows = Vec::new();
    for f in [
        fixture!("refdata_nse.json"),
        fixture!("refdata_bse.json"),
        fixture!("refdata_mcx.json"),
    ] {
        let v = j(f);
        rows.extend(parse_refdata(v["refdata"].as_array().unwrap()));
    }
    rows.extend(parse_indexes(fixture!("indexes.csv")));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    symbols()
        .by_symbol(exchange, symbol)
        .unwrap_or_else(|| SymToken {
            symbol: symbol.into(),
            brsymbol: symbol.into(),
            name: symbol.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: token.into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        })
}

fn order(pt: PriceType, action: Action, price: f64, trigger: f64) -> ResolvedOrder {
    ResolvedOrder {
        symbol: "RELIANCE".into(),
        exchange: Exchange::Nse,
        action,
        quantity: 1,
        price,
        trigger_price: trigger,
        pricetype: pt,
        product: Product::Mis,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: row("RELIANCE", "NSE", "72329"),
    }
}

fn hexframe(name: &str) -> Vec<u8> {
    let v = j(fixture!("feed_frames.json"));
    hex::decode(v[name].as_str().unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// Scalars and order payloads
// ---------------------------------------------------------------------------

#[test]
fn paise_and_strategy_tags() {
    assert_eq!(paise(1270.0), 127000);
    assert_eq!(paise(0.05), 5);
    assert_eq!(paise(f64::NAN), 0);
    assert_eq!(sanitize_strat_tag(None), "openalgo");
    assert_eq!(sanitize_strat_tag(Some("My Strategy_v2")), "my-strategy-v2");
    assert_eq!(sanitize_strat_tag(Some("__x__")), "x");
    assert_eq!(sanitize_strat_tag(Some("***")), "openalgo");
}

#[test]
fn enum_maps_match_the_web() {
    assert_eq!(delivery_type(Product::Cnc), "CNC");
    assert_eq!(delivery_type(Product::Nrml), "CNC");
    assert_eq!(delivery_type(Product::Mis), "IDAY");
    assert_eq!(product_from("IDAY"), "MIS");
    assert_eq!(product_from("cnc"), "CNC");
    assert_eq!(product_from(""), "MIS");
    assert_eq!(price_type(PriceType::Sl), "LIMIT");
    assert_eq!(price_type(PriceType::SlM), "MARKET");
    assert_eq!(validity_type(PriceType::Market), "IOC");
    assert_eq!(validity_type(PriceType::SlM), "IOC");
    assert_eq!(validity_type(PriceType::Limit), "DAY");
    assert_eq!(mapping::ref_id("72329"), Some(72329));
    assert_eq!(mapping::ref_id("NIFTY"), None);
    assert_eq!(mapping::ref_id(""), None);
}

#[test]
fn place_items_match_the_web_payload() {
    // web transform_data docstring example.
    let v = place_item(&order(PriceType::Limit, Action::Buy, 1270.0, 0.0), 72329);
    assert_eq!(
        v,
        json!({"refId": 72329, "qty": 1, "side": "BUY", "deliveryType": "IDAY",
               "priceType": "LIMIT", "validityType": "DAY", "isMultiLeg": false,
               "executionMode": "ENTRY", "entryPrice": 127000, "stratTags": ["openalgo"]})
    );
    let m = place_item(&order(PriceType::Market, Action::Sell, 0.0, 0.0), 72329);
    assert_eq!(m["priceType"], "MARKET");
    assert_eq!(m["validityType"], "IOC");
    assert!(m.get("entryPrice").is_none() && m.get("entryConfig").is_none());
    let sl = place_item(&order(PriceType::Sl, Action::Buy, 1270.0, 1260.0), 72329);
    assert_eq!(sl["entryPrice"], 127000);
    assert_eq!(
        sl["entryConfig"],
        json!({"triggers": {"ltp": {"atOrAbove": {"value": 126000}}}})
    );
    let slm = place_item(&order(PriceType::SlM, Action::Sell, 0.0, 1250.5), 72329);
    assert_eq!(slm["priceType"], "MARKET");
    assert_eq!(
        slm["entryConfig"],
        json!({"triggers": {"ltp": {"atOrBelow": {"value": 125050}}}})
    );
}

#[test]
fn modify_item_is_an_order_level_patch() {
    let m = ResolvedModify {
        order_id: "1001".into(),
        symbol: "RELIANCE".into(),
        exchange: Exchange::Nse,
        action: Action::Sell,
        product: Product::Cnc,
        pricetype: PriceType::Sl,
        quantity: 3,
        price: 100.5,
        trigger_price: 100.0,
        disclosed_quantity: 0,
        instrument: row("RELIANCE", "NSE", "72329"),
    };
    assert_eq!(
        modify_item(&m, 1001),
        json!({"orderId": 1001, "qty": 3, "deliveryType": "CNC", "priceType": "LIMIT",
               "validityType": "DAY", "executionMode": "ENTRY", "entryPrice": 10050,
               "entryConfig": {"triggers": {"ltp": {"atOrBelow": {"value": 10000}}}}})
    );
}

#[test]
fn exchange_folding() {
    assert_eq!(map_exchange("NSE", "OPT"), "NFO");
    assert_eq!(map_exchange("BSE", "FUT"), "BFO");
    assert_eq!(map_exchange("MCX", "FUT"), "MCX");
    assert_eq!(map_exchange("nse", "STOCK"), "NSE");
    assert_eq!(candidate_exchanges("NSE", ""), ["NFO", "NSE"]);
    assert_eq!(candidate_exchanges("NSE", "STOCK"), ["NSE"]);
    assert_eq!(candidate_exchanges("BSE", "OPT"), ["BFO"]);
    assert_eq!(candidate_exchanges("MCX", ""), ["MCX"]);
    assert!(candidate_exchanges("", "OPT").is_empty());
    let s = symbols();
    // ref id first, then broker symbol; NSE with no type probes NFO first.
    assert_eq!(
        resolve_instrument(&s, "NSE", "", "99999", ""),
        Some(("NIFTY27OCT2625000CE".into(), "NFO".into()))
    );
    assert_eq!(
        resolve_instrument(&s, "NSE", "STOCK", "", "RELIANCE"),
        Some(("RELIANCE".into(), "NSE".into()))
    );
    assert_eq!(resolve_instrument(&s, "NSE", "STOCK", "1", "NOPE"), None);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_from_buckets() {
    let book = order_book(&j(fixture!("orders.json")), &symbols());
    assert_eq!(book.len(), 6);
    let by = |id: &str| book.iter().find(|o| o.order_id == id).unwrap().clone();

    let o = by("1001");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(o.status, "open");
    assert_eq!(o.order_type, "LIMIT");
    assert_eq!(o.product, "MIS");
    assert_eq!(
        (o.quantity, o.filled_quantity, o.pending_quantity),
        (10, 4, 6)
    );
    assert_eq!(o.price, 1425.0);
    assert_eq!(o.average_price, 1424.5);
    assert_eq!(o.exchange_order_id.as_deref(), Some("1100000012345"));
    assert_eq!(o.order_timestamp, "2026-10-03 05:00:45");

    // Instrument on legs[0]; a working stop is "trigger pending".
    let o = by("1002");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("NIFTY27OCT2625000CE", "NFO")
    );
    assert_eq!(o.order_type, "SL-M");
    assert_eq!(o.status, "trigger pending");
    assert_eq!(o.trigger_price, 120.0);
    assert_eq!(o.side, "SELL");
    assert_eq!(o.order_timestamp, "2026-10-03 04:00:00");

    let o = by("1003");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("NIFTY27OCT26FUT", "NFO")
    );
    assert_eq!(o.status, "complete");
    assert_eq!(o.average_price, 25100.5);

    let o = by("1004");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("CRUDEOIL19OCT26FUT", "MCX")
    );
    assert_eq!(o.status, "rejected");

    // expired -> cancelled; unknown instrument keeps the broker symbol.
    let o = by("1005");
    assert_eq!(o.status, "cancelled");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str()),
        ("UNKNOWNCO", "NSE")
    );

    assert_eq!(by("1006").status, "open");
    assert!(order_book(&json!({"orders": {}}), &symbols()).is_empty());
    assert!(order_book(&json!({}), &symbols()).is_empty());
}

#[test]
fn trade_book_is_filled_orders() {
    let t = trade_book(&j(fixture!("orders.json")), &symbols());
    assert_eq!(t.len(), 2);
    let part = t.iter().find(|x| x.order_id == "1001").unwrap();
    assert_eq!(part.quantity, 4);
    assert_eq!(part.average_price, 1424.5);
    assert_eq!(part.trade_value, 5698.0);
    let full = t.iter().find(|x| x.order_id == "1003").unwrap();
    assert_eq!(
        (full.symbol.as_str(), full.product.as_str()),
        ("NIFTY27OCT26FUT", "CNC")
    );
    assert_eq!(full.timestamp, "2026-10-03 04:30:00");
}

#[test]
fn positions_use_live_and_documented_names() {
    let p = positions(&j(fixture!("positions.json")), &symbols());
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!((p[0].quantity, p[0].product.as_str()), (10, "MIS"));
    assert_eq!(
        (p[0].average_price, p[0].ltp, p[0].pnl),
        (1424.5, 1426.5, 20.0)
    );
    assert_eq!(
        (p[1].symbol.as_str(), p[1].exchange.as_str(), p[1].quantity),
        ("NIFTY27OCT2625000CE", "NFO", -75)
    );
    assert_eq!(p[1].ltp, 110.0);
    assert_eq!(p[2].quantity, 0);
}

#[test]
fn holdings_are_in_rupees() {
    let h = holdings(&j(fixture!("holdings.json")), &symbols());
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(h[0].product, "CNC");
    assert_eq!(
        (h[0].quantity, h[0].average_price, h[0].ltp),
        (5, 1200.0, 1426.5)
    );
    assert_eq!((h[0].pnl, h[0].pnl_percentage), (1132.5, 18.88));
    assert_eq!(h[0].close_price, 1430.1);
    assert!(holdings(&json!({"portfolio": {}}), &symbols()).is_empty());
}

#[test]
fn funds_and_margin() {
    let f = mapping::funds(&j(fixture!("funds.json"))).unwrap();
    assert_eq!(f.available_cash, 100000.5);
    assert_eq!(f.collateral, 2000.0);
    assert_eq!(f.m2m_realized, -15.0);
    assert_eq!(f.m2m_unrealized, 12.5);
    assert_eq!(f.utilised_debits, 3500.0);
    assert!(mapping::funds(&json!({"message": "x"})).is_none());

    let m = mapping::margin(&j(fixture!("margin.json"))).unwrap();
    assert_eq!(m.total_margin_required, 121350.0);
    assert_eq!(m.span_margin, 120000.0);
    assert_eq!(m.exposure_margin, 0.0);
    let only_margin = mapping::margin(&json!({"marginInfo": {"totalMargin": 50}})).unwrap();
    assert_eq!(only_margin.total_margin_required, 50.0);
    assert_eq!(
        mapping::margin(&json!({"error": "Orders cannot be placed from this IP address"}))
            .unwrap_err(),
        "Orders cannot be placed from this IP address"
    );
}

#[test]
fn timestamps_format_like_the_web() {
    assert_eq!(
        format_timestamp(&json!("2026-06-22T05:00:45.054721358Z")),
        "2026-06-22 05:00:45"
    );
    assert_eq!(
        format_timestamp(&json!(1_000_000_000_000_000_000i64)),
        "2001-09-09 01:46:40"
    );
    assert_eq!(format_timestamp(&json!("garbage")), "garbage");
    assert_eq!(format_timestamp(&Value::Null), "");
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_and_depth_from_order_books() {
    let key = QuoteKey::new("NSE", "RELIANCE");
    let q = data::quote_from_orderbook(&key, &j(fixture!("orderbook_l1.json"))).unwrap();
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close),
        (1426.5, 1426.0, 1427.0, 1430.1)
    );
    assert_eq!((q.bid_qty, q.ask_qty, q.volume), (100, 200, 1234567));
    assert_eq!((q.open, q.high, q.low, q.oi), (0.0, 0.0, 0.0, 0));
    assert_eq!(q.change, -3.6);
    assert!(data::quote_from_orderbook(&key, &json!({})).is_none());

    let d = data::depth_from_orderbook(&key, &j(fixture!("orderbook_l5.json"))).unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.bids[0].price, 1426.0);
    assert_eq!(d.bids[3], DepthLevel::default());
    assert_eq!(d.asks[4].price, 1427.2);
    // The web totals every level it was sent (six asks here).
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (303, 1215));
    assert_eq!(
        (d.open, d.high, d.low, d.prev_close),
        (1420.0, 1430.0, 1410.0, 1430.1)
    );
}

#[test]
fn history_query_and_chunks() {
    assert_eq!(chunk_days("1s"), 7);
    assert_eq!(chunk_days("1m"), 30);
    assert_eq!(chunk_days("5m"), 60);
    assert_eq!(chunk_days("1h"), 90);
    assert_eq!(chunk_days("D"), 365);
    assert_eq!(chunk_days("W"), 1000);
    assert_eq!(chunk_days("M"), 1500);
    assert_eq!(
        history_query_target("NIFTY", "NSE_INDEX"),
        Some(("NSE", "INDEX"))
    );
    assert_eq!(
        history_query_target("NIFTY27OCT2625000CE", "NFO"),
        Some(("NSE", "OPT"))
    );
    assert_eq!(
        history_query_target("NIFTY27OCT26FUT", "NFO"),
        Some(("NSE", "FUT"))
    );
    assert_eq!(
        history_query_target("SENSEX30OCT2682000PE", "BFO"),
        Some(("BSE", "OPT"))
    );
    assert_eq!(
        history_query_target("CRUDEOIL19OCT26FUT", "MCX"),
        Some(("MCX", "FUT"))
    );
    assert_eq!(history_query_target("SBIN", "BSE"), Some(("BSE", "STOCK")));
    assert_eq!(history_query_target("X", "CDS"), None);

    let b = history_body("NSE", "STOCK", "RELIANCE", "a", "b", "1m");
    assert_eq!(
        b,
        json!({"query": [{"exchange": "NSE", "type": "STOCK", "values": ["RELIANCE"],
            "fields": ["open", "high", "low", "close", "tick_volume"],
            "startDate": "a", "endDate": "b", "interval": "1m",
            "intraDay": false, "realTime": false}]})
    );
    let idx = history_body("NSE", "INDEX", "NIFTY", "a", "b", "1d");
    assert_eq!(
        idx["query"][0]["fields"],
        json!(["open", "high", "low", "close"])
    );
}

#[test]
fn history_candles_are_merged_from_field_series() {
    let mut c = BTreeMap::new();
    data::merge_chart(&j(fixture!("timeseries.json")), "RELIANCE", &mut c).unwrap();
    let out = data::finish_candles(c.clone(), "1m");
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].timestamp, 1759463100);
    assert_eq!(
        (
            out[0].open,
            out[0].high,
            out[0].low,
            out[0].close,
            out[0].volume
        ),
        (1420.0, 1422.0, 1419.0, 1421.0, 5000)
    );
    assert_eq!(out[1].volume, 3200);
    // Daily candles land on midnight UTC and collapse.
    let daily = data::finish_candles(c, "D");
    assert_eq!(daily.len(), 1);
    assert_eq!(daily[0].timestamp, 1759449600);

    let mut none = BTreeMap::new();
    assert_eq!(
        data::merge_chart(
            &json!({"error": "invalid field tick_volume"}),
            "X",
            &mut none
        ),
        Err("invalid field tick_volume".to_string())
    );
    data::merge_chart(&json!({"message": "charts", "result": []}), "X", &mut none).unwrap();
    assert!(none.is_empty());
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn refdata_rows_follow_the_openalgo_format() {
    let s = symbols();
    let r = s.by_symbol("NSE", "RELIANCE").unwrap();
    assert_eq!(
        (r.token.as_str(), r.instrument_type.as_str()),
        ("72329", "EQ")
    );
    assert_eq!((r.expiry.as_str(), r.tick_size, r.lot_size), ("", 0.1, 1));

    let o = s.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!(o.brsymbol, "NIFTY26OCT25000CE");
    assert_eq!((o.brexchange.as_str(), o.token.as_str()), ("NSE", "99999"));
    assert_eq!(
        (o.expiry.as_str(), o.strike, o.lot_size),
        ("27-OCT-26", 25000.0, 75)
    );
    assert_eq!(
        (o.name.as_str(), o.instrument_type.as_str()),
        ("NIFTY", "CE")
    );
    assert_eq!(o.tick_size, 0.05);

    // Fractional strike keeps its fraction.
    assert!(s.by_symbol("NFO", "NIFTY27OCT2625012.5PE").is_some());
    let f = s.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!((f.instrument_type.as_str(), f.strike), ("FUT", 0.0));
    // String expiry, BSE options fold to BFO.
    let b = s.by_symbol("BFO", "SENSEX30OCT2682000PE").unwrap();
    assert_eq!(b.brexchange, "BSE");
    assert!(s.by_symbol("BSE", "SBIN").is_some());
    let m = s.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!(
        (m.exchange.as_str(), m.tick_size, m.lot_size),
        ("MCX", 1.0, 100)
    );
    assert_eq!(s.len(), 14);
}

#[test]
fn index_csv_is_renamed() {
    let rows = parse_indexes(fixture!("indexes.csv"));
    assert_eq!(rows.len(), 7);
    let find = |sym: &str| rows.iter().find(|r| r.symbol == sym).unwrap().clone();
    let n = find("NIFTY");
    assert_eq!(
        (n.exchange.as_str(), n.brexchange.as_str()),
        ("NSE_INDEX", "NSE")
    );
    assert_eq!((n.token.as_str(), n.brsymbol.as_str()), ("NIFTY", "NIFTY"));
    assert_eq!(
        (n.instrument_type.as_str(), n.tick_size, n.lot_size),
        ("INDEX", 0.05, 0)
    );
    assert_eq!(find("INDIAVIX").brsymbol, "INDIA_VIX");
    assert_eq!(find("NIFTYMIDCAP100").name, "Nifty Midcap 100");
    assert_eq!(find("BSESENSEXNEXT50").exchange, "BSE_INDEX");
    assert_eq!(find("BSECAPITALGOODS").name, "BSE Capital Goods, Index");
    assert!(parse_indexes("A,B\n1,2\n").is_empty());
    assert!(parse_indexes("").is_empty());
}

// ---------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------

fn fsub(
    symbol: &str,
    exchange: &str,
    token: &str,
    brsymbol: &str,
    mode: FeedMode,
) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: brsymbol.into(),
        brexchange: String::new(),
        mode,
        depth: 5,
    }
}

fn texts(m: &[Message]) -> Vec<String> {
    m.iter()
        .map(|x| match x {
            Message::Text(t) => t.clone(),
            other => panic!("not text: {:?}", other),
        })
        .collect()
}

#[test]
fn subscribe_frames_carry_the_token_and_web_shapes() {
    let mut f = NubraFeed::new("wss://x", "TOK", SymbolResolver::new());
    let frames = texts(&f.subscribe_frames(&[
        fsub("NIFTY", "NSE_INDEX", "NIFTY", "NIFTY", FeedMode::Quote),
        fsub("RELIANCE", "NSE", "72329", "RELIANCE", FeedMode::Depth),
        fsub("SENSEX", "BSE_INDEX", "SENSEX", "SENSEX", FeedMode::Ltp),
    ]));
    assert_eq!(
        frames,
        [
            r#"batch_subscribe TOK index {"instruments":[],"indexes":["Bse Sensex"]} BSE"#,
            r#"batch_subscribe TOK index_bucket {"instruments":[],"indexes":["Bse Sensex"]} 1d BSE"#,
            r#"batch_subscribe TOK index {"instruments":[],"indexes":["Nifty 50","RELIANCE"]} NSE"#,
            r#"batch_subscribe TOK index_bucket {"instruments":[],"indexes":["Nifty 50","RELIANCE"]} 1d NSE"#,
            r#"batch_subscribe TOK orderbook {"instruments":[72329],"indexes":[]}"#,
            "batch_subscribe TOK orderbook_depth 5",
        ]
    );
    assert_eq!(f.tracked(), 3);
    let un = texts(&f.unsubscribe_frames(&[fsub(
        "RELIANCE",
        "NSE",
        "72329",
        "RELIANCE",
        FeedMode::Depth,
    )]));
    assert_eq!(
        un,
        [
            r#"batch_unsubscribe TOK index {"instruments":[],"indexes":["RELIANCE"]} NSE"#,
            r#"batch_unsubscribe TOK index_bucket {"instruments":[],"indexes":["RELIANCE"]} 1d NSE"#,
            r#"batch_unsubscribe TOK orderbook {"instruments":[72329],"indexes":[]}"#,
        ]
    );
    assert_eq!(f.tracked(), 2);
    assert_eq!(
        streaming::subscription_name(&fsub("NIFTYIT", "NSE_INDEX", "X", "NIFTYIT", FeedMode::Ltp)),
        "NIFTYIT"
    );
}

#[test]
fn ws_request_sends_bearer_and_device_id() {
    let f = NubraFeed::new(
        "wss://api.nubra.io/apibatch/ws",
        "TOK",
        SymbolResolver::new(),
    );
    let r = f.ws_request().unwrap();
    assert_eq!(r.headers()["Authorization"], "Bearer TOK");
    assert_eq!(r.headers()["x-device-id"], "OPENALGO");
    assert!(f.heartbeat().is_some());
}

#[test]
fn index_frames_tick_indices_and_instruments() {
    let mut f = NubraFeed::new("wss://x", "TOK", SymbolResolver::new());
    f.subscribe_frames(&[
        fsub("NIFTY", "NSE_INDEX", "NIFTY", "NIFTY", FeedMode::Quote),
        fsub("RELIANCE", "NSE", "72329", "RELIANCE", FeedMode::Ltp),
    ]);
    // WebSocketMsgIndex: 1 indexname, 3 index_value, 4 high, 5 low,
    // 7 changepercent (float), 9 prev_close, 10 exchange (paise).
    let ev = f.parse(&Message::Binary(hexframe("index")));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(n) = &ev[0] else { panic!() };
    assert_eq!(
        (n.symbol.as_str(), n.exchange.as_str(), n.mode),
        ("NIFTY", "NSE_INDEX", 2)
    );
    assert_eq!(
        (n.ltp, n.high, n.low, n.close),
        (25012.35, 25100.0, 24900.5, 24888.0)
    );
    assert_eq!(n.change, 124.35);
    let FeedEvent::Tick(r) = &ev[1] else { panic!() };
    assert_eq!((r.symbol.as_str(), r.mode, r.ltp), ("RELIANCE", 1, 1426.5));
    // LTP mode carries only the price.
    assert_eq!((r.high, r.volume), (0.0, 0));

    // WebSocketMsgIndexBucket: 5 open after the index channel is live only
    // contributes the open.
    let ev = f.parse(&Message::Binary(hexframe("bucket")));
    assert!(ev.is_empty());
    let ev = f.parse(&Message::Binary(hexframe("index")));
    let FeedEvent::Tick(n) = &ev[0] else { panic!() };
    assert_eq!(n.open, 24900.0);
}

#[test]
fn bucket_alone_drives_an_index_quote() {
    let mut f = NubraFeed::new("wss://x", "TOK", SymbolResolver::new());
    f.subscribe_frames(&[fsub(
        "NIFTY",
        "NSE_INDEX",
        "NIFTY",
        "NIFTY",
        FeedMode::Quote,
    )]);
    let ev = f.parse(&Message::Binary(hexframe("bucket")));
    let FeedEvent::Tick(n) = &ev[0] else { panic!() };
    assert_eq!(
        (n.ltp, n.open, n.high, n.low, n.volume),
        (25012.35, 24900.0, 25100.0, 24890.0, 5)
    );
}

#[test]
fn order_book_frames_tick_and_depth() {
    let mut f = NubraFeed::new("wss://x", "TOK", SymbolResolver::new());
    f.subscribe_frames(&[fsub(
        "RELIANCE",
        "NSE",
        "72329",
        "RELIANCE",
        FeedMode::Depth,
    )]);
    // WebSocketMsgOptionChainItem: 12 oi, 14 ref_id.
    assert!(f.parse(&Message::Binary(hexframe("greeks"))).is_empty());
    // WebSocketMsgOrderBook: 3 bids / 4 asks {1 price, 2 qty, 3 orders},
    // 5 ltp, 6 ltq, 7 volume, 8 ref_id.
    let ev = f.parse(&Message::Binary(hexframe("orderbook")));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(
        (t.ltp, t.last_quantity, t.volume, t.oi),
        (1426.5, 10, 1234567, 987654)
    );
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!()
    };
    assert_eq!(d.buy.len(), 5);
    assert_eq!(
        (d.buy[0].price, d.buy[0].quantity, d.buy[0].orders),
        (1426.0, 100, 1)
    );
    assert_eq!(d.sell[4].price, 1427.2);
    assert_eq!((d.total_buy_quantity, d.total_sell_quantity), (510, 1010));

    // Mode change keeps the channels and changes the shape.
    let frames = f.mode_change_frames(
        &fsub("RELIANCE", "NSE", "72329", "RELIANCE", FeedMode::Depth),
        &fsub("RELIANCE", "NSE", "72329", "RELIANCE", FeedMode::Quote),
    );
    assert!(frames.is_empty());
    let ev = f.parse(&Message::Binary(hexframe("orderbook")));
    assert_eq!(ev.len(), 1);
}

#[test]
fn feed_refusal_and_noise() {
    let mut f = NubraFeed::new("wss://x", "TOK", SymbolResolver::new());
    assert!(matches!(
        f.parse(&Message::Text("Invalid Token".into()))[..],
        [FeedEvent::AuthFailed(_)]
    ));
    assert!(f.parse(&Message::Binary(vec![1, 2, 3])).is_empty());
    assert!(f.parse(&Message::Text("OK".into())).is_empty());
    // Frames for instruments nobody subscribed are dropped.
    assert!(f.parse(&Message::Binary(hexframe("orderbook"))).is_empty());
}

#[test]
fn order_updates_decode_and_resolve() {
    let s = symbols();
    let u = decode_order_update(&hexframe("order"), &s).unwrap();
    assert_eq!(u.orderid, "1234567890");
    assert_eq!(
        (u.symbol.as_str(), u.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(
        (u.action.as_str(), u.order_status.as_str()),
        ("BUY", "complete")
    );
    assert_eq!((u.pricetype.as_str(), u.product.as_str()), ("LIMIT", "MIS"));
    assert_eq!(
        (u.quantity, u.filled_quantity, u.pending_quantity),
        (10, 10, 0)
    );
    assert_eq!(u.price, 1427.0);
    // tradeFill price wins over the average.
    assert_eq!(u.average_price, 1426.55);
    assert_eq!(u.rejection_reason, "");

    let r = decode_order_update(&hexframe("order_rejected"), &s).unwrap();
    assert_eq!(r.order_status, "rejected");
    assert_eq!(r.rejection_reason, "Insufficient funds");
    // Known ref id: the OpenAlgo symbol from the master contract.
    assert_eq!(
        (r.symbol.as_str(), r.exchange.as_str()),
        ("NIFTY27OCT2625000CE", "NFO")
    );
    // Unknown ref id: the broker symbol, exchange folded for an option.
    let unknown = decode_order_update(&hexframe("order_rejected"), &SymbolResolver::new()).unwrap();
    assert_eq!(
        (unknown.symbol.as_str(), unknown.exchange.as_str()),
        ("NIFTY26OCT25000CE", "NFO")
    );
    assert_eq!(
        (r.action.as_str(), r.product.as_str(), r.pricetype.as_str()),
        ("SELL", "CNC", "MARKET")
    );
    assert_eq!(r.pending_quantity, 5);

    // Market frames are not order updates.
    assert!(decode_order_update(&hexframe("index"), &s).is_none());
    assert_eq!(streaming::order_status(4), "trigger pending");
    assert_eq!(streaming::order_status(6), "expired");
    assert_eq!(streaming::order_status(99), "open");
}

#[test]
fn order_session_subscribes_and_detects_refusal() {
    let mut s = OrderSession::new("TOK");
    assert_eq!(
        texts(&s.on_open()),
        ["subscribe TOK notifications notification"]
    );
    let step = s.on_upstream(Message::Text("Invalid Token".into()));
    assert!(step.auth_failed.is_some());
    let step = s.on_upstream(Message::Binary(vec![1]));
    assert_eq!(step.down.len(), 1);
    assert!(s.on_upstream(Message::Text("OK".into())).down.is_empty());
}

#[test]
fn wire_walker_handles_truncation() {
    assert!(proto::decode_fields(&[0x08]).is_none());
    assert!(proto::decode_fields(&[0x0a, 0x05, 0x01]).is_none());
    assert!(proto::decode_fields(&[0x0b]).is_none());
    let f = proto::decode_fields(&[0x08, 0x96, 0x01, 0x15, 1, 0, 0, 0]).unwrap();
    assert_eq!(proto::first_int(&f, 1), 150);
    assert_eq!(proto::first_int(&f, 2), 1);
    assert_eq!(proto::first_str(&f, 9), "");
    let wrapped = proto::wrap("BatchWebSocketGreeksMessage", vec![]);
    assert!(matches!(
        proto::decode_market(&wrapped),
        Some(proto::MarketFrame::Greeks(_))
    ));
}

// ---------------------------------------------------------------------------
// Identity and auth helpers
// ---------------------------------------------------------------------------

#[test]
fn totp_handling_matches_the_web() {
    assert_eq!(normalize_totp(" 12345 ").as_deref(), Some("012345"));
    assert_eq!(normalize_totp("123456").as_deref(), Some("123456"));
    assert_eq!(normalize_totp("1234567"), None);
    assert_eq!(normalize_totp("12a456"), None);
    assert_eq!(normalize_totp(""), None);
    assert_eq!(totp_candidates("123456"), [json!(123456)]);
    assert_eq!(totp_candidates("012345"), [json!(12345), json!("012345")]);
}

#[test]
fn phone_is_masked_like_the_web() {
    // web: f"{phone[:5]}***{phone[-2:]}" if len(phone) > 7 else "***"
    assert_eq!(mask_phone("9876543210"), "98765***10");
    assert_eq!(mask_phone("+919876543210"), "+9198***10");
    assert_eq!(mask_phone("12345678"), "12345***78");
    assert_eq!(mask_phone("1234567"), "***");
    assert_eq!(mask_phone(""), "***");
}

#[test]
fn identity_and_capabilities() {
    let b = NubraBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "nubra");
    assert_eq!(
        b.login_kind(),
        LoginKind::TwoStep {
            step1: &[],
            step2: &["otp"]
        }
    );
    assert!(!b.requires_totp());
    assert!(b.as_any().is_some_and(|a| a.is::<NubraBroker>()));
    assert!(!b.otp_pending());
    let c = b.capabilities();
    assert!(c.history && c.margin && c.streaming && !c.gtt);
    assert_eq!(c.depth_levels, &[5]);
    let keys: Vec<&str> = b.timeframe_map().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        keys,
        ["1s", "1m", "2m", "3m", "5m", "15m", "30m", "1h", "D", "W", "M"]
    );
    assert_eq!(b.supported_exchanges().len(), 7);
    assert!(b.create_feed(&AuthToken::new("")).is_err());
    assert!(b.create_feed(&AuthToken::new("tok")).is_ok());
    assert!(b.order_socket(&AuthToken::new("tok")).is_ok());
}
