//! Unit tests for the Motilal Oswal mappings, with payloads built from the
//! web code and the Motilal API documentation samples (docs 17, 18, 21, 22,
//! 25, 26, 33, 34). No account data: ids are placeholders.

use super::data::{history_from_quote, index_quote_from_rows, quote_from_ltp_data};
use super::funds::funds_from_rows;
use super::mapping::*;
use super::master_contract::*;
use super::orders::*;
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Product, Validity};
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, Message};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/motilal/", $name))
    };
}

fn rows(text: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(text).unwrap();
    v["data"].as_array().cloned().unwrap()
}

fn sym(
    symbol: &str,
    exchange: &str,
    brexchange: &str,
    token: &str,
    lot: i32,
    tick: f64,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: brexchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: lot,
        instrument_type: "EQ".into(),
        tick_size: tick,
    }
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        sym("RELIANCE", "NSE", "NSE", "2885", 1, 0.1),
        sym("INFY", "NSE", "NSE", "1594", 1, 0.05),
        sym("RELAXO", "NSE", "NSE", "10838", 1, 0.05),
        sym("RELIANCE", "BSE", "BSE", "500325", 1, 0.05),
        sym("TATASTEEL", "BSE", "BSE", "500470", 1, 0.05),
        sym("NIFTY28OCT26FUT", "NFO", "NSEFO", "35001", 75, 0.1),
        sym("CRUDEOIL19OCT26FUT", "MCX", "MCX", "239484", 100, 1.0),
        sym("NIFTY", "NSE_INDEX", "NSE", "26000", 1, 0.05),
    ]);
    r
}

fn auth() -> AuthToken {
    AuthToken::new("AT1:::ACC1:::<USER_ID>:::KEY1:::SEC1")
}

// ---------------------------------------------------------------------------
// Session and login
// ---------------------------------------------------------------------------

#[test]
fn session_round_trips_and_redacts() {
    let s = MotilalSession::parse(&auth()).unwrap();
    assert_eq!(s.auth_token.expose(), "AT1");
    assert_eq!(s.access_token.as_ref().unwrap().expose(), "ACC1");
    assert_eq!(s.client_code, "<USER_ID>");
    assert_eq!(s.api_key.expose(), "KEY1");
    assert_eq!(s.api_secret.as_ref().unwrap().expose(), "SEC1");
    assert_eq!(s.compose(), "AT1:::ACC1:::<USER_ID>:::KEY1:::SEC1");
    let dbg = format!("{:?}", s);
    assert!(!dbg.contains("AT1") && !dbg.contains("KEY1"));

    // Optional parts may be empty; the client code falls back to user_id.
    let s =
        MotilalSession::parse(&AuthToken::new("AT2:::::::::KEY2:::").with_user_id("C9")).unwrap();
    assert!(s.access_token.is_none() && s.api_secret.is_none());
    assert_eq!(s.client_code, "C9");
    assert_eq!(s.compose(), "AT2::::::C9:::KEY2:::");

    for bad in ["", "AT", "AT:::ACC", ":::ACC:::C:::KEY", "AT:::ACC:::C:::"] {
        let e = MotilalSession::parse(&AuthToken::new(bad)).unwrap_err();
        assert!(e.client_message().contains("expired"), "{}", bad);
    }
}

#[test]
fn login_hash_and_body() {
    // sha256("Secret@1" + "APIKEY123"), checked with `shasum -a 256`.
    assert_eq!(
        auth::password_hash("Secret@1", "APIKEY123"),
        "8c2f8c3bf0c80649c38daebf55ff459ec42f8286d72e179c56cbb5aaad1b35ee"
    );
    let b = auth::login_body(
        "<USER_ID>",
        "Secret@1",
        "APIKEY123",
        "18/10/1988",
        " 123456 ",
    );
    assert_eq!(b["userid"], "<USER_ID>");
    assert_eq!(b["2FA"], "18/10/1988");
    assert_eq!(b["totp"], "123456");
    assert_eq!(b["password"].as_str().unwrap().len(), 64);
    // Blank TOTP is omitted (OTP path), as on the web.
    let b = auth::login_body("U", "p", "k", "01/01/2000", "");
    assert!(b.get("totp").is_none());
}

#[test]
fn unverified_token_gate_fails_open() {
    for v in ["FALSE", "false", "0", "NO", "n"] {
        assert!(
            auth::is_unverified(&json!({"isAuthTokenVerified": v})),
            "{}",
            v
        );
    }
    assert!(auth::is_unverified(&json!({"isAuthTokenVerified": false})));
    for v in [json!("TRUE"), json!(""), json!("maybe"), json!(true)] {
        assert!(!auth::is_unverified(&json!({"isAuthTokenVerified": v})));
    }
    assert!(!auth::is_unverified(&json!({})));
}

#[test]
fn error_envelopes_map_to_trader_messages() {
    use reqwest::StatusCode;
    let e = motilal_error(
        StatusCode::OK,
        &json!({"status":"FAILURE","message":"Invalid Token","errorcode":"MO8001"}),
        "x",
    );
    assert!(matches!(e, AppError::Auth(_)));
    let e = motilal_error(
        StatusCode::OK,
        &json!({"status":"FAILURE","message":"Insufficient funds","errorcode":"MO5001"}),
        "x",
    );
    assert_eq!(e.client_message(), "Motilal Oswal: Insufficient funds");
    let e = motilal_error(
        StatusCode::OK,
        &json!({"status":"FAILURE","errorcode":"MO2035"}),
        "x",
    );
    assert!(e.client_message().contains("static IP"));
    let e = motilal_error(StatusCode::OK, &json!({"status":"FAILURE"}), "fallback");
    assert_eq!(e.client_message(), "fallback");
    assert!(is_success(&json!({"status":"SUCCESS"})));
    assert!(!is_success(&json!({"status":"FAILURE"})));
}

