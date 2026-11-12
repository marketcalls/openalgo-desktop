//! Sahi adapter tests: mapping from recorded shapes (docs.sahi.com), then
//! HTTP round trips against a local fake Sahi (the cases of the web's
//! `test/test_sahi_rest_mapping.py` and more).

use super::data::{daily_ohlc, to_candle, to_depth, to_quote, window_ms};
use super::funds::funds_from;
use super::mapping::*;
use super::master_contract::{cash_symbol, index_symbol, parse};
use super::ratelimit::{Category, SahiLimiter};
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::ratelimit::Limits;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, StatusCode as Http};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/sahi/", $name))
    };
}

fn fixture_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse(fixture!("instruments.csv")).unwrap());
    r
}

fn order(symbol: &str, exchange: Exchange, pricetype: PriceType) -> ResolvedOrder {
    let r = master();
    let instrument = r.by_symbol(exchange.as_str(), symbol).unwrap();
    ResolvedOrder {
        symbol: symbol.into(),
        exchange,
        action: Action::Buy,
        quantity: 1,
        price: 9000.5,
        trigger_price: 0.0,
        pricetype,
        product: Product::Cnc,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument,
    }
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_maps_every_kind_and_keeps_special_symbols() {
    let rows = parse(fixture!("instruments.csv")).unwrap();
    let find = |s: &str, ex: &str| {
        rows.iter()
            .find(|r| r.symbol == s && r.exchange == ex)
            .unwrap_or_else(|| panic!("{} {}", s, ex))
    };
    for s in ["BAJAJ-AUTO", "M&M"] {
        let r = find(s, "NSE");
        assert_eq!((r.brsymbol.as_str(), r.instrument_type.as_str()), (s, "EQ"));
    }
    // A quoted company name with a comma does not shift the columns.
    assert_eq!(find("M&M", "NSE").name, "Mahindra & Mahindra, Ltd");
    let be = find("3IINFOLTD", "NSE");
    assert_eq!(
        (be.brsymbol.as_str(), be.token.as_str()),
        ("3IINFOLTD-BE", "10914")
    );
    let nifty = find("NIFTY", "NSE_INDEX");
    assert_eq!(
        (nifty.brsymbol.as_str(), nifty.brexchange.as_str()),
        ("Nifty 50", "NSE")
    );
    assert_eq!(find("BANKNIFTY", "NSE_INDEX").token, "10855");
    // The BSE index listed twice keeps the lowest id.
    assert_eq!(find("SENSEX", "BSE_INDEX").token, "20000");
    let ce = find("NIFTY27OCT2625000CE", "NFO");
    assert_eq!(ce.brsymbol, "NIFTY27OCT26C25000");
    assert_eq!(
        (ce.expiry.as_str(), ce.strike, ce.lot_size),
        ("27-OCT-26", 25000.0, 65)
    );
    assert_eq!(
        (ce.instrument_type.as_str(), ce.name.as_str()),
        ("CE", "NIFTY")
    );
    let fut = find("M&M23NOV26FUT", "NFO");
    assert_eq!((fut.instrument_type.as_str(), fut.strike), ("FUT", 0.0));
    assert_eq!(fut.brsymbol, "M&M23NOV26F");
    let bfo = find("SENSEX05NOV2681500CE", "BFO");
    assert_eq!(bfo.brexchange, "BFO");
    // MCX is not loaded; an impossible expiry is skipped.
    assert!(rows.iter().all(|r| r.brexchange != "MCX"));
    assert!(rows.iter().all(|r| r.brsymbol != "BAD"));
    // RELIANCE is listed on NSE and BSE.
    assert_eq!(find("RELIANCE", "BSE").token, "725069");
    let mut keys: Vec<_> = rows.iter().map(|r| (&r.symbol, &r.exchange)).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), rows.len());
}

