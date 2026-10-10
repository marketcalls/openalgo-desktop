//! HDFC Sky mapping tests against payloads built from the web plugin and
//! the HDFC Sky API docs (`src-tauri/tests/fixtures/brokers/hdfcsky/`).

use super::auth::access_token_of;
use super::data::{chart_symbols, interval_spec, parse_candles, parse_ltp, resample, Resample};
use super::mapping::*;
use super::master_contract::{classify_index, parse_csv};
use super::proto::{self, packet_type as pt};
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use prost::Message as _;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/hdfcsky/", $name))
    };
}

fn j(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_csv(fixture!("CompactScrip.csv")).unwrap());
    r
}

fn epoch(y: i32, m: u32, d: u32, h: u32, mi: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap().timestamp()
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_symbols() {
    let rows = parse_csv(fixture!("CompactScrip.csv")).unwrap();
    // 20 data rows: the MCX commodity definition and the duplicate
    // RELIANCE-EQ are dropped.
    assert_eq!(rows.len(), 18);
    let r = master();
    let rel = r.by_symbol("NSE", "RELIANCE").unwrap();
    assert_eq!(rel.brsymbol, "RELIANCE-EQ");
    assert_eq!(rel.token, "2885");
    assert_eq!(rel.expiry, "");
    assert_eq!(rel.strike, 0.0);
    assert_eq!(rel.instrument_type, "EQ");
    assert_eq!(rel.name, "RELIANCE INDUSTRIES LTD");
    // Only -EQ is stripped; a dash inside the name survives.
    assert_eq!(
        r.by_symbol("NSE", "BAJAJ-AUTO").unwrap().brsymbol,
        "BAJAJ-AUTO-EQ"
    );
    // BSE strips its group suffix.
    let bse = r.by_symbol("BSE", "RELIANCE").unwrap();
    assert_eq!(
        (bse.brsymbol.as_str(), bse.token.as_str()),
        ("RELIANCE-A", "500325")
    );
    assert_eq!(bse.tick_size, 0.05);
}

#[test]
fn master_contract_indices() {
    let r = master();
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.brsymbol, "Nifty 50");
    assert_eq!(nifty.brexchange, "NSE");
    assert_eq!(nifty.instrument_type, "EQ");
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
    assert!(r.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    // Unmapped NSE names fall back to the space-free uppercase form.
    assert!(r.by_symbol("NSE_INDEX", "NIFTYAUTO").is_some());
    let sensex = r.by_symbol("BSE_INDEX", "SENSEX").unwrap();
    assert_eq!(sensex.brexchange, "BSE");
    assert!(r.by_symbol("BSE_INDEX", "SENSEX50").is_some());
    assert!(r.by_symbol("NSE", "Nifty 50").is_none());
    assert_eq!(classify_index("Nifty Fin Service", "NSE_INDEX"), "FINNIFTY");
    assert_eq!(classify_index("BSE HC", "BSE_INDEX"), "BSEHEALTHCARE");
    assert_eq!(classify_index("Nifty Pharma", "NSE_INDEX"), "NIFTYPHARMA");
}

#[test]
fn master_contract_derivatives() {
    let r = master();
    let fut = r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY26OCTFUT");
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.strike, 0.0);
    assert_eq!(fut.lot_size, 65);
    assert_eq!(fut.instrument_type, "FUT");
    assert_eq!(fut.name, "NIFTY");
    let ce = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    assert_eq!((ce.strike, ce.instrument_type.as_str()), (25000.0, "CE"));
    // Weekly with the O/N/D month letter.
    let wk = r.by_symbol("NFO", "NIFTY06OCT2625000PE").unwrap();
    assert_eq!(wk.brsymbol, "NIFTY26O0625000PE");
    assert_eq!(wk.name, "NIFTY");
    // Weekly with a month digit (BFO).
    let sx = r.by_symbol("BFO", "SENSEX13AUG2669500CE").unwrap();
    assert_eq!(sx.brsymbol, "SENSEX2681369500CE");
    assert_eq!(sx.name, "SENSEX");
    // Currency weekly future: company name is a copy of the symbol, the
    // stripped underlying wins.
    let eur = r.by_symbol("CDS", "EURINR01OCT26FUT").unwrap();
    assert_eq!(eur.name, "EURINR");
    assert_eq!(eur.tick_size, 0.0025);
    assert!(r.by_symbol("NFO", "M&M25AUG264050PE").is_some());
    assert!(r.by_symbol("NFO", "IDEA25AUG267.5CE").is_some());
    let crude = r.by_symbol("MCX", "CRUDEOIL19OCT26FUT").unwrap();
    assert_eq!((crude.name.as_str(), crude.lot_size), ("CRUDEOIL", 100));
    assert!(r.by_symbol("MCX", "CRUDEOIL").is_none());
    // A symbol that does not follow the pattern falls back to company name.
    assert!(r.by_symbol("NFO", "ZZTEST27OCT26FUT").is_some());
}