// ---------------------------------------------------------------------------
// Vocabulary (transform_data.py)
// ---------------------------------------------------------------------------

#[test]
fn exchange_maps() {
    for (oa, mo) in [
        ("NSE", "NSE"),
        ("BSE", "BSE"),
        ("NFO", "NSEFO"),
        ("CDS", "NSECD"),
        ("MCX", "MCX"),
        ("BFO", "BSEFO"),
    ] {
        assert_eq!(map_exchange(oa), mo);
        assert_eq!(reverse_map_exchange(mo), oa);
    }
}

#[test]
fn product_maps_are_exchange_aware() {
    assert_eq!(map_product_type("CNC", "NSE"), "DELIVERY");
    assert_eq!(map_product_type("MIS", "BSE"), "VALUEPLUS");
    assert_eq!(map_product_type("NRML", "NSE"), "NORMAL");
    for ex in ["NFO", "MCX", "CDS", "BFO"] {
        for p in ["CNC", "MIS", "NRML"] {
            assert_eq!(map_product_type(p, ex), "NORMAL", "{} {}", p, ex);
        }
    }
    assert_eq!(map_product_type("XYZ", "NSE"), "VALUEPLUS");
    assert_eq!(reverse_map_product_type("DELIVERY"), "CNC");
    assert_eq!(reverse_map_product_type("valueplus"), "MIS");
    assert_eq!(reverse_map_product_type("NORMAL"), "NRML");
    assert_eq!(reverse_map_product_type("SELLFROMDP"), "CNC");
    assert_eq!(reverse_map_product_type("BTST"), "CNC");
    assert_eq!(reverse_map_product_type("MTF"), "NRML");
    assert_eq!(reverse_map_product_type("WHAT"), "MIS");
}

#[test]
fn order_types_and_statuses() {
    assert_eq!(map_order_type(PriceType::Market), "MARKET");
    assert_eq!(map_order_type(PriceType::Limit), "LIMIT");
    assert_eq!(map_order_type(PriceType::Sl), "STOPLOSS");
    assert_eq!(map_order_type(PriceType::SlM), "STOPLOSS");
    assert_eq!(oa_pricetype("Stoploss", 10.0), "SL");
    assert_eq!(oa_pricetype("STOPLOSS", 0.0), "SL-M");
    assert_eq!(oa_pricetype("Market", 0.0), "MARKET");
    for (raw, oa) in [
        ("Traded", "complete"),
        ("COMPLETE", "complete"),
        ("Sent", "open"),
        ("Confirm", "open"),
        ("Partial", "open"),
        ("Unknown", "open"),
        ("Rejected", "rejected"),
        ("Error", "rejected"),
        ("Cancel", "cancelled"),
        ("CANCELLED", "cancelled"),
        ("weird", "open"),
    ] {
        assert_eq!(map_order_status(raw), oa, "{}", raw);
    }
    assert_eq!(segment("NFO"), "DERIVATIVES");
    assert_eq!(segment("NSE"), "CASH");
    assert_eq!(segment("NSE_INDEX"), "CASH");
}

#[test]
fn precision_scaling_follows_the_web() {
    assert_eq!(precision(&json!({}), None), None);
    assert_eq!(precision(&json!({"precision": null}), Some(2)), Some(2));
    assert_eq!(precision(&json!({"precision": 4}), None), Some(4));
    assert_eq!(precision(&json!({"precision": "2"}), None), Some(2));
    assert_eq!(precision(&json!({"precision": "x"}), None), Some(2));
    assert_eq!(precision(&json!({"precision": 12}), None), Some(2));
    assert_eq!(precision(&json!({"precision": -1}), None), Some(2));
    assert_eq!(scale(278400.0, Some(2)), 2784.0);
    assert_eq!(scale(394305.0, None), 394305.0);
}

// ---------------------------------------------------------------------------
// Books (order_data.py)
// ---------------------------------------------------------------------------

