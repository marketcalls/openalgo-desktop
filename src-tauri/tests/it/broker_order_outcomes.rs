//! LOG-08 P0-1, per adapter: a placement whose answer is lost is never
//! read as "not placed". Against a local fake broker, each adapter's
//! `place_order` runs inside a placement and is classified:
//!
//! * a server error (503, with a JSON error body),
//! * a success status whose body is cut off (not JSON),
//! * a success that carries no order id
//!
//! are uncertain (the order may exist); a readable refusal is refused.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::outcome::{classify, placing, PlaceOutcome};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::Broker;
use parking_lot::Mutex;
use std::sync::Arc;

/// What the fake answers to every request.
#[derive(Clone)]
struct Answer {
    status: u16,
    body: String,
}

struct Fake {
    answer: Arc<Mutex<Answer>>,
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fake() -> Fake {
    let answer = Arc::new(Mutex::new(Answer {
        status: 200,
        body: "{}".into(),
    }));
    let a = answer.clone();
    let app = Router::new().fallback(move || {
        let a = a.lock().clone();
        async move {
            let r: Response = (
                StatusCode::from_u16(a.status).unwrap(),
                [("content-type", "application/json")],
                a.body,
            )
                .into_response();
            r
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Fake { answer, base, task }
}

fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![SymToken {
        symbol: "SBIN".into(),
        brsymbol: "SBIN-EQ".into(),
        name: "SBIN".into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: "3045".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }]);
    r
}

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
    *f.answer.lock() = Answer {
        status,
        body: body.to_string(),
    };
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
