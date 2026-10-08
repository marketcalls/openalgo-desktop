//! Upstox mapping tests against payloads built from the web code and the
//! Upstox API documentation (`src-tauri/tests/fixtures/brokers/upstox/`).
//! The HTTP adapter suite lives in `tests/broker_upstox_rest.rs`, the feed
//! end to end in `tests/broker_upstox_feed.rs`.

use super::data::{
    chunk_days, filter_by_date, find_by_token, normalise_key, parse_candles, to_depth, to_quote,
    today_candle, upstox_interval,
};
use super::funds::{funds_from_v3, is_service_hours, margin_instruments, parse_margin};
use super::gtt::{build_rules, iso_timestamp, map_gtt_book, market_protection, modify_body};
use super::mapping::*;
use super::master_contract::{parse_gz, parse_json, reformat_symbol};
use super::orders::{modify_body as order_modify_body, place_body};
use super::proto::{self, feed::FeedUnion, full_feed::FullFeedUnion};
use super::streaming::{
    instrument_key, normalise_for_test, normalize_order_update, sub_frame, UpstoxFeed,
};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::NaiveDate;
use prost::Message as _;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/upstox/", $name))
    };
}

fn books() -> Value {
    serde_json::from_str(fixture!("books.json")).unwrap()
}

fn market() -> Value {
    serde_json::from_str(fixture!("market.json")).unwrap()
}

fn data<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    assert_eq!(v["status"], "success");
    serde_json::from_value(v["data"].clone()).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_json(fixture!("instruments.json").as_bytes()).unwrap());
    r
}

fn broker() -> UpstoxBroker {
    UpstoxBroker::new(master())
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_rows_and_symbols() {
    let rows = parse_json(fixture!("instruments.json").as_bytes()).unwrap();
    // 21 instruments; NSE_COM and an unmapped segment are dropped.
    assert_eq!(rows.len(), 19);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(sbin.token, "NSE_EQ|INE062A01020");
    assert_eq!(sbin.brexchange, "NSE_EQ");
    assert_eq!(sbin.tick_size, 0.05);
    assert_eq!(sbin.expiry, "");
    assert_eq!(r.by_symbol("NSE", "RELIANCE").unwrap().tick_size, 0.1);
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY FUT 27 OCT 26");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.name, "NIFTY");
    let ce = r.by_symbol("NFO", "NIFTY06OCT2624500CE").unwrap();
    assert_eq!(ce.strike, 24500.0);
    assert_eq!(ce.token, "NSE_FO|40551");
    assert!(r.by_symbol("NFO", "NIFTY06OCT2624500PE").is_some());
    // Decimal strikes are concatenated verbatim.
    assert!(r.by_symbol("NFO", "VEDL27OCT26292.5CE").is_some());
    assert!(r.by_symbol("BFO", "SENSEX29OCT2682000PE").is_some());
    let usd = r.by_symbol("CDS", "USDINR27OCT26FUT").unwrap();
    assert_eq!(usd.brexchange, "NCD_FO");
    assert_eq!(usd.tick_size, 0.0025);
    let crude = r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!(crude.tick_size, 1.0);
    assert_eq!(crude.lot_size, 100);
    // Expiry pickers see the master's DD-MMM-YY values.
    assert_eq!(r.expiries("NFO", "NIFTY", None), ["06-OCT-26", "27-OCT-26"]);
}

#[test]
fn master_contract_index_renames_by_exchange() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "NIFTY 50");
    assert_eq!(nifty.token, "NSE_INDEX|Nifty 50");
    assert_eq!(nifty.tick_size, 0.0);
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    // SENSEX is not renamed; BSE short names only on BSE_INDEX rows.
    assert!(r.by_symbol("BSE_INDEX", "SENSEX").is_some());
    assert!(r.by_symbol("BSE_INDEX", "BSEAUTO").is_some());
    assert_eq!(r.by_symbol("BSE", "AUTO").unwrap().name, "AUTO ETF");
    assert!(r.by_symbol("GLOBAL_INDEX", "DOWJONES").is_some());
    assert!(r.by_symbol("GLOBAL_INDEX", "GIFTNIFTY").is_some());
    let brent = r.by_symbol("GLOBAL_INDEX", "BRENTOIL").unwrap();
    assert_eq!(brent.token, "GLOBAL_INDICATOR|BZUSD");
    assert_eq!(brent.brexchange, "GLOBAL_INDICATOR");
}

#[test]
fn reformat_is_positional() {
    assert_eq!(
        reformat_symbol("NIFTY FUT 26 DEC 24", "FUT"),
        "NIFTY26DEC24FUT"
    );
    assert_eq!(
        reformat_symbol("NIFTY 24000 CE 26 DEC 24", "CE"),
        "NIFTY26DEC2424000CE"
    );
    // Wrong part counts stay as Upstox sent them.
    assert_eq!(
        reformat_symbol("BANK X FUT 26 DEC 24", "FUT"),
        "BANK X FUT 26 DEC 24"
    );
    assert_eq!(reformat_symbol("RELIANCE", "EQ"), "RELIANCE");
}