#[test]
fn order_book_rows_are_normalised() {
    let o = map_orders(&rows(fixture!("order_book.json")), &master());
    assert_eq!(o.len(), 5);
    let a = &o[0];
    assert_eq!(
        (a.symbol.as_str(), a.exchange.as_str(), a.product.as_str()),
        ("RELIANCE", "NSE", "CNC")
    );
    assert_eq!((a.side.as_str(), a.quantity, a.price), ("BUY", 10, 2400.5));
    assert_eq!(
        (a.order_type.as_str(), a.status.as_str()),
        ("LIMIT", "open")
    );
    // lastmodifiedtime "0" falls back to entrydatetime.
    assert_eq!(a.order_timestamp, "03-Oct-2026 09:20:01");
    assert_eq!(a.order_id, "1300000000000001");

    let b = &o[1];
    assert_eq!(b.symbol, "NIFTY28OCT26FUT");
    assert_eq!(b.exchange, "NFO");
    assert_eq!(b.product, "NRML");
    assert_eq!(b.order_type, "SL");
    assert_eq!(b.trigger_price, 102.0);

    let c = &o[2];
    assert_eq!(
        (c.symbol.as_str(), c.exchange.as_str()),
        ("RELIANCE", "BSE")
    );
    assert_eq!(c.product, "MIS");
    assert_eq!(c.status, "complete");
    // Executed: the average price is shown.
    assert_eq!(c.price, 2401.25);
    assert_eq!(c.order_type, "MARKET");

    let d = &o[3];
    assert_eq!(d.symbol, "CRUDEOIL19OCT26FUT");
    assert_eq!(d.status, "rejected");
    assert_eq!(d.rejection_reason.as_deref(), Some("RMS: margin exceeds"));
    assert_eq!(d.order_timestamp, "03-Oct-2026 11:00:00");

    // A row that publishes precision is scaled.
    let e = &o[4];
    assert_eq!(
        (e.symbol.as_str(), e.price, e.status.as_str()),
        ("INFY", 1500.0, "cancelled")
    );

    // Unknown scrip keeps the broker symbol.
    let o = map_orders(&rows(fixture!("order_book.json")), &SymbolResolver::new());
    assert_eq!(o[1].symbol, "NIFTY");
}

#[test]
fn trade_book_scales_by_precision_default_two() {
    let t = map_trades(&rows(fixture!("trade_book.json")), &master());
    assert_eq!(t.len(), 3);
    // doc 18: 278400 x 20 = 5568000 with precision 2 -> 2784.00.
    assert_eq!(
        (t[0].average_price, t[0].trade_value, t[0].quantity),
        (2784.0, 55680.0, 20)
    );
    assert_eq!(
        (t[0].symbol.as_str(), t[0].product.as_str()),
        ("RELIANCE", "CNC")
    );
    assert_eq!(t[0].trade_id, "50001");
    // precision 0 and absent both mean 2.
    assert_eq!(t[1].average_price, 101.5);
    assert_eq!(t[1].symbol, "NIFTY28OCT26FUT");
    assert_eq!(t[1].exchange, "NFO");
    assert_eq!(t[2].average_price, 2401.25);
    assert_eq!(t[2].side, "BUY");
}

#[test]
fn positions_net_average_and_pnl() {
    let p = map_positions(&rows(fixture!("positions.json")), &master());
    assert_eq!(p.len(), 3);
    assert_eq!(
        (p[0].symbol.as_str(), p[0].product.as_str(), p[0].quantity),
        ("RELIANCE", "MIS", 10)
    );
    assert_eq!(p[0].average_price, 2400.5);
    assert_eq!((p[0].ltp, p[0].pnl), (2410.0, 95.0));
    assert_eq!(p[1].quantity, -75);
    assert_eq!(p[1].average_price, 101.5);
    assert_eq!(p[1].pnl, 92.5);
    assert_eq!((p[1].realized_pnl, p[1].unrealized_pnl), (-20.0, 112.5));
    assert_eq!(p[1].exchange, "NFO");
    assert_eq!((p[2].quantity, p[2].average_price), (0, 0.0));
}

#[test]
fn holdings_pick_exchange_from_tokens() {
    let h = map_holdings(&rows(fixture!("holdings.json")), &master());
    assert_eq!(h.len(), 3);
    assert_eq!(
        (h[0].symbol.as_str(), h[0].exchange.as_str()),
        ("RELAXO", "NSE")
    );
    assert_eq!(
        (h[0].quantity, h[0].average_price, h[0].pnl),
        (10, 850.5, 0.0)
    );
    assert_eq!(
        (h[1].symbol.as_str(), h[1].exchange.as_str()),
        ("TATASTEEL", "BSE")
    );
    assert_eq!((h[1].pnl, h[1].pnl_percentage), (120.0, 25.0));
    assert_eq!(
        (h[2].symbol.as_str(), h[2].exchange.as_str()),
        ("UNLISTED XYZ", "NSE")
    );
    assert!(h.iter().all(|x| x.product == "CNC"));
}

#[test]
fn funds_follow_srno_rows() {
    let f = funds_from_rows(&rows(fixture!("margin_detail.json")));
    assert_eq!(f.available_cash, 50000000.0);
    assert_eq!(f.collateral, 474919.06);
    assert_eq!(f.utilised_debits, 622432.4);
    assert_eq!(f.m2m_unrealized, -19500.0);
    assert_eq!(f.m2m_realized, 910.0);

    // Without srno 201: invert the net identity from srno 102.
    let r = vec![
        json!({"srno": 102, "amount": 49833896.66}),
        json!({"srno": 220, "amount": 474919.06}),
        json!({"srno": 300, "amount": 622432.4}),
        json!({"srno": 600, "amount": -18590}),
    ];
    let f = funds_from_rows(&r);
    assert_eq!(f.available_cash, 50000000.0);
    // No split rows: the combined MTM is reported as unrealised.
    assert_eq!(f.m2m_unrealized, -18590.0);

    // Without srno 300: per-segment usage headers are summed.
    let r = vec![
        json!({"srno": "301", "amount": "100.5"}),
        json!({"srno": 321, "amount": 200}),
        json!({"srno": 302, "amount": 999}),
    ];
    assert_eq!(funds_from_rows(&r).utilised_debits, 300.5);
    assert_eq!(funds_from_rows(&[]), Funds::default());
}