#[test]
fn master_contract_rejects_unknown_header() {
    assert!(parse_csv("a,b\n1,2\n").is_err());
    assert!(parse_csv("").is_err());
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

#[test]
fn exchange_codes() {
    assert_eq!(to_rest_exchange("NSE_INDEX"), "NSE");
    assert_eq!(to_rest_exchange("BSE_INDEX"), "BSE");
    assert_eq!(to_rest_exchange("NFO"), "NFO");
    assert_eq!(to_ltp_exchange("NSE_INDEX"), "NSE_INDEX");
    assert_eq!(to_oa_exchange("NCD"), "CDS");
    assert_eq!(to_oa_exchange("nfo"), "NFO");
    assert_eq!(ws_scrip_id("CDS", "60001"), "NCD_60001");
    assert_eq!(ws_scrip_id("NSE_INDEX", "26000"), "NSE_INDEX_26000");
    assert_eq!(ws_scrip_id("NSE", "2885"), "NSE_2885");
    assert_eq!(
        from_ws_scrip_id("NSE_INDEX_26000"),
        Some(("NSE_INDEX", "26000"))
    );
    assert_eq!(from_ws_scrip_id("NCD_1"), Some(("CDS", "1")));
    assert_eq!(from_ws_scrip_id("XYZ_1"), None);
}

#[test]
fn order_constants() {
    assert_eq!(order_type(PriceType::SlM), "SLM");
    assert_eq!(order_type(PriceType::Market), "MARKET");
    assert_eq!(reverse_order_type("SLM"), "SL-M");
    assert_eq!(reverse_order_type("SL-M"), "SL-M");
    assert_eq!(reverse_order_type("BO"), "BO");
    assert_eq!(reverse_product("MTF"), Some("CNC"));
    assert_eq!(reverse_product("NRML"), Some("NRML"));
    assert_eq!(reverse_product("XX"), None);
    assert_eq!(product_code(Product::Nrml), "0");
    assert_eq!(product_code(Product::Cnc), "1");
    assert_eq!(product_code(Product::Mis), "2");
}

#[test]
fn status_map_matches_web() {
    for (raw, want) in [
        ("COMPLETE", "complete"),
        ("MODIFY_REJECTED", "rejected"),
        ("BRACKET_ORDER_CANCELLED", "cancelled"),
        ("SL_TRIGGER_CONFIRMED", "trigger pending"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("PARTIAL_TRADE", "open"),
        ("RMS_VALIDATION_COMPLETED", "open"),
        ("pending", "open"),
        ("SOMETHING_NEW", "something_new"),
    ] {
        assert_eq!(map_status(raw), want, "{}", raw);
    }
    assert!(is_cancellable("PENDING"));
    assert!(is_cancellable("TRIGGER_PENDING"));
    assert!(is_cancellable("AMO_REQ_RECEIVED"));
    assert!(!is_cancellable("COMPLETE"));
    // Not in the broker vocabulary: never cancelled blindly.
    assert!(!is_cancellable("OPEN"));
}

#[test]
fn series_types() {
    let r = master();
    let st = |ex: &str, sym: &str| series_type(&r.by_symbol(ex, sym).unwrap());
    assert_eq!(st("NSE", "RELIANCE"), "EQ");
    assert_eq!(st("BSE", "RELIANCE"), "A");
    assert_eq!(st("NSE_INDEX", "NIFTY"), "INDICES");
    assert_eq!(st("BSE_INDEX", "SENSEX"), "IDX");
    assert_eq!(st("NFO", "NIFTY27OCT26FUT"), "FUTIDX");
    assert_eq!(st("NFO", "NIFTY27OCT2625000CE"), "OPTIDX");
    assert_eq!(st("NFO", "M&M25AUG264050PE"), "OPTSTK");
    assert_eq!(st("NFO", "ZZTEST27OCT26FUT"), "FUTSTK");
    assert_eq!(st("BFO", "SENSEX13AUG2669500CE"), "IO");
    assert_eq!(st("MCX", "CRUDEOIL19OCT26FUT"), "FUTCOM");
    assert_eq!(st("CDS", "EURINR01OCT26FUT"), "FUTCUR");
}

#[test]
fn jwt_client_id() {
    use base64::Engine;
    let enc = |v: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    let tok = format!(
        "{}.{}.sig",
        enc(&json!({"alg":"HS256"})),
        enc(&json!({"sub":"S0190007"}))
    );
    assert_eq!(client_id_from_jwt(&tok).as_deref(), Some("S0190007"));
    let tok2 = format!("h.{}.s", enc(&json!({"client_id": 42})));
    assert_eq!(client_id_from_jwt(&tok2).as_deref(), Some("42"));
    assert_eq!(client_id_from_jwt("opaque"), None);
    assert_eq!(client_id_from_jwt("a.!!!.c"), None);
    let auth = AuthToken::new(format!("KEY:{}", tok));
    let s = HdfcSkyBroker::session(&auth).unwrap();
    assert_eq!(
        (s.api_key.as_str(), s.client_id.as_str()),
        ("KEY", "S0190007")
    );
    assert!(HdfcSkyBroker::session(&AuthToken::new("nocolon")).is_err());
}

#[test]
fn token_exchange_answers() {
    assert_eq!(
        access_token_of(&json!({"accessToken": "a"})).as_deref(),
        Some("a")
    );
    assert_eq!(
        access_token_of(&json!({"status": "success", "data": {"access_token": "b"}})).as_deref(),
        Some("b")
    );
    assert_eq!(access_token_of(&json!({"message": "bad"})), None);
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

fn order(symbol: &str, exchange: &str, pricetype: &str) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: "BUY".into(),
            quantity: 65,
            price: 0.0,
            order_type: pricetype.into(),
            product: "NRML".into(),
            validity: "DAY".into(),
            trigger_price: Some(24800.0),
            disclosed_quantity: None,
            amo: false,
        },
        &master(),
    )
    .unwrap()
}