#[test]
fn master_contract_gzip_and_bad_json() {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(fixture!("instruments.json").as_bytes())
        .unwrap();
    let gz = enc.finish().unwrap();
    assert_eq!(parse_gz(&gz).unwrap().len(), 19);
    let e = parse_json("{\"not\": \"a list\"}".as_bytes()).unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_gz(b"not gzip").is_err());
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let raw: Vec<UpstoxOrder> = data(&serde_json::from_str(fixture!("orders.json")).unwrap());
    let orders = map_orders(raw, &master());
    assert_eq!(orders.len(), 6);
    let o = &orders[0];
    // SBIN-EQ resolves through the instrument key.
    assert_eq!(o.symbol, "SBIN");
    assert_eq!(o.product, "CNC");
    assert_eq!(o.status, "complete");
    assert_eq!(o.average_price, 570.95);
    assert_eq!(o.exchange_order_id.as_deref(), Some("1300000025660919"));
    assert_eq!(orders[1].symbol, "NIFTY06OCT2624500CE");
    assert_eq!(orders[1].product, "NRML");
    assert_eq!(orders[1].status, "open");
    assert_eq!(orders[1].order_type, "SL-M");
    assert_eq!(orders[1].trigger_price, 120.5);
    assert_eq!(orders[2].product, "MIS");
    assert_eq!(orders[3].status, "cancelled");
    let rej = &orders[4];
    assert_eq!(rej.status, "rejected");
    assert_eq!(rej.symbol, "NIFTY27OCT26FUT");
    assert!(rej
        .rejection_reason
        .as_deref()
        .unwrap()
        .starts_with("Insufficient funds"));
    assert_eq!(orders[5].symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(orders[5].status, "open");
    let s = order_stats(&orders);
    assert_eq!(
        s,
        OrderStats {
            total_buy_orders: 3,
            total_sell_orders: 3,
            total_completed_orders: 1,
            total_open_orders: 3,
            total_rejected_orders: 1,
        }
    );
    // Cancel-all reads Upstox's own lowercase statuses.
    assert!(is_cancellable_raw("open"));
    assert!(is_cancellable_raw("trigger pending"));
    assert!(!is_cancellable_raw("put order req received"));
}

#[test]
fn trade_book() {
    let raw: Vec<UpstoxTrade> = data(&books()["trades"]);
    let trades = map_trades(raw, &master());
    assert_eq!(trades[0].symbol, "SBIN");
    assert_eq!(trades[0].trade_id, "50091502");
    assert_eq!(trades[0].product, "CNC");
    assert_eq!(trades[1].symbol, "NIFTY06OCT2624500CE");
    assert_eq!(trades[1].product, "MIS");
    assert_eq!(trades[1].trade_value, 75.0 * 101.2);
    assert_eq!(trades[1].timestamp, "2026-09-21 14:00:01");
}

#[test]
fn positions_fall_back_for_average_and_resolve_symbols() {
    let raw: Vec<UpstoxPosition> = data(&books()["positions"]);
    let p = map_positions(raw, &master());
    assert_eq!(p[0].symbol, "SBIN");
    // null average_price, buy_price 0 -> day_buy_price.
    assert_eq!(p[0].average_price, 571.0);
    assert_eq!(p[0].product, "CNC");
    assert_eq!(p[0].ltp, 573.5);
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].average_price, 101.2);
    assert_eq!(p[1].sell_quantity, 75);
    assert_eq!(p[2].quantity, 0);
    assert_eq!(p[2].average_price, 0.0);
    assert_eq!(p[2].realized_pnl, -40.0);
}

