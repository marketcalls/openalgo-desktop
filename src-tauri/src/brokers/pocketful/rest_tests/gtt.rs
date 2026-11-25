//! Pocketful GTT rules (web `test/test_pocketful_account.py`, its GTT
//! section), against a local fake answering the docs' own examples
//! (`tests/fixtures/brokers/pocketful/gtt.json`), plus the desktop's three
//! outcomes of a create and its secrets check.

use super::{capture, Fake, Reply, CLIENT};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::pocketful::gtt::{
    self, broker_reason, expiry_date, gtt_rows, order_block, valid_trigger_id, BAD_ID, GTT_PATH,
    NO_CLIENT_ID,
};
use crate::brokers::pocketful::PocketfulBroker;
use crate::brokers::types::{AuthToken, GttLeg, GttOrder, GttRequest, GttTriggerType, QuoteKey};
use crate::brokers::Broker;
use crate::error::AppError;
use chrono::NaiveDate;
use serde_json::{json, Value};

const GTT_ID: &str = "ea036a20-2073-4bfb-a852-34662c4bd770";
const HELD_ID: &str = "673a20c8-80d5-4a0c-8a34-23f20fe79661";
const CANCEL_ID: &str = "e06f52ab-6cf4-492d-a014-c6e0e5843a93";
const CHECK_BEFORE_PLACING: &str = "Check the GTT list before placing it again";

/// One of the docs' example answers.
fn doc(name: &str) -> Value {
    let all: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/brokers/pocketful/gtt.json"
    ))
    .unwrap();
    assert!(all.get(name).is_some(), "no fixture {}", name);
    all[name].clone()
}

fn answer(name: &str) -> Reply {
    Reply::Json(200, doc(name))
}

fn book(rules: Vec<Value>) -> Reply {
    Reply::Json(
        200,
        json!({"data": rules, "message": "", "status": "success"}),
    )
}

fn book_path() -> String {
    format!("{}/{}", GTT_PATH, CLIENT)
}

fn rule_path(id: &str) -> String {
    format!("{}/{}/{}", GTT_PATH, CLIENT, id)
}

/// The web test's `today_ist`.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
}

fn session() -> AuthToken {
    AuthToken::new("jwt.abc.def").with_user_id(CLIENT)
}

fn row(
    symbol: &str,
    exchange: &str,
    token: &str,
    brsymbol: &str,
    expiry: &str,
    tick: f64,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.replace("_INDEX", ""),
        token: token.into(),
        expiry: expiry.into(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: tick,
    }
}

/// The web test's master contract stand-in.
fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        row("IDEA", "NSE", "14366", "IDEA-EQ", "", 0.01),
        row("TCS", "NSE", "11536", "TCS-EQ", "", 0.1),
        row("NIFTY", "NSE_INDEX", "26000", "Nifty 50", "", 0.0),
        row(
            "NIFTY24NOV2624000CE",
            "NFO",
            "40001",
            "NIFTY26N2424000CE",
            "24-NOV-26",
            0.05,
        ),
    ]);
    r
}

/// The web test's `SINGLE`.
fn single() -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "IDEA"),
        trigger_type: GttTriggerType::Single,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 1,
        pricetype: PriceType::Limit,
        price: 11.3,
        trigger_price: 13.6,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 13.6,
        target: 0.0,
        last_price: None,
        expires_at: None,
    }
}

/// The web test's `MODIFY` (its trigger id is [`HELD_ID`]).
fn modify() -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "TCS"),
        price: 3600.0,
        trigger_price: 3605.0,
        triggerprice_tg: 3605.0,
        ..single()
    }
}

/// The documented rule with one field of its order changed.
fn rule_with(key: &str, value: Value) -> Value {
    let mut r = doc("rule");
    r["order"][key] = value;
    r
}

/// An address nothing listens on: a request there never leaves.
async fn closed_base() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{}", addr)
}

