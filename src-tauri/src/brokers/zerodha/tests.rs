//! Zerodha mapping tests against recorded Kite payloads
//! (`src-tauri/tests/fixtures/brokers/zerodha/`) and, where the web recorded
//! the same instrument, against the web's `/api/v1` fixtures.

use super::data::{parse_candles, to_depth, to_quote, KiteQuote};
use super::funds::{funds_from_margins, m2m, parse_margin, KiteMargins};
use super::gtt::{apply_mpp, gtt_body, map_gtt_book, KiteGtt};
use super::mapping::*;
use super::master_contract::parse_instruments;
use super::orders::{modify_order_form, place_order_form};
use super::streaming::KiteFeed;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use serde_json::Value;
use std::collections::HashMap;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/zerodha/", $name))
    };
}

fn web_fixture(path: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/web/rest")
        .join(path);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e));
    serde_json::from_str(&text).unwrap()
}

fn data<T: serde::de::DeserializeOwned>(json: &str) -> T {
    let env: KiteEnvelope<T> = serde_json::from_str(json).unwrap();
    assert_eq!(env.status, "success");
    env.data.unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_instruments(fixture!("instruments.csv")).unwrap());
    r
}

fn broker() -> ZerodhaBroker {
    ZerodhaBroker::new(master())
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_columns_by_header_name() {
    let rows = parse_instruments(fixture!("instruments.csv")).unwrap();
    // 27 data rows, one on an unknown exchange is dropped.
    assert_eq!(rows.len(), 26);
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY26OCTFUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 65);
    assert_eq!(fut.tick_size, 0.1);
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.token, "10011906::::39109");
    assert_eq!(fut.brexchange, "NFO");
    let pe = r.by_symbol("NFO", "NIFTY06OCT2622400PE").unwrap();
    assert_eq!(pe.brsymbol, "NIFTY26O0622400PE");
    assert_eq!(pe.strike, 22400.0);
    assert_eq!(pe.name, "NIFTY");
    let vedl = r.by_symbol("NFO", "VEDL27OCT26292.5CE").unwrap();
    assert_eq!(vedl.lot_size, 1150);
    let eq = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(eq.expiry, "");
    assert_eq!(eq.strike, 0.0);
    assert_eq!(eq.name, "STATE BANK OF INDIA");
    // Quoted name with a comma does not shift the columns.
    let tata = r.by_symbol("NSE", "TATASTEEL").unwrap();
    assert_eq!(tata.name, "TATA STEEL, LTD");
    assert_eq!(tata.tick_size, 0.01);
}

#[test]
fn master_contract_indices_and_global() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "NIFTY 50");
    assert_eq!(nifty.brexchange, "NSE");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    // BSE renames only on BSE_INDEX rows.
    assert!(r.by_symbol("BSE_INDEX", "BSEAUTO").is_some());
    assert_eq!(r.by_symbol("BSE", "AUTO").unwrap().name, "AUTO ETF");
    let us30 = r.by_symbol("GLOBAL_INDEX", "US30").unwrap();
    assert_eq!(us30.brexchange, "GLOBAL");
    let gift = r.by_symbol("GLOBAL_INDEX", "GIFTNIFTY").unwrap();
    assert_eq!(gift.brexchange, "NSEIX");
    assert_eq!(gift.brsymbol, "GIFT NIFTY");
    assert!(r.by_symbol("MCX_INDEX", "MCXMETLDEX").is_some());
    let usd = r.by_symbol("CDS", "USDINR27OCT26FUT").unwrap();
    assert_eq!(usd.tick_size, 0.0025);
    // Blank name falls back to the tradingsymbol.
    assert_eq!(r.by_symbol("NCO", "GOLD").unwrap().name, "GOLD");
}

