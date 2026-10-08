//! Unit tests for the Definedge mappings, built from the web code
//! (`broker/definedge/**`) and recorded-shape payloads (no account data).

use super::data::{
    api_exchange, chunk_days, depth_from, history_chunks, parse_history_csv, quote_from, resample,
    session_open_minute, Bar, HistoryWindow,
};
use super::funds::{full_year_expiry, funds_from_limits, margin_positions, parse_margin};
use super::mapping::*;
use super::master_contract::{expiry, parse_allmaster, parse_line};
use super::streaming::{scrip, DefinedgeFeed, DefinedgeOrderFeed};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{BrokerFeed, FeedEvent, FeedMode, FeedSubscription};
use crate::brokers::common::symbols::tests::row;
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::json;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/definedge/", $name))
    };
}

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut opt = row("NIFTY28OCT2525000CE", "NIFTY28OCT25C25000", "NFO", "43001");
    opt.name = "NIFTY".into();
    opt.expiry = "28-OCT-25".into();
    opt.strike = 25000.0;
    opt.instrument_type = "CE".into();
    r.load(vec![
        row("SBIN", "SBIN-EQ", "NSE", "3045"),
        row("INFY", "INFY-EQ", "NSE", "1594"),
        opt,
        row("CRUDEOIL20OCT25FUT", "CRUDEOIL20OCT25", "MCX", "4501"),
    ]);
    r
}

fn dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn resolved(symbols: &SymbolResolver, pt: PriceType, exchange: Exchange) -> ResolvedOrder {
    let (sym, ex) = match exchange {
        Exchange::Nfo => ("NIFTY28OCT2525000CE", "NFO"),
        _ => ("SBIN", "NSE"),
    };
    ResolvedOrder {
        symbol: sym.into(),
        exchange,
        action: Action::Buy,
        quantity: 10,
        price: 812.5,
        trigger_price: 810.0,
        pricetype: pt,
        product: Product::Mis,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument: symbols.by_symbol(ex, sym).unwrap(),
    }
}

// ---- auth -----------------------------------------------------------------

#[test]
fn auth_code_is_sha256_of_token_otp_secret() {
    // sha256("tok" + "123456" + "sec")
    assert_eq!(
        auth::auth_code("tok", "123456", "sec"),
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"tok123456sec"))
    );
    assert_eq!(auth::auth_code("a", "b", "c").len(), 64);
}

#[test]
fn session_round_trip_and_redaction() {
    let s = DefinedgeSession::parse(&AuthToken::new("key1:::suser1:::apitok")).unwrap();
    assert_eq!(s.api_session_key, "key1");
    assert_eq!(s.susertoken, "suser1");
    assert_eq!(s.api_token, "apitok");
    assert_eq!(s.compose(), "key1:::suser1:::apitok");
    assert!(!format!("{:?}", s).contains("key1"));
    assert!(DefinedgeSession::parse(&AuthToken::new("nope")).is_err());
    assert!(DefinedgeSession::parse(&AuthToken::new(":::a:::b")).is_err());
    // An empty susertoken still parses (the web stores `key::::::tok`).
    assert!(DefinedgeSession::parse(&AuthToken::new("k::::::t")).is_ok());
}

#[test]
fn login_kind_is_otp_two_step() {
    let b = DefinedgeBroker::new(SymbolResolver::new());
    assert_eq!(
        b.login_kind(),
        LoginKind::TwoStep {
            step1: &[],
            step2: &["otp"]
        }
    );
    assert!(!b.otp_pending());
    assert_eq!(b.id(), "definedge");
    assert!(b.capabilities().margin && b.capabilities().history);
}

// ---- order bodies ----------------------------------------------------------