#[test]
fn holdings_guard_zero_average() {
    let raw: Vec<UpstoxHolding> = data(&books()["holdings"]);
    let h = map_holdings(raw, &master());
    assert_eq!(h[0].symbol, "NHPC");
    assert_eq!(h[0].product, "CNC");
    assert_eq!(h[0].pnl, 103.33);
    assert_eq!(h[0].pnl_percentage, 3.72);
    assert_eq!(h[0].t1_quantity, 2);
    assert_eq!(h[1].average_price, 0.0);
    assert_eq!(h[1].pnl_percentage, 0.0);
    let stats = PortfolioStats::from_holdings(&h);
    assert_eq!(stats.totalholdingvalue, 36.0 * 80.0 + 10.0 * 1400.0);
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[test]
fn funds_from_the_v3_breakdown() {
    let f = funds_from_v3(&books()["funds_v3"]["data"]);
    assert_eq!(f.available_cash, 125000.5);
    assert_eq!(f.collateral, 40000.0);
    assert_eq!(f.utilised_debits, 35000.25);
    assert!(is_service_hours(
        reqwest::StatusCode::LOCKED,
        &books()["funds_service_hours"]
    ));
    assert!(!is_service_hours(
        reqwest::StatusCode::BAD_REQUEST,
        &books()["funds_service_hours"]
    ));
}

#[test]
fn margin_request_and_response() {
    let b = broker();
    let leg = |sym: &str, ex: &str, price: f64| MarginLeg {
        key: QuoteKey::new(ex, sym),
        action: Action::Buy,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price,
        trigger_price: 0.0,
    };
    let v = margin_instruments(
        &b,
        &[
            leg("NIFTY27OCT26FUT", "NFO", 0.0),
            leg("NOPE", "NFO", 0.0),
            leg("NIFTY06OCT2624500CE", "NFO", 101.2),
        ],
    );
    assert_eq!(v.len(), 2);
    assert_eq!(
        v[0],
        json!({"instrument_key": "NSE_FO|52168", "quantity": 75, "transaction_type": "BUY", "product": "D"})
    );
    assert_eq!(v[1]["price"], 101.2);
    let m = parse_margin(&books()["margin"]["data"]);
    assert_eq!(m.total_margin_required, 165802.5);
    assert_eq!(m.span_margin, 126712.5);
    assert_eq!(m.exposure_margin, 31500.0);
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quotes_match_inner_instrument_token() {
    let q = market()["quotes"]["data"].clone();
    let rel = find_by_token(&q, "NSE_EQ|INE002A01018").unwrap();
    let k = QuoteKey::new("NSE", "RELIANCE");
    let quote = to_quote(&k, &rel);
    assert_eq!(quote.ltp, 1410.2);
    // prev_close_price, not the live ohlc.close.
    assert_eq!(quote.close, 1398.0);
    assert_eq!(quote.bid, 1410.1);
    assert_eq!(quote.ask, 1410.3);
    assert_eq!(quote.bid_qty, 120);
    assert_eq!(quote.volume, 5123456);
    assert_eq!(quote.change, 12.2);
    let opt = to_quote(
        &QuoteKey::new("NFO", "X"),
        &find_by_token(&q, "NSE_FO|40551").unwrap(),
    );
    assert_eq!(opt.oi, 4523100);
    assert!(find_by_token(&q, "NSE_EQ|NOPE").is_none());
}

#[test]
fn depth_is_padded_to_five() {
    let q = market()["quotes"]["data"].clone();
    let rel = find_by_token(&q, "NSE_EQ|INE002A01018").unwrap();
    let dep = to_depth(&QuoteKey::new("NSE", "RELIANCE"), &rel);
    assert_eq!(dep.bids.len(), 5);
    assert_eq!(dep.asks.len(), 5);
    assert_eq!(dep.bids[2].price, 1409.9);
    assert_eq!(dep.bids[3].price, 0.0);
    assert_eq!(dep.asks[1].quantity, 66);
    assert_eq!(dep.prev_close, 1398.0);
    assert_eq!(dep.total_buy_qty, 300000);
    assert_eq!(dep.high, 1415.5);
}

#[test]
fn reversed_quote_arguments_are_swapped() {
    let k = normalise_key(&QuoteKey::new("RELIANCE", "NSE"));
    assert_eq!(
        (k.exchange.as_str(), k.symbol.as_str()),
        ("NSE", "RELIANCE")
    );
    let k = normalise_key(&QuoteKey::new("NSE", "RELIANCE"));
    assert_eq!(k.symbol, "RELIANCE");
}

#[test]
fn instrument_key_falls_back_between_index_and_cash() {
    let b = broker();
    assert_eq!(
        super::data::instrument_key(&b, "NIFTY", "NSE").as_deref(),
        Some("NSE_INDEX|Nifty 50")
    );
    assert_eq!(
        super::data::instrument_key(&b, "SBIN", "NSE_INDEX").as_deref(),
        Some("NSE_EQ|INE062A01020")
    );
    assert_eq!(super::data::instrument_key(&b, "NOPE", "NSE"), None);
}

#[test]
fn history_intervals_and_chunks() {
    assert_eq!(upstox_interval("1m").unwrap(), ("minutes", 1));
    assert_eq!(upstox_interval("4h").unwrap(), ("hours", 4));
    assert_eq!(upstox_interval("W").unwrap(), ("weeks", 1));
    assert!(upstox_interval("7m")
        .unwrap_err()
        .client_message()
        .contains("not supported by Upstox"));
    assert_eq!(chunk_days("minutes", 15), 30);
    assert_eq!(chunk_days("minutes", 60), 90);
    assert_eq!(chunk_days("hours", 2), 90);
    assert_eq!(chunk_days("days", 1), 3650);
    assert_eq!(chunk_days("months", 1), 7300);
    assert_eq!(chunk_days("minutes", 7), 30);
}

#[test]
fn candles_daily_keep_the_ist_date_at_utc_midnight() {
    let rows: Vec<Vec<Value>> =
        serde_json::from_value(market()["history_day"]["data"]["candles"].clone()).unwrap();
    let c = parse_candles(&rows, true);
    // 2026-09-18 00:00 UTC
    assert_eq!(c[0].timestamp, 1789689600);
    assert_eq!(c[0].close, 1398.0);
    assert_eq!(c[0].volume, 4300000);
    let rows: Vec<Vec<Value>> =
        serde_json::from_value(market()["history_minute"]["data"]["candles"].clone()).unwrap();
    let c = parse_candles(&rows, false);
    // 09:16 IST = 03:46 UTC
    assert_eq!(c[0].timestamp, 1789689600 + 3 * 3600 + 46 * 60);
}

#[test]
fn intraday_filter_rewrites_to_milliseconds() {
    let rows = vec![
        vec![json!("2026-09-18T09:15:00+05:30"), json!(1.0)],
        vec![json!("2026-09-19T09:15:00+05:30"), json!(2.0)],
        vec![json!(1789703100000_i64), json!(3.0)],
    ];
    let kept = filter_by_date(rows, d(2026, 9, 18), d(2026, 9, 18));
    assert_eq!(kept.len(), 2);
    // 09:15 IST on the 18th = 03:45 UTC.
    assert_eq!(kept[0][0], json!(1789703100000.0));
    assert_eq!(kept[1][1], json!(3.0));
    // Numeric timestamps parse as UTC milliseconds afterwards.
    let c = parse_candles(&kept, false);
    assert_eq!(c[0].timestamp, 1789703100);
}

#[test]
fn todays_daily_candle_from_quotes() {
    let q = Quote {
        ltp: 1410.2,
        open: 1400.0,
        high: 1415.5,
        low: 1395.1,
        volume: 5123456,
        ..Default::default()
    };
    let today = d(2026, 9, 21);
    let c = today_candle(&q, today, &[]).unwrap();
    assert_eq!(c[0], json!("2026-09-21T00:00:00+05:30"));
    assert_eq!(c[4], json!(1410.2));
    let parsed = parse_candles(std::slice::from_ref(&c), true);
    assert_eq!(parsed[0].timestamp, 1789948800);
    // A quote identical to the last bar is stale.
    let last = vec![
        json!("2026-09-18T00:00:00+05:30"),
        json!(1400.0),
        json!(1415.5),
        json!(1395.1),
        json!(1410.2),
        json!(5123456),
        json!(0),
    ];
    assert!(today_candle(&q, today, &[last]).is_none());
    assert!(today_candle(&Quote::default(), today, &[]).is_none());
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, qty: i64, pricetype: PriceType) -> ResolvedOrder {
    let r = master();
    let row = r.by_symbol(exchange, symbol).unwrap();
    ResolvedOrder {
        symbol: symbol.into(),
        exchange: exchange.parse().unwrap(),
        action: Action::Buy,
        quantity: qty,
        price: 101.5,
        trigger_price: 100.0,
        pricetype,
        product: Product::Nrml,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: row,
    }
}

#[test]
fn place_body_zeroes_fields_the_order_type_does_not_use() {
    let o = resolved("NIFTY06OCT2624500CE", "NFO", 75, PriceType::Market);
    assert_eq!(
        place_body(&o),
        json!({
            "quantity": 75, "product": "D", "validity": "DAY", "price": 0.0, "tag": "openalgo",
            "instrument_token": "NSE_FO|40551", "order_type": "MARKET", "transaction_type": "BUY",
            "disclosed_quantity": 0, "trigger_price": 0.0, "is_amo": false
        })
    );
    let b = place_body(&resolved("NIFTY06OCT2624500CE", "NFO", 75, PriceType::Sl));
    assert_eq!(
        (b["price"].as_f64(), b["trigger_price"].as_f64()),
        (Some(101.5), Some(100.0))
    );
    let b = place_body(&resolved("NIFTY06OCT2624500CE", "NFO", 75, PriceType::SlM));
    assert_eq!(
        (b["price"].as_f64(), b["trigger_price"].as_f64()),
        (Some(0.0), Some(100.0))
    );
    let b = place_body(&resolved(
        "NIFTY06OCT2624500CE",
        "NFO",
        75,
        PriceType::Limit,
    ));
    assert_eq!(
        (b["price"].as_f64(), b["trigger_price"].as_f64()),
        (Some(101.5), Some(0.0))
    );
}

#[test]
fn modify_body_passes_prices_through() {
    let r = master();
    let m = ResolvedModify {
        order_id: "260921025562881".into(),
        symbol: "NIFTY06OCT2624500CE".into(),
        exchange: Exchange::Nfo,
        action: Action::Sell,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        quantity: 75,
        price: 99.0,
        trigger_price: 98.0,
        disclosed_quantity: 0,
        instrument: r.by_symbol("NFO", "NIFTY06OCT2624500CE").unwrap(),
    };
    assert_eq!(
        order_modify_body(&m),
        json!({"quantity": 75, "validity": "DAY", "price": 99.0, "order_id": "260921025562881",
               "order_type": "LIMIT", "disclosed_quantity": 0, "trigger_price": 98.0})
    );
}

#[test]
fn errors_are_trader_facing() {
    let e = upstox_error(reqwest::StatusCode::UNAUTHORIZED, &books()["token_expired"]);
    assert_eq!(e.code(), "AUTH_ERROR");
    let e = upstox_error(reqwest::StatusCode::BAD_REQUEST, &books()["order_rejected"]);
    assert_eq!(e.client_message(), "UDAPI1040: Price not required");
    let e = upstox_error(reqwest::StatusCode::OK, &books()["rate_limited"]);
    assert!(e.client_message().contains("limiting requests"));
    let e = upstox_error(reqwest::StatusCode::BAD_REQUEST, &json!({}));
    assert_eq!(e.client_message(), "Upstox refused the request.");
}

// ---------------------------------------------------------------------------
// GTT
// ---------------------------------------------------------------------------

fn gtt(kind: GttTriggerType, action: Action, pricetype: PriceType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        trigger_type: kind,
        action,
        product: Product::Cnc,
        quantity: 10,
        pricetype,
        price: 1350.0,
        trigger_price: 0.0,
        triggerprice_sl: 1300.0,
        stoploss: 1295.0,
        triggerprice_tg: 1500.0,
        target: 1505.0,
        last_price: Some(1400.0),
    }
}

#[test]
fn gtt_single_direction_follows_last_price() {
    let row = master().by_symbol("NSE", "RELIANCE").unwrap();
    let mut req = gtt(GttTriggerType::Single, Action::Buy, PriceType::Limit);
    let (kind, side, rules) = build_rules(&req, &row, 1400.0);
    assert_eq!((kind, side), ("SINGLE", Action::Buy));
    assert_eq!(
        rules,
        vec![json!({"strategy": "ENTRY", "trigger_type": "BELOW", "trigger_price": 1300.0})]
    );
    req.trigger_price = 1450.0;
    assert_eq!(
        build_rules(&req, &row, 1400.0).2[0]["trigger_type"],
        "ABOVE"
    );
    req.trigger_price = 1400.0;
    assert_eq!(
        build_rules(&req, &row, 1400.0).2[0]["trigger_type"],
        "IMMEDIATE"
    );
    // Without a last price the populated field decides.
    req.trigger_price = 0.0;
    assert_eq!(build_rules(&req, &row, 0.0).2[0]["trigger_type"], "ABOVE");
}

#[test]
fn gtt_oco_is_a_bracket_with_inverted_entry() {
    let row = master().by_symbol("NSE", "RELIANCE").unwrap();
    // Exit side SELL -> entry BUY (long): target above, stop below.
    let req = gtt(GttTriggerType::Oco, Action::Sell, PriceType::Limit);
    let (kind, side, rules) = build_rules(&req, &row, 1400.0);
    assert_eq!((kind, side), ("MULTIPLE", Action::Buy));
    assert_eq!(rules[0]["strategy"], "ENTRY");
    assert_eq!(rules[0]["trigger_price"], 1400.0);
    assert_eq!(
        rules[1],
        json!({"strategy": "TARGET", "trigger_type": "IMMEDIATE", "trigger_price": 1500.0})
    );
    assert_eq!(rules[2]["trigger_price"], 1300.0);
    // Exit side BUY -> entry SELL (short): target below.
    let req = gtt(GttTriggerType::Oco, Action::Buy, PriceType::Limit);
    let (_, side, rules) = build_rules(&req, &row, 1400.0);
    assert_eq!(side, Action::Sell);
    assert_eq!(rules[1]["trigger_price"], 1300.0);
    assert_eq!(rules[2]["trigger_price"], 1500.0);
}

#[test]
fn gtt_market_uses_market_protection_and_modify_strips_it() {
    let row = master().by_symbol("NSE", "RELIANCE").unwrap();
    let req = gtt(GttTriggerType::Single, Action::Buy, PriceType::Market);
    assert_eq!(market_protection(&req, &row, 1300.0), Some(1));
    assert_eq!(market_protection(&req, &row, 50.0), Some(2));
    let (_, _, rules) = build_rules(&req, &row, 1400.0);
    assert_eq!(rules[0]["market_protection"], 1);
    let m = modify_body(&req, &row, 1400.0, "GTT-1");
    assert_eq!(m["gtt_order_id"], "GTT-1");
    assert!(m["rules"][0].get("market_protection").is_none());
    assert!(m.get("instrument_token").is_none());
    assert!(m.get("transaction_type").is_none());
    let opt = master().by_symbol("NFO", "NIFTY06OCT2624500CE").unwrap();
    assert_eq!(market_protection(&req, &opt, 5.0), Some(5));
}

#[test]
fn gtt_book_keeps_live_triggers_and_hides_entry() {
    let b = broker();
    let book = map_gtt_book(&market()["gtt_book"]["data"], &b);
    assert_eq!(book.len(), 2);
    let s = &book[0];
    assert_eq!(s.trigger_id, "GTT-C26210900001");
    assert_eq!(s.trigger_type, "single");
    assert_eq!(s.status, "active");
    assert_eq!(s.symbol, "RELIANCE");
    assert_eq!(s.exchange, "NSE");
    assert_eq!(s.legs[0].product, "CNC");
    assert_eq!(s.created_at, "2025-09-21T09:59:59Z");
    assert_eq!(s.expires_at, "2026-09-21T14:13:20Z");
    let m = &book[1];
    assert_eq!(m.trigger_type, "two-leg");
    assert_eq!(m.symbol, "NIFTY06OCT2624500CE");
    assert_eq!(m.trigger_prices, vec![80.0, 140.0]);
    assert_eq!(m.legs[0].action, "SELL");
    assert_eq!(m.legs[0].product, "MIS");
    assert_eq!(m.created_at, "2025-09-21T09:59:59Z");
    assert_eq!(iso_timestamp(&json!(1758448799)), "2025-09-21T09:59:59Z");
    assert_eq!(
        iso_timestamp(&json!("2026-01-01T00:00:00Z")),
        "2026-01-01T00:00:00Z"
    );
    assert_eq!(iso_timestamp(&json!(12)), "");
    assert_eq!(iso_timestamp(&Value::Null), "");
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
    let r = master();
    let row = r.by_symbol(exchange, symbol).unwrap();
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: row.token.clone(),
        brsymbol: row.brsymbol.clone(),
        brexchange: row.brexchange.clone(),
        mode,
        depth: 5,
    }
}