fn unreachable_broker(base: String) -> PocketfulBroker {
    PocketfulBroker::with_base_url(symbols(), base, "ws://127.0.0.1:9", "http://127.0.0.1:9/c")
}

/// Web test_pocketful_account.py::test_a_single_gtt_is_created_as_the_docs_show
#[tokio::test]
async fn a_single_gtt_is_created_as_the_docs_show() {
    let fake = Fake::start().await;
    fake.on(GTT_PATH, vec![answer("created")]);
    let b = fake.broker(symbols());

    let placed = gtt::place_gtt_on(&b, &session(), &single(), today())
        .await
        .unwrap();

    assert_eq!(placed.trigger_id, GTT_ID);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("POST", GTT_PATH)
    );
    let mut body = seen[0].json();
    let id = body["order"]["user_order_id"].take();
    assert!(id.as_u64().is_some_and(|n| n < 2_000_000_000), "{}", id);
    body["order"]["user_order_id"] = json!(122321);
    assert_eq!(
        body,
        json!({
            "action_type": "single_order",
            "expiry_time": "2027-11-17",
            "order": {
                "client_id": CLIENT,
                "disclosed_quantity": 0,
                "exchange": "NSE",
                "instrument_token": "14366",
                "market_protection_percentage": 0,
                "order_side": "BUY",
                "order_type": "LIMIT",
                "price": 11.3,
                "product": "CNC",
                "quantity": 1,
                "sl_order_price": 0,
                "sl_order_quantity": 0,
                "sl_trigger_price": 0,
                "trigger_price": 13.6,
                "user_order_id": 122321,
            },
        })
    );
}

/// Web test_pocketful_account.py::test_a_market_gtt_is_a_limit_protected_from_its_trigger:
/// measured from the trigger, where the child order is placed, a BUY above
/// it and a SELL below it (IDEA 2% on a 0.01 tick, TCS 0.5% on a 0.10 tick).
#[test]
fn a_market_gtt_is_a_limit_protected_from_its_trigger() {
    let r = symbols();
    for (action, symbol, trigger, expected) in [
        (Action::Buy, "IDEA", 13.6, 13.87),
        (Action::Sell, "TCS", 1480.0, 1472.6),
    ] {
        let req = GttRequest {
            key: QuoteKey::new("NSE", symbol),
            action,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: trigger,
            triggerprice_tg: trigger,
            ..single()
        };
        let row = r.by_symbol("NSE", symbol).unwrap();
        let order = order_block(&req, &row, trigger, CLIENT, 1).unwrap();
        assert_eq!(order["order_type"], "LIMIT");
        assert_eq!(order["trigger_price"], json!(trigger));
        assert_eq!(order["price"], json!(expected), "{}", symbol);
    }
}

/// Web test_pocketful_account.py::test_a_derivative_gtt_expires_with_its_contract_unless_told
#[tokio::test]
async fn a_derivative_gtt_expires_with_its_contract_unless_told() {
    let fake = Fake::start().await;
    fake.on(GTT_PATH, vec![answer("created")]);
    let b = fake.broker(symbols());
    let option = GttRequest {
        key: QuoteKey::new("NFO", "NIFTY24NOV2624000CE"),
        product: Product::Nrml,
        quantity: 75,
        ..single()
    };

    gtt::place_gtt_on(&b, &session(), &option, today())
        .await
        .unwrap();
    let told = GttRequest {
        expires_at: Some("2026-11-20T15:30:00+05:30".into()),
        ..option
    };
    gtt::place_gtt_on(&b, &session(), &told, today())
        .await
        .unwrap();

    let expiries: Vec<Value> = fake
        .to(GTT_PATH)
        .iter()
        .map(|s| s.json()["expiry_time"].clone())
        .collect();
    assert_eq!(expiries, [json!("2026-11-24"), json!("2026-11-20")]);
}