#[test]
fn place_body_matches_transform_data() {
    let s = symbols();
    let o = resolved(&s, PriceType::Limit, Exchange::Nse);
    let b = place_body(&o);
    assert_eq!(
        b,
        json!({
            "tradingsymbol": "SBIN-EQ",
            "exchange": "NSE",
            "quantity": 10,
            "price": 812.5,
            "price_type": "LIMIT",
            "product_type": "INTRADAY",
            "order_type": "BUY",
            "algo_id": "99999"
        })
    );
    // MARKET and SL-M send price "0"; SL-M carries the trigger.
    let m = place_body(&resolved(&s, PriceType::Market, Exchange::Nse));
    assert_eq!(m["price"], "0");
    assert!(m.get("trigger_price").is_none());
    let slm = place_body(&resolved(&s, PriceType::SlM, Exchange::Nse));
    assert_eq!(slm["price"], "0");
    assert_eq!(slm["price_type"], "SL-MARKET");
    assert_eq!(slm["trigger_price"], 810.0);
    let sl = place_body(&resolved(&s, PriceType::Sl, Exchange::Nfo));
    assert_eq!(sl["price_type"], "SL-LIMIT");
    assert_eq!(sl["tradingsymbol"], "NIFTY28OCT25C25000");
    let mut dq = resolved(&s, PriceType::Limit, Exchange::Nse);
    dq.disclosed_quantity = 5;
    dq.product = Product::Cnc;
    let b = place_body(&dq);
    assert_eq!(b["disclosed_quantity"], 5);
    assert_eq!(b["product_type"], "CNC");
}

#[test]
fn algo_ids_per_exchange() {
    assert_eq!(algo_id("NSE"), "99999");
    assert_eq!(algo_id("NFO"), "99999");
    assert_eq!(algo_id("MCX"), "99999");
    assert_eq!(algo_id("BSE"), "9999999999999999");
    assert_eq!(algo_id("BFO"), "9999999999999999");
    assert_eq!(algo_id("BCD"), "9999999999999999");
}

#[test]
fn modify_body_is_all_strings() {
    let s = symbols();
    let m = ResolvedModify {
        order_id: "25100300000103".into(),
        symbol: "SBIN".into(),
        exchange: Exchange::Nse,
        action: Action::Sell,
        product: Product::Nrml,
        pricetype: PriceType::Sl,
        quantity: 7,
        price: 100.0,
        trigger_price: 99.5,
        disclosed_quantity: 0,
        instrument: s.by_symbol("NSE", "SBIN").unwrap(),
    };
    assert_eq!(
        modify_body(&m),
        json!({
            "order_id": "25100300000103",
            "tradingsymbol": "SBIN-EQ",
            "exchange": "NSE",
            "quantity": "7",
            "price": "100.0",
            "price_type": "SL-LIMIT",
            "product_type": "NORMAL",
            "order_type": "SELL",
            "trigger_price": "99.5",
            "validity": "DAY"
        })
    );
    let mut lim = m.clone();
    lim.pricetype = PriceType::Limit;
    assert!(modify_body(&lim).get("trigger_price").is_none());
}

#[test]
fn enum_maps() {
    assert_eq!(product_code(Product::Mis), "INTRADAY");
    assert_eq!(product_code(Product::Cnc), "CNC");
    assert_eq!(product_code(Product::Nrml), "NORMAL");
    assert_eq!(price_type_code(PriceType::Sl), "SL-LIMIT");
    assert_eq!(price_type_code(PriceType::SlM), "SL-MARKET");
    assert_eq!(book_product("NSE", "NORMAL"), "CNC");
    assert_eq!(book_product("BSE", "NORMAL"), "CNC");
    assert_eq!(book_product("NFO", "NORMAL"), "NRML");
    assert_eq!(book_product("MCX", "INTRADAY"), "MIS");
    assert_eq!(book_product("NSE", "CNC"), "CNC");
    assert_eq!(book_price_type("SL-LIMIT"), "SL");
    assert_eq!(book_price_type("SL-MARKET"), "SL-M");
    assert_eq!(book_price_type("MARKET"), "MARKET");
    for (raw, want) in [
        ("COMPLETE", "complete"),
        ("EXECUTED", "complete"),
        ("REJECTED", "rejected"),
        ("OPEN", "open"),
        ("NEW", "open"),
        ("REPLACED", "open"),
        ("PENDING", "open"),
        ("TRIGGER PENDING", "open"),
        ("TRIGGER_PENDING", "open"),
        ("CANCELED", "cancelled"),
        ("CANCELLED", "cancelled"),
        ("AMO RECEIVED", "amo received"),
    ] {
        assert_eq!(order_status(raw), want, "{}", raw);
    }
    assert_eq!(py_float(100.0), "100.0");
    assert_eq!(py_float(99.55), "99.55");
}