// ---------------------------------------------------------------------------
// Quotes and history (data.py)
// ---------------------------------------------------------------------------

#[test]
fn ltp_quote_is_paise() {
    let v: Value = serde_json::from_str(fixture!("ltp.json")).unwrap();
    let q = quote_from_ltp_data(&v["data"], &QuoteKey::new("NSE", "RELIANCE"));
    assert_eq!((q.ltp, q.bid, q.ask), (3224.0, 3230.0, 3239.0));
    assert_eq!(
        (q.open, q.high, q.low, q.close),
        (3200.0, 3250.5, 3190.0, 3210.0)
    );
    assert_eq!((q.volume, q.oi), (75937, 0));
    assert_eq!((q.change, q.change_percent), (14.0, 0.44));
}

#[test]
fn index_quote_rows_are_rupees() {
    let rows = vec![
        json!({"scripcode": "26009", "ltp": 51000.5}),
        json!({"scripcode": 26000, "open": 17451.25, "high": 17500, "low": 17400, "close": 17450, "ltp": 17480.5}),
    ];
    let k = QuoteKey::new("NSE_INDEX", "NIFTY");
    let q = index_quote_from_rows(&rows, "26000", &k).unwrap();
    assert_eq!(
        (q.ltp, q.open, q.close, q.volume),
        (17480.5, 17451.25, 17450.0, 0)
    );
    // No match: the first row, as on the web.
    assert_eq!(index_quote_from_rows(&rows, "1", &k).unwrap().ltp, 51000.5);
    assert!(index_quote_from_rows(&[], "1", &k).is_none());
}

#[test]
fn todays_bar_from_quote() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
    let q = Quote {
        ltp: 105.0,
        open: 100.0,
        high: 104.0,
        low: 0.0,
        volume: 7,
        ..Default::default()
    };
    let c = history_from_quote(&q, today).unwrap();
    assert_eq!(c.timestamp, 1790985600); // 2026-10-03T00:00:00Z
    assert_eq!(
        (c.open, c.high, c.low, c.close, c.volume),
        (100.0, 105.0, 100.0, 105.0, 7)
    );
    let pre = Quote::default();
    assert!(history_from_quote(&pre, today).is_none());
    let only_ltp = Quote {
        ltp: 50.0,
        ..Default::default()
    };
    let c = history_from_quote(&only_ltp, today).unwrap();
    assert_eq!((c.open, c.low, c.high), (50.0, 50.0, 50.0));
}

// ---------------------------------------------------------------------------
// Orders (order_api.py)
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: PriceType, qty: i64) -> ResolvedOrder {
    let m = master();
    let instrument = m.by_symbol(exchange, symbol).unwrap();
    ResolvedOrder {
        symbol: symbol.into(),
        exchange: exchange.parse().unwrap(),
        action: Action::Buy,
        quantity: qty,
        price: 0.0,
        trigger_price: 0.0,
        pricetype,
        product: Product::Mis,
        validity: Validity::Day,
        disclosed_quantity: 0,
        amo: false,
        instrument,
    }
}

#[test]
fn lots_conversion_refuses_bad_quantities() {
    assert_eq!(quantity_in_lots("N", "NFO", 150, 75).unwrap(), 2);
    assert_eq!(quantity_in_lots("S", "NSE", 7, 0).unwrap(), 7);
    let e = quantity_in_lots("N", "NFO", 75, 0).unwrap_err();
    assert!(e.client_message().contains("lot size"));
    let e = quantity_in_lots("N", "NFO", 100, 75).unwrap_err();
    assert!(e
        .client_message()
        .contains("not a multiple of the lot size 75"));
}

#[test]
fn market_orders_get_price_protection() {
    let o = resolved("RELIANCE", "NSE", PriceType::Market, 10);
    // The web slab reads the symbol suffix: RELIANCE ends in CE, so the
    // option slab (1 % above 500) applies, rounded to the 0.10 tick.
    assert_eq!(protected_order(&o, Some(2400.0)), ("LIMIT", 2424.0));
    assert_eq!(protected_order(&o, None), ("MARKET", 0.0));
    assert_eq!(protected_order(&o, Some(0.0)), ("MARKET", 0.0));

    let mut sl = resolved("RELIANCE", "NSE", PriceType::SlM, 10);
    sl.trigger_price = 2400.0;
    sl.action = Action::Sell;
    assert_eq!(protected_order(&sl, None), ("STOPLOSS", 2376.0));
    sl.trigger_price = 0.0;
    assert_eq!(protected_order(&sl, None).0, "STOPLOSS");

    let mut lim = resolved("RELIANCE", "NSE", PriceType::Limit, 10);
    lim.price = 2399.5;
    assert_eq!(protected_order(&lim, None), ("LIMIT", 2399.5));
}