#[test]
fn cash_and_index_symbols() {
    assert_eq!(cash_symbol("3IINFOLTD-BE", "NSE", "BE"), "3IINFOLTD");
    assert_eq!(cash_symbol("BAJAJ-AUTO", "NSE", "EQ"), "BAJAJ-AUTO");
    assert_eq!(cash_symbol("BAJAJ-AUTO", "NSE", "BE"), "BAJAJ-AUTO");
    assert_eq!(cash_symbol("X-BE", "BSE", "BE"), "X-BE");
    assert_eq!(index_symbol("Nifty  Fin Service", "NSE"), "FINNIFTY");
    assert_eq!(index_symbol("Nifty Auto", "NSE"), "NIFTYAUTO");
    assert_eq!(index_symbol("OILGAS", "BSE"), "BSEOIL&GAS");
}

#[test]
fn a_master_without_its_columns_is_refused() {
    assert!(parse("id,symbol\n1,X\n").is_err());
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

#[test]
fn paise_round_trip() {
    assert_eq!(to_paise(2499.55), 249955);
    assert_eq!(to_paise(0.05), 5);
    assert_eq!(to_paise(187.125), 18713);
    assert_eq!(to_paise(f64::NAN), 0);
    assert_eq!(from_paise(Some(&json!(249955))), 2499.55);
    assert_eq!(from_paise(None), 0.0);
}

#[test]
fn limit_order_for_special_symbols() {
    for s in ["BAJAJ-AUTO", "M&M"] {
        let body = place_body(&order(s, Exchange::Nse, PriceType::Limit)).unwrap();
        assert_eq!(
            body,
            json!({
                "exchange_segment": "NSE",
                "symbol": s,
                "txn_type": "B",
                "qty": 1,
                "order_type": "LMT",
                "product": "C",
                "validity": "DAY",
                "price": 900050
            })
        );
    }
}

#[test]
fn market_and_stop_orders() {
    let mut o = order("NIFTY27OCT2625000CE", Exchange::Nfo, PriceType::Market);
    o.action = Action::Sell;
    o.product = Product::Nrml;
    o.quantity = 65;
    let b = place_body(&o).unwrap();
    assert_eq!(b["symbol"], "NIFTY27OCT26C25000");
    assert_eq!(
        (b["order_type"].clone(), b["price"].clone()),
        (json!("MKT"), json!(0))
    );
    assert!(b.get("trigger_price").is_none());

    let mut slm = order("M&M", Exchange::Nse, PriceType::SlM);
    slm.product = Product::Mis;
    slm.trigger_price = 3000.0;
    slm.disclosed_quantity = 1;
    let b = place_body(&slm).unwrap();
    assert_eq!(b["order_type"], "SL-MKT");
    assert_eq!(
        (b["price"].clone(), b["trigger_price"].clone()),
        (json!(0), json!(300000))
    );
    assert_eq!(
        (b["product"].clone(), b["disclosed_qty"].clone()),
        (json!("I"), json!(1))
    );
}

#[test]
fn an_index_is_not_traded() {
    let o = order("NIFTY", Exchange::NseIndex, PriceType::Market);
    assert!(place_body(&o).is_err());
}

#[test]
fn modify_payload() {
    let m = ResolvedModify {
        order_id: "1".into(),
        symbol: "M&M".into(),
        exchange: Exchange::Nse,
        action: Action::Buy,
        product: Product::Mis,
        pricetype: PriceType::Sl,
        quantity: 10,
        price: 101.5,
        trigger_price: 101.0,
        disclosed_quantity: 0,
        instrument: master().by_symbol("NSE", "M&M").unwrap(),
    };
    assert_eq!(
        modify_body(&m),
        json!({
            "qty": 10,
            "order_type": "SL-LMT",
            "validity": "DAY",
            "price": 10150,
            "trigger_price": 10100,
            "disclosed_qty": 0
        })
    );
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_rows() {
    let r = master();
    let v = fixture_json(fixture!("order_book.json"));
    let rows: Vec<Order> = v["orders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| map_order(o, &r))
        .collect();
    let o = &rows[0];
    assert_eq!((o.symbol.as_str(), o.exchange.as_str()), ("M&M", "NSE"));
    assert_eq!((o.side.as_str(), o.status.as_str()), ("BUY", "open"));
    assert_eq!(
        (o.quantity, o.filled_quantity, o.pending_quantity),
        (10, 4, 6)
    );
    assert_eq!((o.price, o.average_price), (2500.0, 2499.75));
    assert_eq!(
        (o.order_type.as_str(), o.product.as_str()),
        ("LIMIT", "MIS")
    );
    assert_eq!(o.order_timestamp, "2025-05-21 09:00:01");
    let rej = &rows[1];
    assert_eq!(rej.symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        (rej.status.as_str(), rej.order_type.as_str()),
        ("rejected", "SL-M")
    );
    assert_eq!(rej.trigger_price, 120.5);
    assert_eq!(rej.rejection_reason.as_deref(), Some("Insufficient margin"));
    assert_eq!(rej.pending_quantity, 0);
}

#[test]
fn status_map() {
    assert_eq!(order_status("Completed"), "complete");
    assert_eq!(order_status("PartialFilled"), "cancelled");
    assert_eq!(order_status("Open"), "open");
    // Named in Sahi's docs outside their status table: an OpenAlgo status,
    // never an unknown word.
    assert_eq!(order_status("AwaitingTrigger"), "trigger pending");
    assert!(is_working("OpenPartial") && !is_working("PartialFilled"));
}

#[test]
fn trade_rows() {
    let v = fixture_json(fixture!("trades.json"));
    let t = map_trade(&v["trades"][0], &master());
    assert_eq!((t.symbol.as_str(), t.product.as_str()), ("M&M", "MIS"));
    assert_eq!((t.average_price, t.trade_value), (2499.5, 9998.0));
    assert_eq!(t.trade_id, "80123456");
}

#[test]
fn positions_pnl_is_realised_plus_marked_open() {
    let r = master();
    let v = fixture_json(fixture!("positions.json"));
    let p = map_position(&v["positions"][0], 49.0, &r);
    // realised 25.00 + (49 - 50) * -10 = 35.00
    assert_eq!(
        (p.symbol.as_str(), p.quantity, p.pnl),
        ("3IINFOLTD", -10, 35.0)
    );
    assert_eq!((p.realized_pnl, p.unrealized_pnl), (25.0, 10.0));
    assert_eq!((p.buy_value, p.sell_value), (245.0, 750.0));
    let f = map_position(&v["positions"][1], 0.0, &r);
    assert_eq!(
        (f.symbol.as_str(), f.overnight_quantity),
        ("RELIANCE23NOV26FUT", 500)
    );
    // No LTP: the open part is left out, not guessed.
    assert_eq!(f.pnl, 0.0);
}

#[test]
fn holdings_prefer_the_nse_listing() {
    let v = fixture_json(fixture!("holdings.json"));
    let ltps = HashMap::from([("12027".to_string(), 2510.0)]);
    let r = master();
    let rows: Vec<Holding> = v["holdings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| map_holding(h, &ltps, &r))
        .collect();
    assert_eq!(rows.len(), 1);
    let h = &rows[0];
    assert_eq!(
        (h.symbol.as_str(), h.exchange.as_str()),
        ("RELIANCE", "NSE")
    );
    assert_eq!(
        (h.quantity, h.t1_quantity, h.average_price),
        (10, 2, 2450.5)
    );
    assert_eq!((h.pnl, h.pnl_percentage), (595.0, 2.43));
}

// ---------------------------------------------------------------------------
// Funds, market data, errors
// ---------------------------------------------------------------------------

#[test]
fn funds_take_collateral_out_of_available_cash() {
    let f = funds_from(&fixture_json(fixture!("funds.json")), 25.0, 10.0).unwrap();
    assert_eq!((f.available_cash, f.collateral), (95000.0, 20000.0));
    assert_eq!(f.utilised_debits, 10125.0);
    assert_eq!(
        (f.span, f.exposure, f.opening_balance),
        (8000.0, 2000.0, 100000.0)
    );
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (25.0, 10.0));
    assert!(funds_from(&json!({"balance": {}}), 0.0, 0.0).is_err());
}

#[test]
fn quote_and_depth_from_a_quotes_entry() {
    let v = fixture_json(fixture!("quotes.json"));
    let key = QuoteKey::new("NSE", "RELIANCE");
    let q = to_quote(&key, &v["data"]["12027"], (2500.0, 2520.0, 2490.0, 2495.0));
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.bid_qty),
        (2510.0, 2509.5, 2510.5, 100)
    );
    assert_eq!(
        (q.volume, q.oi, q.close, q.change),
        (12450, 0, 2495.0, 15.0)
    );
    let d = to_depth(&key, &v["data"]["12027"], (0.0, 0.0, 0.0, 2495.0));
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.bids[1].price, d.bids[1].orders), (2509.0, 7));
    assert_eq!(d.asks[4].price, 0.0);
    assert_eq!((d.total_buy_qty, d.total_sell_qty), (350, 80));
}

