//! Rupeezy mapping tests against payloads built from the web plugin and the
//! Vortex docs (`src-tauri/tests/fixtures/brokers/rupeezy/`). Most cases are
//! the web's `test/test_rupeezy_broker.py` (openalgo 9aa6f2e30), named in
//! each test's doc comment.

use super::books::{
    map_holdings, map_orders, map_positions, map_trades, trade_value, VortexHolding, VortexOrder,
    VortexPosition, VortexTrade,
};
use super::client::{category_for, retry_after, Category};
use super::data::{format_quote, parse_candles, to_depth};
use super::funds::{funds_from, positions_pnl};
use super::gtt::{map_gtt_book, modify_body, place_body, VortexGtt};
use super::mapping::*;
use super::master_contract::{index_symbol, parse_master};
use super::orders::{modify_order_body, place_order_body, rows_of};
use super::streaming::{decode_packet, split_frame, vortex_mode, RupeezyFeed, MAX_INSTRUMENTS};
use super::*;
use crate::brokers::common::history::sort_dedupe;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use std::time::Duration;

const CSV: &str = include_str!("../../../tests/fixtures/brokers/rupeezy/master.csv");
const RESPONSES: &str = include_str!("../../../tests/fixtures/brokers/rupeezy/responses.json");

fn resp(key: &str) -> Value {
    let all: Value = serde_json::from_str(RESPONSES).unwrap();
    all[key].clone()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_master(CSV).unwrap());
    r
}

fn row(symbol: &str, exchange: &str) -> SymbolData {
    master().by_symbol(exchange, symbol).unwrap()
}

fn order(symbol: &str, exchange: &str, action: Action, qty: i64, pt: PriceType) -> ResolvedOrder {
    let instrument = row(symbol, exchange);
    ResolvedOrder {
        symbol: symbol.into(),
        exchange: exchange.parse().unwrap(),
        action,
        quantity: qty,
        price: 0.0,
        trigger_price: 0.0,
        pricetype: pt,
        product: Product::Mis,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument,
    }
}

// ---------------------------------------------------------------------------
// Auth and transport
// ---------------------------------------------------------------------------

/// Web `auth_api.authenticate_broker`: SHA256(application_id + auth +
/// api_key), no separators.
#[test]
fn checksum_concatenates_application_id_auth_and_api_key() {
    assert_eq!(
        checksum("APP1", "auth9", "KEY1"),
        "4cae26ebae3b14f8ebecd25ea2de1dcb1cf376a44b15be380b2193ce2d187a3a"
    );
}

/// Web `test_rate_limit_categories`, `test_data_rate_limit_categories`.
#[test]
fn requests_are_paced_by_their_vortex_category() {
    assert_eq!(category_for("/user/funds"), Category::Account);
    assert_eq!(category_for("/user/session"), Category::Account);
    assert_eq!(category_for("/trading/orders/gtt"), Category::Trading);
    assert_eq!(category_for("/data/quotes"), Category::Quotes);
    assert_eq!(category_for("/data/history"), Category::History);
    let p = Pacers::default();
    assert_eq!(p.quotes.interval(), Duration::from_millis(1050));
    assert_eq!(p.account.interval(), Duration::from_millis(1050));
    assert_eq!(p.trading.interval(), Duration::from_millis(110));
}

/// Web `test_order_ids_with_reserved_characters_are_escaped_in_the_path`.
#[test]
fn order_ids_with_reserved_characters_are_escaped_in_the_path() {
    assert_eq!(
        order_path("/trading/orders/regular", "OFPIM00001?:"),
        "/trading/orders/regular/OFPIM00001%3F%3A"
    );
    assert_eq!(
        order_path("/trading/orders/gtt", "fc3e3201-8d34"),
        "/trading/orders/gtt/fc3e3201-8d34"
    );
}

/// Web `_retry_after_seconds` and `test_a_read_honours_an_http_date_retry_after`.
#[test]
fn retry_after_reads_seconds_or_an_http_date() {
    let now = Utc.with_ymd_and_hms(2015, 10, 21, 7, 27, 58).unwrap();
    assert_eq!(
        retry_after(Some("1.5"), 0, now),
        Duration::from_millis(1500)
    );
    assert_eq!(
        retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 0, now),
        Duration::from_secs(2)
    );
    // A date in the past waits nothing; garbage falls back to 1 s, 2 s.
    assert_eq!(
        retry_after(Some("Wed, 21 Oct 2015 07:00:00 GMT"), 0, now),
        Duration::ZERO
    );
    assert_eq!(retry_after(Some("soon"), 1, now), Duration::from_secs(2));
    assert_eq!(retry_after(None, 0, now), Duration::from_secs(1));
    assert_eq!(retry_after(Some("1e300"), 0, now), Duration::MAX);
}

// ---------------------------------------------------------------------------
// Vocabulary and quantities
// ---------------------------------------------------------------------------

/// Web `test_mcx_is_sent_in_lots_and_read_back_in_units`.
#[test]
fn mcx_is_sent_in_lots_and_read_back_in_units() {
    assert_eq!(to_vortex_quantity(200, "MCX_FO", 100).unwrap(), 2);
    assert_eq!(from_vortex_quantity(2, "MCX_FO", 100), 200);
    let e = to_vortex_quantity(150, "MCX_FO", 100).unwrap_err();
    assert!(e
        .client_message()
        .contains("not a multiple of the lot size 100"));
}

/// Web `test_other_segments_are_sent_in_units` (BSE_FO takes units too).
#[test]
fn other_segments_are_sent_in_units() {
    assert_eq!(to_vortex_quantity(130, "NSE_FO", 65).unwrap(), 130);
    assert_eq!(to_vortex_quantity(40, "BSE_FO", 20).unwrap(), 40);
    assert_eq!(to_vortex_quantity(5, "NSE_EQ", 1).unwrap(), 5);
    assert_eq!(from_vortex_quantity(130, "NSE_FO", 65), 130);
}