#[test]
fn place_and_modify_bodies() {
    let mut o = resolved("NIFTY28OCT26FUT", "NFO", PriceType::Limit, 150);
    o.price = 25000.5;
    o.product = Product::Mis;
    let b = place_order_body(&o, "LIMIT", 25000.5, 2);
    assert_eq!(
        b,
        json!({
            "exchange": "NSEFO", "symboltoken": 35001, "buyorsell": "BUY",
            "ordertype": "LIMIT", "producttype": "NORMAL", "orderduration": "DAY",
            "price": 25000.5, "triggerprice": 0.0, "quantityinlot": 2,
            "disclosedquantity": 0, "amoorder": "N"
        })
    );
    let m = master();
    let r = ResolvedModify {
        order_id: "13001".into(),
        symbol: "NIFTY28OCT26FUT".into(),
        exchange: Exchange::Nfo,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Sl,
        quantity: 75,
        price: 101.0,
        trigger_price: 100.0,
        disclosed_quantity: 0,
        instrument: m.by_symbol("NFO", "NIFTY28OCT26FUT").unwrap(),
    };
    let b = modify_order_body(&r, 1, "03-Oct-2026 09:20:00", 0);
    assert_eq!(b["newordertype"], "STOPLOSS");
    assert_eq!(b["newquantityinlot"], 1);
    assert_eq!(b["lastmodifiedtime"], "03-Oct-2026 09:20:00");
    assert_eq!(b["neworderduration"], "DAY");
    assert_eq!(b["newgoodtilldate"], "");
    assert_eq!(b["uniqueorderid"], "13001");
}

#[test]
fn modify_time_fallbacks_and_cancellable_statuses() {
    assert_eq!(last_modified_time(&json!({"lastmodifiedtime": "a"})), "a");
    assert_eq!(
        last_modified_time(
            &json!({"lastmodifiedtime": "0", "recordinserttime": "r", "entrydatetime": "e"})
        ),
        "r"
    );
    assert_eq!(
        last_modified_time(&json!({"lastmodifiedtime": "0", "entrydatetime": "e"})),
        "e"
    );
    assert_eq!(last_modified_time(&json!({"lastmodifiedtime": "0"})), "0");
    for s in ["Confirm", "SENT", "open", "Partial"] {
        assert!(is_cancellable(&json!({"orderstatus": s})), "{}", s);
    }
    for s in ["Traded", "Cancel", "Rejected", "Unknown"] {
        assert!(!is_cancellable(&json!({"orderstatus": s})), "{}", s);
    }
}

// ---------------------------------------------------------------------------
// Master contract (master_contract_db.py)
// ---------------------------------------------------------------------------

fn find<'a>(rows: &'a [SymToken], symbol: &str, exchange: &str) -> &'a SymToken {
    rows.iter()
        .find(|r| r.symbol == symbol && r.exchange == exchange)
        .unwrap_or_else(|| panic!("{} {} missing", symbol, exchange))
}

#[test]
fn expiry_comes_from_the_scrip_name() {
    assert_eq!(
        expiry_from_scripname("TGBL 30-OCT-2025 CE 1180"),
        "30-OCT-25"
    );
    assert_eq!(expiry_from_scripname("NIFTY 28-Oct-2026 FUT"), "28-OCT-26");
    assert_eq!(expiry_from_scripname("INFY EQ"), "");
    assert_eq!(expiry_from_scripname("X 31-Feb-2026 FUT"), "");
    assert_eq!(expiry_from_scripname("X 1-Jan-2027 FUT"), "01-JAN-27");
}

#[test]
fn cash_masters_strip_series_and_prefer_eq() {
    let nse = parse_scrip_csv(fixture!("scrip_nse.csv"), "NSE");
    let infy = find(&nse, "INFY", "NSE");
    assert_eq!(
        (infy.brsymbol.as_str(), infy.token.as_str()),
        ("INFY EQ", "1594")
    );
    assert_eq!(
        (infy.instrument_type.as_str(), infy.strike, infy.lot_size),
        ("EQ", 0.0, 1)
    );
    // EQ wins over the temporary D1 series of the same scrip.
    let chola: Vec<_> = nse.iter().filter(|r| r.symbol == "CHOLAFIN").collect();
    assert_eq!(chola.len(), 1);
    assert_eq!(chola[0].token, "685");
    // No short name: the series is stripped from the scrip name.
    assert_eq!(find(&nse, "745AP33", "NSE").tick_size, 0.01);
    // IDX rows become NSE_INDEX and are normalised.
    let n = find(&nse, "NIFTY", "NSE_INDEX");
    assert_eq!((n.token.as_str(), n.brexchange.as_str()), ("26000", "NSE"));

    let bse = parse_scrip_csv(fixture!("scrip_bse.csv"), "BSE");
    assert_eq!(find(&bse, "RELIANCE", "BSE").token, "500325");
}

#[test]
fn derivative_masters_build_openalgo_symbols() {
    let fo = parse_scrip_csv(fixture!("scrip_nsefo.csv"), "NSEFO");
    let fut = find(&fo, "NIFTY28OCT26FUT", "NFO");
    assert_eq!(
        (
            fut.expiry.as_str(),
            fut.instrument_type.as_str(),
            fut.strike,
            fut.lot_size
        ),
        ("28-OCT-26", "FUT", 0.0, 75)
    );
    assert_eq!(
        (fut.brexchange.as_str(), fut.brsymbol.as_str()),
        ("NSEFO", "NIFTY 28-Oct-2026 FUT")
    );
    let ce = find(&fo, "NIFTY28OCT2625000CE", "NFO");
    assert_eq!((ce.instrument_type.as_str(), ce.strike), ("CE", 25000.0));
    let pe = find(&fo, "TGBL30OCT251180.5PE", "NFO");
    assert_eq!(pe.expiry, "30-OCT-25");
    // A derivative without an expiry keeps the raw name rather than colliding.
    assert_eq!(find(&fo, "BROKEN NOEXPIRY CE", "NFO").expiry, "");

    let cd = parse_scrip_csv(fixture!("scrip_nsecd.csv"), "NSECD");
    let usd = find(&cd, "USDINR23OCT26FUT", "CDS");
    assert_eq!(usd.tick_size, 0.05);
    find(&cd, "GBPUSD27AUG261.4CE", "CDS");

    let mcx = parse_scrip_csv(fixture!("scrip_mcx.csv"), "MCX");
    assert_eq!(find(&mcx, "CRUDEOIL19OCT26FUT", "MCX").lot_size, 100);
    let und = find(&mcx, "CRUDEOIL", "MCX");
    assert_eq!((und.instrument_type.as_str(), und.strike), ("COM", 0.0));

    let bfo = parse_scrip_csv(fixture!("scrip_bsefo.csv"), "BSEFO");
    find(&bfo, "SENSEX03SEP2684200PE", "BFO");
    find(&bfo, "BANKEX25SEP26FUT", "BFO");
    // BIT stays unmapped on purpose.
    find(&bfo, "BIT25SEP26FUT", "BFO");
}

