//! The session mint and the key test against a fake realtime server (the
//! web's TestMintingRefusesBeforeItLeavesTheMachine,
//! TestWhatIsPostedAndWhatComesBack and TestTheProbe).

use super::{message_of, mint, put_voice, switch_on, voice_harness, ANSWER, KEY, OFFER};
use crate::agent::voice::mint::{
    COULD_NOT_CONNECT, MINT_TIMEOUT, NOTHING_TO_TEST, NO_KEY, SWITCHED_OFF, UNREACHABLE, UNUSABLE,
};
use crate::agent::voice::providers::voice_provider_spec;
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn refusals_come_before_anything_leaves_the_machine() {
    let (h, fake) = voice_harness(vec![]).await;
    // Off and no key: the switch is named first.
    let (s, _, body) = mint(&h, OFFER).await;
    assert_eq!(
        (s, message_of(&body)),
        (StatusCode::BAD_REQUEST, SWITCHED_OFF.into())
    );
    put_voice(&h, json!({"voice_enabled": true})).await;
    let (_, _, body) = mint(&h, OFFER).await;
    assert_eq!(message_of(&body), NO_KEY);
    switch_on(&h).await;
    for offer in ["hello", r#"{"sdp": "v=0"}"#, "o=- 0 0 IN IP4 0.0.0.0", "42"] {
        let (s, _, body) = mint(&h, offer).await;
        assert_eq!(
            (s, message_of(&body)),
            (StatusCode::BAD_REQUEST, COULD_NOT_CONNECT.into())
        );
    }
    let (s, _, body) = mint(&h, "  \r\n ").await;
    assert_eq!(
        (s, message_of(&body).as_str()),
        (StatusCode::BAD_REQUEST, "A connection offer is required")
    );
    let (s, _, body) = mint(&h, &format!("v=0\r\n{}", "a".repeat(256_001))).await;
    assert_eq!(
        (s, message_of(&body).as_str()),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "That connection offer is too large"
        )
    );
    assert!(fake.calls().is_empty(), "no refusal reached the provider");
}