fn feed() -> UpstoxFeed {
    UpstoxFeed::new(
        super::streaming::Authorizer::market(
            crate::brokers::common::http::client(),
            "http://127.0.0.1:9",
            "token-not-used-here",
        ),
        master(),
    )
}

fn frame_json(m: &Message) -> Value {
    match m {
        Message::Binary(b) => serde_json::from_slice(b).unwrap(),
        other => panic!("expected a binary frame, got {:?}", other),
    }
}

#[test]
fn subscribe_sends_binary_json_per_wire_mode() {
    let mut f = feed();
    use crate::brokers::common::streaming::BrokerFeed;
    // Ready as soon as the signed socket opens; no acknowledgement.
    assert!(!f.awaits_auth_ack());
    let frames = f.subscribe_frames(&[
        sub("RELIANCE", "NSE", FeedMode::Ltp),
        sub("NIFTY", "NSE_INDEX", FeedMode::Quote),
        sub("NIFTY06OCT2624500CE", "NFO", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 2);
    let a = frame_json(&frames[0]);
    assert_eq!(a["method"], "sub");
    assert_eq!(a["data"]["mode"], "ltpc");
    assert_eq!(a["data"]["instrumentKeys"], json!(["NSE_EQ|INE002A01018"]));
    assert_eq!(a["guid"].as_str().unwrap().len(), 20);
    let b = frame_json(&frames[1]);
    assert_eq!(b["data"]["mode"], "full");
    assert_eq!(
        b["data"]["instrumentKeys"],
        json!(["NSE_INDEX|Nifty 50", "NSE_FO|40551"])
    );
    // Already live in this mode: nothing to send.
    assert!(f
        .subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Ltp)])
        .is_empty());
    // Quote <-> depth share the `full` stream.
    let q = sub("NIFTY06OCT2624500CE", "NFO", FeedMode::Quote);
    let dep = sub("NIFTY06OCT2624500CE", "NFO", FeedMode::Depth);
    assert!(f.mode_change_frames(&dep, &q).is_empty());
    // LTP -> quote is an unsub then a sub.
    let ch = f.mode_change_frames(
        &sub("RELIANCE", "NSE", FeedMode::Ltp),
        &sub("RELIANCE", "NSE", FeedMode::Quote),
    );
    assert_eq!(frame_json(&ch[0])["method"], "unsub");
    assert!(frame_json(&ch[0])["data"].get("mode").is_none());
    assert_eq!(frame_json(&ch[1])["data"]["mode"], "full");
    let un = f.unsubscribe_frames(&[sub("NIFTY", "NSE_INDEX", FeedMode::Quote)]);
    assert_eq!(
        frame_json(&un[0])["data"]["instrumentKeys"],
        json!(["NSE_INDEX|Nifty 50"])
    );
    // A reconnect forgets what the socket carried.
    f.on_connected();
    assert_eq!(
        f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)])
            .len(),
        1
    );
}