#[test]
fn index_masters_and_aliases() {
    let nse = parse_index_csv(fixture!("index_nse.csv"), "NSE");
    let syms: Vec<&str> = nse.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["NIFTY", "BANKNIFTY", "INDIAVIX", "NIFTYMIDCAP150"]);
    assert!(nse
        .iter()
        .all(|r| r.exchange == "NSE_INDEX" && r.instrument_type == "INDEX"));
    assert_eq!(
        (nse[0].brsymbol.as_str(), nse[0].lot_size, nse[0].tick_size),
        ("Nifty 50", 1, 0.05)
    );
    let bse = parse_index_csv(fixture!("index_bse.csv"), "BSE");
    let syms: Vec<&str> = bse.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(
        syms,
        ["SENSEX", "BANKEX", "SENSEX50", "BSECAPITALGOODS", "BSE1000"]
    );
    assert_eq!(normalize_nse_index("Nifty Fin Service"), "FINNIFTY");
    assert_eq!(normalize_bse_index("MIDSEL"), "BSEMIDCAPSELECTINDEX");

    // Final dedupe: the scrip-master NIFTY row wins over the index master.
    let mut all = parse_scrip_csv(fixture!("scrip_nse.csv"), "NSE");
    all.extend(nse);
    let d = dedupe(all);
    assert_eq!(d.iter().filter(|r| r.symbol == "NIFTY").count(), 1);
}

// ---------------------------------------------------------------------------
// Broadcast feed (motilal_websocket.py)
// ---------------------------------------------------------------------------

/// One 30-byte packet: header (exchange char, i32 scrip, i32 time, type)
/// then a 20-byte body.
pub(crate) fn pkt(ex: u8, scrip: i32, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![ex];
    p.extend_from_slice(&scrip.to_le_bytes());
    p.extend_from_slice(&1000i32.to_le_bytes());
    p.push(kind);
    let mut b = body.to_vec();
    b.resize(20, 0);
    p.extend(b);
    p
}

fn f(v: f32) -> [u8; 4] {
    v.to_le_bytes()
}

fn i(v: i32) -> [u8; 4] {
    v.to_le_bytes()
}

#[test]
fn login_packet_layout() {
    let p = login_packet("AB1234");
    assert_eq!(p.len(), LOGIN_LEN);
    assert_eq!(p[0], b'Q');
    assert_eq!(u16::from_le_bytes([p[1], p[2]]), 111);
    assert_eq!(p[3], 6);
    assert_eq!(&p[4..19], b"AB1234         ");
    assert_eq!(p[19], 6);
    assert_eq!(&p[20..50], b"AB1234                        ");
    assert_eq!(&p[50..53], &[1, 1, 1]);
    assert_eq!(p[53], 5);
    assert_eq!(&p[54..64], b"1.0.0     ");
    assert_eq!(&p[64..69], &[0, 0, 0, 0, 1]);
    assert!(p[69..].iter().all(|b| *b == b' '));
}

#[test]
fn register_packet_layout() {
    let p = register_packet("NSEFO", "DERIVATIVES", 35001, true);
    assert_eq!(p.len(), REGISTER_LEN);
    assert_eq!(p[0], b'D');
    assert_eq!(u16::from_le_bytes([p[1], p[2]]), 7);
    assert_eq!((p[3], p[4]), (b'N', b'D'));
    assert_eq!(i32::from_le_bytes([p[5], p[6], p[7], p[8]]), 35001);
    assert_eq!(p[9], 1);
    let p = register_packet("BSEFO", "CASH", 7, false);
    assert_eq!((p[3], p[4], p[9]), (b'G', b'C', 0));
    assert_eq!(exchange_char("NSECD"), b'C');
    assert_eq!(exchange_char("MCX"), b'M');
    assert_eq!(exchange_char("NCDEX"), b'D');
    let t: Value = serde_json::from_str(&index_frame("C1", "NSE", true)).unwrap();
    assert_eq!(
        t,
        json!({"clientid":"C1","action":"IndexRegister","exchange":"NSE"})
    );
}