#[tokio::test]
async fn the_answer_comes_back_as_sdp_and_the_request_carries_only_stored_settings() {
    let (h, fake) = voice_harness(vec![]).await;
    switch_on(&h).await;
    put_voice(
        &h,
        json!({"voice_speaker": "cedar", "voice_agent_name": "Ava"}),
    )
    .await;
    // An LF-only offer whose last terminator was stripped.
    let lf = OFFER.replace("\r\n", "\n");
    let (s, content_type, body) = mint(&h, lf.trim_end()).await;
    assert_eq!(s, StatusCode::OK, "{}", body);
    assert_eq!(content_type, "application/sdp");
    assert_eq!(body, ANSWER);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/v1/live/sessions");
    assert_eq!(call.authorization, format!("Bearer {}", KEY));
    assert!(
        !call.body.to_string().contains(KEY),
        "the key is never in the body"
    );
    let session = &call.body["session"];
    assert_eq!(session["delegation"], json!({"type": "client"}));
    assert_eq!(session["model"], "gpt-live-1");
    assert_eq!(session["audio"]["output"]["voice"], "cedar");
    assert_eq!(call.body["transport"]["type"], "webrtc");
    assert_eq!(call.body["transport"]["sdp"], OFFER, "sent CRLF-terminated");
    let instructions = session["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("You are Ava,"));
    assert!(!instructions.contains("Orders:"), "no spoken trading");
    // A successful mint records that the key was used.
    let (_, v) = h.json("GET", "/agent/api/voice", None).await;
    assert!(v["data"]["key"]["last_used_at"].is_string(), "{}", v);
    // With spoken trading reachable, the order block is added.
    h.json(
        "PUT",
        "/agent/api/settings",
        Some(json!({"trading_enabled": true})),
    )
    .await;
    put_voice(&h, json!({"voice_trading_enabled": true})).await;
    mint(&h, OFFER).await;
    let armed = fake.calls()[1].body["session"]["instructions"].clone();
    assert!(armed
        .as_str()
        .unwrap()
        .contains("ask the trader to confirm"));
}

/// The URL is the provider's constant, never a request's, and the call
/// has its own timeout.
#[test]
fn the_url_is_a_constant_and_the_call_is_bounded() {
    let spec = voice_provider_spec("openai").unwrap();
    assert_eq!(spec.sessions_url, "https://api.openai.com/v1/live/sessions");
    assert_eq!(MINT_TIMEOUT, std::time::Duration::from_secs(20));
    let voice = crate::agent::voice::VoiceModule::default();
    assert_eq!(voice.sessions_url(spec), spec.sessions_url);
}

#[tokio::test]
async fn a_refusal_becomes_a_sentence_and_only_a_server_error_is_retried() {
    let (h, fake) = voice_harness(vec![]).await;
    switch_on(&h).await;
    let cases = [
        (401, "{}", "would not accept your key", 1),
        (403, "{}", "would not accept your key", 1),
        (404, "{}", "cannot use that voice", 1),
        (429, "{}", "slow down", 1),
        (
            400,
            r#"{"error":{"code":"invalid_model"}}"#,
            "cannot speak",
            1,
        ),
        (500, "Internal Server Error", "credit", 2),
    ];
    for (status, body, says, calls) in cases {
        let before = fake.calls().len();
        for _ in 0..calls {
            fake.reply(status, body);
        }
        let (s, _, text) = mint(&h, OFFER).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", status);
        assert!(message_of(&text).contains(says), "{}: {}", status, text);
        assert!(
            !text.contains(body) || body == "{}",
            "the provider's body is not echoed"
        );
        assert_eq!(fake.calls().len() - before, calls, "{}", status);
    }
    // A server error that passes on the retry mints.
    fake.reply(503, "");
    let (s, _, text) = mint(&h, OFFER).await;
    assert_eq!((s, text.as_str()), (StatusCode::OK, ANSWER));
}

#[tokio::test]
async fn an_unusable_answer_or_no_answer_is_refused() {
    let (h, fake) = voice_harness(vec![]).await;
    switch_on(&h).await;
    for body in [
        "{}",
        r#"{"transport": {}}"#,
        "not json",
        r#"{"transport": {"sdp": 7}}"#,
    ] {
        fake.reply(200, body);
        let (_, _, text) = mint(&h, OFFER).await;
        assert_eq!(message_of(&text), UNUSABLE, "{}", body);
    }
    // Nothing listening: the trader is told to check the connection, and
    // nothing of the key or the error reaches the answer.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    h.ctx()
        .agent
        .voice()
        .set_sessions_url(&format!("http://127.0.0.1:{}/v1/live/sessions", port));
    let (s, _, text) = mint(&h, OFFER).await;
    assert_eq!(
        (s, message_of(&text)),
        (StatusCode::BAD_REQUEST, UNREACHABLE.into())
    );
    assert!(!text.contains(KEY));
}

#[tokio::test]
async fn the_key_test_works_before_voice_is_on_and_reports_a_refusal() {
    let (h, fake) = voice_harness(vec![]).await;
    let test = || {
        let h = &h;
        async move {
            let (s, v) = h
                .json("POST", "/agent/api/voice/test", Some(json!({})))
                .await;
            assert_eq!(s, StatusCode::OK, "{}", v);
            v
        }
    };
    let v = test().await;
    assert_eq!(
        (v["ok"].as_bool(), v["message"].as_str()),
        (Some(false), Some(NOTHING_TO_TEST))
    );
    assert_eq!(
        (v["model"].as_str(), v["latency_ms"].as_u64()),
        (Some("gpt-live-1"), Some(0))
    );
    assert!(fake.calls().is_empty());
    h.json("PUT", "/agent/api/voice/key", Some(json!({"api_key": KEY})))
        .await;
    let v = test().await;
    assert_eq!(v["ok"], true, "voice is still off: {}", v);
    assert_eq!(v["message"], "gpt-live-1 answered. Your key works.");
    assert!(v["data"]["key"]["last_used_at"].is_string());
    let probe = &fake.calls()[0].body["transport"]["sdp"];
    assert_eq!(probe, crate::agent::voice::mint::PROBE_OFFER);
    fake.reply(401, "{}");
    let v = test().await;
    assert_eq!(v["ok"], false);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("would not accept your key"));
    assert_eq!(v["data"]["voice_enabled"], false);
}