// ---- books ------------------------------------------------------------------

#[test]
fn order_book_rows() {
    let s = symbols();
    let body = v(fixture!("order_book.json"));
    let orders: Vec<Order> = rows(&body, "orders")
        .iter()
        .map(|o| map_order(o, &s))
        .collect();
    assert_eq!(orders.len(), 5);
    let a = &orders[0];
    assert_eq!(a.symbol, "SBIN");
    assert_eq!(a.side, "BUY");
    assert_eq!(a.product, "MIS");
    assert_eq!(a.order_type, "MARKET");
    assert_eq!(a.status, "complete");
    assert_eq!(a.average_price, 812.35);
    assert_eq!(a.filled_quantity, 10);
    assert_eq!(a.order_timestamp, "03-10-2025 09:16:03");
    let b = &orders[1];
    assert_eq!(b.symbol, "NIFTY28OCT2525000CE");
    assert_eq!(b.order_type, "SL");
    assert_eq!(b.product, "NRML");
    assert_eq!(b.status, "open");
    assert_eq!(b.trigger_price, 121.0);
    assert_eq!(orders[2].product, "CNC");
    assert_eq!(orders[3].status, "rejected");
    assert_eq!(orders[3].product, "CNC");
    assert_eq!(
        orders[3].rejection_reason.as_deref(),
        Some("RMS: insufficient holdings")
    );
    assert_eq!(orders[4].status, "cancelled");
    assert_eq!(orders[4].symbol, "CRUDEOIL20OCT25FUT");
    // cancel-all picks the broker statuses the web treats as cancellable.
    let ids: Vec<String> = rows(&body, "orders")
        .iter()
        .filter(|o| is_cancellable(o))
        .map(order_id)
        .collect();
    assert_eq!(ids, ["25100300000102", "25100300000103"]);
}

#[test]
fn trade_book_rows() {
    let s = symbols();
    let body = v(fixture!("trade_book.json"));
    let t: Vec<Trade> = rows(&body, "trades")
        .iter()
        .map(|x| map_trade(x, &s))
        .collect();
    assert_eq!(t[0].symbol, "SBIN");
    assert_eq!(t[0].product, "MIS");
    assert_eq!(t[0].trade_value, 8123.5);
    assert_eq!(t[0].timestamp, "03-10-2025 09:16:03");
    assert_eq!(t[0].trade_id, "50001");
    // fill_price 0 falls back to average_traded_price.
    assert_eq!(t[1].average_price, 118.4);
    assert_eq!(t[1].trade_value, 8880.0);
    assert_eq!(t[1].product, "NRML");
    assert_eq!(t[1].side, "SELL");
    assert_eq!(t[1].timestamp, "03-10-2025 09:30:00");
}

#[test]
fn position_rows() {
    let s = symbols();
    let body = v(fixture!("positions.json"));
    let p: Vec<Position> = rows(&body, "positions")
        .iter()
        .map(|x| map_position(x, &s))
        .collect();
    assert_eq!(p[0].symbol, "SBIN");
    assert_eq!(p[0].quantity, 10);
    assert_eq!(p[0].pnl, 26.5);
    assert_eq!(p[0].ltp, 815.0);
    assert_eq!(p[0].buy_quantity, 10);
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].product, "NRML");
    // Closed positions report the realized P&L.
    assert_eq!(p[2].quantity, 0);
    assert_eq!(p[2].pnl, -42.75);
    assert_eq!(p[2].product, "CNC");
}