fn feed_frame() -> Vec<u8> {
    let mut frame = Vec::new();
    // A LTP: rate, last qty, cumulative volume, average, OI (:651-690).
    frame.extend(pkt(
        b'N',
        2885,
        b'A',
        &[f(2410.5), i(4), i(75937), f(2405.25), i(0)].concat(),
    ));
    // G day OHLC: open, high, low, previous close (:717-742).
    frame.extend(pkt(
        b'N',
        2885,
        b'G',
        &[f(2400.0), f(2420.0), f(2390.0), f(2398.0)].concat(),
    ));
    // B depth level 1: bid, bid qty, bid orders, ask, ask qty, ask orders (:595-649).
    let mut d = Vec::new();
    d.extend(f(2410.0));
    d.extend(i(100));
    d.extend(3i16.to_le_bytes());
    d.extend(f(2411.0));
    d.extend(i(50));
    d.extend(2i16.to_le_bytes());
    frame.extend(pkt(b'N', 2885, b'B', &d));
    // m OI (:744-772) and W circuits (:692-715).
    frame.extend(pkt(b'N', 2885, b'm', &[i(1200), i(1300), i(1100)].concat()));
    frame.extend(pkt(b'N', 2885, b'W', &[f(2600.0), f(2200.0)].concat()));
    // Heartbeat and an unknown type are skipped; trailing junk ignored.
    frame.extend(pkt(b'N', 2885, b'1', &[]));
    frame.extend(pkt(b'N', 2885, b'z', &[]));
    frame.extend([1, 2, 3]);
    frame
}

#[test]
fn packets_decode_with_cited_offsets() {
    let p = packets(&feed_frame());
    assert_eq!(p.len(), 7);
    assert_eq!(
        (p[0].exchange, p[0].scrip, p[0].time, p[0].kind),
        (b'N', 2885, 1000, b'A')
    );
    let mut st = FeedState::default();
    st.register("NSE", 2885);
    let changed = st.apply(&feed_frame());
    assert_eq!(changed.len(), 5);
    let d = st.get("NSE", 2885).unwrap();
    assert_eq!(
        (d.ltp, d.ltq, d.volume, d.avg_price),
        (2410.5, 4, 75937, 2405.25)
    );
    assert_eq!(
        (d.open, d.high, d.low, d.prev_close),
        (2400.0, 2420.0, 2390.0, 2398.0)
    );
    assert_eq!(
        (d.oi, d.upper_circuit, d.lower_circuit),
        (1200, 2600.0, 2200.0)
    );
    let b = d.bids[0].unwrap();
    let a = d.asks[0].unwrap();
    assert_eq!((b.price, b.quantity, b.orders), (2410.0, 100, 3));
    assert_eq!((a.price, a.quantity, a.orders), (2411.0, 50, 2));
    assert!(d.complete(true, false));
    assert!(!d.complete(true, true));
    assert!(st.authenticated());

    // Unregistered scrips are ignored by default, and unregistering drops data.
    let mut st2 = FeedState::default();
    assert!(st2.apply(&feed_frame()).is_empty());
    st.unregister("NSE", 2885);
    assert!(st.is_empty());
    assert_eq!(st.registrations(), 0);
}

#[test]
fn nse_and_nsefo_share_the_wire_character() {
    let mut st = FeedState::default();
    assert!(st.register("NSEFO", 35001));
    // Same numeric token on NSE cash: first registration wins.
    assert!(!st.register("NSE", 35001));
    st.apply(&pkt(b'N', 35001, b'A', &f(101.5)));
    assert_eq!(st.get("NSEFO", 35001).unwrap().ltp, 101.5);
    assert!(st.get("NSE", 35001).is_none());
}

#[test]
fn index_packets_on_a_quote_socket() {
    let mut st = FeedState::new(true);
    st.apply(&pkt(b'N', 26000, b'H', &f(25012.75)));
    let d = st.get("NSE", 26000).unwrap();
    assert_eq!((d.index_rate, d.ltp), (Some(25012.75), 25012.75));
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

#[test]
fn live_feed_frames_and_events() {
    let mut feed = MotilalFeed::new("wss://example.invalid/feed", "C1");
    let open = feed.on_connected();
    assert!(matches!(&open[0], Message::Binary(b) if b.len() == LOGIN_LEN));
    assert!(feed.awaits_auth_ack());
    let frames = feed.subscribe_frames(&[
        sub("RELIANCE", "NSE", "2885", FeedMode::Depth),
        sub("NIFTY28OCT26FUT", "NFO", "35001", FeedMode::Ltp),
        sub("BAD", "NSE", "x", FeedMode::Ltp),
    ]);
    assert_eq!(frames.len(), 2);
    assert!(matches!(&frames[1], Message::Binary(b) if b[3] == b'N' && b[4] == b'D'));
    assert_eq!(feed.registered(), 2);

    let ev = feed.parse(&Message::Binary(feed_frame()));
    assert_eq!(ev[0], FeedEvent::AuthOk);
    let ticks: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t),
            _ => None,
        })
        .collect();
    let depths: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Depth(d) => Some(d),
            _ => None,
        })
        .collect();
    assert!(!ticks.is_empty() && !depths.is_empty());
    let last = ticks.last().unwrap();
    assert_eq!(
        (last.symbol.as_str(), last.exchange.as_str(), last.mode),
        ("RELIANCE", "NSE", 3)
    );
    assert_eq!(
        (last.ltp, last.open, last.close, last.oi),
        (2410.5, 2400.0, 2398.0, 1200)
    );
    assert_eq!(last.change, 12.5);
    let d = depths.last().unwrap();
    assert_eq!((d.buy.len(), d.sell.len(), d.buy[0].price), (5, 5, 2410.0));

    // A second frame does not announce the session again.
    let ev = feed.parse(&Message::Binary(pkt(b'N', 35001, b'A', &f(101.0))));
    assert!(matches!(&ev[0], FeedEvent::Tick(t) if t.mode == 1 && t.ltp == 101.0 && t.open == 0.0));

    // Mode change is local; unsubscribe drops state.
    let old = sub("NIFTY28OCT26FUT", "NFO", "35001", FeedMode::Ltp);
    let new = sub("NIFTY28OCT26FUT", "NFO", "35001", FeedMode::Quote);
    assert!(feed.mode_change_frames(&old, &new).is_empty());
    let un = feed.unsubscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Depth), new]);
    assert!(matches!(&un[0], Message::Binary(b) if b[9] == 0));
    assert_eq!((feed.registered(), feed.cached()), (0, 0));
}

