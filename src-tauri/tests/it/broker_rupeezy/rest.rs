//! The Vortex REST paths against the fake (cases from the web's
//! `test/test_rupeezy_broker.py` where it has them).

use super::{fixture, master, serve, setup, Fake, Script, APP, KEY};
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::outcome::{classify, placing, PlaceOutcome};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::rupeezy::{RupeezyBroker, RupeezyUrls};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use serde_json::json;
use std::sync::Arc;

fn auth() -> AuthToken {
    AuthToken::new("<TOKEN>")
}

fn order(
    symbols: &SymbolResolver,
    symbol: &str,
    exchange: &str,
    side: &str,
    qty: i32,
) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: side.into(),
            quantity: qty,
            price: 801.5,
            order_type: "LIMIT".into(),
            product: "MIS".into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        },
        symbols,
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Sign-in and headers
// ---------------------------------------------------------------------------

/// Web `authenticate_broker` and `test_authenticated_calls_send_the_api_key`.
#[tokio::test]
async fn sign_in_exchanges_the_auth_code_and_later_calls_send_both_keys() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = RupeezyBroker::with_base_url(master(), RupeezyUrls::rebased(&host, "ws://127.0.0.1:1"));
    let creds = |secret: &str| BrokerCredentials {
        api_key: APP.into(),
        api_secret: Some(secret.into()),
        auth_code: Some("auth9".into()),
        ..Default::default()
    };
    // A wrong API key fails the checksum: refused with Vortex's reason.
    let e = b.authenticate(creds("WRONG")).await.unwrap_err();
    assert!(
        matches!(&e, AppError::Auth(m) if m.contains("Invalid checksum")),
        "{:?}",
        e
    );
    let resp = b.authenticate(creds(KEY)).await.unwrap();
    assert_eq!(resp.auth_token, "<TOKEN>");
    assert_eq!(resp.user_id, "<USER_ID>");
    assert_eq!(resp.user_name.as_deref(), Some("TEST TRADER"));
    let session = fake.calls("POST", "/user/session");
    let last = session.last().unwrap();
    assert_eq!(last.body["token"], "auth9");
    assert_eq!(last.body["applicationId"], APP);
    assert_eq!(last.api_key, KEY);
    assert!(last.authorization.is_empty());
    // Every later call: the bearer token and the app's API key.
    b.get_order_book(&AuthToken::new(resp.auth_token))
        .await
        .unwrap();
    let book = fake.calls("GET", "/trading/orders");
    assert_eq!(book[0].authorization, "Bearer <TOKEN>");
    assert_eq!(book[0].api_key, KEY);
    assert_eq!(
        (book[0].q("limit"), book[0].q("offset")),
        (vec!["500".to_string()], vec!["1".to_string()])
    );
    // Without a code nothing is sent.
    let mut none = creds(KEY);
    none.auth_code = None;
    assert!(matches!(
        b.authenticate(none).await,
        Err(AppError::Validation(_))
    ));
}