#[test]
fn holdings_rows() {
    let s = symbols();
    let body = v(fixture!("holdings.json"));
    let h = map_holdings(rows(&body, "holdings"), &s);
    assert_eq!(h.len(), 1, "zero-quantity holdings are skipped");
    assert_eq!(h[0].symbol, "INFY");
    assert_eq!(h[0].exchange, "NSE");
    assert_eq!(h[0].quantity, 25);
    assert_eq!(h[0].t1_quantity, 5);
    assert_eq!(h[0].isin.as_deref(), Some("INE009A01021"));
    assert_eq!(h[0].pnl, 0.0);
    let stats = PortfolioStats::from_holdings(&h);
    assert_eq!(stats.totalholdingvalue, stats.totalinvvalue);
    assert_eq!(stats.totalprofitandloss, 0.0);
}

// ---- funds and margin -------------------------------------------------------

#[test]
fn funds_from_limits_sums_segments() {
    let f = funds_from_limits(&v(fixture!("limits.json"))).unwrap();
    assert_eq!(f.available_cash, 250000.46);
    assert_eq!(f.collateral, 50000.0);
    assert_eq!(f.m2m_unrealized, 80.25);
    assert_eq!(f.m2m_realized, 457.25);
    assert_eq!(f.utilised_debits, 12345.68);
    let head = funds_from_limits(
        &json!({"status":"SUCCESS","cash":"1","currentUnrealizedMtom":"5","currentRealizedPNL":"-3","currentRealizedPNLEquityIntraday":"99"}),
    )
    .unwrap();
    assert_eq!((head.m2m_unrealized, head.m2m_realized), (5.0, -3.0));
    assert!(funds_from_limits(&json!({"status":"ERROR","message":"x"})).is_none());
}

#[test]
fn margin_payload_and_response() {
    let s = symbols();
    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY28OCT2525000CE"),
            action: Action::Sell,
            quantity: 75,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
        MarginLeg {
            key: QuoteKey::new("NFO", "NOPE"),
            action: Action::Buy,
            quantity: 1,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
    ];
    let p = margin_positions(&legs, &s);
    assert_eq!(
        p,
        vec![json!({
            "product_type": "NORMAL",
            "exchange": "NFO",
            "symbol_name": "NIFTY",
            "tradingsymbol": "NIFTY28OCT25C25000",
            "open_buy_qty": 0,
            "open_sell_qty": 75,
            "expiry": "28-OCT-2025",
            "option_strike": "25000",
            "option_type": "CE"
        })]
    );
    assert_eq!(full_year_expiry("28-OCT-25"), "28-OCT-2025");
    assert_eq!(full_year_expiry("28-OCT-99"), "28-OCT-1999");
    assert_eq!(full_year_expiry("weird"), "weird");
    let m = parse_margin(&json!({"status":"SUCCESS","span":"100000.5","exposure":25000})).unwrap();
    assert_eq!(m.total_margin_required, 125000.5);
    assert_eq!(m.span_margin, 100000.5);
    let e = parse_margin(&json!({"status":"FAILURE","message":"Invalid symbol"})).unwrap_err();
    assert_eq!(e.client_message(), "Definedge: Invalid symbol");
}

// ---- quotes -----------------------------------------------------------------