#[test]
fn subscription_caps_drop_the_excess() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed();
    let many = |n: usize, mode: FeedMode, prefix: &str| -> Vec<FeedSubscription> {
        (0..n)
            .map(|i| FeedSubscription {
                symbol: format!("S{}{}", prefix, i),
                exchange: "NSE".into(),
                token: format!("NSE_EQ|{}{}", prefix, i),
                brsymbol: String::new(),
                brexchange: "NSE_EQ".into(),
                mode,
                depth: 5,
            })
            .collect()
    };
    let frames = f.subscribe_frames(&many(2100, FeedMode::Quote, "A"));
    let total: usize = frames
        .iter()
        .map(|m| {
            frame_json(m)["data"]["instrumentKeys"]
                .as_array()
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(total, 2000);
    // A second mode is capped by the combined budget (1500 for full).
    let frames = f.subscribe_frames(&many(10, FeedMode::Ltp, "B"));
    assert!(frames.is_empty());
}

#[test]
fn instrument_keys_from_master_rows() {
    assert_eq!(
        instrument_key("NSE_EQ", "NSE_EQ|INE002A01018"),
        "NSE_EQ|INE002A01018"
    );
    assert_eq!(instrument_key("NCD_FO", "10765"), "NCD_FO|10765");
    let m = sub_frame("unsub", &["A|1".to_string()], Some("full"));
    assert!(frame_json(&m)["data"].get("mode").is_none());
}

fn ltpc(ltp: f64, cp: f64) -> proto::Ltpc {
    proto::Ltpc {
        ltp,
        ltt: 1758448799000,
        ltq: 25,
        cp,
        iep: None,
    }
}

fn market_ff() -> proto::MarketFullFeed {
    proto::MarketFullFeed {
        ltpc: Some(ltpc(101.2, 118.4)),
        market_level: Some(proto::MarketLevel {
            bid_ask_quote: vec![
                proto::Quote {
                    bid_q: 75,
                    bid_p: 101.0,
                    ask_q: 150,
                    ask_p: 101.4,
                },
                proto::Quote {
                    bid_q: 300,
                    bid_p: 101.15,
                    ask_q: 0,
                    ask_p: 0.0,
                },
                proto::Quote {
                    bid_q: 0,
                    bid_p: 0.0,
                    ask_q: 225,
                    ask_p: 101.25,
                },
            ],
        }),
        option_greeks: None,
        market_ohlc: Some(proto::MarketOhlc {
            ohlc: vec![
                proto::Ohlc {
                    interval: "I1".into(),
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    vol: 1,
                    ts: 1,
                },
                proto::Ohlc {
                    interval: "1d".into(),
                    open: 110.0,
                    high: 125.0,
                    low: 95.0,
                    close: 101.2,
                    vol: 9876000,
                    ts: 1758400000000,
                },
            ],
        }),
        atp: 104.7,
        vtt: 9876000,
        oi: 4523100.0,
        iv: 0.12,
        tbq: 120000.0,
        tsq: 98000.0,
        ..Default::default()
    }
}

fn response(feeds: Vec<(&str, proto::Feed)>) -> Vec<u8> {
    proto::FeedResponse {
        r#type: proto::Type::LiveFeed as i32,
        feeds: feeds.into_iter().map(|(k, f)| (k.to_string(), f)).collect(),
        current_ts: 1758448800000,
        market_info: None,
    }
    .encode_to_vec()
}

#[test]
fn protobuf_ltpc_frame_decodes_to_a_tick() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Ltp)]);
    let bytes = response(vec![(
        "NSE_EQ|INE002A01018",
        proto::Feed {
            feed_union: Some(FeedUnion::Ltpc(ltpc(1410.2, 1398.0))),
            request_mode: proto::RequestMode::Ltpc as i32,
        },
    )]);
    let ev = f.parse(&Message::Binary(bytes));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("not a tick")
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(t.mode, 1);
    assert_eq!(t.ltp, 1410.2);
    assert_eq!(t.close, 1398.0);
    assert_eq!(t.change, 12.2);
    assert_eq!(t.last_quantity, 25);
    assert_eq!(t.last_trade_time_ms, 1758448799000);
}