/// After a restart the API key comes back from the saved secret; a logout
/// drops it.
#[tokio::test]
async fn a_resumed_session_sends_the_saved_api_key_until_logout() {
    let (b, fake) = setup().await;
    b.get_trade_book(&auth()).await.unwrap();
    b.on_logout().await;
    b.get_trade_book(&auth()).await.unwrap();
    let calls = fake.calls("GET", "/trading/trades");
    assert_eq!(
        (calls[0].api_key.as_str(), calls[1].api_key.as_str()),
        (KEY, "")
    );
    // No token at all is a session to log in again, sent nowhere.
    let e = b.get_trade_book(&AuthToken::new("")).await.unwrap_err();
    assert!(matches!(e, AppError::Auth(_)));
    assert_eq!(fake.calls("GET", "/trading/trades").len(), 2);
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn place_modify_and_cancel_send_vortex_bodies_on_escaped_paths() {
    let (b, fake) = setup().await;
    let symbols = master();
    let placed = placing(b.place_order(&auth(), &order(&symbols, "SBIN", "NSE", "BUY", 10))).await;
    assert_eq!(placed.unwrap().order_id, "NXAAE00002K3");
    let body = &fake.calls("POST", "/trading/orders/regular")[0].body;
    assert_eq!(
        body,
        &json!({"ticker": "NSE:SBIN", "transaction_type": "BUY", "product": "INTRADAY", "variety": "RL",
                "quantity": 10, "price": 801.5, "trigger_price": 0.0, "disclosed_quantity": 0,
                "validity": "DAY", "is_amo": false, "order_identifier": "openalgo"})
    );
    // From the order service: the request's strategy names the order.
    let o = order(&symbols, "SBIN", "NSE", "BUY", 10);
    placing(b.place_order_with_strategy(&auth(), &o, "Momentum"))
        .await
        .unwrap();
    let sent = fake.calls("POST", "/trading/orders/regular");
    assert_eq!(sent[1].body["order_identifier"], "Momentum");
    // Web `test_modify_reads_order_history_under_the_orders_key`: the
    // newest history row's traded quantity, the id escaped.
    let modify = ResolvedModify::resolve(
        "OFPIM00003?:",
        &ModifyOrderRequest {
            symbol: "IDEA".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "LIMIT".into(),
            quantity: 10,
            price: 11.4,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &symbols,
    )
    .unwrap();
    let r = b.modify_order(&auth(), &modify).await.unwrap();
    assert_eq!(r.order_id, "OFPIM00003?:");
    assert_eq!(
        fake.calls("GET", "/trading/orders/OFPIM00003%3F%3A").len(),
        1
    );
    let put = &fake.calls("PUT", "/trading/orders/regular/OFPIM00003%3F%3A")[0].body;
    assert_eq!(
        put,
        &json!({"variety": "RL", "quantity": 10, "traded_quantity": 3, "price": 11.4,
                "trigger_price": 0.0, "disclosed_quantity": 0, "validity": "DAY"})
    );
    b.cancel_order(&auth(), "OFPIM00001?:").await.unwrap();
    assert_eq!(
        fake.calls("DELETE", "/trading/orders/regular/OFPIM00001%3F%3A")
            .len(),
        1
    );
}

/// Web `test_modify_reports_an_unreadable_order_as_an_error_not_missing`.
#[tokio::test]
async fn a_modify_on_an_unreadable_order_is_an_error_not_a_missing_order() {
    let (b, fake) = setup().await;
    let symbols = master();
    let req = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 1,
        price: 1.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    fake.script(Script::new(
        "GET",
        "/trading/orders/X",
        200,
        r#"{"status":"error","message":"Invalid session"}"#,
    ));
    let e = b
        .modify_order(
            &auth(),
            &ResolvedModify::resolve("X", &req, &symbols).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, AppError::Broker(m) if m == "Invalid session"),
        "{:?}",
        e
    );
    fake.script(Script::new(
        "GET",
        "/trading/orders/Y",
        200,
        r#"{"status":"success","orders":[]}"#,
    ));
    let e = b
        .modify_order(
            &auth(),
            &ResolvedModify::resolve("Y", &req, &symbols).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, AppError::NotFound(m) if m == "Order Y was not found."),
        "{:?}",
        e
    );
    assert!(fake.calls_under("/trading/orders/regular").is_empty());
}