/// BR-05: orders placed within the same millisecond (basket legs go ten at
/// a time) get distinct ids; the clock's id is used once it moves past the
/// last one, and ids stay below 1e9 across the wrap.
#[test]
fn order_ids_are_distinct_within_one_millisecond() {
    let ids = super::mapping::OrderIds::new();
    let now = 1_759_650_123_456;
    let got: Vec<i64> = (0..10).map(|_| ids.next(now)).collect();
    let distinct: std::collections::HashSet<i64> = got.iter().copied().collect();
    assert_eq!(distinct.len(), 10, "{:?}", got);
    assert_eq!(got[0], 650_123_456);
    assert_eq!(got[9], 650_123_465);
    // The clock moved on past them: its own id again.
    assert_eq!(ids.next(now + 100), 650_123_556);
    // Near the wrap: never 1e9 or more.
    let wrap = super::mapping::OrderIds::new();
    let edge = 1_999_999_999;
    assert_eq!(wrap.next(edge), 999_999_999);
    assert_eq!(wrap.next(edge), 0);
    assert_eq!(wrap.next(edge + 5), 4);
    // Concurrent callers never share one.
    let shared = std::sync::Arc::new(super::mapping::OrderIds::new());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let s = shared.clone();
            std::thread::spawn(move || (0..100).map(|_| s.next(now)).collect::<Vec<_>>())
        })
        .collect();
    let all: Vec<i64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let unique: std::collections::HashSet<i64> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len());
}

#[test]
fn place_and_modify_bodies() {
    let o = order("NIFTY27OCT26FUT", "NFO", "LIMIT");
    let b = place_body(&o, "S1", "LIMIT", 25010.5, user_order_id(1_759_650_123_456));
    assert_eq!(
        b,
        json!({
            "exchange": "NFO",
            "instrument_token": "35001",
            "client_id": "S1",
            "order_type": "LIMIT",
            "order_side": "BUY",
            "product": "NRML",
            "quantity": 65,
            "price": 25010.5,
            "trigger_price": 24800.0,
            "disclosed_quantity": 0,
            "validity": "DAY",
            "device": "WEB",
            "execution_type": "REGULAR",
            "amo": false,
            "user_order_id": 650_123_456,
        })
    );
    let idx = ResolvedOrder {
        exchange: crate::brokers::common::mapping::Exchange::NseIndex,
        ..order("NIFTY27OCT26FUT", "NFO", "LIMIT")
    };
    assert_eq!(place_body(&idx, "S1", "LIMIT", 1.0, 1)["exchange"], "NSE");

    let m = ResolvedModify::resolve(
        "OID9",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "SL-M".into(),
            quantity: 3,
            price: 0.0,
            trigger_price: 1390.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let mb = modify_body(&m, "S1");
    assert_eq!(mb["oms_order_id"], "OID9");
    assert_eq!(mb["order_type"], "SLM");
    assert_eq!(mb["instrument_token"], "2885");
    assert_eq!(mb["execution_type"], "REGULAR");
    assert!(mb.get("order_side").is_none());
}

#[test]
fn margin_legs_and_result() {
    let r = master();
    let row = r.by_symbol("NFO", "NIFTY27OCT2625000CE").unwrap();
    let leg = MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: Action::Sell,
        quantity: 65,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 210.0,
        trigger_price: 0.0,
    };
    assert_eq!(
        margin_leg(&leg, &row, 25123.6),
        json!({
            "segment": "FutOpt",
            "series": "OPTIDX",
            "exchange": "NFO",
            "side": "SELL",
            "mode": "NEW",
            "symbol": "NIFTY26OCT25000CE",
            "underlying": 25124,
            "token": "40001",
            "quantity": "65",
            "price": "210.0",
            "product": "0",
        })
    );
    assert_eq!(margin_segment("MCX"), "Commodities");
    assert_eq!(margin_segment("NSE_INDEX"), "Capital");

    let res = parse_margin(&j(fixture!("margin.json"))["result"]);
    assert_eq!(res.total_margin_required, 114000.5);
    assert_eq!(res.span_margin, 95000.5);
    assert_eq!(res.exposure_margin, 20000.0);
    // Empty combined block: the legs are summed.
    let legs_only = json!({
        "combined_margin": {},
        "individual_margin_values": [
            {"span": 120000, "exposure_margin": 25000},
            {"premium_margin": 13650}
        ]
    });
    let res = parse_margin(&legs_only);
    assert_eq!(res.total_margin_required, 158650.0);
    assert_eq!(res.span_margin, 120000.0);
}