/// Web gtt_data.expiry_date: the request's date (its first ten characters,
/// in the formats the web reads, from today on), else the contract's, else
/// a year from today.
#[test]
fn the_expiry_is_the_requests_else_the_contracts_else_a_year() {
    let r = symbols();
    let idea = r.by_symbol("NSE", "IDEA").unwrap();
    let option_row = r.by_symbol("NFO", "NIFTY24NOV2624000CE").unwrap();
    let option = GttRequest {
        key: QuoteKey::new("NFO", "NIFTY24NOV2624000CE"),
        ..single()
    };
    assert_eq!(
        expiry_date(&single(), &idea, today()).unwrap(),
        "2027-11-17"
    );
    assert_eq!(
        expiry_date(&option, &option_row, today()).unwrap(),
        "2026-11-24"
    );
    // A contract already past its expiry gets the year, as anything else.
    let later = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
    assert_eq!(
        expiry_date(&option, &option_row, later).unwrap(),
        "2027-11-25"
    );
    let sent = |s: &str| GttRequest {
        expires_at: Some(s.into()),
        ..single()
    };
    for (text, want) in [
        ("2026-11-17", "2026-11-17"),
        ("2026-11-20T15:30:00+05:30", "2026-11-20"),
        ("20-NOV-26", "2026-11-20"),
        ("20nov26", "2026-11-20"),
        ("20NOV2026", "2026-11-20"),
    ] {
        assert_eq!(
            expiry_date(&sent(text), &idea, today()).unwrap(),
            want,
            "{}",
            text
        );
    }
    for text in ["2026-11-16", "20-Nov-2026", "tomorrow", " "] {
        let e = expiry_date(&sent(text), &idea, today()).unwrap_err();
        assert!(
            matches!(&e, AppError::Refused(m) if m.contains("expiry")),
            "{}: {:?}",
            text,
            e
        );
    }
    // An empty one is not sent: the default applies, as on the web.
    assert_eq!(
        expiry_date(&sent(""), &idea, today()).unwrap(),
        "2027-11-17"
    );
}

/// Web test_pocketful_account.py::test_a_gtt_pocketful_cannot_take_is_refused_unsent,
/// with the desktop's own two: a stop price type and a size below one.
#[tokio::test]
async fn a_gtt_pocketful_cannot_take_is_refused_unsent() {
    let fake = Fake::start().await;
    let b = fake.broker(symbols());
    let cases = vec![
        (
            GttRequest {
                trigger_type: GttTriggerType::Oco,
                stoploss: 9.0,
                target: 14.0,
                ..single()
            },
            "OCO",
        ),
        (
            GttRequest {
                price: 0.0,
                ..single()
            },
            "limit price",
        ),
        (
            GttRequest {
                key: QuoteKey::new("NSE_INDEX", "NIFTY"),
                ..single()
            },
            "index",
        ),
        (
            GttRequest {
                key: QuoteKey::new("NSE", "NOSUCH"),
                ..single()
            },
            "master contract",
        ),
        (
            GttRequest {
                expires_at: Some("2026-11-16".into()),
                ..single()
            },
            "expiry",
        ),
        (
            GttRequest {
                trigger_price: 0.0,
                triggerprice_tg: 0.0,
                ..single()
            },
            "trigger price",
        ),
        (
            GttRequest {
                pricetype: PriceType::Sl,
                ..single()
            },
            "LIMIT or MARKET",
        ),
        (
            GttRequest {
                quantity: 0,
                ..single()
            },
            "whole number",
        ),
    ];
    for (req, words) in cases {
        let e = gtt::place_gtt_on(&b, &session(), &req, today())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, AppError::Refused(m) if m.contains(words)),
            "{}: {:?}",
            words,
            e
        );
    }
    assert!(fake.seen().is_empty(), "nothing is sent");
}