/// LOG-08: a placement whose answer is lost is never "not placed"; only
/// Vortex's own refusal is.
#[tokio::test]
async fn placement_outcomes_follow_what_vortex_answered() {
    let (b, fake) = setup().await;
    let symbols = master();
    let o = order(&symbols, "SBIN", "NSE", "BUY", 1);
    let attempt = |status: u16, body: &'static str| {
        fake.script(Script::new("POST", "/trading/orders/regular", status, body).once());
        let b = &b;
        let o = o.clone();
        async move { classify(&placing(b.place_order(&auth(), &o)).await) }
    };
    for (status, body) in [
        (503, r#"{"status":"error","message":"Service unavailable"}"#),
        (200, r#"{"status":"succ"#),
        (200, r#"{"status":"success","data":{}}"#),
        (200, "{}"),
        (200, "null"),
    ] {
        let r = attempt(status, body).await;
        assert!(r.is_uncertain(), "{} {} -> {:?}", status, body, r);
    }
    for (status, body) in [
        (
            400,
            r#"{"status":"error","code":"e-103","message":"Insufficient funds"}"#,
        ),
        (200, r#"{"status":"error","message":"Insufficient funds"}"#),
    ] {
        let r = attempt(status, body).await;
        assert!(
            matches!(&r, PlaceOutcome::Refused { reason } if reason == "Insufficient funds"),
            "{} {} -> {:?}",
            status,
            body,
            r
        );
    }
    // Web `test_a_write_is_not_resent_after_429`: Vortex refused it, once.
    let before = fake.calls("POST", "/trading/orders/regular").len();
    let r = attempt(429, r#"{"status":"error","message":"Too many requests"}"#).await;
    assert!(matches!(r, PlaceOutcome::Refused { .. }), "{:?}", r);
    assert_eq!(
        fake.calls("POST", "/trading/orders/regular").len(),
        before + 1
    );
}

/// A connection dropped after the order went out: uncertain, never resent.
#[tokio::test]
async fn an_order_on_a_dropped_connection_is_uncertain_and_not_resent() {
    let (host, accepted) = dropping_server(usize::MAX).await;
    let b = RupeezyBroker::with_base_url(master(), RupeezyUrls::rebased(&host, "ws://127.0.0.1:1"));
    let o = order(&master(), "SBIN", "NSE", "BUY", 1);
    let r = classify(&placing(b.place_order(&auth(), &o)).await);
    assert!(r.is_uncertain(), "{:?}", r);
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Web `test_a_read_is_retried_once_on_a_dead_connection`.
#[tokio::test]
async fn a_read_is_retried_once_on_a_dead_connection() {
    let (host, accepted) = dropping_server(1).await;
    let b = RupeezyBroker::with_base_url(master(), RupeezyUrls::rebased(&host, "ws://127.0.0.1:1"));
    let trades = b.get_trade_book(&auth()).await.unwrap();
    assert!(trades.is_empty());
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A loopback server that reads a request and closes the connection
/// without answering, `drops` times, then answers `{"status":"success",
/// "trades":[]}`. Counts the connections it accepts.
async fn dropping_server(drops: usize) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", l.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 8192];
            let _ = s.read(&mut buf).await;
            if n < drops {
                drop(s);
                continue;
            }
            let body = r#"{"status":"success","trades":[]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes()).await;
        }
    });
    (host, accepted)
}

/// Web `test_a_read_honours_an_http_date_retry_after`; a wait past the
/// queue bound is not taken.
#[tokio::test]
async fn a_read_retries_a_429_within_the_queue_bound() {
    let (b, fake) = setup().await;
    fake.script(
        Script::new(
            "GET",
            "/trading/trades",
            429,
            r#"{"status":"error","message":"Too many requests"}"#,
        )
        .retry_after("Wed, 21 Oct 2015 07:28:00 GMT")
        .once(),
    );
    assert_eq!(b.get_trade_book(&auth()).await.unwrap().len(), 3);
    assert_eq!(fake.calls("GET", "/trading/trades").len(), 2);
    fake.script(
        Script::new(
            "GET",
            "/trading/trades",
            429,
            r#"{"status":"error","message":"Too many requests"}"#,
        )
        .retry_after("120")
        .once(),
    );
    let e = b.get_trade_book(&auth()).await.unwrap_err();
    assert!(
        matches!(&e, AppError::Broker(m) if m == "Too many requests"),
        "{:?}",
        e
    );
    assert_eq!(fake.calls("GET", "/trading/trades").len(), 3);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// Web `test_open_positions_get_live_ltp_and_pnl`,
/// `test_holdings_without_last_price_are_priced_from_quotes`.
#[tokio::test]
async fn books_carry_openalgo_symbols_and_live_prices() {
    let (b, fake) = setup().await;
    let orders = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(
        orders
            .iter()
            .map(|o| (o.symbol.as_str(), o.status.as_str()))
            .collect::<Vec<_>>(),
        [
            ("SBIN", "open"),
            ("CRUDEOIL20MAR28FUT", "complete"),
            ("NIFTY27OCT2625000CE", "rejected"),
            ("INFY", "trigger pending")
        ]
    );
    let trades = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(trades[2].symbol, "GOLDM05NOV26FUT");
    let positions = b.get_positions(&auth()).await.unwrap();
    let ltp = fake.calls("GET", "/data/quotes");
    assert_eq!(ltp.len(), 1);
    // Open rows only, one call, ltp mode.
    assert_eq!(
        ltp[0].q("q"),
        ["MCX:CRUDEOIL28MARFUT", "MCX:GOLDPETAL26OCTFUT", "NSE:SBIN"]
    );
    assert_eq!(ltp[0].q("mode"), ["ltp"]);
    let by = |s: &str| positions.iter().find(|p| p.symbol == s).unwrap().clone();
    assert_eq!((by("SBIN").ltp, by("SBIN").pnl), (954.0, 40.0));
    assert_eq!((by("INFY").ltp, by("INFY").pnl), (0.0, 25.0));
    assert_eq!(by("GOLDPETAL30OCT26FUT").pnl, 10.0);
    assert_eq!(
        (
            by("CRUDEOIL20MAR28FUT").quantity,
            by("CRUDEOIL20MAR28FUT").pnl
        ),
        (-200, 10_000.0)
    );
}

#[tokio::test]
async fn holdings_without_a_price_are_priced_from_one_quotes_call() {
    let (b, fake) = setup().await;
    let h = b.get_holdings(&auth()).await.unwrap();
    // Only the unpriced row is asked for.
    assert_eq!(
        fake.calls("GET", "/data/quotes")[0].q("q"),
        ["BSE:RELIANCE"]
    );
    assert_eq!(
        h.iter()
            .map(|x| (x.symbol.as_str(), x.exchange.as_str(), x.ltp))
            .collect::<Vec<_>>(),
        [("IDEA", "NSE", 13.21), ("RELIANCE", "BSE", 1410.0)]
    );
}

/// Web `test_trade_book_fills_bare_trades_from_the_order_book`.
#[tokio::test]
async fn bare_trades_are_completed_from_the_order_book() {
    let (b, fake) = setup().await;
    fake.script(Script::new(
        "GET",
        "/trading/trades",
        200,
        &fixture("trades_bare").to_string(),
    ));
    fake.script(Script::new(
        "GET",
        "/trading/orders",
        200,
        &fixture("orders_for_bare_trade").to_string(),
    ));
    let t = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(
        (
            t[0].symbol.as_str(),
            t[0].exchange.as_str(),
            t[0].side.as_str(),
            t[0].product.as_str(),
            t[0].average_price
        ),
        ("GOLDPETAL30OCT26FUT", "MCX", "SELL", "NRML", 14929.0)
    );
}

/// Web `test_smart_orders_read_positions_without_quotes`; an unreadable
/// position book refuses the smart order.
#[tokio::test]
async fn the_open_position_is_read_without_quotes_and_strictly() {
    let (b, fake) = setup().await;
    for (symbol, exchange, product, want) in [
        ("SBIN", Exchange::Nse, Product::Mis, 10),
        ("SBIN", Exchange::Nse, Product::Cnc, 0),
        ("CRUDEOIL20MAR28FUT", Exchange::Mcx, Product::Nrml, -200),
    ] {
        let got = b
            .get_open_position(&auth(), symbol, exchange, product)
            .await
            .unwrap();
        assert_eq!(got, want, "{}", symbol);
    }
    assert!(fake.calls("GET", "/data/quotes").is_empty());
    fake.script(
        Script::new(
            "GET",
            "/trading/portfolio/positions",
            200,
            r#"{"status":"success","data":{}}"#,
        )
        .once(),
    );
    let e = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
        .await
        .unwrap_err();
    assert!(
        e.client_message()
            .contains("could not read your open position from Rupeezy"),
        "{}",
        e
    );
    fake.script(
        Script::new(
            "GET",
            "/trading/portfolio/positions",
            200,
            r#"{"status":"error","message":"Invalid session"}"#,
        )
        .once(),
    );
    assert!(b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
        .await
        .is_err());
}

/// Web `test_close_all_reports_refused_square_offs`,
/// `test_close_all_success_when_every_order_is_accepted`,
/// `test_close_all_does_not_call_an_unreadable_book_empty`.
#[tokio::test]
async fn close_all_squares_off_in_vortex_units_and_reports_refusals() {
    let (b, fake) = setup().await;
    fake.refuse_tickers
        .lock()
        .push("MCX:GOLDPETAL26OCTFUT".into());
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert_eq!(r.failed.len(), 1);
    assert!(
        r.failed[0].starts_with("GOLDPETAL30OCT26FUT (MCX): Insufficient funds"),
        "{:?}",
        r.failed
    );
    assert!(r.uncertain.is_empty());
    let sent: Vec<(String, String, i64, String)> = fake
        .calls("POST", "/trading/orders/regular")
        .iter()
        .map(|c| {
            (
                c.body["ticker"].as_str().unwrap().to_string(),
                c.body["transaction_type"].as_str().unwrap().to_string(),
                c.body["quantity"].as_i64().unwrap(),
                c.body["variety"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        sent,
        [
            (
                "NSE:SBIN".to_string(),
                "SELL".to_string(),
                10,
                "RL-MKT".to_string()
            ),
            (
                "MCX:GOLDPETAL26OCTFUT".into(),
                "SELL".into(),
                2,
                "RL-MKT".into()
            ),
            // 2 lots short: bought back as 200 units, sent as 2 lots.
            (
                "MCX:CRUDEOIL28MARFUT".into(),
                "BUY".into(),
                2,
                "RL-MKT".into()
            ),
        ]
    );
    // The web's close-all strategy names every exit.
    assert!(fake
        .calls("POST", "/trading/orders/regular")
        .iter()
        .all(|c| c.body["order_identifier"] == "Squareoff"));
    // No quotes on the way to an exit.
    assert!(fake.calls("GET", "/data/quotes").is_empty());
    fake.script(Script::new(
        "GET",
        "/trading/portfolio/positions",
        200,
        r#"{"status":"error","message":"Invalid session"}"#,
    ));
    let e = b.close_all_positions(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("Invalid session"), "{}", e);
}

/// Web `test_cancel_all_raises_when_the_book_cannot_be_read`.
#[tokio::test]
async fn cancel_all_cancels_open_orders_and_refuses_an_unread_book() {
    let (b, fake) = setup().await;
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["NXAAE00002K3", "OFPIM00001?:"]);
    assert_eq!(
        fake.calls("DELETE", "/trading/orders/regular/OFPIM00001%3F%3A")
            .len(),
        1
    );
    fake.script(Script::new(
        "GET",
        "/trading/orders",
        200,
        r#"{"status":"success"}"#,
    ));
    let e = b.cancel_all_orders(&auth()).await.unwrap_err();
    assert!(
        e.client_message().contains("no order was cancelled"),
        "{}",
        e
    );
}

/// BR-03: a success whose order list is `null` is an order book that was
/// not read. Cancel all refuses with nothing cancelled and the order book
/// is an error; Vortex's empty book, `[]`, stays empty.
#[tokio::test]
async fn a_null_order_list_is_not_an_empty_book() {
    let (b, fake) = setup().await;
    let null = r#"{"status":"success","orders":null}"#;
    for _ in 0..2 {
        fake.script(Script::new("GET", "/trading/orders", 200, null).once());
    }
    let e = b.cancel_all_orders(&auth()).await.unwrap_err();
    assert!(
        e.client_message().contains("no order was cancelled"),
        "{}",
        e
    );
    assert!(fake.calls_under("/trading/orders/regular").is_empty());
    assert!(b.get_order_book(&auth()).await.is_err());

    let empty = r#"{"status":"success","orders":[]}"#;
    for _ in 0..2 {
        fake.script(Script::new("GET", "/trading/orders", 200, empty).once());
    }
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert!(r.cancelled.is_empty() && r.failed.is_empty(), "{:?}", r);
    assert!(b.get_order_book(&auth()).await.unwrap().is_empty());
    assert!(fake.calls_under("/trading/orders/regular").is_empty());
}

// ---------------------------------------------------------------------------
// Funds
// ---------------------------------------------------------------------------

/// Web `test_funds_pnl_reconciles_with_the_position_book`, against the fake.
#[tokio::test]
async fn funds_take_the_combined_bucket_and_pnl_from_priced_positions() {
    let (b, fake) = setup().await;
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!(
        (f.available_cash, f.collateral, f.utilised_debits),
        (1500.0, 25.0, 120.5)
    );
    // Realized: INFY's booked 25. Unrealized: SBIN 40, GOLDPETAL 10,
    // CRUDEOIL short 200 units from 6500 to 6450.
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (25.0, 10_050.0));
    // Positions unreadable: the buckets' booked figures, no unrealized.
    fake.script(Script::new(
        "GET",
        "/trading/portfolio/positions",
        500,
        r#"{"status":"error","message":"down"}"#,
    ));
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (999.0, 0.0));
    fake.script(Script::new(
        "GET",
        "/user/funds",
        200,
        r#"{"status":"error","message":"Invalid session"}"#,
    ));
    let e = b.get_funds(&auth()).await.unwrap_err();
    assert!(
        matches!(&e, AppError::Broker(m) if m == "Invalid session"),
        "{:?}",
        e
    );
}

// ---------------------------------------------------------------------------
// Quotes and history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_quote_and_its_depth_come_from_the_full_quote() {
    let (b, fake) = setup().await;
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (q.ltp, q.bid_qty, q.ask_qty, q.close),
        (954.0, 54, 2827, 958.75)
    );
    let call = &fake.calls("GET", "/data/quotes")[0];
    assert_eq!(
        (call.q("q"), call.q("mode")),
        (vec!["NSE:SBIN".to_string()], vec!["full".to_string()])
    );
    let e = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "NOPE"))
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Validation(_)));
}