#[test]
fn quote_and_depth_from_quotes_body() {
    let body = v(fixture!("quote.json"));
    let k = QuoteKey::new("NSE", "SBIN");
    let q = quote_from(&body, &k, 0);
    assert_eq!(q.ltp, 812.35);
    assert_eq!(q.open, 805.0);
    // The quotes API has no previous close; the web reports day_open.
    assert_eq!(q.close, 805.0);
    assert_eq!(q.bid, 812.3);
    assert_eq!(q.ask, 812.4);
    assert_eq!(q.volume, 1234567);
    let no_open = quote_from(&json!({"ltp":"5"}), &k, 7);
    assert_eq!((no_open.close, no_open.oi), (5.0, 7));
    let dpt = depth_from(&body, &k, 0);
    assert_eq!(dpt.bids.len(), 5);
    assert_eq!(dpt.asks[4].price, 812.6);
    assert_eq!(dpt.total_buy_qty, 1500);
    assert_eq!(dpt.total_sell_qty, 1750);
    assert_eq!(dpt.ltq, 25);
    assert_eq!(dpt.prev_close, 805.0);
    assert_eq!(api_exchange("NSE_INDEX"), "NSE");
    assert_eq!(api_exchange("BSE_INDEX"), "BSE");
    assert_eq!(api_exchange("MCX_INDEX"), "MCX");
    assert_eq!(api_exchange("NFO"), "NFO");
}

// ---- history ----------------------------------------------------------------

#[test]
fn history_csv_parsing() {
    let bars = parse_history_csv(fixture!("history_minute.csv"));
    assert_eq!(bars.len(), 5, "the garbage row is dropped");
    assert_eq!(bars[0].at, dt("2025-09-01 09:15"));
    assert_eq!(bars[0].oi, 5000);
    let daily = parse_history_csv(fixture!("history_day.csv"));
    // 11-digit day (leading zero lost) is left-padded.
    assert_eq!(daily[0].at, dt("2025-09-01 00:00"));
    assert_eq!(daily[1].at, dt("2025-09-02 00:00"));
    assert_eq!(daily[0].oi, 0);
    assert_eq!(daily[1].volume, 200000);
}

#[test]
fn resample_aligns_to_session_open() {
    let bars = parse_history_csv(fixture!("history_minute.csv"));
    let r = resample(bars.clone(), 15, session_open_minute("NSE"));
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].at, dt("2025-09-01 09:15"));
    assert_eq!(
        (r[0].open, r[0].high, r[0].low, r[0].close),
        (100.0, 103.0, 99.5, 102.0)
    );
    assert_eq!(r[0].volume, 4500);
    assert_eq!(r[0].oi, 5200);
    assert_eq!(r[1].at, dt("2025-09-01 09:30"));
    assert_eq!(r[2].at, dt("2025-09-01 09:45"));
    // 1h bins open at 09:15 for NSE and 09:00 for MCX.
    let h = resample(bars.clone(), 60, session_open_minute("NSE"));
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].at, dt("2025-09-01 09:15"));
    let mcx = resample(bars, 60, session_open_minute("MCX"));
    assert_eq!(mcx[0].at, dt("2025-09-01 09:00"));
    assert_eq!(session_open_minute("CDS"), 0);
    assert_eq!(session_open_minute("NFO"), 15);
    let one = Bar {
        at: dt("2025-09-01 00:05"),
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1,
        oi: 0,
    };
    // Before the origin the bin still floors correctly.
    assert_eq!(resample(vec![one], 30, 15)[0].at, dt("2025-08-31 23:45"));
}