// ---------------------------------------------------------------------------
// Books and funds
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised() {
    let r = master();
    let mut rows = unwrap_rows(&j(fixture!("orders_pending.json")), "orders");
    rows.extend(unwrap_rows(&j(fixture!("orders_completed.json")), "orders"));
    let o = map_orders(&rows, &r);
    assert_eq!(o.len(), 7);
    assert_eq!(o[0].symbol, "RELIANCE");
    assert_eq!(o[0].status, "open");
    assert_eq!(o[0].exchange_order_id.as_deref(), Some("1100000012345678"));
    assert_eq!(o[0].pending_quantity, 10);
    assert_eq!(o[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!(o[1].status, "trigger pending");
    assert_eq!((o[1].quantity, o[1].trigger_price), (65, 24800.0));
    assert_eq!(o[2].symbol, "BAJAJ-AUTO");
    assert_eq!(o[3].status, "open");
    let done = &o[4];
    assert_eq!(done.status, "complete");
    assert_eq!(done.product, "CNC");
    assert_eq!(done.average_price, 1401.5);
    assert_eq!(done.filled_quantity, 10);
    assert_eq!(o[5].exchange, "CDS");
    assert_eq!(o[5].symbol, "EURINR01OCT26FUT");
    assert_eq!(o[5].status, "cancelled");
    assert_eq!(o[6].status, "rejected");
    assert_eq!(o[6].order_type, "MARKET");
    assert_eq!(
        o[6].rejection_reason.as_deref(),
        Some("RMS: insufficient margin")
    );
}

#[test]
fn trades_positions_holdings() {
    let r = master();
    let t = map_trades(&unwrap_rows(&j(fixture!("trades.json")), "trades"), &r);
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].symbol, "RELIANCE");
    assert_eq!(t[0].trade_value, 14015.0);
    assert_eq!(t[1].symbol, "NIFTY27OCT26FUT");
    assert_eq!((t[1].quantity, t[1].average_price), (65, 25000.0));
    assert_eq!(t[1].timestamp, "05-10-2026 09:40:00");

    let p = map_positions(
        &unwrap_rows(&j(fixture!("positions.json")), "positions"),
        &r,
    );
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].symbol, "NIFTY27OCT26FUT");
    assert_eq!(p[0].quantity, -65);
    assert_eq!(p[0].average_price, 25000.0);
    assert_eq!(p[0].pnl, 6500.0);
    assert_eq!(p[1].symbol, "RELIANCE");
    assert_eq!(
        (p[1].quantity, p[1].average_price, p[1].pnl),
        (10, 1400.0, 100.0)
    );
    assert_eq!((p[2].quantity, p[2].pnl), (0, 100.0));

    let h = map_holdings(&unwrap_rows(&j(fixture!("holdings.json")), "holdings"), &r);
    assert_eq!(h[0].symbol, "RELIANCE");
    assert_eq!(h[0].product, "CNC");
    assert_eq!((h[0].pnl, h[0].pnl_percentage), (550.0, 8.46));
    assert_eq!(h[0].isin.as_deref(), Some("INE002A01018"));
    // Unknown scrip keeps the broker symbol; zero cost has 0 percent.
    assert_eq!(h[1].symbol, "UNLISTEDCO");
    assert_eq!(h[1].pnl_percentage, 0.0);
}