#[tokio::test]
async fn depth_has_five_levels_a_side() {
    let (b, _) = setup().await;
    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (d.bids.len(), d.asks.len(), d.bids[0].price, d.ltq),
        (5, 5, 953.95, 5)
    );
}

/// Web `test_multiquotes_reports_every_leg` and `get_multiquotes`: legs
/// not in the master first, then the resolved legs in request order, from
/// one call.
#[tokio::test]
async fn multiquotes_answer_every_leg_in_the_webs_order_from_one_call() {
    let (b, fake) = setup().await;
    let keys = [
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE", "IDEA"),
        QuoteKey::new("NSE", "SBIN"),
    ];
    let out = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(fake.calls("GET", "/data/quotes").len(), 1);
    assert_eq!(
        fake.calls("GET", "/data/quotes")[0].q("q"),
        ["NSE:IDEA", "NSE:NIFTYIDX", "NSE:SBIN"]
    );
    assert_eq!(
        out.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(),
        ["NOPE", "SBIN", "NIFTY", "IDEA", "SBIN"]
    );
    assert_eq!(
        out[0].error.as_deref(),
        Some("Symbol NOPE not found on NSE")
    );
    assert_eq!(out[1].data.as_ref().unwrap().ltp, 954.0);
    assert_eq!(out[2].data.as_ref().unwrap().ltp, 25140.15);
    assert_eq!(out[3].error.as_deref(), Some("No quote data available"));
    assert_eq!(out[4].data.as_ref().unwrap().symbol, "SBIN");
}