#[test]
fn history_window_and_chunks() {
    let now = dt("2025-10-03 11:42");
    let w = HistoryWindow::new("1m", d(2025, 8, 1), d(2025, 10, 3), now);
    assert_eq!(w.from, dt("2025-08-01 00:00"));
    assert_eq!(w.to, dt("2025-10-03 11:42"));
    let c = history_chunks(&w, "1m");
    assert_eq!(c[0], (dt("2025-08-01 00:00"), dt("2025-08-30 23:59")));
    assert_eq!(c[1].0, dt("2025-08-31 00:00"));
    assert_eq!(c.last().unwrap().1, dt("2025-10-03 11:42"));
    for pair in c.windows(2) {
        assert_eq!(pair[0].1.date().succ_opt().unwrap(), pair[1].0.date());
    }
    let past = HistoryWindow::new("5m", d(2025, 9, 1), d(2025, 9, 2), now);
    assert_eq!(past.to, dt("2025-09-02 23:59"));
    let dw = HistoryWindow::new("D", d(2024, 1, 1), d(2025, 6, 30), now);
    assert!(dw.daily);
    let dc = history_chunks(&dw, "D");
    assert_eq!(dc.len(), 2);
    assert_eq!(dc[0], (dt("2024-01-01 00:00"), dt("2024-12-30 00:00")));
    assert_eq!(dc[1], (dt("2024-12-31 00:00"), dt("2025-06-30 00:00")));
    assert_eq!(chunk_days("15m"), 150);
    assert_eq!(chunk_days("1h"), 180);
}

#[test]
fn candle_epochs() {
    // Intraday IST wall time -> UTC epoch.
    assert_eq!(candle_epoch(dt("2025-09-01 09:15"), false), 1756698300);
    // Daily: naive midnight read as UTC.
    assert_eq!(candle_epoch(dt("2025-09-01 00:00"), true), 1756684800);
}

// ---- master contract ----------------------------------------------------------

#[test]
fn master_contract_rows() {
    let rows = parse_allmaster(fixture!("allmaster.csv"));
    let find = |ex: &str, sym: &str| {
        rows.iter()
            .find(|r| r.exchange == ex && r.symbol == sym)
            .unwrap_or_else(|| panic!("{} {} missing", ex, sym))
            .clone()
    };
    let sbin = find("NSE", "SBIN");
    assert_eq!(sbin.brsymbol, "SBIN-EQ");
    assert_eq!(sbin.instrument_type, "EQ");
    assert_eq!(sbin.strike, 1.0);
    assert_eq!(sbin.tick_size, 0.05);
    // NSE drops non-equity series.
    assert!(!rows.iter().any(|r| r.token == "99999"));
    // BSE keeps the symbol, drops empty ones.
    assert_eq!(find("BSE", "SBIN").brsymbol, "SBIN");
    assert!(!rows.iter().any(|r| r.token == "123"));
    // Indices: cleaned and renamed; duplicates keep the first row.
    let nifty = find("NSE_INDEX", "NIFTY");
    assert_eq!(nifty.token, "26000");
    assert_eq!(nifty.brexchange, "NSE");
    assert_eq!(nifty.instrument_type, "IDX");
    assert_eq!(
        rows.iter()
            .filter(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY")
            .count(),
        1
    );
    assert_eq!(find("NSE_INDEX", "BANKNIFTY").token, "26009");
    assert_eq!(find("BSE_INDEX", "SENSEX").token, "999901");
    assert_eq!(find("BSE_INDEX", "SENSEX50").token, "999902");
    assert_eq!(find("MCX_INDEX", "MCXBULLDEX").token, "4599");
    // Derivatives.
    let fut = find("NFO", "NIFTY28OCT25FUT");
    assert_eq!(fut.expiry, "28-OCT-25");
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.lot_size, 75);
    assert_eq!(fut.brsymbol, "NIFTY28OCT25F");
    let ce = find("NFO", "NIFTY28OCT2525000CE");
    assert_eq!(ce.strike, 25000.0);
    assert_eq!(ce.instrument_type, "CE");
    assert_eq!(find("NFO", "ZYDUSLIFE30SEP251360PE").strike, 1360.0);
    assert_eq!(find("NFO", "VEDL28OCT25292.5CE").strike, 292.5);
    // BFO expiry with the leading zero lost.
    let bfo = find("BFO", "SENSEX07OCT25FUT");
    assert_eq!(bfo.expiry, "07-OCT-25");
    find("BFO", "SENSEX07OCT2580000PE");
    find("CDS", "USDINR28OCT25FUT");
    let cds = find("CDS", "USDINR28OCT2588.5CE");
    assert_eq!(cds.tick_size, 0.0025);
    find("MCX", "CRUDEOIL20OCT25FUT");
    let mcx = find("MCX", "CRUDEOIL16OCT255400CE");
    assert_eq!(mcx.tick_size, 0.1);
    assert_eq!(mcx.lot_size, 100);
}