fn ist_ms(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    crate::clock::ist_offset()
        .with_ymd_and_hms(y, m, d, hh, mm, 0)
        .unwrap()
        .timestamp_millis()
}

use chrono::TimeZone;

#[test]
fn daily_ohlc_from_candles() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
    let c = |d: u32, close: i64| json!([ist_ms(2026, 10, d, 0, 0), 100, 200, 50, close, 7]);
    let (o, h, l, prev) = daily_ohlc(&[c(1, 150), c(2, 180)], today);
    assert_eq!((o, h, l, prev), (1.0, 2.0, 0.5, 1.5));
    // Before the open: no day OHLC, the latest close is the previous close.
    assert_eq!(daily_ohlc(&[c(1, 150)], today), (0.0, 0.0, 0.0, 1.5));
    assert_eq!(daily_ohlc(&[], today), (0.0, 0.0, 0.0, 0.0));
}

#[test]
fn candles_are_stamped_like_the_web() {
    let c = json!([ist_ms(2026, 10, 1, 9, 16), 100, 200, 50, 150, 7, 12]);
    let intraday = to_candle(&c, false).unwrap();
    assert_eq!(intraday.timestamp, ist_ms(2026, 10, 1, 9, 16) / 1000);
    assert_eq!((intraday.close, intraday.volume, intraday.oi), (1.5, 7, 12));
    let d = json!([ist_ms(2026, 10, 1, 0, 0), 100, 200, 50, 150, 7]);
    let daily = to_candle(&d, true).unwrap();
    let utc_midnight = NaiveDate::from_ymd_opt(2026, 10, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    assert_eq!((daily.timestamp, daily.oi), (utc_midnight, 0));
    let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    let (from, to) = window_ms(day, day, i64::MAX);
    assert_eq!(from, ist_ms(2026, 10, 1, 0, 0));
    assert_eq!(to, ist_ms(2026, 10, 1, 23, 59) + 59_000);
    assert_eq!(window_ms(day, day, 5).1, 5);
}

#[test]
fn errors_name_the_cause() {
    let e = fixture_json(fixture!("errors.json"));
    let msg = |status: u16, k: &str| {
        error_for(StatusCode::from_u16(status).unwrap(), &e[k], "/v1/orders").client_message()
    };
    assert!(msg(403, "ip_not_allowed").contains("registered static IP"));
    assert!(matches!(
        error_for(StatusCode::UNAUTHORIZED, &e["unauthorized"], "/v1/x"),
        AppError::Auth(_)
    ));
    assert!(msg(403, "forbidden").contains("Full access"));
    assert_eq!(
        msg(400, "validation"),
        "Sahi: qty must not exceed the exchange freeze quantity of 1800 for this instrument"
    );
    assert!(msg(503, "maintenance").contains("07:00 IST"));
    assert!(msg(502, "validation").contains("not responding"));
    let login = |status: u16, k: &str| {
        auth::login_error(StatusCode::from_u16(status).unwrap(), &e[k]).client_message()
    };
    assert!(login(403, "login_forbidden").contains("click Approve"));
    assert!(login(401, "login_locked").contains("frozen or locked"));
    assert!(login(401, "unauthorized").contains("pk_"));
}

#[test]
fn categories_follow_the_published_table() {
    assert_eq!(
        Category::of(&Method::POST, "/v1/orders"),
        Category::OrderWrite
    );
    assert_eq!(
        Category::of(&Method::DELETE, "/v1/orders/1"),
        Category::OrderWrite
    );
    assert_eq!(
        Category::of(&Method::GET, "/v1/orders/1/trades"),
        Category::OrderRead
    );
    assert_eq!(
        Category::of(&Method::GET, "/v1/marketdata/optionchain/expiries"),
        Category::OptionChain
    );
    assert_eq!(Category::Quotes.limits().windows.len(), 1);
    assert_eq!(Category::OrderWrite.limits().windows[1].max, 240);
}

#[test]
fn adapter_identity() {
    let b = SahiBroker::new(SymbolResolver::new());
    assert_eq!((b.id(), b.name()), ("sahi", "Sahi"));
    // The login card's optional TOTP code (web #2441).
    assert_eq!(b.login_kind(), LoginKind::DirectTotp { fields: &["totp"] });
    assert!(!b.requires_totp());
    assert!(b.capabilities().streaming && !b.capabilities().gtt);
    assert!(!b.supported_exchanges().contains(&Exchange::Mcx));
}

// ---------------------------------------------------------------------------
// HTTP round trips against a fake Sahi
// ---------------------------------------------------------------------------

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn unpaced() -> SahiLimiter {
    SahiLimiter::with(|_| Limits {
        windows: vec![],
        per_day: None,
    })
}

fn broker(base: &str) -> SahiBroker {
    SahiBroker::with_urls(
        master(),
        base,
        base,
        format!("{}/master.csv", base),
        "ws://127.0.0.1:9",
    )
    .with_limiter(unpaced())
}

fn auth() -> AuthToken {
    AuthToken::new("tok123")
}

#[tokio::test]
async fn place_sends_bearer_and_user_agent_and_retries_a_throttle() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = seen.clone();
    let calls = Arc::new(Mutex::new(0u32));
    let c = calls.clone();
    let app = Router::new().route(
        "/v1/orders",
        post(move |h: HeaderMap, Json(body): Json<Value>| async move {
            *c.lock() += 1;
            if *c.lock() == 1 {
                return (
                    Http::TOO_MANY_REQUESTS,
                    [("Retry-After", "0")],
                    Json(json!({"code": "RATE_LIMITED"})),
                )
                    .into_response();
            }
            s.lock().push(format!(
                "{}|{}|{}",
                h["authorization"].to_str().unwrap(),
                h["user-agent"].to_str().unwrap(),
                body["symbol"]
            ));
            (Http::CREATED, Json(json!({"order_ids": ["2610190013"]}))).into_response()
        }),
    );
    let b = broker(&serve(app).await);
    let o = order("BAJAJ-AUTO", Exchange::Nse, PriceType::Limit);
    let r = outcome::placing(b.place_order(&auth(), &o)).await.unwrap();
    assert_eq!(r.order_id, "2610190013");
    assert_eq!(*calls.lock(), 2);
    assert_eq!(
        seen.lock().as_slice(),
        ["Bearer tok123|openalgo-sahi/1.0|\"BAJAJ-AUTO\""]
    );
}