/// Web `get_multiquotes`: a batch that fails (here an ended session) leaves
/// its legs without data; the call still answers every leg.
#[tokio::test]
async fn a_failed_quotes_batch_answers_its_legs_without_data() {
    let (b, fake) = setup().await;
    fake.script(Script::new(
        "GET",
        "/data/quotes",
        401,
        r#"{"status":"error","message":"Invalid session"}"#,
    ));
    let keys = [QuoteKey::new("NSE", "SBIN"), QuoteKey::new("NSE", "NOPE")];
    let out = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(
        out.iter()
            .map(|r| (r.symbol.as_str(), r.error.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("NOPE", Some("Symbol NOPE not found on NSE")),
            ("SBIN", Some("No quote data available"))
        ]
    );
}

/// Web `test_daily_candles_land_on_the_ist_date`,
/// `test_history_with_no_data_returns_the_empty_frame`.
#[tokio::test]
async fn history_asks_ist_days_and_lands_daily_candles_on_their_date() {
    let (b, fake) = setup().await;
    let req = |interval: &str, start: (u32, u32), end: (u32, u32)| HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: interval.into(),
        start: NaiveDate::from_ymd_opt(2026, start.0, start.1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, end.0, end.1).unwrap(),
    };
    let c = b
        .get_history(&auth(), &req("D", (10, 2), (10, 5)))
        .await
        .unwrap();
    assert_eq!(
        c.iter().map(|c| c.timestamp).collect::<Vec<_>>(),
        [1791072000, 1791158400]
    );
    let call = &fake.calls("GET", "/data/history")[0];
    assert_eq!(
        (
            call.q("ticker"),
            call.q("resolution"),
            call.q("from"),
            call.q("to")
        ),
        (
            vec!["NSE:SBIN".to_string()],
            vec!["1D".to_string()],
            vec!["1790879400".to_string()],
            vec!["1791224999".to_string()]
        )
    );
    assert!(b
        .get_history(&auth(), &req("M", (10, 2), (10, 5)))
        .await
        .unwrap()
        .is_empty());
    let e = b
        .get_history(&auth(), &req("W", (10, 2), (10, 5)))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, AppError::Broker(m) if m == "Invalid resolution"),
        "{:?}",
        e
    );
    let e = b
        .get_history(&auth(), &req("7m", (10, 2), (10, 5)))
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::Validation(_)));
}

