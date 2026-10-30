//! `test/test_flow_webhook_payload.py`, ported: what a webhook body becomes
//! (parity R-50, T-073, X-106..X-111), and the secret check per auth type
//! (R-52, T-072).

use super::*;

fn read(body: &str, form: bool) -> Value {
    let fields = if form {
        urlencoded_fields(body.as_bytes())
    } else {
        Map::new()
    };
    Value::Object(read_payload(body.as_bytes(), &fields))
}

/// X-106: JSON is read whatever the sender declared (the route never looks
/// at the declared type for JSON; it never answers 415).
#[test]
fn json_body_parses_whatever_the_sender_declared() {
    for form in [false, true] {
        assert_eq!(
            read(r#"{"secret": "s", "qty": 10}"#, form),
            json!({"secret": "s", "qty": 10})
        );
    }
}

/// X-107: plain text lands under `message`.
#[test]
fn plain_text_lands_under_message() {
    assert_eq!(
        read("BUY RELIANCE 10", false),
        json!({"message": "BUY RELIANCE 10"})
    );
}

/// X-108: a form body becomes its fields, as text, first value per key.
#[test]
fn form_encoded_body_becomes_fields() {
    assert_eq!(
        read("secret=s&qty=10&qty=11", true),
        json!({"secret": "s", "qty": "10"})
    );
}

/// X-109: JSON that is not an object keeps its text and its value.
#[test]
fn json_that_is_not_an_object_keeps_both_forms() {
    assert_eq!(
        read("[1, 2, 3]", false),
        json!({"message": "[1, 2, 3]", "payload": [1, 2, 3]})
    );
    assert_eq!(read("42", false), json!({"message": "42", "payload": 42}));
}

/// X-110: an empty body is `{}`, not a message.
#[test]
fn empty_body_is_an_empty_dict_not_a_message() {
    for body in ["", "   ", "\n"] {
        assert_eq!(read(body, false), json!({}));
        assert_eq!(read(body, true), json!({}));
    }
}

/// X-111: malformed JSON is kept as text rather than dropped.
#[test]
fn malformed_json_is_kept_as_text_rather_than_dropped() {
    assert_eq!(
        read(r#"{"secret": "s""#, false),
        json!({"message": r#"{"secret": "s""#})
    );
    assert_eq!(read("null", false), json!({"message": "null"}));
}

/// T-073 (decision 24): Python's json.loads reads TradingView's bare
/// `NaN` and `Infinity`, so the body is a dict and its secret is found (a
/// strict JSON reader kept it as text under `message`).
#[test]
fn nan_and_infinity_from_tradingview_are_json() {
    assert_eq!(
        read(r#"{"secret": "s", "price": NaN, "stop": -Infinity}"#, false),
        json!({"secret": "s", "price": "nan", "stop": "-inf"})
    );
}

fn workflow(auth: &str) -> Workflow {
    Workflow {
        id: 1,
        name: "W".into(),
        description: None,
        nodes: json!([]),
        edges: json!([]),
        is_active: true,
        schedule_job_id: None,
        webhook_token: Some("tok".into()),
        webhook_secret: Some(crate::security::Secret::new("right")),
        webhook_enabled: true,
        webhook_auth_type: auth.into(),
        created_at: None,
        updated_at: None,
    }
}

fn call(payload: Value, url_secret: Option<&str>) -> WebhookCall {
    WebhookCall {
        token: "tok".into(),
        payload: payload.as_object().unwrap().clone(),
        url_secret: url_secret.map(String::from),
    }
}

fn refusal(r: Result<(), Answer>) -> (u16, Value, bool) {
    let a = r.unwrap_err();
    (
        a.reply.status,
        a.reply.body["error"].clone(),
        a.wrong_secret,
    )
}

/// T-072, R-52: payload auth pops the secret before the run sees it; URL
/// auth reads `?secret=` and leaves a body `secret` alone; a missing secret
/// is not a wrong one; a secret that is not text is a wrong secret, never a
/// 500 (WEB-DEFECT: the web's compare_digest raised on `123`).
#[test]
fn the_secret_is_checked_as_the_auth_type_says() {
    let w = workflow("payload");
    let mut c = call(json!({"secret": "right", "qty": 1}), Some("ignored"));
    assert!(check_secret(&w, &mut c).is_ok());
    assert_eq!(
        Value::Object(c.payload),
        json!({"qty": 1}),
        "the run never sees it"
    );
    assert_eq!(
        refusal(check_secret(
            &w,
            &mut call(json!({"message": "BUY"}), Some("right"))
        )),
        (401, json!(MISSING_IN_PAYLOAD), false)
    );
    assert_eq!(
        refusal(check_secret(
            &w,
            &mut call(json!({"secret": "wrong"}), None)
        )),
        (401, json!("Invalid webhook secret"), true)
    );
    assert_eq!(
        refusal(check_secret(&w, &mut call(json!({"secret": 123}), None))),
        (401, json!("Invalid webhook secret"), true)
    );
    let u = workflow("url");
    let mut c = call(json!({"secret": "kept"}), Some("right"));
    assert!(check_secret(&u, &mut c).is_ok());
    assert_eq!(c.payload["secret"], "kept");
    assert_eq!(
        refusal(check_secret(
            &u,
            &mut call(json!({"secret": "right"}), None)
        )),
        (
            401,
            json!("Missing webhook secret in URL. Use ?secret=your_secret"),
            false
        )
    );
    assert_eq!(
        refusal(check_secret(&u, &mut call(json!({}), Some("nope")))),
        (401, json!("Invalid webhook secret"), true)
    );
}

/// R-52 (WEB-DEFECT): a workflow with no secret never runs on its token
/// alone (the web skipped authentication when the secret was empty).
#[test]
fn no_secret_never_authenticates() {
    let mut w = workflow("payload");
    w.webhook_secret = None;
    let (status, _, wrong) = refusal(check_secret(&w, &mut call(json!({}), None)));
    assert_eq!((status, wrong), (401, false));
    w.webhook_auth_type = "url".into();
    assert_eq!(
        refusal(check_secret(&w, &mut call(json!({}), Some("x")))).0,
        401
    );
}