/// Web test_pocketful_account.py::test_a_refused_gtt_reads_as_pocketfuls_reason,
/// and a session Pocketful refuses reads the same way.
#[tokio::test]
async fn a_refused_gtt_reads_as_pocketfuls_reason() {
    for (reply, want) in [
        (
            answer("create_error"),
            "Pocketful refused to place the GTT: `order` `product` is invalid",
        ),
        (
            Reply::Json(401, doc("unauthorised")),
            "Pocketful refused to place the GTT: Request Unauthorised",
        ),
    ] {
        let fake = Fake::start().await;
        fake.on(GTT_PATH, vec![reply]);
        let b = fake.broker(symbols());

        let e = gtt::place_gtt_on(&b, &session(), &single(), today())
            .await
            .unwrap_err();

        assert!(matches!(&e, AppError::Refused(m) if m == want), "{:?}", e);
    }
}

/// Web test_pocketful_account.py::test_an_unconfirmed_create_is_not_retried_and_says_to_check:
/// a reply lost after sending, one that cannot be read and a success
/// without an id leave the GTT unknown, sent once.
#[tokio::test]
async fn an_unconfirmed_create_is_not_retried_and_says_to_check() {
    for (reply, lost) in [
        (Reply::Cut, true),
        (Reply::Text(504, "<html>504 Gateway Time-out</html>"), false),
        (
            Reply::Json(
                200,
                json!({"data": {}, "message": "GTT created successfully", "status": "success"}),
            ),
            false,
        ),
    ] {
        let fake = Fake::start().await;
        fake.on(GTT_PATH, vec![reply.clone()]);
        let b = fake.broker(symbols());

        let e = gtt::place_gtt_on(&b, &session(), &single(), today())
            .await
            .unwrap_err();

        match &e {
            AppError::Uncertain(u) => {
                assert!(u.message.contains(CHECK_BEFORE_PLACING), "{:?}", u);
                assert_eq!(u.unanswered, lost, "{:?}", reply);
                assert!(!u.internal);
            }
            other => panic!("{:?}: {:?}", reply, other),
        }
        assert_eq!(fake.seen().len(), 1, "never sent twice: {:?}", reply);
    }
}

/// Desktop (CLAUDE.md, three outcomes; the web reads both as refusals): a
/// server error, even with Pocketful's reason, and an answer that is
/// neither success nor Pocketful's failure leave a create unknown.
#[tokio::test]
async fn a_create_with_no_definite_answer_is_uncertain() {
    for (reply, words) in [
        (
            Reply::Json(
                500,
                json!({"data": {}, "error_code": 45000, "status": "error",
                    "message": "Error from backend: (500)-request timed out"}),
            ),
            "Pocketful answered with an error (Request timed out)",
        ),
        (Reply::Json(200, json!({})), "did not say whether it worked"),
    ] {
        let fake = Fake::start().await;
        fake.on(GTT_PATH, vec![reply.clone()]);
        let b = fake.broker(symbols());

        let e = gtt::place_gtt_on(&b, &session(), &single(), today())
            .await
            .unwrap_err();

        assert!(
            matches!(&e, AppError::Uncertain(u)
                if u.message.contains(words) && u.message.contains(CHECK_BEFORE_PLACING)),
            "{:?}: {:?}",
            reply,
            e
        );
        assert_eq!(fake.seen().len(), 1);
    }
}

/// Desktop: a create, change or cancel that never reached Pocketful is
/// refused and says so (the web cannot tell, and says it may have been
/// made); a session without a client id sends nothing (web `NO_CLIENT_ID`).
#[tokio::test]
async fn a_write_that_never_left_is_refused() {
    let b = unreachable_broker(closed_base().await);

    let placed = gtt::place_gtt_on(&b, &session(), &single(), today())
        .await
        .unwrap_err();
    let cancelled = gtt::cancel_gtt(&b, &session(), GTT_ID).await.unwrap_err();

    assert!(
        matches!(&placed, AppError::Refused(m)
            if m.contains("could not reach Pocketful, so the GTT was not placed")),
        "{:?}",
        placed
    );
    assert!(
        matches!(&cancelled, AppError::Refused(m) if m.contains("was not cancelled")),
        "{:?}",
        cancelled
    );

    let fake = Fake::start().await;
    let b = fake.broker(symbols());
    let anonymous = AuthToken::new("jwt.abc.def");
    for e in [
        gtt::place_gtt_on(&b, &anonymous, &single(), today())
            .await
            .unwrap_err(),
        gtt::modify_gtt(&b, &anonymous, HELD_ID, &modify())
            .await
            .unwrap_err(),
        gtt::cancel_gtt(&b, &anonymous, GTT_ID).await.unwrap_err(),
        gtt::get_gtt_book(&b, &anonymous, false).await.unwrap_err(),
    ] {
        assert!(
            matches!(&e, AppError::Refused(m) if m == NO_CLIENT_ID),
            "{:?}",
            e
        );
    }
    assert!(fake.seen().is_empty());
}