/// Intraday history goes 30 days a call.
#[tokio::test]
async fn intraday_history_is_chunked_by_thirty_days() {
    let (b, fake) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    let calls = fake.calls("GET", "/data/history");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].q("resolution"), ["5"]);
    // The same two candles twice, deduplicated.
    assert_eq!(c.len(), 2);
}

// ---------------------------------------------------------------------------
// GTT and master
// ---------------------------------------------------------------------------

fn gtt_req(trigger_type: GttTriggerType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type,
        action: Action::Sell,
        product: Product::Mis,
        quantity: 100,
        pricetype: PriceType::Limit,
        price: 790.0,
        trigger_price: 795.0,
        triggerprice_sl: 770.0,
        stoploss: 769.0,
        triggerprice_tg: 860.0,
        target: 861.0,
        last_price: None,
        expires_at: None,
    }
}

#[tokio::test]
async fn gtt_place_modify_cancel_and_book() {
    let (b, fake) = setup().await;
    let r = b
        .place_gtt(&auth(), &gtt_req(GttTriggerType::Oco))
        .await
        .unwrap();
    assert_eq!(r.trigger_id, "fc3e3201-8d34-4b1c-9b7a-0c1f5c3e2a10");
    let body = &fake.calls("POST", "/trading/orders/gtt")[0].body;
    assert_eq!(
        (
            &body["stoploss"]["trigger_price"],
            &body["profit"]["trigger_price"]
        ),
        (&json!(770.0), &json!(860.0))
    );
    let id = "8f141670-59b3-4754-81c7-a072f3598475";
    b.modify_gtt(&auth(), id, &gtt_req(GttTriggerType::Oco))
        .await
        .unwrap();
    let put = &fake.calls("PUT", &format!("/trading/orders/gtt/{}", id))[0].body;
    assert_eq!(put.as_array().unwrap().len(), 2);
    assert_eq!(put[0]["id"], 1);
    let e = b
        .modify_gtt(&auth(), "missing", &gtt_req(GttTriggerType::Oco))
        .await
        .unwrap_err();
    assert!(matches!(e, AppError::NotFound(_)));
    b.cancel_gtt(&auth(), id).await.unwrap();
    // An empty success (204) is a success.
    fake.script(Script::new("DELETE", "/trading/orders/gtt/x", 204, ""));
    assert_eq!(b.cancel_gtt(&auth(), "x").await.unwrap().trigger_id, "x");
    let book = b.get_gtt_book(&auth(), false).await.unwrap();
    assert_eq!(book.len(), 2);
    assert_eq!(b.get_gtt_book(&auth(), true).await.unwrap().len(), 3);
    assert!(b.capabilities().gtt);
}

#[tokio::test]
async fn the_master_downloads_without_a_session() {
    let (b, fake) = setup().await;
    let rows = b
        .download_master_contract(&AuthToken::new(""))
        .await
        .unwrap();
    assert_eq!(rows.len(), 19);
    assert!(fake.calls("GET", "/master.csv")[0].authorization.is_empty());
}