#[test]
fn master_contract_helpers() {
    assert_eq!(expiry("28102025"), "28-OCT-25");
    assert_eq!(expiry("7102025"), "07-OCT-25");
    assert_eq!(expiry(""), "");
    assert_eq!(expiry("bad"), "bad");
    assert!(parse_line(",,,").is_none());
    let r = parse_line("NSE,1,X,X-SG,EQ,,5,1,,0").unwrap();
    assert_eq!(r.symbol, "X");
}

// ---- streaming --------------------------------------------------------------

fn sub(symbol: &str, exchange: &str, brex: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: brex.into(),
        mode,
        depth: 5,
    }
}

fn text_of(m: &crate::brokers::common::streaming::Message) -> Value {
    match m {
        crate::brokers::common::streaming::Message::Text(t) => v(t),
        _ => Value::Null,
    }
}

#[test]
fn feed_connect_subscribe_and_decode() {
    let mut f = DefinedgeFeed::new("wss://x/NorenWSTRTP/", "UID1", "suser");
    // definedge_websocket.py:364-375
    let c = f.on_connected();
    assert_eq!(
        text_of(&c[0]),
        json!({"t":"c","uid":"UID1","actid":"UID1","source":"TRTP","susertoken":"suser"})
    );
    assert!(f.awaits_auth_ack());
    assert_eq!(
        f.parse_text(r#"{"t":"ck","s":"Ok","uid":"UID1"}"#),
        [FeedEvent::AuthOk]
    );
    assert!(matches!(
        f.parse_text(r#"{"t":"ck","s":"Not_Ok"}"#)[0],
        FeedEvent::AuthFailed(_)
    ));
    let subs = vec![
        sub("SBIN", "NSE", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY", "NSE_INDEX", "NSE", "26000", FeedMode::Ltp),
        sub(
            "NIFTY28OCT2525000CE",
            "NFO",
            "NFO",
            "43001",
            FeedMode::Depth,
        ),
    ];
    let frames = f.subscribe_frames(&subs);
    assert_eq!(
        text_of(&frames[0]),
        json!({"t":"t","k":"NSE|3045#NSE|26000"})
    );
    assert_eq!(text_of(&frames[1]), json!({"t":"d","k":"NFO|43001"}));
    assert_eq!(
        scrip(&sub("X", "BSE_INDEX", "", "1", FeedMode::Ltp)),
        "BSE|1"
    );

    // tk snapshot then tf delta (definedge_adapter.py:1039-1063 keys).
    let ev = f.parse_text(
        r#"{"t":"tk","e":"NSE","tk":"3045","lp":"812.35","o":"805","h":"818.9","l":"801.1","c":"800","v":"1000","ap":"810.2","ltq":"5","tbq":"100","tsq":"200","ft":"1759470000"}"#,
    );
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.symbol.as_str(), t.mode), ("SBIN", 2));
    assert_eq!(t.close, 800.0);
    assert_eq!(t.change, 12.35);
    assert_eq!(t.last_trade_time_ms, 1759470000000);
    let ev = f.parse_text(r#"{"t":"tf","e":"NSE","tk":"3045","lp":"813","o":"0"}"#);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.ltp, 813.0);
    assert_eq!(t.open, 805.0, "a zero open never overwrites the snapshot");
    assert_eq!(t.volume, 1000);

    // Depth frames produce a tick and a five-level depth.
    let ev = f.parse_text(
        r#"{"t":"dk","e":"NFO","tk":"43001","lp":"120","oi":"150000","bp1":"119.9","bq1":"75","bo1":"2","sp1":"120.1","sq1":"150","so1":"3","tbq":"9000","tsq":"8000"}"#,
    );
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(dp) = &ev[1] else {
        panic!()
    };
    assert_eq!(dp.buy.len(), 5);
    assert_eq!(dp.buy[0].price, 119.9);
    assert_eq!(dp.sell[0].orders, 3);
    assert_eq!(dp.total_buy_quantity, 9000);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!(t.oi, 150000);

    // Unknown scrips are ignored; unsubscribe drops the snapshot.
    assert!(f
        .parse_text(r#"{"t":"tf","e":"BSE","tk":"9","lp":"1"}"#)
        .is_empty());
    assert_eq!(f.cached(), 2);
    let un = f.unsubscribe_frames(&subs[..1]);
    assert_eq!(text_of(&un[0]), json!({"t":"u","k":"NSE|3045"}));
    let un = f.unsubscribe_frames(&subs[2..]);
    assert_eq!(text_of(&un[0]), json!({"t":"ud","k":"NFO|43001"}));
    assert_eq!(f.cached(), 0);
    assert!(f
        .parse_text(r#"{"t":"tf","e":"NSE","tk":"3045","lp":"1"}"#)
        .is_empty());
    let (every, hb) = f.heartbeat().unwrap();
    assert_eq!(every, std::time::Duration::from_secs(30));
    assert_eq!(text_of(&hb), json!({"t":"h"}));
}

#[test]
fn order_feed_frames_and_updates() {
    let mut f = DefinedgeOrderFeed::new("wss://x/", "UID1", "suser", symbols());
    let c = f.on_connected();
    assert_eq!(c.len(), 2);
    assert_eq!(text_of(&c[1]), json!({"t":"o","actid":"UID1"}));
    let ev = f.parse_text(
        r#"{"t":"om","norenordno":"25100300000101","tsym":"SBIN-EQ","exch":"NSE","trantype":"B","qty":"10","prc":"0","trgprc":"0","prctyp":"MKT","prd":"I","status":"COMPLETE","fillshares":"4","avgprc":"812.35"}"#,
    );
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!()
    };
    assert_eq!(u.symbol, "SBIN");
    assert_eq!(u.action, "BUY");
    assert_eq!(u.pricetype, "MARKET");
    assert_eq!(u.product, "MIS");
    assert_eq!(u.order_status, "complete");
    assert_eq!(u.pending_quantity, 6);
    assert_eq!(u.rejection_reason, "");
    let ev = f.parse_text(
        r#"{"t":"om","norenordno":"2","tsym":"X","exch":"NFO","trantype":"S","qty":"1","prctyp":"SL-LMT","prd":"M","status":"REJECTED","rejreason":"margin"}"#,
    );
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!()
    };
    assert_eq!((u.pricetype.as_str(), u.product.as_str()), ("SL", "NRML"));
    assert_eq!(u.rejection_reason, "margin");
    assert_eq!(stream_status("TRIGGER_PENDING"), "trigger pending");
    assert_eq!(stream_status(""), "open");
    assert_eq!(f.parse_text(r#"{"t":"ck","s":"OK"}"#), [FeedEvent::AuthOk]);
}

#[test]
fn feed_identity_needs_uid() {
    let a = AuthToken::new("k:::suser:::t").with_user_id("UID1");
    assert_eq!(
        streaming::feed_identity(&a).unwrap(),
        ("UID1".to_string(), "suser".to_string())
    );
    let fed = AuthToken::new("k:::old:::t")
        .with_feed(Some("fresh"))
        .with_user_id("U");
    assert_eq!(streaming::feed_identity(&fed).unwrap().1, "fresh");
    assert!(streaming::feed_identity(&AuthToken::new("k:::s:::t")).is_err());
}