/// Web test_pocketful_account.py::test_a_modify_keeps_the_rules_expiry_and_sends_its_id
#[tokio::test]
async fn a_modify_keeps_the_rules_expiry_and_sends_its_id() {
    let fake = Fake::start().await;
    fake.on(&book_path(), vec![book(vec![doc("rule")])]);
    fake.on(GTT_PATH, vec![answer("modified")]);
    let b = fake.broker(symbols());

    let changed = gtt::modify_gtt(&b, &session(), HELD_ID, &modify())
        .await
        .unwrap();

    assert_eq!(changed.trigger_id, HELD_ID);
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    let (read, write) = (&seen[0], &seen[1]);
    assert_eq!(
        (read.method.as_str(), read.path.as_str()),
        ("GET", book_path().as_str())
    );
    assert_eq!(
        read.query_pairs(),
        [("client_id".to_string(), CLIENT.to_string())]
    );
    assert_eq!(
        (write.method.as_str(), write.path.as_str()),
        ("PUT", GTT_PATH)
    );
    let body = write.json();
    assert_eq!(
        (&body["id"], &body["expiry_time"], &body["action_type"]),
        (
            &json!(HELD_ID),
            &json!("2022-10-17"),
            &json!("single_order")
        )
    );
    assert_eq!(
        (&body["order"]["price"], &body["order"]["trigger_price"]),
        (&json!(3600.0), &json!(3605.0))
    );
    assert_eq!(body["order"]["device"], "web");
}

/// Web test_pocketful_account.py::test_a_modify_that_cannot_apply_is_refused_before_writing
#[tokio::test]
async fn a_modify_that_cannot_apply_is_refused_before_writing() {
    let mut oco = doc("rule");
    oco["action_type"] = json!("oco_order");
    let cases = vec![
        (vec![], modify(), "no longer active"),
        (
            vec![rule_with("status", json!("Triggered"))],
            modify(),
            "active",
        ),
        (
            vec![doc("rule")],
            GttRequest {
                key: QuoteKey::new("NSE", "IDEA"),
                ..modify()
            },
            "cannot be changed",
        ),
        (
            vec![doc("rule")],
            GttRequest {
                action: Action::Sell,
                ..modify()
            },
            "cannot be changed",
        ),
        // Same token and side on another exchange: tokens are per exchange.
        (
            vec![rule_with("exchange", json!("BSE"))],
            modify(),
            "cannot be changed",
        ),
        (vec![oco], modify(), "not a single trigger"),
    ];
    for (rules, req, words) in cases {
        let fake = Fake::start().await;
        fake.on(&book_path(), vec![book(rules)]);
        let b = fake.broker(symbols());

        let e = gtt::modify_gtt(&b, &session(), HELD_ID, &req)
            .await
            .unwrap_err();

        let gone = words.contains("active");
        assert!(
            match &e {
                AppError::NotFound(m) => gone && m.contains(words),
                AppError::Refused(m) => !gone && m.contains(words),
                _ => false,
            },
            "{}: {:?}",
            words,
            e
        );
        let methods: Vec<String> = fake.seen().into_iter().map(|s| s.method).collect();
        assert_eq!(methods, ["GET"], "{}", words);
    }
}