#[test]
fn index_subscriptions_register_on_the_real_exchange() {
    assert_eq!(feed_exchange("NSE_INDEX"), "NSE");
    assert_eq!(feed_exchange("BSE_INDEX"), "BSE");
    assert_eq!(feed_exchange("NFO"), "NSEFO");
    let mut feed = MotilalFeed::new("wss://example.invalid/feed", "C1");
    let f0 = feed.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Ltp)]);
    assert!(matches!(&f0[0], Message::Binary(b) if b[3] == b'N' && b[4] == b'C'));
    let ev = feed.parse(&Message::Binary(pkt(b'N', 26000, b'H', &f(25000.0))));
    assert!(ev
        .iter()
        .any(|e| matches!(e, FeedEvent::Tick(t) if t.symbol == "NIFTY" && t.ltp == 25000.0)));
}

// ---------------------------------------------------------------------------
// Order stream (motilal_order_adapter.py)
// ---------------------------------------------------------------------------

#[test]
fn order_stream_frames_and_updates() {
    let s = MotilalSession::parse(&auth()).unwrap();
    let mut f = MotilalOrderFeed::new("wss://example.invalid/ws", &s, master());
    let open = f.on_connected();
    let first: Value = match &open[0] {
        Message::Text(t) => serde_json::from_str(t).unwrap(),
        _ => panic!(),
    };
    assert_eq!(
        first,
        json!({"clientid":"<USER_ID>","authtoken":"AT1","apikey":"KEY1"})
    );
    assert!(matches!(&open[1], Message::Text(t) if t.contains("OrderSubscribe")));
    let (every, hb) = f.heartbeat().unwrap();
    assert_eq!(every.as_secs(), 30);
    assert!(matches!(hb, Message::Text(t) if t.contains("heartbeat")));

    let frame = json!({
        "uniqueorderid": "1300000000000002", "exchange": "NSEFO", "symboltoken": 35001,
        "symbol": "NIFTY 28-Oct-2026 FUT", "buyorsell": "sell", "ordertype": "STOPLOSS",
        "producttype": "NORMAL", "price": 0, "triggerprice": 102, "orderqty": 75,
        "qtytradedtoday": 25, "averageprice": 101.95, "orderstatus": "PARTIAL"
    });
    let ev = f.parse(&Message::Text(frame.to_string()));
    let FeedEvent::OrderUpdate(u) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (u.symbol.as_str(), u.exchange.as_str()),
        ("NIFTY28OCT26FUT", "NFO")
    );
    assert_eq!(
        (u.action.as_str(), u.pricetype.as_str(), u.product.as_str()),
        ("SELL", "SL-M", "NRML")
    );
    assert_eq!(
        (
            u.order_status.as_str(),
            u.filled_quantity,
            u.pending_quantity
        ),
        ("open", 25, 50)
    );
    assert_eq!(u.average_price, 101.95);

    let rej = json!({"uniqueorderid":"9","exchange":"NSE","orderstatus":"Rejected","error":"RMS"});
    let ev = f.parse(&Message::Text(rej.to_string()));
    assert!(
        matches!(&ev[0], FeedEvent::OrderUpdate(u) if u.order_status == "rejected" && u.rejection_reason == "RMS")
    );
    // Trade frames, errors and acks produce nothing; MO1001 stops the feed.
    assert!(f
        .parse(&Message::Text(
            json!({"tradeno":"1","uniqueorderid":"9"}).to_string()
        ))
        .is_empty());
    assert!(f
        .parse(&Message::Text(json!({"status":"ok"}).to_string()))
        .is_empty());
    let ev = f.parse(&Message::Text(
        json!({"errorcode":"MO1001","message":"x"}).to_string(),
    ));
    assert!(matches!(&ev[0], FeedEvent::AuthFailed(_)));
}

#[test]
fn broker_identity() {
    let b = MotilalBroker::new(SymbolResolver::new());
    assert_eq!(b.id(), "motilal");
    assert_eq!(b.timeframe_map(), &[("D", "D")]);
    assert!(!b.capabilities().margin);
    assert!(b.capabilities().order_feed);
    assert!(matches!(b.login_kind(), LoginKind::DirectTotp { fields } if fields.contains(&"dob")));
    assert!(b.create_feed(&auth()).is_ok());
    assert!(b.create_order_feed(&auth()).is_ok());
    assert!(b.create_feed(&AuthToken::new("bad")).is_err());
}