/// Web `test_product_round_trip`.
#[test]
fn product_round_trip() {
    assert_eq!(product(Product::Mis), "INTRADAY");
    assert_eq!(product(Product::Cnc), "DELIVERY");
    assert_eq!(product(Product::Nrml), "DELIVERY");
    assert_eq!(product_from_vortex("NSE", "DELIVERY"), "CNC");
    assert_eq!(product_from_vortex("NFO", "DELIVERY"), "NRML");
    assert_eq!(product_from_vortex("NSE", "MTF"), "CNC");
    assert_eq!(product_from_vortex("MCX", "INTRADAY"), "MIS");
}

#[test]
fn statuses_and_varieties_map_to_openalgo() {
    for (vortex, oa) in [
        ("EXECUTED", "complete"),
        ("PENDING", "open"),
        ("OMSXMITTED", "open"),
        ("TRIGGER PENDING", "trigger pending"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("CANCELLED", "cancelled"),
        ("REJECTED", "rejected"),
        ("AMO_PLACED", "amo_placed"),
    ] {
        assert_eq!(map_status(vortex), oa, "{}", vortex);
    }
    assert!(is_open("PENDING") && is_open("TRIGGER PENDING") && !is_open("EXECUTED"));
    for (pt, v) in [
        (PriceType::Market, "RL-MKT"),
        (PriceType::Limit, "RL"),
        (PriceType::Sl, "SL"),
        (PriceType::SlM, "SL-MKT"),
    ] {
        assert_eq!(variety(pt), v);
        assert_eq!(price_type_from_variety(v), pt.as_str());
    }
    assert_eq!(ticker("NSE_FO", "NIFTY26OCTFUT"), "NSE:NIFTY26OCTFUT");
    assert_eq!(split_ticker("NSE:SBIN"), "SBIN");
    assert_eq!(oa_exchange("BSE_FO"), "BFO");
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

/// Web `test_master_symbols` (and the BSE option and renamed indices).
#[test]
fn master_symbols_follow_the_openalgo_format() {
    let m = master();
    for (symbol, exchange, brsymbol, brexchange, itype) in [
        ("RELIANCE", "NSE", "RELIANCE", "NSE_EQ", "EQ"),
        ("RELIANCE", "BSE", "RELIANCE", "BSE_EQ", "EQ"),
        ("PAPADMALJI-ST", "NSE", "PAPADMALJI-ST", "NSE_EQ", "EQ"),
        ("NIFTY", "NSE_INDEX", "NIFTYIDX", "NSE_EQ", "EQ"),
        ("NIFTYIT", "NSE_INDEX", "NIFTYITIDX", "NSE_EQ", "EQ"),
        (
            "NIFTYMIDCAP100",
            "NSE_INDEX",
            "NIFTYMID100IDX",
            "NSE_EQ",
            "EQ",
        ),
        ("SENSEX", "BSE_INDEX", "SENSEXIDX", "BSE_EQ", "EQ"),
        ("BSEREALTY", "BSE_INDEX", "REALTYIDX", "BSE_EQ", "EQ"),
        ("NIFTY27OCT26FUT", "NFO", "NIFTY26OCTFUT", "NSE_FO", "FUT"),
        (
            "NIFTY27OCT2625000CE",
            "NFO",
            "NIFTY26OCT25000CE",
            "NSE_FO",
            "CE",
        ),
        (
            "BANKBARODA27OCT26177.5PE",
            "NFO",
            "BANKBARODA26OCT177.5PE",
            "NSE_FO",
            "PE",
        ),
        (
            "SENSEX29OCT2682000CE",
            "BFO",
            "SENSEX26OCT82000CE",
            "BSE_FO",
            "CE",
        ),
        (
            "CRUDEOIL20MAR28FUT",
            "MCX",
            "CRUDEOIL28MARFUT",
            "MCX_FO",
            "FUT",
        ),
    ] {
        let r = m
            .by_symbol(exchange, symbol)
            .unwrap_or_else(|| panic!("{} {}", symbol, exchange));
        assert_eq!(
            (
                r.brsymbol.as_str(),
                r.brexchange.as_str(),
                r.instrument_type.as_str()
            ),
            (brsymbol, brexchange, itype),
            "{}",
            symbol
        );
    }
}

/// Web `test_master_scaling_and_expiry`.
#[test]
fn master_scales_ticks_and_formats_expiry() {
    let fut = row("NIFTY27OCT26FUT", "NFO");
    assert_eq!(
        (fut.expiry.as_str(), fut.strike, fut.lot_size, fut.tick_size),
        ("27-OCT-26", 0.0, 65, 0.1)
    );
    let opt = row("BANKBARODA27OCT26177.5PE", "NFO");
    assert_eq!(
        (opt.strike, opt.tick_size, opt.name.as_str()),
        (177.5, 0.01, "BANKBARODA")
    );
    let crude = row("CRUDEOIL20MAR28FUT", "MCX");
    assert_eq!((crude.lot_size, crude.tick_size), (100, 1.0));
    let rel = row("RELIANCE", "NSE");
    assert_eq!(
        (rel.expiry.as_str(), rel.name.as_str(), rel.token.as_str()),
        ("", "RELIANCE INDUSTRIES LTD", "2885")
    );
    // A quoted name with a comma keeps its columns.
    assert_eq!(
        row("PAPADMALJI-ST", "NSE").name,
        "PAPADMALJI AGRO FOODS, LTD"
    );
}

/// Web `test_ineligible_contracts_are_dropped_but_indices_kept`; segments
/// OpenAlgo does not list (NSE_CUR) are dropped.
#[test]
fn ineligible_contracts_are_dropped_but_indices_kept() {
    let rows = parse_master(CSV).unwrap();
    let has = |s: &str, e: &str| rows.iter().any(|r| r.symbol == s && r.exchange == e);
    assert!(!has("DEADCO", "NSE"));
    assert!(has("NIFTY", "NSE_INDEX") && has("SENSEX", "BSE_INDEX"));
    assert!(!rows.iter().any(|r| r.brexchange == "NSE_CUR"));
    assert_eq!(rows.len(), 19);
}

/// Web `test_index_names_follow_the_openalgo_list`.
#[test]
fn index_names_follow_the_openalgo_list() {
    assert_eq!(index_symbol("NIFTYMID100", "NSE_INDEX"), "NIFTYMIDCAP100");
    assert_eq!(index_symbol("REALTY", "NSE_INDEX"), "NIFTYREALTY");
    assert_eq!(index_symbol("REALTY", "BSE_INDEX"), "BSEREALTY");
    assert_eq!(index_symbol("BANKNIFTY", "NSE_INDEX"), "BANKNIFTY");
}

#[test]
fn a_master_without_its_columns_is_refused() {
    let e = parse_master("token,exchange,symbol\n1,NSE_EQ,X\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
}

// ---------------------------------------------------------------------------
// Order bodies
// ---------------------------------------------------------------------------

/// Web `test_limit_order_payload`.
#[test]
fn limit_order_payload() {
    let mut o = order("SBIN", "NSE", Action::Buy, 5, PriceType::Limit);
    o.price = 801.5;
    o.product = Product::Cnc;
    assert_eq!(
        place_order_body(&o, "test").unwrap(),
        json!({
            "ticker": "NSE:SBIN",
            "transaction_type": "BUY",
            "product": "DELIVERY",
            "variety": "RL",
            "quantity": 5,
            "price": 801.5,
            "trigger_price": 0.0,
            "disclosed_quantity": 0,
            "validity": "DAY",
            "is_amo": false,
            "order_identifier": "test",
        })
    );
}

/// Web `test_market_and_stop_orders`, `test_mcx_disclosed_quantity_is_sent_in_lots`.
#[test]
fn market_and_stop_orders_and_mcx_lots() {
    let mut mkt = order(
        "CRUDEOIL20MAR28FUT",
        "MCX",
        Action::Sell,
        200,
        PriceType::Market,
    );
    mkt.price = 6500.0;
    mkt.product = Product::Nrml;
    let b = place_order_body(&mkt, "").unwrap();
    assert_eq!(
        (
            &b["ticker"],
            &b["variety"],
            &b["price"],
            &b["quantity"],
            &b["product"]
        ),
        (
            &json!("MCX:CRUDEOIL28MARFUT"),
            &json!("RL-MKT"),
            &json!(0.0),
            &json!(2),
            &json!("DELIVERY")
        )
    );
    let mut slm = order(
        "CRUDEOIL20MAR28FUT",
        "MCX",
        Action::Sell,
        100,
        PriceType::SlM,
    );
    slm.trigger_price = 6400.0;
    let b = place_order_body(&slm, "").unwrap();
    assert_eq!(
        (&b["variety"], &b["trigger_price"], &b["price"]),
        (&json!("SL-MKT"), &json!(6400.0), &json!(0.0))
    );
    let mut lim = order(
        "CRUDEOIL20MAR28FUT",
        "MCX",
        Action::Buy,
        500,
        PriceType::Limit,
    );
    lim.disclosed_quantity = 200;
    let b = place_order_body(&lim, "").unwrap();
    assert_eq!(
        (&b["quantity"], &b["disclosed_quantity"]),
        (&json!(5), &json!(2))
    );
    // Not a whole number of lots: refused before anything is sent.
    let odd = order(
        "CRUDEOIL20MAR28FUT",
        "MCX",
        Action::Buy,
        150,
        PriceType::Limit,
    );
    assert!(matches!(
        place_order_body(&odd, ""),
        Err(crate::error::AppError::Validation(_))
    ));
}

/// Web `transform_data`: `order_identifier = str(strategy or "openalgo")[:50]`.
#[test]
fn the_order_identifier_is_the_strategy_cut_to_fifty_characters() {
    use super::orders::order_identifier;
    assert_eq!(order_identifier("NIFTY scalper"), "NIFTY scalper");
    assert_eq!(order_identifier(""), "openalgo");
    let long = "x".repeat(60);
    assert_eq!(order_identifier(&long), "x".repeat(50));
    // Cut by character, like a Python slice, not by byte.
    assert_eq!(order_identifier(&"\u{20b9}".repeat(55)).chars().count(), 50);
    let b = place_order_body(
        &order("SBIN", "NSE", Action::Buy, 1, PriceType::Market),
        "Momentum",
    )
    .unwrap();
    assert_eq!(b["order_identifier"], "Momentum");
}

#[test]
fn modify_body_carries_the_traded_quantity_in_vortex_units() {
    let m = ResolvedModify {
        order_id: "OFPIM00003?:".into(),
        symbol: "CRUDEOIL20MAR28FUT".into(),
        exchange: crate::brokers::common::mapping::Exchange::Mcx,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Sl,
        quantity: 300,
        price: 6410.0,
        trigger_price: 6400.0,
        disclosed_quantity: 0,
        instrument: row("CRUDEOIL20MAR28FUT", "MCX"),
    };
    assert_eq!(
        modify_order_body(&m, 1).unwrap(),
        json!({
            "variety": "SL",
            "quantity": 3,
            "traded_quantity": 1,
            "price": 6410.0,
            "trigger_price": 6400.0,
            "disclosed_quantity": 0,
            "validity": "DAY",
        })
    );
}

/// Web `payload.get("orders") or payload.get("data")`: an empty first list
/// falls to the second; neither key at all is a book that was not sent.
#[test]
fn rows_are_read_from_the_first_list_that_has_them() {
    let v = json!({"orders": [], "data": [{"order_id": "A"}]});
    let rows: Vec<VortexOrder> = rows_of(&v, &["orders", "data"]).unwrap();
    assert_eq!(rows[0].order_id, "A");
    let empty: Vec<VortexOrder> = rows_of(&json!({"orders": []}), &["orders", "data"]).unwrap();
    assert!(empty.is_empty());
    assert!(rows_of::<VortexOrder>(&json!({"status": "success"}), &["orders"]).is_none());
    // `null` is no list: not an empty book (BR-03).
    assert!(rows_of::<VortexOrder>(&json!({"orders": null}), &["orders", "data"]).is_none());
    assert!(
        rows_of::<VortexOrder>(&json!({"orders": null, "data": null}), &["orders", "data"])
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

fn parsed<T: serde::de::DeserializeOwned>(key: &str, list: &str) -> Vec<T> {
    rows_of(&resp(key), &[list]).unwrap()
}

#[test]
fn order_book_maps_to_openalgo_symbols_units_and_statuses() {
    let rows: Vec<VortexOrder> = parsed("orders", "orders");
    let book = map_orders(&rows, &master());
    let sbin = &book[0];
    assert_eq!(
        (
            sbin.symbol.as_str(),
            sbin.exchange.as_str(),
            sbin.product.as_str(),
            sbin.order_type.as_str(),
            sbin.status.as_str(),
            sbin.quantity,
            sbin.price
        ),
        ("SBIN", "NSE", "MIS", "LIMIT", "open", 10, 801.5)
    );
    assert_eq!(sbin.exchange_order_id.as_deref(), Some("1100000012345678"));
    let crude = &book[1];
    assert_eq!(
        (
            crude.symbol.as_str(),
            crude.exchange.as_str(),
            crude.product.as_str(),
            crude.order_type.as_str(),
            crude.status.as_str(),
            crude.quantity,
            crude.filled_quantity,
            crude.average_price
        ),
        (
            "CRUDEOIL20MAR28FUT",
            "MCX",
            "NRML",
            "MARKET",
            "complete",
            200,
            200,
            6500.0
        )
    );
    let opt = &book[2];
    assert_eq!(
        (
            opt.symbol.as_str(),
            opt.status.as_str(),
            opt.order_type.as_str()
        ),
        ("NIFTY27OCT2625000CE", "rejected", "SL")
    );
    assert_eq!(
        opt.rejection_reason.as_deref(),
        Some("RMS: margin exceeds available funds")
    );
    let infy = &book[3];
    assert_eq!(
        (
            infy.order_id.as_str(),
            infy.status.as_str(),
            infy.order_type.as_str(),
            infy.product.as_str()
        ),
        ("OFPIM00001?:", "trigger pending", "SL-M", "CNC")
    );
}

/// Web `test_trade_book_maps_to_openalgo`, `test_mcx_trade_value_uses_the_quotation_multiplier`.
#[test]
fn trade_book_maps_to_openalgo_and_values_mcx_by_quotation() {
    let rows: Vec<VortexTrade> = parsed("trades", "trades");
    let t = map_trades(&rows, &master());
    assert_eq!(
        (
            t[0].symbol.as_str(),
            t[0].exchange.as_str(),
            t[0].product.as_str(),
            t[0].side.as_str(),
            t[0].quantity,
            t[0].average_price,
            t[0].trade_value,
            t[0].order_id.as_str(),
            t[0].timestamp.as_str()
        ),
        (
            "ACC",
            "NSE",
            "MIS",
            "SELL",
            1,
            1856.75,
            1856.75,
            "NXAAE00002@6",
            "2026-10-19 13:06:43"
        )
    );
    // MCX trades come back in lots; OpenAlgo shows units (2 lots x 100).
    assert_eq!(
        (t[1].exchange.as_str(), t[1].product.as_str(), t[1].quantity),
        ("MCX", "NRML", 200)
    );
    assert_eq!(t[1].trade_value, 1_300_000.0);
    // GOLDM: lot 100 g quoted per 10 g. One lot at 7,000 is 70,000.
    assert_eq!((t[2].quantity, t[2].trade_value), (100, 70_000.0));
    assert_eq!(
        trade_value(100, 6500.0, "MCX", Some("CRUDEOIL"), 100),
        650_000.0
    );
    assert_eq!(trade_value(5, 954.0, "NSE", Some("SBIN"), 1), 4770.0);
}

/// Web `test_trade_book_fills_bare_trades_from_the_order_book`.
#[test]
fn a_bare_trade_is_completed_from_its_order() {
    let mut trades: Vec<VortexTrade> = parsed("trades_bare", "trades");
    let orders: Vec<VortexOrder> = parsed("orders_for_bare_trade", "orders");
    trades[0].fill_from(&orders[0]);
    let t = &trades[0];
    assert_eq!(
        (
            t.ticker.as_str(),
            t.exchange.as_str(),
            t.transaction_type.as_str(),
            t.product.as_str(),
            t.trade_price
        ),
        (
            "MCX:GOLDPETAL26OCTFUT",
            "MCX_FO",
            "SELL",
            "DELIVERY",
            14929.0
        )
    );
}

fn position(v: Value) -> VortexPosition {
    serde_json::from_value(v).unwrap()
}

/// Bought 10 at 100, sold 5 at 110, 5 still open at 100, now 105.
fn partly_closed() -> Value {
    json!({"ticker": "NSE:SBIN", "exchange": "NSE_EQ", "product": "INTRADAY", "quantity": 5,
           "buy_value": 1000, "sell_value": 550, "average_price": 100, "lot_size": 1, "ltp": 105})
}

/// Web `test_position_pnl_is_booked_only_once_flat`,
/// `test_partly_closed_position_splits_into_realized_and_unrealized`,
/// `test_position_split_is_unknown_without_a_price`.
#[test]
fn position_pnl_and_its_split() {
    let p = |v: Value| position(v).pnl();
    assert_eq!(
        p(json!({"quantity": 0, "buy_value": 1000, "sell_value": 1100})),
        Some(100.0)
    );
    // Open without a price: unknown, not zero.
    assert_eq!(
        p(json!({"quantity": 5, "buy_value": 1000, "sell_value": 0})),
        None
    );
    assert_eq!(
        p(json!({"quantity": 5, "buy_value": 1000, "sell_value": 0, "ltp": 210})),
        Some(50.0)
    );
    let pc = position(partly_closed());
    assert_eq!(pc.pnl_split(), Some((50.0, 25.0)));
    assert_eq!(pc.pnl(), Some(75.0));
    let mut no_ltp = partly_closed();
    no_ltp["ltp"] = json!(0);
    assert_eq!(position(no_ltp).pnl_split(), None);
    let mut no_avg = partly_closed();
    no_avg["average_price"] = json!(0);
    assert_eq!(position(no_avg).pnl_split(), None);
    let mut flat = partly_closed();
    flat["quantity"] = json!(0);
    flat["ltp"] = json!(0);
    assert_eq!(position(flat).pnl_split(), Some((-450.0, 0.0)));
}

/// Web `test_open_positions_get_live_ltp_and_pnl` (prices attached).
#[test]
fn positions_map_with_live_prices() {
    let mut rows: Vec<VortexPosition> = net_positions();
    for (r, ltp) in rows.iter_mut().zip([954.0, 0.0, 14935.0, 6450.0]) {
        r.ltp = ltp;
    }
    let book = map_positions(&rows, &master());
    let by = |s: &str| book.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(
        (by("SBIN").ltp, by("SBIN").pnl, by("SBIN").product.as_str()),
        (954.0, 40.0, "MIS")
    );
    assert_eq!((by("INFY").ltp, by("INFY").pnl), (0.0, 25.0));
    assert_eq!(
        (
            by("GOLDPETAL30OCT26FUT").quantity,
            by("GOLDPETAL30OCT26FUT").pnl
        ),
        (2, 10.0)
    );
    let crude = by("CRUDEOIL20MAR28FUT");
    assert_eq!(
        (crude.quantity, crude.product.as_str(), crude.pnl),
        (-200, "NRML", 10_000.0)
    );
}

fn net_positions() -> Vec<VortexPosition> {
    rows_of(&resp("positions")["data"], &["net"]).unwrap()
}

/// Web `test_holdings_use_vortex_last_price`; a BSE-only holding is listed
/// on BSE.
#[test]
fn holdings_use_vortex_last_price() {
    let mut rows: Vec<VortexHolding> = rows_of(&resp("holdings"), &["data"]).unwrap();
    rows[1].last_price = 1410.0;
    let h = map_holdings(&rows, &master());
    assert_eq!(
        (
            h[0].symbol.as_str(),
            h[0].exchange.as_str(),
            h[0].quantity,
            h[0].ltp,
            h[0].pnl,
            h[0].pnl_percentage
        ),
        ("IDEA", "NSE", 3, 13.21, 1.53, 4.02)
    );
    assert_eq!(
        (
            h[1].symbol.as_str(),
            h[1].exchange.as_str(),
            h[1].quantity,
            h[1].pnl
        ),
        ("RELIANCE", "BSE", 3, 30.0)
    );
    assert_eq!(h[1].isin.as_deref(), Some("INE002A01018"));
}

// ---------------------------------------------------------------------------
// Funds
// ---------------------------------------------------------------------------

/// Web `test_funds_sum_nse_and_mcx`: positions unreadable, realized falls
/// back to the buckets.
#[test]
fn funds_sum_nse_and_mcx_without_positions() {
    let f = funds_from(&resp("funds"), None).unwrap();
    assert_eq!(
        (
            f.available_cash,
            f.collateral,
            f.m2m_unrealized,
            f.m2m_realized,
            f.utilised_debits
        ),
        (2000.0, 200.0, 0.0, 40.0, 100.0)
    );
}

/// Web `test_funds_prefer_the_combined_bucket`,
/// `test_funds_accept_a_combined_only_body`,
/// `test_funds_report_unrealised_mtm_from_open_positions`.
#[test]
fn funds_prefer_the_combined_bucket_and_take_pnl_from_positions() {
    let f = funds_from(&resp("funds_combined"), Some((15.0, 40.0))).unwrap();
    assert_eq!(
        (
            f.available_cash,
            f.collateral,
            f.utilised_debits,
            f.m2m_realized,
            f.m2m_unrealized
        ),
        (1500.0, 25.0, 120.5, 15.0, 40.0)
    );
    let only = funds_from(&json!({"exchange_combined": {"net_available": 100}}), None).unwrap();
    assert_eq!(only.available_cash, 100.0);
    let enveloped = json!({"status": "success", "data": {"nse": {"net_available": 7}}});
    assert_eq!(funds_from(&enveloped, None).unwrap().available_cash, 7.0);
    assert!(funds_from(&json!({"status": "success"}), None).is_none());
}

/// Web `test_funds_pnl_reconciles_with_the_position_book`: realized plus
/// unrealized equals the book's net P&L, with no double count.
#[test]
fn funds_pnl_reconciles_with_the_position_book() {
    let rows: Vec<VortexPosition> = [
        partly_closed(),
        json!({"ticker": "NSE:INFY", "exchange": "NSE_EQ", "product": "INTRADAY", "quantity": 0,
               "buy_value": 1000, "sell_value": 1025, "lot_size": 1}),
        json!({"ticker": "MCX:GOLDPETAL26OCTFUT", "exchange": "MCX_FO", "product": "DELIVERY",
               "quantity": 2, "buy_value": 29860, "sell_value": 0, "average_price": 14930,
               "lot_size": 1, "multiplier": 1, "ltp": 14935}),
    ]
    .into_iter()
    .map(position)
    .collect();
    let (realized, unrealized) = positions_pnl(&rows);
    assert_eq!((realized, unrealized), (75.0, 35.0));
    let net: f64 = map_positions(&rows, &master()).iter().map(|p| p.pnl).sum();
    assert_eq!(realized + unrealized, net);
    assert_eq!(net, 110.0);
}

// ---------------------------------------------------------------------------
// Quotes, depth, history
// ---------------------------------------------------------------------------

/// Web `test_quotes_carry_top_of_book_quantities`, `test_multiquotes_reports_every_leg`.
#[test]
fn quotes_carry_top_of_book_and_previous_close() {
    let q = &resp("quote_full")["data"]["NSE:SBIN"];
    let key = QuoteKey::new("NSE", "SBIN");
    let quote = format_quote(&key, q);
    assert_eq!(
        (
            quote.ltp,
            quote.bid,
            quote.ask,
            quote.bid_qty,
            quote.ask_qty,
            quote.close
        ),
        (954.0, 953.95, 954.0, 54, 2827, 958.75)
    );
    assert_eq!((quote.change, quote.change_percent), (-4.75, -0.5));
    let depth = to_depth(&key, q);
    assert_eq!((depth.bids.len(), depth.asks.len()), (5, 5));
    assert_eq!(depth.bids[2], DepthLevel::default());
    assert_eq!((depth.asks[1].price, depth.asks[1].orders), (954.05, 1));
    assert_eq!(
        (
            depth.ltq,
            depth.prev_close,
            depth.total_buy_qty,
            depth.total_sell_qty
        ),
        (5, 958.75, 512633, 689415)
    );
    // An index quote has no book: zero levels, not a failure.
    let idx = format_quote(&key, &resp("quote_full")["data"]["NSE:NIFTYIDX"]);
    assert_eq!((idx.ltp, idx.bid, idx.ask_qty), (25140.15, 0.0, 0));
}

/// Web `test_daily_candles_land_on_the_ist_date`.
#[test]
fn daily_candles_land_on_the_ist_date_oldest_first() {
    let c = sort_dedupe(parse_candles(&resp("history_day"), true));
    assert_eq!(
        c.iter().map(|c| c.timestamp).collect::<Vec<_>>(),
        [1791072000, 1791158400]
    );
    assert_eq!(c.iter().map(|c| c.close).collect::<Vec<_>>(), [1.0, 2.0]);
    assert_eq!((c[0].volume, c[0].oi), (10, 0));
    let m = parse_candles(&resp("history_minute"), false);
    assert_eq!(m[0].timestamp, 1791178200);
    assert!(parse_candles(&resp("history_no_data"), false).is_empty());
}

// ---------------------------------------------------------------------------
// GTT
// ---------------------------------------------------------------------------

fn gtt(trigger_type: GttTriggerType, action: Action, symbol: &str, exchange: &str) -> GttRequest {
    GttRequest {
        key: QuoteKey::new(exchange, symbol),
        trigger_type,
        action,
        product: Product::Cnc,
        quantity: 3,
        pricetype: PriceType::Limit,
        price: 0.0,
        trigger_price: 0.0,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 0.0,
        target: 0.0,
        last_price: None,
        expires_at: None,
    }
}

fn book() -> Vec<VortexGtt> {
    rows_of(&resp("gtt_book"), &["data"]).unwrap()
}

/// Web `test_single_gtt`.
#[test]
fn single_gtt_body() {
    let mut r = gtt(GttTriggerType::Single, Action::Buy, "SBIN", "NSE");
    r.price = 790.0;
    r.trigger_price = 795.0;
    assert_eq!(
        place_body(&r, &row("SBIN", "NSE")).unwrap(),
        json!({
            "ticker": "NSE:SBIN",
            "transaction_type": "BUY",
            "product": "DELIVERY",
            "gtt_trigger_type": "single",
            "quantity": 3,
            "price": 790.0,
            "trigger_price": 795.0,
            "variety": "RL",
        })
    );
}

/// Web `test_oco_legs_follow_the_side`.
#[test]
fn oco_legs_follow_the_side() {
    for (action, sl_trigger, profit_trigger) in
        [(Action::Sell, 780.0, 850.0), (Action::Buy, 850.0, 780.0)]
    {
        let mut r = gtt(GttTriggerType::Oco, action, "SBIN", "NSE");
        r.product = Product::Mis;
        r.pricetype = PriceType::Market;
        r.quantity = 2;
        (r.triggerprice_sl, r.stoploss, r.triggerprice_tg, r.target) = (780.0, 779.0, 850.0, 851.0);
        let b = place_body(&r, &row("SBIN", "NSE")).unwrap();
        assert_eq!(
            (&b["gtt_trigger_type"], &b["product"]),
            (&json!("oco"), &json!("INTRADAY"))
        );
        assert_eq!(b["stoploss"]["trigger_price"], json!(sl_trigger));
        assert_eq!(b["profit"]["trigger_price"], json!(profit_trigger));
        // MARKET legs go as RL-MKT with no limit price.
        assert_eq!(
            (&b["stoploss"]["variety"], &b["stoploss"]["price"]),
            (&json!("RL-MKT"), &json!(0.0))
        );
    }
}

/// Web `test_gtt_quantity_follows_mcx_lots`.
#[test]
fn gtt_quantity_follows_mcx_lots() {
    let mut r = gtt(
        GttTriggerType::Single,
        Action::Sell,
        "CRUDEOIL20MAR28FUT",
        "MCX",
    );
    r.quantity = 200;
    (r.price, r.trigger_price) = (6400.0, 6410.0);
    let b = place_body(&r, &row("CRUDEOIL20MAR28FUT", "MCX")).unwrap();
    assert_eq!(b["quantity"], json!(2));
}

/// Web `test_gtt_book_maps_to_the_openalgo_shape`,
/// `test_gtt_book_hides_finished_triggers_unless_asked`, `test_a_fired_oco_reports_triggered`.
#[test]
fn gtt_book_maps_to_the_openalgo_shape() {
    let active = map_gtt_book(&book(), &master(), false);
    assert_eq!(active.len(), 2);
    let oco = &active[0];
    assert_eq!(
        (
            oco.trigger_type.as_str(),
            oco.status.as_str(),
            oco.symbol.as_str(),
            oco.exchange.as_str()
        ),
        ("two-leg", "active", "SBIN", "NSE")
    );
    assert_eq!(oco.trigger_prices, [780.0, 850.0]);
    assert_eq!(
        oco.legs
            .iter()
            .map(|l| l.pricetype.as_str())
            .collect::<Vec<_>>(),
        ["LIMIT", "MARKET"]
    );
    assert_eq!(
        (oco.legs[0].product.as_str(), oco.legs[0].quantity),
        ("MIS", 190)
    );
    let single = &active[1];
    assert_eq!(
        (
            single.trigger_type.as_str(),
            single.symbol.as_str(),
            single.legs[0].quantity,
            single.legs[0].product.as_str()
        ),
        ("single", "CRUDEOIL20MAR28FUT", 200, "NRML")
    );
    let all = map_gtt_book(&book(), &master(), true);
    assert_eq!(all.len(), 3);
    assert_eq!(
        (all[2].symbol.as_str(), all[2].status.as_str()),
        ("IDEA", "triggered")
    );
}

/// Web `test_gtt_modify_matches_legs_by_price_and_keeps_the_trail`.
#[test]
fn gtt_modify_matches_legs_by_price_and_keeps_the_trail() {
    let mut r = gtt(GttTriggerType::Oco, Action::Sell, "SBIN", "NSE");
    r.quantity = 100;
    (r.triggerprice_sl, r.stoploss, r.triggerprice_tg, r.target) = (770.0, 769.0, 860.0, 861.0);
    let body = modify_body(&r, &book()[0], &row("SBIN", "NSE")).unwrap();
    let legs = body.as_array().unwrap();
    assert_eq!(
        legs.iter()
            .map(|l| (
                l["id"].clone(),
                l["trigger_price"].clone(),
                l["price"].clone()
            ))
            .collect::<Vec<_>>(),
        [
            (json!(1), json!(770.0), json!(769.0)),
            (json!(2), json!(860.0), json!(861.0))
        ]
    );
    assert_eq!(
        legs[0]["trail"],
        json!({"id": 9, "trail_jump_point": 2, "trail_jump_type": "Point"})
    );
    assert!(legs[1].get("trail").is_none());
}

/// Web `test_gtt_modify_refuses_a_change_of_type`.
#[test]
fn gtt_modify_refuses_a_change_of_type() {
    let mut r = gtt(GttTriggerType::Single, Action::Sell, "SBIN", "NSE");
    (r.price, r.trigger_price) = (790.0, 795.0);
    let e = modify_body(&r, &book()[0], &row("SBIN", "NSE")).unwrap_err();
    assert!(e.client_message().contains("between SINGLE and OCO"));
    let oco = gtt(
        GttTriggerType::Oco,
        Action::Sell,
        "CRUDEOIL20MAR28FUT",
        "MCX",
    );
    assert!(modify_body(&oco, &book()[1], &row("CRUDEOIL20MAR28FUT", "MCX")).is_err());
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn exch(name: &str) -> Vec<u8> {
    let mut b = name.as_bytes().to_vec();
    b.resize(10, 0);
    b
}

fn ltp_packet() -> Vec<u8> {
    let mut p = exch("NSE_EQ");
    p.extend(3045i32.to_le_bytes());
    p.extend(954.35f64.to_le_bytes());
    p
}

fn ohlcv_packet() -> Vec<u8> {
    let mut p = exch("NSE_FO");
    p.extend(51440i32.to_le_bytes());
    p.extend(104.1f64.to_le_bytes());
    p.extend(1_700_000_000i32.to_le_bytes());
    for v in [100.15f64, 120.0, 95.5, 99.8] {
        p.extend(v.to_le_bytes());
    }
    p.extend(23_700i32.to_le_bytes());
    p
}

fn full_packet() -> Vec<u8> {
    let mut p = exch("MCX_FO");
    p.extend(585_909i32.to_le_bytes());
    p.extend(6500.0f64.to_le_bytes());
    p.extend(1_700_000_001i32.to_le_bytes());
    for v in [6400.0f64, 6550.0, 6390.0, 6420.0] {
        p.extend(v.to_le_bytes());
    }
    p.extend(12_345i32.to_le_bytes()); // volume
    p.extend(1_700_000_002i32.to_le_bytes()); // last update
    p.extend(7i32.to_le_bytes()); // ltq
    p.extend(6480.5f64.to_le_bytes()); // average price
    p.extend(111i64.to_le_bytes());
    p.extend(222i64.to_le_bytes());
    p.extend(3333i32.to_le_bytes()); // oi
    for i in 0..5i32 {
        p.extend((6499.0 - f64::from(i)).to_le_bytes());
        p.extend((10 + i).to_le_bytes());
        p.extend((1 + i).to_le_bytes());
    }
    for i in 0..5i32 {
        p.extend((6501.0 + f64::from(i)).to_le_bytes());
        p.extend((20 + i).to_le_bytes());
        p.extend((2 + i).to_le_bytes());
    }
    p.extend(715_000i32.to_le_bytes());
    p.extend(585_000i32.to_le_bytes());
    p
}

fn frame(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut f = (packets.len() as u16).to_le_bytes().to_vec();
    for p in packets {
        f.extend((p.len() as u16).to_le_bytes());
        f.extend(p);
    }
    f
}

/// Web `test_packet_sizes_match_the_documented_modes`, `test_a_frame_carries_several_packets`,
/// `test_unknown_packet_size_is_ignored`.
#[test]
fn frames_split_into_packets_by_documented_size() {
    assert_eq!(
        (
            ltp_packet().len(),
            ohlcv_packet().len(),
            full_packet().len()
        ),
        (22, 62, 266)
    );
    let f = frame(&[ltp_packet(), ohlcv_packet(), full_packet()]);
    assert_eq!(
        split_frame(&f).iter().map(|p| p.len()).collect::<Vec<_>>(),
        [22, 62, 266]
    );
    assert!(split_frame(&[0]).is_empty());
    assert!(decode_packet(&[0u8; 30]).is_none());
    // A count larger than the frame stops at its end.
    let mut cut = frame(&[ltp_packet()]);
    cut[0] = 5;
    assert_eq!(split_frame(&cut).len(), 1);
}

/// Web `test_ltp_packet`, `test_ohlcv_packet`, `test_full_packet_fields_and_depth`.
#[test]
fn packets_decode_at_the_documented_offsets() {
    let l = decode_packet(&ltp_packet()).unwrap();
    assert_eq!(
        (l.exchange.as_str(), l.token, l.ltp, l.has_ohlcv),
        ("NSE_EQ", 3045, 954.35, false)
    );
    let o = decode_packet(&ohlcv_packet()).unwrap();
    assert_eq!((o.exchange.as_str(), o.token), ("NSE_FO", 51440));
    assert_eq!(
        (o.open, o.high, o.low, o.close),
        (100.15, 120.0, 95.5, 99.8)
    );
    assert_eq!((o.volume, o.ltt), (23_700, 1_700_000_000));
    let f = decode_packet(&full_packet()).unwrap();
    assert_eq!(
        (f.exchange.as_str(), f.token, f.ltp),
        ("MCX_FO", 585_909, 6500.0)
    );
    assert_eq!(
        (f.close, f.volume, f.ltq, f.average_price),
        (6420.0, 12_345, 7, 6480.5)
    );
    assert_eq!(
        (f.total_buy_quantity, f.total_sell_quantity, f.oi),
        (111, 222, 3333)
    );
    let (buy, sell) = f.depth.unwrap();
    assert_eq!(
        (buy[0].price, buy[0].quantity, buy[0].orders),
        (6499.0, 10, 1)
    );
    assert_eq!(
        (sell[4].price, sell[4].quantity, sell[4].orders),
        (6505.0, 24, 6)
    );
    // The price band arrives in paise.
    assert_eq!((f.upper_limit, f.lower_limit), (7150.0, 5850.0));
}

fn sub(
    symbol: &str,
    exchange: &str,
    token: &str,
    brsymbol: &str,
    brex: &str,
    mode: FeedMode,
) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: brsymbol.into(),
        brexchange: brex.into(),
        mode,
        depth: 5,
    }
}

fn text(m: &Message) -> Value {
    match m {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        other => panic!("{:?}", other),
    }
}

#[test]
fn feed_subscribes_per_ticker_and_changes_mode_by_subscribing_again() {
    let mut f = RupeezyFeed::new("wss://wire.rupeezy.in/ws", "tok/+=");
    let req = f.ws_request().unwrap();
    assert_eq!(req.uri().query(), Some("auth_token=tok%2F%2B%3D"));
    let s = sub(
        "NIFTY",
        "NSE_INDEX",
        "26000",
        "NIFTYIDX",
        "NSE_EQ",
        FeedMode::Ltp,
    );
    let frames = f.subscribe_frames(std::slice::from_ref(&s));
    assert_eq!(
        text(&frames[0]),
        json!({"ticker": "NSE:NIFTYIDX", "mode": "ltp", "message_type": "subscribe"})
    );
    let depth = s.with_mode(FeedMode::Depth);
    let change = f.mode_change_frames(&s, &depth);
    assert_eq!(change.len(), 1);
    assert_eq!(text(&change[0])["mode"], "full");
    assert_eq!(text(&change[0])["message_type"], "subscribe");
    // Unsubscribe names the mode it now has; a second one sends nothing.
    let un = f.unsubscribe_frames(std::slice::from_ref(&s));
    assert_eq!(
        text(&un[0]),
        json!({"ticker": "NSE:NIFTYIDX", "mode": "full", "message_type": "unsubscribe"})
    );
    assert!(f.unsubscribe_frames(&[s]).is_empty());
    assert_eq!(vortex_mode(FeedMode::Quote), "ohlcv");
    assert!(f.is_auth_failure(Some(401)) && !f.is_auth_failure(Some(500)));
}

#[test]
fn ticks_reach_their_subscription_under_the_openalgo_symbol() {
    let mut f = RupeezyFeed::new("ws://x", "tok");
    f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", "SBIN", "NSE_EQ", FeedMode::Ltp),
        sub(
            "CRUDEOIL20MAR28FUT",
            "MCX",
            "585909",
            "CRUDEOIL28MARFUT",
            "MCX_FO",
            FeedMode::Depth,
        ),
    ]);
    let ev = f.parse(&Message::Binary(frame(&[
        ltp_packet(),
        full_packet(),
        ohlcv_packet(),
    ])));
    // The unsubscribed NSE_FO packet is dropped; the full one adds depth.
    assert_eq!(ev.len(), 3);
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev[0])
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode, t.ltp),
        ("SBIN", "NSE", 1, 954.35)
    );
    let FeedEvent::Tick(c) = &ev[1] else {
        panic!("{:?}", ev[1])
    };
    assert_eq!(
        (c.symbol.as_str(), c.mode, c.close, c.oi),
        ("CRUDEOIL20MAR28FUT", 3, 6420.0, 3333)
    );
    assert_eq!((c.change, c.change_percent), (80.0, 1.25));
    assert_eq!(
        (c.timestamp_ms, c.last_trade_time_ms),
        (1_700_000_002_000, 1_700_000_001_000)
    );
    let FeedEvent::Depth(d) = &ev[2] else {
        panic!("{:?}", ev[2])
    };
    assert_eq!((d.buy.len(), d.sell[0].price), (5, 6501.0));
    assert_eq!(
        f.parse(&Message::Binary(vec![0])),
        vec![FeedEvent::Heartbeat]
    );
    assert!(f
        .parse(&Message::Text("{\"type\":\"order\"}".into()))
        .is_empty());
}

/// Web `test_a_refused_subscribe_leaves_no_stale_adapter_entry`: Vortex
/// carries 1000 instruments a connection; a new connection starts empty.
#[test]
fn the_feed_stops_at_the_vortex_instrument_limit() {
    let mut f = RupeezyFeed::new("ws://x", "tok");
    let subs: Vec<FeedSubscription> = (0..=MAX_INSTRUMENTS)
        .map(|i| {
            sub(
                &format!("S{}", i),
                "NSE",
                &i.to_string(),
                &format!("S{}", i),
                "NSE_EQ",
                FeedMode::Ltp,
            )
        })
        .collect();
    assert_eq!(f.subscribe_frames(&subs).len(), MAX_INSTRUMENTS);
    // Already subscribed instruments may still change mode.
    let again = subs[0].with_mode(FeedMode::Quote);
    assert_eq!(f.mode_change_frames(&subs[0], &again).len(), 1);
    assert!(f.on_connected().is_empty());
    assert_eq!(f.subscribe_frames(&subs[MAX_INSTRUMENTS..]).len(), 1);
}