/// Web test_pocketful_account.py::test_a_modify_pocketful_refuses_reads_without_its_backend_code
#[tokio::test]
async fn a_modify_pocketful_refuses_reads_without_its_backend_code() {
    let fake = Fake::start().await;
    fake.on(&book_path(), vec![book(vec![doc("rule")])]);
    fake.on(GTT_PATH, vec![Reply::Json(500, doc("modify_refused"))]);
    let b = fake.broker(symbols());

    let e = gtt::modify_gtt(&b, &session(), HELD_ID, &modify())
        .await
        .unwrap_err();

    assert!(
        matches!(&e, AppError::Refused(m)
            if m == "Pocketful refused to change the GTT: Cannot modify, no data found with this id"),
        "{:?}",
        e
    );
}

/// Web (gtt_api.modify_gtt_order and cancel_gtt_order): a change or cancel
/// whose reply was lost or unreadable may have been made, and says so.
#[tokio::test]
async fn a_change_or_cancel_without_an_answer_says_to_check() {
    for reply in [Reply::Cut, Reply::Text(502, "<html>502 Bad Gateway</html>")] {
        let fake = Fake::start().await;
        fake.on(&book_path(), vec![book(vec![doc("rule")])]);
        fake.on(GTT_PATH, vec![reply.clone()]);
        fake.on(&rule_path(GTT_ID), vec![reply.clone()]);
        let b = fake.broker(symbols());

        let changed = gtt::modify_gtt(&b, &session(), HELD_ID, &modify())
            .await
            .unwrap_err();
        let cancelled = gtt::cancel_gtt(&b, &session(), GTT_ID).await.unwrap_err();

        for (e, words) in [
            (changed, "the change may or may not have been made"),
            (cancelled, "the GTT may or may not have been cancelled"),
        ] {
            assert!(
                matches!(&e, AppError::Uncertain(u) if u.message.contains(words)
                    && u.message.contains("Check the GTT list before trying again")),
                "{:?}: {:?}",
                reply,
                e
            );
        }
        assert_eq!(fake.to(GTT_PATH).len(), 1, "{:?}", reply);
        assert_eq!(fake.to(&rule_path(GTT_ID)).len(), 1, "{:?}", reply);
    }
}

/// Web test_pocketful_account.py::test_a_cancel_deletes_by_client_and_rule_id
#[tokio::test]
async fn a_cancel_deletes_by_client_and_rule_id() {
    let fake = Fake::start().await;
    fake.on(&rule_path(CANCEL_ID), vec![answer("cancelled")]);
    let b = fake.broker(symbols());

    let cancelled = gtt::cancel_gtt(&b, &session(), CANCEL_ID).await.unwrap();

    assert_eq!(cancelled.trigger_id, CANCEL_ID);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "DELETE");
    assert_eq!(
        seen[0].path,
        "/api/v1/event/gtt/DEMO1/e06f52ab-6cf4-492d-a014-c6e0e5843a93"
    );
}

/// Web test_pocketful_account.py::test_a_cancel_of_an_unknown_rule_reads_plainly
#[tokio::test]
async fn a_cancel_of_an_unknown_rule_reads_plainly() {
    let fake = Fake::start().await;
    fake.on(
        &rule_path(GTT_ID),
        vec![Reply::Json(500, doc("cancel_error"))],
    );
    let b = fake.broker(symbols());

    let e = gtt::cancel_gtt(&b, &session(), GTT_ID).await.unwrap_err();

    assert!(
        matches!(&e, AppError::Refused(m)
            if m == "Pocketful refused to cancel the GTT: Event id not found"),
        "{:?}",
        e
    );
}