#[test]
fn funds_labels_and_named_mtm() {
    let f = funds_from_view(&j(fixture!("funds.json"))["data"]);
    assert_eq!(f.available_cash, 100000.0);
    assert_eq!(f.utilised_debits, 2500.75);
    assert_eq!(f.used_margin, 2500.75);
    assert_eq!(f.collateral, 5000.0);
    // Top-level fields win over the labelled rows; MTF rows are ignored.
    assert_eq!(f.m2m_realized, 125.5);
    assert_eq!(f.m2m_unrealized, -40.25);
    let dicts = json!({"values": [{"0": "Available Margin", "1": "7"}]});
    assert_eq!(funds_from_view(&dicts).available_cash, 7.0);
    assert_eq!(funds_from_view(&json!({})), Funds::default());
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn ltp_rows() {
    let m = parse_ltp(&json!({"data": [
        {"exchange": "nse", "token": 2885, "ltp": "1410.5", "prev_close": 1400},
        {"exchange": "NSE_INDEX", "token": "26000", "ltp": 25000.1, "prev_close": null}
    ]}));
    assert_eq!(
        m[&("NSE".to_string(), "2885".to_string())],
        (1410.5, 1400.0)
    );
    assert_eq!(
        m[&("NSE_INDEX".to_string(), "26000".to_string())],
        (25000.1, 0.0)
    );
}

#[test]
fn chart_symbol_candidates() {
    let r = master();
    assert_eq!(
        chart_symbols(&r.by_symbol("NSE_INDEX", "NIFTY").unwrap()),
        ["NIFTY", "NIFTY 50"]
    );
    assert_eq!(
        chart_symbols(&r.by_symbol("NSE_INDEX", "INDIAVIX").unwrap()),
        ["INDIAVIX", "INDIA VIX"]
    );
    assert_eq!(
        chart_symbols(&r.by_symbol("BSE_INDEX", "SENSEX").unwrap()),
        ["SENSEX"]
    );
    assert_eq!(
        chart_symbols(&r.by_symbol("NSE", "RELIANCE").unwrap()),
        ["RELIANCE"]
    );
    assert_eq!(
        chart_symbols(&r.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap()),
        ["NIFTY26OCTFUT"]
    );
}

fn results(v: &Value) -> Vec<Vec<Value>> {
    v["data"]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_array().unwrap().clone())
        .collect()
}

#[test]
fn intraday_candles_sorted_and_resampled() {
    let c = parse_candles(&results(&j(fixture!("candles_minute.json"))), false);
    // Malformed row dropped, duplicate 09:15 removed, sorted.
    assert_eq!(c.len(), 5);
    assert_eq!(c[0].timestamp, epoch(2026, 10, 5, 3, 45));
    assert_eq!(c[0].open, 1400.0);
    assert!(c.windows(2).all(|w| w[0].timestamp < w[1].timestamp));

    let five = resample(&c, interval_spec("5m").unwrap().1);
    assert_eq!(five.len(), 2);
    assert_eq!(five[0].timestamp, epoch(2026, 10, 5, 3, 45));
    assert_eq!(
        (
            five[0].open,
            five[0].high,
            five[0].low,
            five[0].close,
            five[0].volume
        ),
        (1400.0, 1406.0, 1399.0, 1405.0, 9600)
    );
    assert_eq!(five[1].timestamp, epoch(2026, 10, 5, 4, 45));

    // pandas 60min bins are aligned to the UTC hour.
    let hour = resample(&c, interval_spec("1h").unwrap().1);
    assert_eq!(hour[0].timestamp, epoch(2026, 10, 5, 3, 0));
    assert_eq!(hour[1].timestamp, epoch(2026, 10, 5, 4, 0));
    assert_eq!(resample(&c, Resample::None), c);
}

#[test]
fn daily_weekly_monthly_candles() {
    let d = parse_candles(&results(&j(fixture!("candles_day.json"))), true);
    assert_eq!(d.len(), 5);
    // Daily candles land on IST midnight.
    assert_eq!(d[0].timestamp, epoch(2026, 9, 28, 5, 30));
    assert_eq!(
        NaiveDate::from_ymd_opt(2026, 9, 28).unwrap().weekday(),
        chrono::Weekday::Mon
    );
    let w = resample(&d, interval_spec("W").unwrap().1);
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].timestamp, epoch(2026, 9, 28, 0, 0));
    assert_eq!(
        (w[0].open, w[0].close, w[0].volume),
        (1350.0, 1380.0, 6_000_000)
    );
    assert_eq!(w[1].timestamp, epoch(2026, 10, 5, 0, 0));
    assert_eq!((w[1].high, w[1].low), (1405.0, 1375.0));
    let m = resample(&d, interval_spec("M").unwrap().1);
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].timestamp, epoch(2026, 9, 1, 0, 0));
    assert_eq!(m[1].timestamp, epoch(2026, 10, 1, 0, 0));
    assert_eq!((m[1].open, m[1].close), (1360.0, 1400.0));
    assert_eq!(interval_spec("D"), Some(("DAY", Resample::None)));
    assert_eq!(interval_spec("2h"), None);
    assert_eq!(TIMEFRAME_MAP.len(), 10);
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

fn depth(qty: i64, price: f64, orders: i64, buy: bool) -> proto::MarketDepthDto {
    proto::MarketDepthDto {
        quantity: qty,
        price,
        number_of_orders: orders,
        buy_flag: buy,
    }
}

