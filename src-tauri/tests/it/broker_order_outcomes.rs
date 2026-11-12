//! LOG-08 P0-1, per adapter: a placement whose answer is lost is never
//! read as "not placed". Against a local fake broker, each adapter's
//! `place_order` runs inside a placement and is classified:
//!
//! * a server error (503, with a JSON error body),
//! * a success status whose body is cut off (not JSON),
//! * a success that carries no order id,
//! * a success status with `{}` or `null` (no failure marker)
//!
//! are uncertain (the order may exist); only the broker's explicit failure
//! is refused.

use crate::broker_support::{answer, fake, symbols, Fake};
use openalgo_desktop_lib::brokers::common::outcome::{classify, placing, PlaceOutcome};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::Broker;

fn order(symbols: &SymbolResolver) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 1,
            price: 800.0,
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

/// The three lost answers and one readable refusal, for one adapter.
struct Case {
    name: &'static str,
    /// A success the broker sends without an order id.
    no_id: &'static str,
    /// A readable refusal: status and body.
    refusal: (u16, &'static str),
}

/// One placement against the fake answering `status` and `body`.
async fn attempt(
    b: &dyn Broker,
    auth: &AuthToken,
    f: &Fake,
    status: u16,
    body: &str,
) -> PlaceOutcome {
    f.reply(status, body);
    let o = order(&symbols());
    classify(&placing(b.place_order(auth, &o)).await)
}

/// Every case is run; the failures are reported together.
async fn check(b: &dyn Broker, auth: &AuthToken, f: &Fake, c: &Case) {
    let mut wrong = Vec::new();
    let lost = [
        (
            "a 503 with a JSON body",
            503,
            r#"{"status":"error","message":"Service unavailable"}"#,
        ),
        ("a cut-off 200", 200, r#"{"status":"succ"#),
        ("a 200 without an order id", 200, c.no_id),
        // A success status without the broker's failure marker is never a
        // refusal, however little it says (review P1-1).
        ("a 200 with an empty object", 200, "{}"),
        ("a 200 with null", 200, "null"),
    ];
    for (what, status, body) in lost {
        let r = attempt(b, auth, f, status, body).await;
        if !r.is_uncertain() {
            wrong.push(format!("{} must be uncertain, was {:?}", what, r));
        }
    }
    let r = attempt(b, auth, f, c.refusal.0, c.refusal.1).await;
    if !matches!(r, PlaceOutcome::Refused { .. }) {
        wrong.push(format!("a readable refusal must be refused, was {:?}", r));
    }
    assert!(wrong.is_empty(), "{}: {:#?}", c.name, wrong);
}

/// LOG-08 review: Angel One's placement whose connection never opened sent
/// nothing: a refusal, not "may have been placed" (which would hold a
/// stop-loss exit for the trader during an outage).
#[tokio::test]
async fn angel_a_connection_that_never_opened_is_a_refusal() {
    use openalgo_desktop_lib::brokers::angel::AngelBroker;
    let b = AngelBroker::with_base_url(symbols(), "http://127.0.0.1:9");
    let o = order(&symbols());
    let auth = AuthToken::new("myapikey:jwt.token.sig");
    match classify(&placing(b.place_order(&auth, &o)).await) {
        PlaceOutcome::Refused { reason } => {
            assert!(reason.contains("Could not reach Angel One"), "{}", reason)
        }
        other => panic!("{:?}", other),
    }
}

#[tokio::test]
async fn paytm_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::paytm::PaytmBroker;
    let f = fake().await;
    let b = PaytmBroker::with_base_url(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    check(
        &b,
        &AuthToken::new("acc-1"),
        &f,
        &Case {
            name: "paytm",
            no_id: r#"{"status":"success","message":"ok","data":[{}]}"#,
            refusal: (
                400,
                r#"{"status":"error","message":"Insufficient funds","data":null}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn mstock_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::mstock::MstockBroker;
    let f = fake().await;
    let b = MstockBroker::with_urls(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    check(
        &b,
        &AuthToken::new("eyJhbGciOiJIUzI1NiJ9.e30.sig:::PRIVATEKEY123"),
        &f,
        &Case {
            name: "mstock",
            no_id: r#"{"status":true,"message":"SUCCESS","data":{}}"#,
            refusal: (200, r#"{"status":false,"message":"RMS: margin exceeds"}"#),
        },
    )
    .await;
}

#[tokio::test]
async fn nubra_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::nubra::NubraBroker;
    let f = fake().await;
    let b = NubraBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1").with_fast_timings();
    check(
        &b,
        &AuthToken::new("SESS1"),
        &f,
        &Case {
            name: "nubra",
            no_id: r#"{"orders":[{}]}"#,
            refusal: (400, r#"{"error":"Insufficient margin"}"#),
        },
    )
    .await;
}

#[tokio::test]
async fn samco_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::samco::SamcoBroker;
    let f = fake().await;
    let b = SamcoBroker::with_urls(symbols(), &f.base, &f.base, "ws://127.0.0.1:1");
    check(
        &b,
        &AuthToken::new("<SESSION_TOKEN>"),
        &f,
        &Case {
            name: "samco",
            no_id: r#"{"status":"Success","statusMessage":"Order placed"}"#,
            refusal: (
                200,
                r#"{"status":"Failure","statusMessage":"Insufficient funds"}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn firstock_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::firstock::FirstockBroker;
    let f = fake().await;
    let b = FirstockBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    check(
        &b,
        &AuthToken::new("AB1234:::jkey-1"),
        &f,
        &Case {
            name: "firstock",
            no_id: r#"{"status":"success","data":{}}"#,
            refusal: (
                200,
                r#"{"status":"failed","error":{"message":"RMS: margin exceeds"}}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn xts_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
    let f = fake().await;
    let b = XtsBroker::with_base_url(
        &openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG,
        symbols(),
        &f.base,
    );
    check(
        &b,
        &AuthToken::new("eyJhbGciOiJIUzI1NiJ9.interactive.sig"),
        &f,
        &Case {
            name: "xts",
            no_id: r#"{"type":"success","result":{}}"#,
            refusal: (
                400,
                r#"{"type":"error","code":"e-orders-0005","description":"Insufficient margin"}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn tradejini_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
    let f = fake().await;
    let b = TradejiniBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    check(
        &b,
        &AuthToken::new("APPKEY0123456789:access-xyz"),
        &f,
        &Case {
            name: "tradejini",
            no_id: r#"{"s":"ok","d":{}}"#,
            refusal: (200, r#"{"s":"error","msg":"Insufficient margin"}"#),
        },
    )
    .await;
}

/// Web test/test_tradejini_order_refusal.py (web #2385, #2444): a refused
/// order is never placed, and an "ok" is never a refusal. Tradejini's
/// error envelope (over a 200 or a 400) is a refusal with its reason; an
/// "ok" whatever its payload's shape, without an order id, is uncertain
/// with the web's words; so is a 200 that cannot be read. (A 5xx is
/// uncertain on the desktop, never "refused": tradejini_lost_answers.)
#[tokio::test]
async fn tradejini_never_reads_a_refusal_as_placed_nor_an_ok_as_refused() {
    use openalgo_desktop_lib::brokers::tradejini::TradejiniBroker;
    // The web's `NO_ORDER_ID_MESSAGE`, word for word.
    const NO_ORDER_ID_MESSAGE: &str = "Tradejini accepted the order but did not return an order id. Check the order book before placing it again.";
    let f = fake().await;
    let b = TradejiniBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    let auth = AuthToken::new("APPKEY0123456789:access-xyz");
    for body in [
        r#"{"s":"ok","d":{"msg":"Order Placed Successfully"}}"#,
        r#"{"s":"ok"}"#,
        r#"{"s":"ok","d":null}"#,
        r#"{"s":"ok","d":"Order Placed"}"#,
        r#"{"s":"ok","d":[]}"#,
    ] {
        match attempt(&b, &auth, &f, 200, body).await {
            PlaceOutcome::Uncertain { reason, .. } => {
                assert_eq!(reason, NO_ORDER_ID_MESSAGE, "{}", body)
            }
            other => panic!("{}: {:?}", body, other),
        }
    }
    for body in ["not json", r#"["unexpected"]"#] {
        let r = attempt(&b, &auth, &f, 200, body).await;
        assert!(r.is_uncertain(), "{}: {:?}", body, r);
    }
    for (status, body, reason) in [
        (200, r#"{"s":"error","msg":"RMS: Margin Exceeds"}"#, "RMS: Margin Exceeds"),
        (
            400,
            r#"{"s":"error","msg":"Input validation failed. Kindly check your input contains required fields for the request"}"#,
            "Input validation failed. Kindly check your input contains required fields for the request",
        ),
    ] {
        match attempt(&b, &auth, &f, status, body).await {
            PlaceOutcome::Refused { reason: r } => assert_eq!(r, reason, "{}", body),
            other => panic!("{}: {:?}", body, other),
        }
    }
    let r = attempt(&b, &auth, &f, 401, r#"{"s":"error","msg":"Unauthorized"}"#).await;
    assert!(matches!(r, PlaceOutcome::Refused { .. }), "{:?}", r);
}

#[tokio::test]
async fn fivepaisa_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::fivepaisa::{FivepaisaBroker, Session};
    let f = fake().await;
    let b = FivepaisaBroker::with_urls(symbols(), &f.base, format!("{}/master.csv", f.base));
    let auth = AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: "eyJhbGciOiJIUzI1NiJ9.e30.sig".into(),
        }
        .encode(),
    );
    check(
        &b,
        &auth,
        &f,
        &Case {
            name: "fivepaisa",
            no_id: r#"{"head":{"statusDescription":"Success"},"body":{"Status":0,"BrokerOrderID":0}}"#,
            refusal: (
                200,
                r#"{"head":{"statusDescription":"Success"},"body":{"Status":1,"Message":"Insufficient funds"}}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn zerodha_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::zerodha::ZerodhaBroker;
    let f = fake().await;
    let b = ZerodhaBroker::with_base_url(symbols(), &f.base);
    check(
        &b,
        &AuthToken::new("kitekey:accesstok"),
        &f,
        &Case {
            name: "zerodha",
            no_id: r#"{"status":"success","data":{"order_id":""}}"#,
            refusal: (
                400,
                r#"{"status":"error","message":"Insufficient funds","error_type":"InputException"}"#,
            ),
        },
    )
    .await;
}

#[tokio::test]
async fn iiflcapital_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::iiflcapital::IiflCapitalBroker;
    let f = fake().await;
    let b = IiflCapitalBroker::with_base_url(symbols(), &f.base);
    check(
        &b,
        &AuthToken::new("eyJhbGciOiJIUzI1NiJ9.e30.sig"),
        &f,
        &Case {
            name: "iiflcapital",
            no_id: r#"{"status":"Ok","result":[{"status":"Ok"}]}"#,
            refusal: (200, r#"{"status":"error","message":"RMS: margin exceeds"}"#),
        },
    )
    .await;
}

#[tokio::test]
async fn hdfcsecurities_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::hdfcsecurities::HdfcSecuritiesBroker;
    let f = fake().await;
    let b = HdfcSecuritiesBroker::with_base_url(
        symbols(),
        &f.base,
        format!("{}/master", f.base),
        "ws://127.0.0.1:1",
    );
    check(
        &b,
        &AuthToken::new("APIKEY1:acc-tok"),
        &f,
        &Case {
            name: "hdfcsecurities",
            no_id: r#"{"status":"success","data":{}}"#,
            refusal: (200, r#"{"status":"error","message":"RMS: margin exceeds"}"#),
        },
    )
    .await;
}

#[tokio::test]
async fn pocketful_lost_answers_are_uncertain() {
    use openalgo_desktop_lib::brokers::pocketful::PocketfulBroker;
    let f = fake().await;
    let b = PocketfulBroker::with_base_url(
        symbols(),
        &f.base,
        "ws://127.0.0.1:1",
        format!("{}/master", f.base),
    );
    check(
        &b,
        &AuthToken::new("acc-pf-1").with_user_id("AB1234"),
        &f,
        &Case {
            name: "pocketful",
            no_id: r#"{"status":"success","data":{}}"#,
            refusal: (200, r#"{"status":"error","message":"RMS: margin exceeds"}"#),
        },
    )
    .await;
}

// ------------------------------------------------ review P2-3 and P2-4

fn market_order(symbols: &SymbolResolver) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 1,
            price: 0.0,
            order_type: "MARKET".into(),
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

/// Review P2-3: the quote a MARKET order is priced from is read before the
/// order is sent. Its server error never makes the order's own readable
/// refusal "may have been placed".
#[tokio::test]
async fn firstock_a_failed_price_quote_does_not_taint_the_refusal() {
    use openalgo_desktop_lib::brokers::firstock::FirstockBroker;
    let f = fake().await;
    let b = FirstockBroker::with_urls(symbols(), &f.base, "ws://127.0.0.1:1");
    f.routes.lock().push((
        "/getQuote",
        answer(503, r#"{"status":"failed","error":{"message":"busy"}}"#),
    ));
    *f.answer.lock() = answer(
        200,
        r#"{"status":"failed","error":{"message":"RMS: margin exceeds"}}"#,
    );
    let r = placing(b.place_order(
        &AuthToken::new("AB1234:::jkey-1"),
        &market_order(&symbols()),
    ))
    .await;
    assert!(
        matches!(classify(&r), PlaceOutcome::Refused { .. }),
        "{:?}",
        r
    );
}

#[tokio::test]
async fn fivepaisa_a_failed_price_quote_does_not_taint_the_refusal() {
    use openalgo_desktop_lib::brokers::fivepaisa::{FivepaisaBroker, Session};
    let f = fake().await;
    let b = FivepaisaBroker::with_urls(symbols(), &f.base, format!("{}/master.csv", f.base));
    let auth = AuthToken::new(
        Session {
            api_key: "APPKEY".into(),
            client_code: "50001234".into(),
            access_token: "eyJhbGciOiJIUzI1NiJ9.e30.sig".into(),
        }
        .encode(),
    );
    f.routes.lock().push((
        "MarketSnapshot",
        answer(503, r#"{"head":{"statusDescription":"Error"},"body":{}}"#),
    ));
    *f.answer.lock() = answer(
        200,
        r#"{"head":{"statusDescription":"Success"},"body":{"Status":1,"Message":"Insufficient funds"}}"#,
    );
    let r = placing(b.place_order(&auth, &market_order(&symbols()))).await;
    assert!(
        matches!(classify(&r), PlaceOutcome::Refused { .. }),
        "{:?}",
        r
    );
}

/// Review P2-4: a server error whose body says the session ended says
/// nothing about the order: uncertain, never "not placed". A session error
/// with no server error is still a refusal.
#[tokio::test]
async fn xts_a_session_error_on_a_server_error_is_uncertain() {
    use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
    let f = fake().await;
    let b = XtsBroker::with_base_url(
        &openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG,
        symbols(),
        &f.base,
    );
    let auth = AuthToken::new("eyJhbGciOiJIUzI1NiJ9.interactive.sig");
    let r = attempt(
        &b,
        &auth,
        &f,
        503,
        r#"{"type":"error","code":"e-session-0002","description":"Invalid Token"}"#,
    )
    .await;
    assert!(r.is_uncertain(), "{:?}", r);
    let r = attempt(
        &b,
        &auth,
        &f,
        401,
        r#"{"type":"error","code":"e-session-0002","description":"Invalid Token"}"#,
    )
    .await;
    assert!(matches!(r, PlaceOutcome::Refused { .. }), "{:?}", r);
}

#[tokio::test]
async fn noren_a_session_error_on_a_server_error_is_uncertain() {
    use openalgo_desktop_lib::brokers::families::noren::{NorenBroker, NorenEndpoints};
    use openalgo_desktop_lib::brokers::shoonya;
    let f = fake().await;
    let b = NorenBroker::with_endpoints(
        &shoonya::CONFIG,
        symbols(),
        NorenEndpoints::rebased(&shoonya::CONFIG, &f.base, "ws://127.0.0.1:1"),
    );
    let auth = AuthToken::new("AB1234:::sess-tok");
    let expired = r#"{"stat":"Not_Ok","emsg":"Session Expired :  Invalid Session Key"}"#;
    let r = attempt(&b, &auth, &f, 502, expired).await;
    assert!(r.is_uncertain(), "{:?}", r);
    let r = attempt(&b, &auth, &f, 200, expired).await;
    assert!(matches!(r, PlaceOutcome::Refused { .. }), "{:?}", r);
}