/// Web test_pocketful_account.py::test_an_id_pocketful_did_not_issue_never_reaches_a_path
#[tokio::test]
async fn an_id_pocketful_did_not_issue_never_reaches_a_path() {
    let fake = Fake::start().await;
    let b = fake.broker(symbols());
    let long = "x".repeat(65);
    for bad in ["", "../orders", "a/b", "id?x=1", long.as_str()] {
        assert!(!valid_trigger_id(bad), "{}", bad);
        for e in [
            gtt::cancel_gtt(&b, &session(), bad).await.unwrap_err(),
            gtt::modify_gtt(&b, &session(), bad, &modify())
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(&e, AppError::Validation(m) if m == BAD_ID),
                "{}: {:?}",
                bad,
                e
            );
        }
    }
    assert!(valid_trigger_id(GTT_ID));
    assert!(fake.seen().is_empty());
}

/// Web test_pocketful_account.py::test_the_book_maps_the_documented_rule
#[tokio::test]
async fn the_book_maps_the_documented_rule() {
    let fake = Fake::start().await;
    fake.on(&book_path(), vec![book(vec![doc("rule")])]);
    let b = fake.broker(symbols());

    let rows = gtt::get_gtt_book(&b, &session(), false).await.unwrap();

    assert_eq!(
        rows,
        [GttOrder {
            trigger_id: HELD_ID.into(),
            trigger_type: "single".into(),
            status: "active".into(),
            symbol: "TCS".into(),
            exchange: "NSE".into(),
            trigger_prices: vec![3611.45],
            last_price: 0.0,
            legs: vec![GttLeg {
                action: "BUY".into(),
                quantity: 1,
                price: 3611.5,
                pricetype: "LIMIT".into(),
                product: "CNC".into(),
            }],
            created_at: "2021-10-17 18:14:01".into(),
            updated_at: "2021-10-17 18:18:12".into(),
            expires_at: "2022-10-17".into(),
        }]
    );
}

/// Web test_pocketful_account.py::test_the_book_hides_finished_rules_unless_asked
#[tokio::test]
async fn the_book_hides_finished_rules_unless_asked() {
    let fake = Fake::start().await;
    fake.on(
        &book_path(),
        vec![book(vec![rule_with("status", json!("Triggered"))])],
    );
    let b = fake.broker(symbols());

    assert!(gtt::get_gtt_book(&b, &session(), false)
        .await
        .unwrap()
        .is_empty());
    let history = gtt::get_gtt_book(&b, &session(), true).await.unwrap();
    assert_eq!(history[0].status, "triggered");
}

/// Web test_pocketful_account.py::test_no_rules_is_an_empty_book_not_an_error
#[tokio::test]
async fn no_rules_is_an_empty_book_not_an_error() {
    let fake = Fake::start().await;
    fake.on(&book_path(), vec![Reply::Json(500, doc("no_data"))]);
    let b = fake.broker(symbols());

    assert!(gtt::get_gtt_book(&b, &session(), false)
        .await
        .unwrap()
        .is_empty());
}

/// Web test_pocketful_account.py::test_a_book_pocketful_refuses_is_an_error
#[tokio::test]
async fn a_book_pocketful_refuses_is_an_error() {
    let fake = Fake::start().await;
    fake.on(&book_path(), vec![Reply::Json(401, doc("unauthorised"))]);
    let b = fake.broker(symbols());

    let e = gtt::get_gtt_book(&b, &session(), false).await.unwrap_err();

    assert!(
        matches!(&e, AppError::Broker(m)
            if m == "Pocketful refused to list the GTT: Request Unauthorised"),
        "{:?}",
        e
    );
}

/// Web gtt_data.gtt_rows: a list keeps its rules, a single rule with an id
/// is one, anything else is none.
#[test]
fn the_rules_are_read_from_a_list_or_one_rule() {
    let rule = doc("rule");
    let one = std::slice::from_ref(&rule);
    assert_eq!(gtt_rows(Some(&json!([rule.clone(), "x", 1]))), one);
    assert_eq!(gtt_rows(Some(&rule)), one);
    assert!(gtt_rows(Some(&json!({"id": ""}))).is_empty());
    assert!(gtt_rows(Some(&json!("x"))).is_empty());
    assert!(gtt_rows(None).is_empty());
}