fn mbp_packet(token: i64, ptype: i32) -> proto::GenericDto {
    proto::GenericDto {
        instrument_id: token,
        packet_type: ptype,
        packet_timestamp: 1_759_650_000_000,
        mbp_data: Some(proto::MbpData {
            last_traded_price: 1410.5,
            last_trade_time: 1_759_649_999,
            open_price: 1400.0,
            high_price: 1415.0,
            low_price: 1398.0,
            closing_price: 1400.0,
            volume_traded_today: 123_456,
            last_trade_quantity: 7,
            average_trade_price: 1407.25,
            total_buy_quantity: 5000,
            total_sell_quantity: 6000,
            oi: 0,
            lower_circuit_limit: 1260.0,
            upper_circuit_limit: 1540.0,
            market_depth_dto_list: Some(proto::MarketDepthDtoList {
                market_depth_dto: (0..6)
                    .map(|i| depth(100 + i, 1410.0 - i as f64 * 0.1, 1 + i, true))
                    .chain((0..5).map(|i| depth(200 + i, 1410.6 + i as f64 * 0.1, 2, false)))
                    .collect(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn index_packet() -> proto::GenericDto {
    proto::GenericDto {
        instrument_id: 26000,
        packet_type: pt::NSE_INDEX,
        index_data: Some(proto::IndexData {
            index_name: "Nifty 50".into(),
            index_value: 25100.5,
            high_index_value: 25150.0,
            low_index_value: 24990.0,
            opening_index: 25000.0,
            closing_index: 25000.0,
            packet_time_stamp: 1_759_650_001_000,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn frame(packets: Vec<proto::GenericDto>) -> Vec<u8> {
    proto::GenericDtoList {
        generic_dto_list: packets,
    }
    .encode_to_vec()
}

#[test]
fn decodes_index_mbp_and_depth_packets() {
    let heartbeat = proto::GenericDto {
        instrument_id: 1,
        packet_type: pt::HEARTBEAT,
        ..Default::default()
    };
    let greek = proto::GenericDto {
        instrument_id: 40001,
        packet_type: pt::NSE_FO_GREEK,
        greek_data: Some(proto::GreekData {
            delta: 0.5,
            ..Default::default()
        }),
        ..Default::default()
    };
    let ticks = decode_frame(&frame(vec![
        heartbeat,
        index_packet(),
        mbp_packet(2885, pt::NSE_CM_ALL),
        greek,
    ]));
    assert_eq!(ticks.len(), 3);
    let idx = &ticks[0];
    assert_eq!(idx.kind, Kind::Index);
    assert_eq!((idx.token, idx.ltp, idx.close), (26000, 25100.5, 25000.0));
    assert_eq!(idx.timestamp, 1_759_650_001_000);
    let m = &ticks[1];
    assert_eq!(m.kind, Kind::Mbp);
    assert_eq!((m.volume, m.ltq, m.total_buy_quantity), (123_456, 7, 5000));
    // Six bid levels on the wire, five kept; asks split on buyFlag.
    assert_eq!(m.buy.len(), 5);
    assert_eq!(m.sell.len(), 5);
    assert_eq!(m.buy[0].price, 1410.0);
    assert_eq!((m.sell[0].quantity, m.sell[0].orders), (200, 2));
    assert_eq!(ticks[2].kind, Kind::Greek);

    // A bare GenericDTO (no list wrapper) decodes too.
    let bare = mbp_packet(2885, pt::BSE_CM).encode_to_vec();
    let t = decode_frame(&bare);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].packet_type, pt::BSE_CM);
    assert!(decode_frame(&[0xff, 0xff, 0xff]).is_empty());
}

#[test]
fn snapshot_merge_keeps_filled_fields() {
    let mut a = decode_frame(&frame(vec![mbp_packet(35001, pt::NSE_FO_ALL)])).remove(0);
    let oi_only = RawTick {
        token: 35001,
        packet_type: pt::NSE_FO_OI,
        oi: 987_654,
        ..Default::default()
    };
    a.merge(&oi_only);
    assert_eq!(a.oi, 987_654);
    assert_eq!(a.ltp, 1410.5);
    assert!(a.has_depth());
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: exchange.into(),
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
fn subscribe_frames_and_heartbeat() {
    let mut f = HdfcSkyFeed::new(WS_URL, "KEY", "TOK");
    let frames = f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("RELIANCE", "NSE", "2885", FeedMode::Quote),
        sub("EURINR01OCT26FUT", "CDS", "60001", FeedMode::Depth),
    ]);
    assert_eq!(frames.len(), 2);
    assert_eq!(
        text(&frames[0]),
        json!({"heart_beat": false, "subscribe": [{"scripId": "NSE_INDEX_26000", "type": "LTP"}]})
    );
    assert_eq!(
        text(&frames[1]),
        json!({"heart_beat": false, "subscribe": [
            {"scripId": "NSE_2885", "type": "ALL"},
            {"scripId": "NCD_60001", "type": "ALL"}
        ]})
    );
    let un = f.unsubscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
    assert_eq!(
        text(&un[0]),
        json!({"heart_beat": false, "unSubscribe": [{"scripId": "NSE_2885", "type": "ALL"}]})
    );
    let (every, hb) = f.heartbeat().unwrap();
    assert_eq!(every, std::time::Duration::from_secs(10));
    assert_eq!(text(&hb), json!({"heart_beat": true}));
    // 250 scrips go out as three frames of at most 100.
    let many: Vec<FeedSubscription> = (0..250)
        .map(|i| sub(&format!("S{}", i), "NSE", &i.to_string(), FeedMode::Quote))
        .collect();
    let frames = f.subscribe_frames(&many);
    assert_eq!(frames.len(), 3);
    assert_eq!(text(&frames[2])["subscribe"].as_array().unwrap().len(), 50);
    // Quote <-> Depth needs no frames; LTP -> Quote re-subscribes.
    let q = sub("RELIANCE", "NSE", "2885", FeedMode::Quote);
    assert!(f
        .mode_change_frames(&q, &q.with_mode(FeedMode::Depth))
        .is_empty());
    assert_eq!(
        f.mode_change_frames(&q.with_mode(FeedMode::Ltp), &q).len(),
        2
    );
}

#[test]
fn request_carries_query_auth_and_user_agent() {
    let f = HdfcSkyFeed::new("wss://h/wsapi/v1/session", "K&1", "T/2");
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://h/wsapi/v1/session?token=T%2F2&api_key=K%261"
    );
    assert_eq!(req.headers()["User-Agent"], USER_AGENT);
    assert_eq!(req.headers()["Authorization"], "T/2");
}

#[test]
fn ticks_follow_the_subscribed_mode() {
    let mut f = HdfcSkyFeed::new(WS_URL, "K", "T");
    f.subscribe_frames(&[
        sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp),
        sub("RELIANCE", "NSE", "2885", FeedMode::Depth),
        sub("RELIANCE", "BSE", "2885", FeedMode::Quote),
    ]);
    let ev = f.parse(&Message::Binary(frame(vec![
        index_packet(),
        mbp_packet(2885, pt::NSE_CM_ALL),
        mbp_packet(2885, pt::BSE_CM),
        mbp_packet(999, pt::NSE_CM_ALL),
    ])));
    // Index LTP tick, NSE tick + depth, BSE tick; the unknown token is dropped.
    assert_eq!(ev.len(), 4);
    let FeedEvent::Tick(i) = &ev[0] else { panic!() };
    assert_eq!(
        (i.symbol.as_str(), i.exchange.as_str(), i.mode),
        ("NIFTY", "NSE_INDEX", 1)
    );
    assert_eq!((i.ltp, i.close, i.open), (25100.5, 25000.0, 0.0));
    assert_eq!(i.change, 100.5);
    let FeedEvent::Tick(n) = &ev[1] else { panic!() };
    assert_eq!((n.exchange.as_str(), n.mode), ("NSE", 3));
    assert_eq!(
        (n.volume, n.average_price, n.last_quantity),
        (123_456, 1407.25, 7)
    );
    assert_eq!(n.last_trade_time_ms, 1_759_649_999);
    let FeedEvent::Depth(d) = &ev[2] else {
        panic!()
    };
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    assert_eq!(d.total_sell_quantity, 6000);
    let FeedEvent::Tick(bse) = &ev[3] else {
        panic!()
    };
    assert_eq!((bse.exchange.as_str(), bse.mode), ("BSE", 2));
}

/// BF-04: a packet is published only under the subscription of its own
/// segment and token. With NSE and NFO token 35001 both subscribed, an NFO
/// packet maps to NFO; once NFO is unsubscribed, an NFO packet still in
/// flight is dropped instead of being published as the NSE instrument; a
/// packet of a type that carries no segment (an order or trade packet) is
/// dropped even when only one subscription has its token.
#[test]
fn packets_never_borrow_another_segments_symbol() {
    let mut f = HdfcSkyFeed::new(WS_URL, "K", "T");
    f.subscribe_frames(&[
        sub("SBIN", "NSE", "35001", FeedMode::Quote),
        sub("NIFTY27OCT2625000CE", "NFO", "35001", FeedMode::Quote),
    ]);
    let tick_of = |ev: &[FeedEvent]| -> Vec<(String, String)> {
        ev.iter()
            .filter_map(|e| match e {
                FeedEvent::Tick(t) => Some((t.symbol.clone(), t.exchange.clone())),
                _ => None,
            })
            .collect()
    };
    let ev = f.parse(&Message::Binary(frame(vec![mbp_packet(35001, pt::NSE_FO_ALL)])));
    assert_eq!(
        tick_of(&ev),
        [("NIFTY27OCT2625000CE".to_string(), "NFO".to_string())]
    );
    f.unsubscribe_frames(&[sub("NIFTY27OCT2625000CE", "NFO", "35001", FeedMode::Quote)]);
    let ev = f.parse(&Message::Binary(frame(vec![mbp_packet(35001, pt::NSE_FO_ALL)])));
    assert!(ev.is_empty(), "{:?}", ev);
    let ev = f.parse(&Message::Binary(frame(vec![mbp_packet(35001, pt::NSE_CM_ALL)])));
    assert_eq!(tick_of(&ev), [("SBIN".to_string(), "NSE".to_string())]);
    // An order or trade packet (types 8 and 9) carries no segment.
    for ptype in [8, 9, 42] {
        let ev = f.parse(&Message::Binary(frame(vec![mbp_packet(35001, ptype)])));
        assert!(ev.is_empty(), "type {}: {:?}", ptype, ev);
    }
}

#[test]
fn auth_error_text_stops_the_feed() {
    let mut f = HdfcSkyFeed::new(WS_URL, "K", "T");
    let ev = f.parse(&Message::Text(
        r#"{"status":"error","message":"Token expired"}"#.into(),
    ));
    assert!(matches!(ev.as_slice(), [FeedEvent::AuthFailed(_)]));
    assert!(f
        .parse(&Message::Text(r#"{"status":"success"}"#.into()))
        .is_empty());
    assert!(f.parse(&Message::Text("not json".into())).is_empty());
}

#[test]
fn packet_exchanges() {
    assert_eq!(packet_exchange(pt::NSE_FO_OI), Some("NFO"));
    assert_eq!(packet_exchange(pt::NSE_CD_ALL), Some("CDS"));
    assert_eq!(packet_exchange(pt::BSE_INDEX), Some("BSE_INDEX"));
    assert_eq!(packet_exchange(pt::MCX_PKT), Some("MCX"));
    assert_eq!(packet_exchange(pt::HEARTBEAT), None);
    assert_eq!(sub_type(FeedMode::Depth), "ALL");
    assert_eq!(feed_url("wss://x", "k", "t"), "wss://x?token=t&api_key=k");
}

#[test]
fn broker_identity() {
    let b = HdfcSkyBroker::new(master());
    assert_eq!(b.id(), "hdfcsky");
    assert_eq!(
        b.login_kind(),
        LoginKind::Redirect {
            param: "request_token"
        }
    );
    let c = b.capabilities();
    assert!(c.history && c.margin && c.streaming && !c.gtt && !c.order_feed);
    assert_eq!(b.supported_exchanges().len(), 8);
    assert!(b.create_feed(&AuthToken::new("KEY:abc.def.ghi")).is_ok());
    assert!(b.create_feed(&AuthToken::new("bad")).is_err());
}

// ---------------------------------------------------------------------------
// Secrets stay out of logged errors
// ---------------------------------------------------------------------------

const SENTINEL: &str = "SENTINEL-7f3a9c";

/// A loopback port with nothing listening (bound, then released).
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn transport_errors_lose_their_url_before_logging() {
    let url = format!(
        "http://127.0.0.1:{}/oapi/v1/orders?api_key={s}&token={s}",
        closed_port(),
        s = SENTINEL
    );
    let raw = crate::brokers::common::http::client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .unwrap_err();
    // Precondition: the unredacted error does carry the secret.
    assert!(format!("{} {:?}", raw, raw).contains(SENTINEL));
    let e = super::redact(crate::error::AppError::from(raw));
    let shown = format!("{} {:?} {} {}", e, e, e.code(), e.client_message());
    assert!(!shown.contains(SENTINEL), "{}", shown);
    // Non-transport errors pass through untouched.
    let other = super::redact(crate::error::AppError::Broker("kept".into()));
    assert_eq!(other.client_message(), "kept");
}

#[tokio::test]
async fn socket_errors_are_logged_by_kind_only() {
    use tokio_tungstenite::tungstenite::{error::UrlError, http, Error as E};
    let url = format!(
        "ws://127.0.0.1:{}/session?token={s}&api_key={s}",
        closed_port(),
        s = SENTINEL
    );
    let real = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap_err();
    let refused = http::Response::builder()
        .status(401)
        .body(Some(format!("bad token {}", SENTINEL).into_bytes()))
        .unwrap();
    let errors = vec![
        real,
        E::Http(refused),
        E::Io(std::io::Error::other(url.clone())),
        E::Url(UrlError::UnsupportedUrlScheme),
        E::ConnectionClosed,
    ];
    for e in &errors {
        let kind = super::streaming::ws_error_kind(e);
        assert!(!kind.contains(SENTINEL), "{}", kind);
        assert!(!kind.is_empty());
    }
    assert_eq!(
        super::streaming::ws_error_kind(&errors[1]),
        "refused with HTTP 401"
    );
}