#[test]
fn protobuf_full_frame_gives_quote_and_sorted_depth() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed();
    f.subscribe_frames(&[sub("NIFTY06OCT2624500CE", "NFO", FeedMode::Depth)]);
    let feed_msg = proto::Feed {
        feed_union: Some(FeedUnion::FullFeed(proto::FullFeed {
            full_feed_union: Some(FullFeedUnion::MarketFf(market_ff())),
        })),
        request_mode: proto::RequestMode::FullD5 as i32,
    };
    // Matched by the token after the bar when the segment differs.
    let ev = f.parse(&Message::Binary(response(vec![("NSE_FO|40551", feed_msg)])));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("not a tick")
    };
    assert_eq!(t.mode, 3);
    assert_eq!((t.open, t.high, t.low), (110.0, 125.0, 95.0));
    assert_eq!(t.volume, 9876000);
    assert_eq!(t.average_price, 104.7);
    assert_eq!(t.total_buy_quantity, 120000);
    assert_eq!(t.oi, 4523100);
    assert_eq!(t.close, 118.4);
    let FeedEvent::Depth(dp) = &ev[1] else {
        panic!("not depth")
    };
    let bids: Vec<f64> = dp.buy.iter().map(|l| l.price).collect();
    assert_eq!(bids, [101.15, 101.0, 0.0, 0.0, 0.0]);
    let asks: Vec<f64> = dp.sell.iter().map(|l| l.price).collect();
    assert_eq!(asks, [101.25, 101.4, 0.0, 0.0, 0.0]);
    assert_eq!(dp.buy[0].quantity, 300);
    assert_eq!(dp.buy[0].orders, 0);
}