/// Web gtt_data.broker_reason: Pocketful's backend code goes, and the
/// first letter is capitalised.
#[test]
fn pocketfuls_reason_loses_its_backend_code() {
    for (raw, want) in [
        (
            "Error from backend: (500)-event id not found",
            "Event id not found",
        ),
        (
            "error from backend: (1600)-cannot modify, no data found with this id",
            "Cannot modify, no data found with this id",
        ),
        ("Error from backend: no gtt data found", "No gtt data found"),
        (
            "`order` `product` is invalid",
            "`order` `product` is invalid",
        ),
        ("  ", ""),
        ("", ""),
    ] {
        assert_eq!(broker_reason(raw), want, "{}", raw);
    }
}

/// Web test_pocketful_account.py::test_the_gtt_services_read_the_plugins_answers,
/// through the `Broker` trait the GTT service calls (the service's own
/// tests pin how it answers each outcome on the wire): Pocketful has GTT,
/// and a place, a refused place, a cancel and the book come back.
#[tokio::test]
async fn the_gtt_service_reads_the_adapters_answers() {
    let fake = Fake::start().await;
    fake.on(GTT_PATH, vec![answer("created"), answer("create_error")]);
    fake.on(&rule_path(GTT_ID), vec![answer("cancelled")]);
    fake.on(&book_path(), vec![book(vec![doc("rule")])]);
    let b: Box<dyn Broker> = Box::new(fake.broker(symbols()));

    assert!(b.capabilities().gtt);
    let placed = b.place_gtt(&session(), &single()).await.unwrap();
    let refused = b.place_gtt(&session(), &single()).await.unwrap_err();
    let cancelled = b.cancel_gtt(&session(), GTT_ID).await.unwrap();
    let listed = b.get_gtt_book(&session(), false).await.unwrap();

    assert_eq!(placed.trigger_id, GTT_ID);
    assert!(matches!(refused, AppError::Refused(_)), "{:?}", refused);
    assert_eq!(cancelled.trigger_id, CANCEL_ID);
    assert_eq!(listed[0].trigger_id, HELD_ID);
}

/// No GTT error carries the session's access token into a log or an
/// answer (CLAUDE.md "Security model"; the broker-integration skill's
/// secrets check): a fake that cuts or refuses, and a closed port.
#[tokio::test]
async fn gtt_errors_never_carry_the_access_token() {
    const S: &str = "SENTINELq7Zx";
    let auth = AuthToken::new(S).with_user_id(CLIENT);
    let (logs, _guard) = capture();
    let fake = Fake::start().await;
    fake.on(GTT_PATH, vec![Reply::Cut]);
    fake.on(&book_path(), vec![Reply::Json(401, doc("unauthorised"))]);
    fake.on(
        &rule_path(GTT_ID),
        vec![Reply::Text(502, "<html>502 Bad Gateway</html>")],
    );
    let b = fake.broker(symbols());
    let closed = unreachable_broker(closed_base().await);

    let errors = [
        gtt::place_gtt_on(&b, &auth, &single(), today())
            .await
            .unwrap_err(),
        gtt::modify_gtt(&b, &auth, HELD_ID, &modify())
            .await
            .unwrap_err(),
        gtt::cancel_gtt(&b, &auth, GTT_ID).await.unwrap_err(),
        gtt::get_gtt_book(&b, &auth, true).await.unwrap_err(),
        gtt::place_gtt_on(&closed, &auth, &single(), today())
            .await
            .unwrap_err(),
        gtt::get_gtt_book(&closed, &auth, false).await.unwrap_err(),
    ];

    let text = logs.text();
    assert!(
        !text.is_empty(),
        "nothing was captured: the check would be vacuous"
    );
    assert!(!text.contains(S), "{}", text);
    for e in &errors {
        for shown in [e.to_string(), format!("{:?}", e), e.client_message()] {
            assert!(!shown.contains(S), "{}", shown);
        }
    }
}