#[test]
fn master_contract_mcx_lot_sizes() {
    let r = master();
    assert_eq!(
        r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap().lot_size,
        100
    );
    assert_eq!(
        r.by_symbol("MCX", "CRUDEOIL15OCT268650CE")
            .unwrap()
            .lot_size,
        100
    );
    assert_eq!(
        r.by_symbol("MCX", "MCXBULLDEX27OCT26FUT").unwrap().lot_size,
        30
    );
    assert_eq!(
        r.by_symbol("MCX", "MCXBULLDEX24NOV26FUT").unwrap().lot_size,
        15
    );
    assert_eq!(
        r.by_symbol("MCX", "GOLDPETAL30OCT26FUT").unwrap().lot_size,
        1
    );
    // Unknown underlying keeps Kite's 1.
    assert_eq!(r.by_symbol("MCX", "PEPPER30OCT26FUT").unwrap().lot_size, 1);
}

#[test]
fn master_contract_rejects_unknown_header() {
    let e = parse_instruments("a,b,c\n1,2,3\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_instruments("").is_err());
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let orders = map_orders(data(fixture!("orders.json")), &r);
    assert_eq!(orders.len(), 7);
    let o = &orders[0];
    assert_eq!(o.symbol, "NIFTY06OCT2622400PE");
    assert_eq!(o.status, "complete");
    assert_eq!(o.quantity, 65);
    // MCX: 1 Kite contract is 100 OpenAlgo units.
    let crude = &orders[1];
    assert_eq!(crude.symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!((crude.quantity, crude.filled_quantity), (100, 100));
    // Trigger-pending stop orders show as open in the REST book (web #2185).
    assert_eq!(orders[2].status, "open");
    assert_eq!(orders[2].order_type, "SL-M");
    assert_eq!(orders[2].trigger_price, 906.4);
    assert_eq!(orders[3].status, "open");
    assert_eq!(orders[4].status, "cancelled");
    let rej = &orders[5];
    assert_eq!(rej.status, "rejected");
    assert_eq!(rej.symbol, "NIFTY27OCT26FUT");
    assert!(rej
        .rejection_reason
        .as_deref()
        .unwrap()
        .starts_with("Insufficient funds"));
    assert_eq!(rej.exchange_order_id, None);
    assert_eq!(orders[6].status, "open");
}

#[test]
fn order_book_matches_web_fixture_rows() {
    let r = master();
    let orders = map_orders(data(fixture!("orders.json")), &r);
    let web = web_fixture("orderbook/populated.json");
    let rows = web["response"]["body"]["data"]["orders"]
        .as_array()
        .unwrap();
    let mut matched = 0;
    for o in &orders {
        if let Some(w) = rows.iter().find(|w| w["orderid"] == o.order_id.as_str()) {
            matched += 1;
            assert_eq!(w["symbol"], o.symbol.as_str());
            assert_eq!(w["exchange"], o.exchange.as_str());
            assert_eq!(w["action"], o.side.as_str());
            assert_eq!(w["order_status"], o.status.as_str());
            assert_eq!(w["pricetype"], o.order_type.as_str());
            assert_eq!(w["product"], o.product.as_str());
            assert_eq!(w["quantity"].as_i64().unwrap(), i64::from(o.quantity));
            assert_eq!(w["trigger_price"].as_f64().unwrap(), o.trigger_price);
        }
    }
    assert_eq!(matched, 5);
}

#[test]
fn trade_book_values_mcx_by_contract() {
    let r = master();
    let trades = map_trades(data(fixture!("trades.json")), &r);
    assert_eq!(trades[0].symbol, "NIFTY06OCT2622400PE");
    assert_eq!(trades[0].trade_id, "10000001");
    assert_eq!(trades[0].timestamp, "2026-10-03 09:43:52");
    assert!((trades[0].trade_value - 6734.0).abs() < 1e-6);
    let crude = &trades[1];
    assert_eq!(crude.quantity, 100);
    // fill_timestamp null -> order_timestamp
    assert_eq!(crude.timestamp, "2026-10-03 09:42:37");
    assert!((crude.trade_value - 900_800.0).abs() < 1e-6);
    let petal = &trades[2];
    assert_eq!(petal.quantity, 2);
    assert!((petal.trade_value - 22_500.0).abs() < 1e-6);
}

#[test]
fn positions_convert_mcx_and_symbols() {
    let r = master();
    let raw: KitePositions = data(fixture!("positions.json"));
    let p = map_positions(raw.net.unwrap(), &r);
    assert_eq!(p.len(), 4);
    assert_eq!(p[0].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!((p[0].quantity, p[0].buy_quantity), (200, 200));
    assert_eq!(p[1].symbol, "NIFTY06OCT2622400CE");
    assert_eq!(p[1].quantity, -65);
    assert_eq!(p[1].pnl, 438.75);
    assert_eq!(p[3].average_price, 954.12);
}

#[test]
fn holdings_tolerate_nulls_and_force_cnc() {
    let r = master();
    let h = map_holdings(data(fixture!("holdings.json")), &r);
    assert_eq!(h.len(), 3);
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(h[0].t1_quantity, 2);
    assert_eq!(h[0].pnl_percentage, 19.26);
    assert_eq!(h[0].isin.as_deref(), Some("INE062A01020"));
    // Unpriced scrip: zeros, not an error and not a fabricated -100%.
    assert_eq!((h[1].ltp, h[1].pnl, h[1].pnl_percentage), (0.0, 0.0, 0.0));
    assert_eq!(h[1].symbol, "NEWLISTCO");
    assert!(h.iter().all(|x| x.product == "CNC"));
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_derive_cash_and_m2m_like_web() {
    let m: KiteMargins = data(fixture!("margins.json"));
    let f = funds_from_margins(&m);
    assert!((f.available_cash - 341_093.3).abs() < 1e-6);
    assert_eq!(f.collateral, 5000.0);
    assert_eq!(f.utilised_debits, 145_706.55);
    let raw: KitePositions = data(fixture!("positions.json"));
    let positions = raw.net.unwrap();
    let mut ltp = HashMap::new();
    ltp.insert("MCX:CRUDEOIL26OCTFUT".to_string(), 9030.0);
    let (realised, unrealised) = m2m(&positions, &ltp);
    assert!((realised - 12.5).abs() < 1e-9);
    // crude: (9030-9008)*2*100 ; option: (150.2-156.95)*-65 ; SBIN CNC: (954.1-954.1234)*2
    let expected = 4400.0 + 438.75 + (954.1 - 954.1234) * 2.0;
    assert!((unrealised - expected).abs() < 1e-6, "{}", unrealised);
}

#[test]
fn margin_responses_parse() {
    let basket: Value = data(fixture!("margin_basket.json"));
    let r = parse_margin(&basket);
    assert_eq!(r.total_margin_required, 191_119.2);
    assert_eq!(r.span_margin, 112_760.0);
    assert_eq!(r.exposure_margin, 78_359.2);
    let orders: Value = data(fixture!("margin_orders.json"));
    assert_eq!(parse_margin(&orders).total_margin_required, 190.82);
}

#[test]
fn margin_payload_converts_mcx_and_skips_unknown() {
    let b = broker();
    let leg = |sym: &str, ex: &str, qty| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action: Action::Buy,
        quantity: qty,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let p = funds::margin_payload(
        &b,
        &[
            leg("CRUDEOIL19OCT26FUT", "MCX", 200),
            leg("NOPE", "NSE", 1),
            leg("NIFTY27OCT26FUT", "NFO", 65),
        ],
    )
    .unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0]["tradingsymbol"], "CRUDEOIL26OCTFUT");
    assert_eq!(p[0]["quantity"], 2);
    assert_eq!(p[0]["variety"], "regular");
    assert_eq!(p[1]["quantity"], 65);
    assert!(funds::margin_payload(&b, &[leg("CRUDEOIL19OCT26FUT", "MCX", 150)]).is_err());
}

// ---------------------------------------------------------------------------
// Quotes, depth, history
// ---------------------------------------------------------------------------

#[test]
fn quotes_and_depth() {
    let q: HashMap<String, KiteQuote> = data(fixture!("quote.json"));
    let sbin = to_quote(&QuoteKey::new("NSE", "SBIN"), &q["NSE:SBIN"]);
    assert_eq!((sbin.ltp, sbin.bid, sbin.ask), (954.1, 954.05, 954.1));
    assert_eq!((sbin.bid_qty, sbin.ask_qty), (120, 77));
    assert_eq!(sbin.close, 950.65);
    assert_eq!(sbin.volume, 4_823_170);
    let nifty = to_quote(&QuoteKey::new("NSE_INDEX", "NIFTY"), &q["NSE:NIFTY 50"]);
    assert_eq!(nifty.symbol, "NIFTY");
    assert_eq!(nifty.exchange, "NSE_INDEX");
    assert_eq!((nifty.bid, nifty.ask), (0.0, 0.0));

    let d = to_depth(&QuoteKey::new("NSE", "SBIN"), &q["NSE:SBIN"]);
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.bids[3], DepthLevel::default());
    assert_eq!(d.total_buy_qty, 1265);
    assert_eq!(d.total_sell_qty, 1600);
    assert_eq!(d.ltq, 5);
}

#[test]
fn depth_matches_web_fixture_shape() {
    let q: HashMap<String, KiteQuote> = data(fixture!("quote.json"));
    let d = to_depth(
        &QuoteKey::new("MCX", "CRUDEOIL19OCT26FUT"),
        &q["MCX:CRUDEOIL26OCTFUT"],
    );
    let web = web_fixture("depth/crudeoil_future_mcx.json");
    let w = &web["response"]["body"]["data"];
    assert_eq!(w["asks"].as_array().unwrap().len(), d.asks.len());
    assert_eq!(w["ltp"].as_f64().unwrap(), d.ltp);
    assert_eq!(w["oi"].as_i64().unwrap(), d.oi);
    assert_eq!(w["open"].as_f64().unwrap(), d.open);
    assert_eq!(w["prev_close"].as_f64().unwrap(), d.prev_close);
    assert_eq!(w["high"].as_f64().unwrap(), d.high);
}

#[test]
fn instrument_keys_use_kite_prefixes() {
    let r = master();
    assert_eq!(
        data::instrument_key(&r.by_symbol("NSE_INDEX", "NIFTY").unwrap()),
        "NSE:NIFTY 50"
    );
    assert_eq!(
        data::instrument_key(&r.by_symbol("GLOBAL_INDEX", "GIFTNIFTY").unwrap()),
        "NSEIX:GIFT NIFTY"
    );
    assert_eq!(
        data::instrument_key(&r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap()),
        "MCX:CRUDEOIL26OCTFUT"
    );
}

#[test]
fn history_candles() {
    #[derive(serde::Deserialize)]
    struct C {
        candles: Vec<Vec<Value>>,
    }
    let day: C = data(fixture!("historical_day.json"));
    let c = crate::brokers::common::history::sort_dedupe(parse_candles(&day.candles, true));
    assert_eq!(c.len(), 2);
    // 2026-09-30T00:00+05:30 + 5:30 = 2026-09-30T00:00Z
    assert_eq!(c[0].timestamp, 1_790_726_400);
    assert_eq!(c[1].close, 950.65);
    let min: C = data(fixture!("historical_minute.json"));
    let c = parse_candles(&min.candles, false);
    // 2026-10-03T09:15+05:30 = 03:45Z
    assert_eq!(c[0].timestamp, 1_790_999_100);
    assert_eq!(c[1].oi, 13_031_500);
    assert_eq!(data::kite_interval("1h").unwrap(), "60minute");
    let e = data::kite_interval("1w").unwrap_err();
    assert!(e
        .client_message()
        .starts_with("Interval 1w is not supported by Zerodha"));
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, qty: i32, pricetype: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: qty,
        price: 1500.5,
        order_type: pricetype.into(),
        product: "NRML".into(),
        validity: "IOC".into(),
        trigger_price: None,
        disclosed_quantity: Some(0),
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_order_form_matches_web_payload() {
    let form = place_order_form(&resolved("CRUDEOIL19OCT26FUT", "MCX", 200, "LIMIT")).unwrap();
    let m: HashMap<_, _> = form.into_iter().collect();
    assert_eq!(m["tradingsymbol"], "CRUDEOIL26OCTFUT");
    assert_eq!(m["exchange"], "MCX");
    assert_eq!(m["quantity"], "2");
    assert_eq!(m["price"], "1500.5");
    assert_eq!(m["trigger_price"], "0");
    assert_eq!(m["validity"], "DAY");
    assert_eq!(m["market_protection"], "-1");
    assert_eq!(m["tag"], "openalgo");
    assert_eq!(m["order_type"], "LIMIT");
    assert_eq!(m["product"], "NRML");
    let e = place_order_form(&resolved("CRUDEOIL19OCT26FUT", "MCX", 150, "LIMIT")).unwrap_err();
    assert!(e.client_message().contains("multiples of lot size 100"));
}

#[test]
fn modify_order_form_matches_web_payload() {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "SL".into(),
        quantity: 3,
        price: 0.0,
        trigger_price: 900.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("123", &m, &master()).unwrap();
    let f: HashMap<_, _> = modify_order_form(&rm).unwrap().into_iter().collect();
    assert_eq!(f["order_type"], "SL");
    assert_eq!(f["price"], "0");
    assert_eq!(f["trigger_price"], "900");
    assert_eq!(f["validity"], "DAY");
    let m2 = ModifyOrderRequest {
        trigger_price: 0.0,
        ..m
    };
    let rm2 = ResolvedModify::resolve("123", &m2, &master()).unwrap();
    let f2: HashMap<_, _> = modify_order_form(&rm2).unwrap().into_iter().collect();
    assert!(!f2.contains_key("trigger_price"));
}

#[test]
fn kite_errors_are_trader_facing() {
    let v: Value = serde_json::from_str(fixture!("errors.json")).unwrap();
    let env = |k: &str| -> KiteEnvelope<Value> { serde_json::from_value(v[k].clone()).unwrap() };
    let e = env("token_expired");
    assert_eq!(
        kite_error(&e.error_type, &e.message).client_message(),
        "Your Zerodha session has expired. Log in to Zerodha again."
    );
    let e = env("insufficient_funds");
    assert!(kite_error(&e.error_type, &e.message)
        .client_message()
        .starts_with("Insufficient funds"));
    let e = env("no_permission");
    assert!(kite_error(&e.error_type, &e.message)
        .client_message()
        .contains("does not have permission"));
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
fn gtt_single_and_two_leg_bodies() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let (kind, cond, orders) =
        gtt_body(&gtt(GttTriggerType::Single, PriceType::Limit), &row, 954.1).unwrap();
    assert_eq!(kind, "single");
    assert_eq!(cond["trigger_values"], serde_json::json!([900.5]));
    assert_eq!(cond["tradingsymbol"], "SBIN");
    assert_eq!(orders[0]["price"], 901.0);
    let (kind, cond, orders) =
        gtt_body(&gtt(GttTriggerType::Oco, PriceType::Limit), &row, 954.1).unwrap();
    assert_eq!(kind, "two-leg");
    assert_eq!(cond["trigger_values"], serde_json::json!([880.0, 990.0]));
    assert_eq!(orders[0]["price"], 879.0);
    assert_eq!(orders[1]["price"], 991.0);
}

#[test]
fn gtt_market_becomes_protected_limit() {
    let row = master().by_symbol("NSE", "SBIN").unwrap();
    let r = apply_mpp(&gtt(GttTriggerType::Single, PriceType::Market), &row, 954.1);
    assert_eq!(r.pricetype, PriceType::Limit);
    // 954.1 * 1.005 = 958.8705 -> tick 0.05 -> 958.85
    assert_eq!(r.price, 958.85);
    let r = apply_mpp(&gtt(GttTriggerType::Oco, PriceType::Market), &row, 954.1);
    assert_eq!(r.stoploss, 884.4);
    assert_eq!(r.target, 994.95);
}

#[test]
fn gtt_book_maps_symbols_and_mcx_units() {
    let b = broker();
    let rows: Vec<KiteGtt> = data(fixture!("gtt_triggers.json"));
    let book = map_gtt_book(rows.clone(), &b, false);
    assert_eq!(book.len(), 2);
    assert_eq!(book[0].trigger_id, "112127");
    assert_eq!(book[0].symbol, "SBIN");
    assert_eq!(book[1].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(book[1].legs[0].quantity, 100);
    assert_eq!(book[1].trigger_prices, vec![8900.0, 9200.0]);
    assert_eq!(map_gtt_book(rows, &b, true).len(), 3);
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

fn frame(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut v = (packets.len() as u16).to_be_bytes().to_vec();
    for p in packets {
        v.extend((p.len() as u16).to_be_bytes());
        v.extend(p);
    }
    v
}

fn ints(vals: &[i32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_be_bytes()).collect()
}

#[test]
fn subscribe_sends_two_json_frames_with_instrument_tokens() {
    let mut f = KiteFeed::new("key", "token", master());
    let frames = f.subscribe_frames(&[sub("SBIN", "NSE", FeedMode::Depth)]);
    assert_eq!(frames.len(), 2);
    let texts: Vec<Value> = frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            _ => panic!("text frames"),
        })
        .collect();
    assert_eq!(
        texts[0],
        serde_json::json!({"a": "subscribe", "v": [779521]})
    );
    assert_eq!(
        texts[1],
        serde_json::json!({"a": "mode", "v": ["full", [779521]]})
    );
    let un = f.unsubscribe_frames(&[sub("SBIN", "NSE", FeedMode::Depth)]);
    assert_eq!(un.len(), 1);
    let change = f.mode_change_frames(
        &sub("SBIN", "NSE", FeedMode::Ltp),
        &sub("SBIN", "NSE", FeedMode::Quote),
    );
    assert_eq!(change.len(), 1);
}

#[test]
fn feed_url_carries_access_token_half() {
    let b = broker();
    let feed = b.create_feed(&AuthToken::new("kitekey:accesstok")).unwrap();
    let req = feed.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://ws.kite.trade/?api_key=kitekey&access_token=accesstok"
    );
    assert!(b.create_feed(&AuthToken::new("nocolon")).is_err());
}

#[test]
fn binary_ticks_parse_big_endian_with_subscription_exchange() {
    let mut f = KiteFeed::new("k", "t", master());
    f.subscribe_frames(&[
        sub("SBIN", "NSE", FeedMode::Depth),
        sub("NIFTY", "NSE_INDEX", FeedMode::Quote),
        sub("RELIANCE", "NSE", FeedMode::Ltp),
        sub("USDINR27OCT26FUT", "CDS", FeedMode::Ltp),
    ]);
    // LTP packet (8 bytes)
    let ltp = ints(&[738561, 116770]);
    // Index quote packet (28 bytes): token, ltp, high, low, open, close, change
    let idx = ints(&[256265, 2251235, 2254000, 2243055, 2245010, 2248000, 3235]);
    // Full packet (184 bytes)
    let mut full = ints(&[
        779521, 95410, 5, 95287, 4823170, 512633, 689415, 95100, 95740, 94820, 95065,
    ]);
    full.extend(ints(&[1_790_916_300, 0, 0, 0, 1_790_916_301]));
    for i in 0..10i32 {
        full.extend((100 + i as u32).to_be_bytes());
        full.extend((95405 + i).to_be_bytes());
        full.extend((i as u16 + 1).to_be_bytes());
        full.extend([0u8, 0]);
    }
    assert_eq!(full.len(), 184);
    // CDS LTP: segment 3 in the low byte -> /10^7
    let usd_token: u32 = 268043011;
    assert_eq!(usd_token & 0xff, 3);
    let usd = [usd_token.to_be_bytes(), 832_500_000i32.to_be_bytes()].concat();
    let msg = Message::Binary(frame(&[ltp, idx, full, usd]));
    let events = f.parse(&msg);
    let ticks: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ticks.len(), 4);
    assert_eq!(
        (ticks[0].symbol.as_str(), ticks[0].ltp),
        ("RELIANCE", 1167.7)
    );
    assert_eq!(ticks[0].mode, 1);
    let n = &ticks[1];
    assert_eq!(
        (n.exchange.as_str(), n.symbol.as_str()),
        ("NSE_INDEX", "NIFTY")
    );
    assert_eq!(
        (n.ltp, n.high, n.low, n.open, n.close),
        (22512.35, 22540.0, 22430.55, 22450.1, 22480.0)
    );
    let s = &ticks[2];
    assert_eq!(
        (s.ltp, s.last_quantity, s.average_price, s.volume),
        (954.1, 5, 952.87, 4823170)
    );
    assert_eq!(
        (s.total_buy_quantity, s.total_sell_quantity),
        (512633, 689415)
    );
    assert_eq!(
        (s.open, s.high, s.low, s.close),
        (951.0, 957.4, 948.2, 950.65)
    );
    assert_eq!(s.last_trade_time_ms, 1_790_916_300_000);
    assert_eq!(s.mode, 3);
    assert_eq!(ticks[3].ltp, 83.25);
    let depth = events
        .iter()
        .find_map(|e| match e {
            FeedEvent::Depth(d) => Some(d.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(depth.buy.len(), 5);
    assert_eq!(depth.buy[0].price, 954.05);
    assert_eq!(depth.buy[0].quantity, 100);
    assert_eq!(depth.buy[0].orders, 1);
    assert_eq!(depth.sell[0].price, 954.10);
    assert_eq!(depth.sell[4].quantity, 109);
    // Heartbeat and unknown token
    assert_eq!(
        f.parse(&Message::Binary(vec![0])),
        vec![FeedEvent::Heartbeat]
    );
    let unknown = Message::Binary(frame(&[ints(&[42, 100])]));
    assert!(f.parse(&unknown).is_empty());
}

#[test]
fn order_postback_becomes_order_update() {
    let mut f = KiteFeed::new("k", "t", master());
    let ev = f.parse(&Message::Text(fixture!("order_postback.json").to_string()));
    match &ev[..] {
        [FeedEvent::OrderUpdate(u)] => {
            assert_eq!(u.symbol, "CRUDEOIL19OCT26FUT");
            assert_eq!(u.order_status, "complete");
            assert_eq!(u.quantity, 100);
            assert_eq!(u.filled_quantity, 100);
            assert_eq!(u.average_price, 9008.0);
        }
        other => panic!("unexpected {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// HTTP round trips against a local fake Kite (ephemeral port)
// ---------------------------------------------------------------------------

mod http_round_trip {
    use super::*;
    use axum::extract::{Form, RawQuery};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use std::sync::Arc;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn place_order_and_quote() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let s1 = seen.clone();
        let s2 = seen.clone();
        let app = Router::new()
            .route(
                "/orders/regular",
                post(move |h: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
                    s1.lock().push(format!(
                        "{}|{}|{}|{}",
                        h["authorization"].to_str().unwrap(),
                        h["x-kite-version"].to_str().unwrap(),
                        f["tradingsymbol"],
                        f["quantity"]
                    ));
                    Json(serde_json::json!({"status": "success", "data": {"order_id": "151220000000000"}}))
                }),
            )
            .route(
                "/quote",
                get(move |RawQuery(q): RawQuery| async move {
                    s2.lock().push(q.unwrap_or_default());
                    let v: Value = serde_json::from_str(fixture!("quote.json")).unwrap();
                    Json(v)
                }),
            )
            .route(
                "/user/margins",
                get(|| async {
                    Json(serde_json::from_str::<Value>(fixture!("errors.json")).unwrap()["token_expired"].clone())
                }),
            );
        let base = serve(app).await;
        let b = ZerodhaBroker::with_base_url(master(), base);
        let auth = AuthToken::new("kitekey:accesstok");
        let r = b
            .place_order(&auth, &resolved("CRUDEOIL19OCT26FUT", "MCX", 100, "MARKET"))
            .await
            .unwrap();
        assert_eq!(r.order_id, "151220000000000");
        let q = b
            .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
            .await
            .unwrap();
        assert_eq!(q.ltp, 22512.35);
        let e = b.get_funds(&auth).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        let seen = seen.lock().clone();
        assert_eq!(seen[0], "token kitekey:accesstok|3|CRUDEOIL26OCTFUT|1");
        assert_eq!(seen[1], "i=NSE%3ANIFTY%2050");
    }

    #[tokio::test]
    async fn malformed_token_is_refused_before_any_call() {
        let b = ZerodhaBroker::with_base_url(master(), "http://127.0.0.1:9");
        let e = b
            .get_order_book(&AuthToken::new("garbage"))
            .await
            .unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
    }
}