#[test]
fn ltpc_is_carried_forward_and_unknown_keys_ignored() {
    // A depth-only frame without LTPC keeps the last price.
    let mut ff = market_ff();
    ff.ltpc = None;
    let fu = FeedUnion::FullFeed(proto::FullFeed {
        full_feed_union: Some(FullFeedUnion::MarketFf(ff)),
    });
    let cached = ltpc(100.0, 118.4);
    let ev = normalise_for_test("X", "NFO", FeedMode::Quote, Some(&fu), Some(&cached));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("not a tick")
    };
    assert_eq!(t.ltp, 100.0);
    // No full feed at all: the cached trade fields are published.
    let ev = normalise_for_test("X", "NFO", FeedMode::Quote, None, Some(&cached));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("not a tick")
    };
    assert_eq!((t.ltp, t.close), (100.0, 118.4));
    assert!(normalise_for_test("X", "NFO", FeedMode::Quote, None, None).is_empty());
    // Index full feed: quote fields, depth padded with zeros.
    let idx = FeedUnion::FullFeed(proto::FullFeed {
        full_feed_union: Some(FullFeedUnion::IndexFf(proto::IndexFullFeed {
            ltpc: Some(ltpc(24512.35, 24450.0)),
            market_ohlc: None,
        })),
    });
    let ev = normalise_for_test("NIFTY", "NSE_INDEX", FeedMode::Depth, Some(&idx), None);
    let FeedEvent::Depth(dp) = &ev[1] else {
        panic!("not depth")
    };
    assert_eq!(dp.ltp, 24512.35);
    assert!(dp.buy.iter().all(|l| l.price == 0.0));

    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed();
    let bytes = response(vec![(
        "NSE_EQ|UNKNOWN",
        proto::Feed {
            feed_union: Some(FeedUnion::Ltpc(ltpc(1.0, 1.0))),
            request_mode: 0,
        },
    )]);
    assert!(f.parse(&Message::Binary(bytes)).is_empty());
    let info = proto::FeedResponse {
        r#type: proto::Type::MarketInfo as i32,
        feeds: Default::default(),
        current_ts: 1,
        market_info: Some(proto::MarketInfo {
            segment_status: [("NSE_EQ".to_string(), proto::MarketStatus::NormalOpen as i32)]
                .into_iter()
                .collect(),
            ..Default::default()
        }),
    }
    .encode_to_vec();
    assert!(f.parse(&Message::Binary(info)).is_empty());
    assert!(f.parse(&Message::Binary(vec![0xff, 0xff])).is_empty());
}