#[tokio::test]
async fn placement_outcomes_are_three() {
    let reply: Arc<Mutex<(u16, Value)>> = Arc::default();
    let r = reply.clone();
    let app = Router::new().route(
        "/v1/orders",
        post(move || async move {
            let (s, v) = r.lock().clone();
            (Http::from_u16(s).unwrap(), Json(v)).into_response()
        }),
    );
    let b = broker(&serve(app).await);
    let o = order("M&M", Exchange::Nse, PriceType::Market);
    *reply.lock() = (403, json!({"code": "IP_NOT_ALLOWED", "message": "no"}));
    let e = outcome::placing(b.place_order(&auth(), &o))
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Refused(_)), "{:?}", e);
    *reply.lock() = (201, json!({}));
    let e = outcome::placing(b.place_order(&auth(), &o))
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Uncertain(_)), "{:?}", e);
    *reply.lock() = (502, json!({"message": "bad gateway"}));
    let e = outcome::placing(b.place_order(&auth(), &o))
        .await
        .unwrap_err();
    assert!(!matches!(e, AppError::Refused(_)), "{:?}", e);
}

#[tokio::test]
async fn cancel_modify_and_books() {
    let app = Router::new()
        .route(
            "/v1/orders/{id}",
            delete(|| async { Http::NO_CONTENT }).put(|| async { Json(json!({"order_id": "9"})) }),
        )
        .route(
            "/v1/orders",
            get(|| async { Json(fixture_json(fixture!("order_book.json"))) }),
        )
        .route(
            "/v1/orders/{id}/trades",
            get(|| async { Json(fixture_json(fixture!("trades.json"))) }),
        )
        .route(
            "/v1/positions",
            get(|| async { Json(fixture_json(fixture!("positions.json"))) }),
        )
        .route(
            "/v1/marketdata/ltp",
            get(|RawQuery(q): RawQuery| async move {
                assert_eq!(q.as_deref(), Some("i=10914&i=1963491"));
                Json(json!({"data": {"10914": {"last_trade_price": 4900}, "1963491": null}}))
            }),
        );
    let b = broker(&serve(app).await);
    assert_eq!(b.cancel_order(&auth(), "9").await.unwrap().order_id, "9");
    let trades = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(trades.len(), 1);
    let positions = b.get_positions(&auth()).await.unwrap();
    assert_eq!((positions[0].ltp, positions[0].pnl), (49.0, 35.0));
    let open = b
        .get_open_position(&auth(), "3IINFOLTD", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(open, -10);
    let cancelled = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(cancelled.cancelled, ["26101900133912345678"]);
}

#[tokio::test]
async fn an_unreadable_position_book_is_never_flat() {
    let app = Router::new().route(
        "/v1/positions",
        get(|| async { Json(json!({"positions": null})) }),
    );
    let b = broker(&serve(app).await);
    assert!(b.get_positions(&auth()).await.is_err());
    let e = b
        .get_open_position(&auth(), "M&M", Exchange::Nse, Product::Mis)
        .await
        .unwrap_err();
    assert!(e.client_message().contains("so no order was sent"));
}

/// A symbol the master does not know is looked up by its own name, as on
/// Angel and Kotak: it never matches a position row that names no symbol.
#[tokio::test]
async fn an_unknown_symbol_never_matches_a_row_without_one() {
    let app = Router::new().route(
        "/v1/positions",
        get(|| async {
            Json(json!({"positions": [
                {"exchange_segment": "NSE", "product": "I", "net_qty": 3},
                {"exchange_segment": "NSE", "symbol": "NOTINMASTER", "product": "I", "net_qty": 7}
            ]}))
        }),
    );
    let b = broker(&serve(app).await);
    let open = b
        .get_open_position(&auth(), "NOTINMASTER", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(open, 7);
    let none = b
        .get_open_position(&auth(), "ABSENT", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[tokio::test]
async fn history_pages_backward() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = seen.clone();
    let first = ist_ms(2026, 10, 1, 9, 16);
    let app = Router::new().route(
        "/v1/marketdata/candles",
        get(move |RawQuery(q): RawQuery| async move {
            let q = q.unwrap_or_default();
            s.lock().push(q.clone());
            if s.lock().len() == 1 {
                Json(json!({"candles": [[first, 100, 200, 50, 150, 7]], "has_more": true}))
            } else {
                Json(json!({"candles": [[first - 60_000, 100, 100, 100, 100, 0]], "has_more": false}))
            }
        }),
    );
    let b = broker(&serve(app).await);
    let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: "1m".into(),
        start: day,
        end: day,
    };
    let candles = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(candles.len(), 2);
    assert!(candles[0].timestamp < candles[1].timestamp);
    let seen = seen.lock();
    assert!(seen[0].contains("instrument_id=12027") && seen[0].contains("size=1m"));
    assert!(seen[1].contains(&format!("to={}", first - 1)));
    assert!(!seen[0].contains("oi=true"));
}

#[tokio::test]
async fn sign_in_reads_the_profile() {
    let app = Router::new()
        .route(
            "/api/v1/apikeys/token",
            post(|Json(b): Json<Value>| async move {
                if b["key_id"] == "pk_1" && b["secret"] == "sahi_sk_1" {
                    (
                        Http::OK,
                        Json(json!({"access_token": "tokA", "expires_at": "x"})),
                    )
                } else {
                    (Http::FORBIDDEN, Json(json!({"code": "Forbidden"})))
                }
            }),
        )
        .route(
            "/v1/profile",
            get(|h: HeaderMap| async move {
                assert_eq!(h["authorization"], "Bearer tokA");
                Json(json!({"user_id": "AB1234", "name": "Jane"}))
            }),
        );
    let b = broker(&serve(app).await);
    let creds = BrokerCredentials {
        api_key: "pk_1".into(),
        api_secret: Some("sahi_sk_1".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(
        (r.auth_token.as_str(), r.user_id.as_str()),
        ("tokA", "AB1234")
    );
    let bad = BrokerCredentials {
        api_secret: Some("other".into()),
        ..creds
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert!(e.client_message().contains("not approved for today"));
    let none = BrokerCredentials::default();
    assert!(b.authenticate(none).await.is_err());
}

/// A token endpoint that records every body it is sent and answers with
/// `reply` (web test_sahi_auth.py's `mint` fixture); no profile scope.
async fn token_mint(reply: (Http, Value)) -> (SahiBroker, Arc<Mutex<Vec<Value>>>) {
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = sent.clone();
    let app = Router::new().route(
        "/api/v1/apikeys/token",
        post(move |Json(b): Json<Value>| {
            let reply = reply.clone();
            async move {
                seen.lock().push(b);
                (reply.0, Json(reply.1))
            }
        }),
    );
    (broker(&serve(app).await), sent)
}

fn sahi_keys(totp: Option<&str>) -> BrokerCredentials {
    BrokerCredentials {
        api_key: "pk_1".into(),
        api_secret: Some("sahi_sk_1".into()),
        totp: totp.map(str::to_string),
        ..Default::default()
    }
}

fn token_issued() -> (Http, Value) {
    (
        Http::OK,
        json!({"access_token": "tokA", "expires_at": "x", "scope": "s"}),
    )
}

/// Web test/test_sahi_auth.py `test_regular_key_sends_no_totp` and
/// `test_totp_key_sends_the_code_from_the_card` (web #2441): a Regular
/// key's mint carries no `totp` (Sahi refuses one); a TOTP key's carries
/// the card's code, spaces removed, as a string.
#[tokio::test]
async fn a_regular_key_sends_no_code_and_a_totp_key_sends_the_cards() {
    for blank in [None, Some(""), Some("   ")] {
        let (b, sent) = token_mint(token_issued()).await;
        let r = b.authenticate(sahi_keys(blank)).await.unwrap();
        assert_eq!(r.auth_token, "tokA");
        assert_eq!(
            *sent.lock(),
            [json!({"key_id": "pk_1", "secret": "sahi_sk_1"})],
            "{:?}",
            blank
        );
    }
    let (b, sent) = token_mint(token_issued()).await;
    b.authenticate(sahi_keys(Some(" 042 917 "))).await.unwrap();
    assert_eq!(
        *sent.lock(),
        [json!({"key_id": "pk_1", "secret": "sahi_sk_1", "totp": "042917"})]
    );
}

/// Web `test_a_code_that_is_not_six_digits_is_refused_before_sending` and
/// `test_the_api_secret_is_sent_as_saved`.
#[tokio::test]
async fn a_code_that_is_not_six_digits_is_refused_before_sending() {
    for bad in ["12345", "1234567", "12a456", "abcdef"] {
        let (b, sent) = token_mint(token_issued()).await;
        let e = b.authenticate(sahi_keys(Some(bad))).await.unwrap_err();
        assert!(e.client_message().contains("6-digit code"), "{bad}: {e:?}");
        assert!(sent.lock().is_empty(), "{bad}");
    }
    // Nothing after ::: is read: the secret goes to Sahi as saved.
    let (b, sent) = token_mint(token_issued()).await;
    let keys = BrokerCredentials {
        api_secret: Some("sahi_sk_1:::X".into()),
        ..sahi_keys(None)
    };
    b.authenticate(keys).await.unwrap();
    assert_eq!(sent.lock()[0]["secret"], "sahi_sk_1:::X");
}

/// Web `test_a_wrong_code_says_so` and `test_login_refusals_name_the_cause`.
#[tokio::test]
async fn login_refusals_name_the_cause() {
    let (b, _) = token_mint((
        Http::UNAUTHORIZED,
        json!({"code": "InvalidTotp", "message": "The TOTP code is not valid",
               "server_time": "2026-10-27T03:45:12Z"}),
    ))
    .await;
    let e = b.authenticate(sahi_keys(Some("042917"))).await.unwrap_err();
    assert!(e.client_message().contains("did not accept the TOTP code"));
    for (status, code, expected) in [
        (403, "Forbidden", "not approved for today"),
        (403, "Forbidden", "not enabled for your Sahi account"),
        (403, "Forbidden", "enter its code when you log in"),
        (400, "InvalidRequest", "leave the code blank"),
        (429, "TooManyRequests", "wrong TOTP codes"),
        (401, "Locked", "frozen or locked"),
        (401, "Unauthorized", "pk_"),
    ] {
        let m = auth::login_error(
            StatusCode::from_u16(status).unwrap(),
            &json!({"code": code}),
        )
        .client_message();
        assert!(m.contains(expected), "{status} {code}: {m}");
    }
}

// ---------------------------------------------------------------------------
// Secrets never reach a log or an error
// ---------------------------------------------------------------------------

const S: &str = "SENTINELq7Zx";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sentinel_credentials_never_leak() {
    let c = Captured::default();
    let w = c.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(sub);
    let refuse = Router::new().fallback(|| async {
        (
            Http::UNAUTHORIZED,
            Json(json!({"code": "UNAUTHORIZED", "message": "bad"})),
        )
    });
    let refusing = serve(refuse).await;
    // A closed port: transport errors carry the request URL.
    let closed = "http://127.0.0.1:9".to_string();
    let creds = BrokerCredentials {
        api_key: format!("pk_{}", S),
        api_secret: Some(format!("sahi_sk_{}", S)),
        ..Default::default()
    };
    let token = AuthToken::new(S);
    let mut errors = Vec::new();
    for base in [refusing, closed] {
        let b = broker(&base);
        errors.push(b.authenticate(creds.clone()).await.unwrap_err());
        errors.push(b.get_order_book(&token).await.unwrap_err());
        errors.push(b.get_funds(&token).await.unwrap_err());
        let o = order("M&M", Exchange::Nse, PriceType::Market);
        errors.push(
            outcome::placing(b.place_order(&token, &o))
                .await
                .unwrap_err(),
        );
    }
    let logs = c.text();
    assert!(
        !logs.is_empty(),
        "nothing was captured, so the check is vacuous"
    );
    assert!(!logs.contains(S), "{}", logs);
    for e in errors {
        for text in [e.to_string(), format!("{:?}", e), e.client_message()] {
            assert!(!text.contains(S), "{}", text);
        }
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}