#[test]
fn iep_wrapper_presence_survives_decoding() {
    let mut l = ltpc(1.0, 1.0);
    l.iep = Some(proto::DoubleValue { value: 0.0 });
    let bytes = l.encode_to_vec();
    let back = proto::Ltpc::decode(bytes.as_slice()).unwrap();
    assert_eq!(back.iep, Some(proto::DoubleValue { value: 0.0 }));
    let none = proto::Ltpc::decode(ltpc(1.0, 1.0).encode_to_vec().as_slice()).unwrap();
    assert_eq!(none.iep, None);
}

#[test]
fn status_text_frames_carry_no_events() {
    use crate::brokers::common::streaming::BrokerFeed;
    let mut f = feed();
    assert!(f
        .parse(&Message::Text(
            r#"{"status":"failed","method":"sub","error":"bad key"}"#.into()
        ))
        .is_empty());
    assert_eq!(f.parse(&Message::Ping(vec![])), vec![FeedEvent::Heartbeat]);
}

#[test]
fn order_updates_normalise_like_the_web() {
    let r = master();
    let ups = market()["order_updates"].clone();
    let a = normalize_order_update(&ups[0], &r).unwrap();
    assert_eq!(a.symbol, "NHPC");
    assert_eq!(a.product, "CNC");
    assert_eq!(a.order_status, "open");
    assert_eq!(a.pending_quantity, 10);
    assert_eq!(a.rejection_reason, "");
    let b = normalize_order_update(&ups[1], &r).unwrap();
    assert_eq!(b.symbol, "NIFTY06OCT2624500CE");
    assert_eq!(b.order_status, "rejected");
    assert_eq!(b.rejection_reason, "RMS: margin shortfall");
    assert_eq!(b.product, "MIS");
    // max(quantity - filled, 0) when Upstox sends 0 pending.
    assert_eq!(b.pending_quantity, 75);
    assert!(normalize_order_update(&ups[2], &r).is_none());
}

#[test]
fn registry_capabilities() {
    let b = broker();
    assert_eq!(b.id(), "upstox");
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "code" });
    assert!(!b.requires_totp());
    let c = b.capabilities();
    assert!(c.history && c.gtt && c.margin && c.streaming && c.order_feed);
    assert_eq!(b.timeframe_map().len(), 15);
    assert!(b.supported_exchanges().contains(&Exchange::GlobalIndex));
    // A short or empty token is refused before any call.
    assert!(b.create_feed(&AuthToken::new("short")).is_err());
    assert!(b
        .create_feed(&AuthToken::new("a-long-enough-token"))
        .is_ok());
}

#[test]
fn redirect_uri_is_recorded_for_the_exchange() {
    let a = crate::brokers::catalog::authorize_url(
        "upstox",
        "key",
        "http://127.0.0.1:5500/upstox/callback",
        "st",
    )
    .unwrap();
    assert!(a
        .url
        .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5500%2Fupstox%2Fcallback"));
    assert_eq!(a.redirect_uri, "http://127.0.0.1:5500/upstox/callback");
    let creds = BrokerCredentials {
        redirect_uri: Some(a.redirect_uri.clone()),
        ..Default::default()
    };
    assert_eq!(
        super::auth::redirect_uri(&creds),
        "http://127.0.0.1:5500/upstox/callback"
    );
    // Without a recorded redirect the web convention applies.
    assert_eq!(
        super::auth::redirect_uri(&BrokerCredentials::default()),
        super::auth::DEFAULT_REDIRECT_URI
    );
    let form = super::auth::token_form("c", "k", "s", &super::auth::redirect_uri(&creds));
    assert_eq!(
        form[3],
        (
            "redirect_uri",
            "http://127.0.0.1:5500/upstox/callback".to_string()
        )
    );
    assert_eq!(form[4], ("grant_type", "authorization_code".to_string()));
}
