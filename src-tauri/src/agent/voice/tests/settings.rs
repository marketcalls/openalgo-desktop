//! The voice configuration routes: the payload, the checks before a write,
//! and the write-only key (the web's R-031 to R-034, S-051 to S-061).

use super::{put_voice, voice_harness, KEY};
use crate::agent::voice::config::NOT_A_KEY;
use crate::agent::voice::confirm::NAME_REFUSED;
use crate::server::routes::{self, Access};
use axum::http::StatusCode;
use serde_json::json;

/// The eight route-methods of the web, each needing the signed-in user.
#[tokio::test]
async fn the_voice_routes_are_the_webs_eight() {
    let routes: Vec<String> = routes::table()
        .iter()
        .filter(|r| r.path.starts_with("/agent/api/voice"))
        .inspect(|r| assert_eq!(r.access, Access::User, "{}", r.path))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let mut want = vec![
        "GET /agent/api/voice",
        "PUT /agent/api/voice",
        "PUT /agent/api/voice/key",
        "DELETE /agent/api/voice/key",
        "POST /agent/api/voice/test",
        "POST /agent/api/voice/transcript",
        "POST /agent/api/voice/approve",
        "POST /agent/api/voice/session",
    ];
    want.sort_unstable();
    let mut got: Vec<&str> = routes.iter().map(String::as_str).collect();
    got.sort_unstable();
    assert_eq!(got, want);
}

#[tokio::test]
async fn the_configuration_carries_every_setting_and_never_the_key() {
    let (h, _fake) = voice_harness(vec![]).await;
    let (s, v) = h.json("GET", "/agent/api/voice", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    let d = &v["data"];
    let shipped = json!({
        "voice_enabled": false, "voice_provider": "openai", "voice_model": "gpt-live-1",
        "voice_speaker": "marin", "voice_agent_name": "Vega", "voice_trading_enabled": false,
        "voice_confirm_window_seconds": 30, "voice_idle_timeout_seconds": 180,
    });
    for (k, want) in shipped.as_object().unwrap() {
        assert_eq!(&d[k], want, "{}", k);
    }
    assert_eq!(d["defaults"], shipped);
    assert_eq!(d["trading_enabled_master"], false);
    assert_eq!(d["trading_effective"], false);
    assert_eq!(
        d["speakers"],
        json!(["marin", "cedar", "alloy", "echo", "shimmer"])
    );
    assert_eq!(d["known_models"], json!(["gpt-live-1"]));
    assert_eq!(
        d["providers"],
        json!([{"id": "openai", "label": "OpenAI", "default_model": "gpt-live-1"}])
    );
    assert_eq!(
        d["key"],
        json!({"has_value": false, "fingerprint": null, "last_used_at": null})
    );
    let (s, v) = h
        .json(
            "PUT",
            "/agent/api/voice/key",
            Some(json!({"api_key": format!("  {}  ", KEY)})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Voice key stored");
    assert_eq!(v["data"]["key"]["has_value"], true);
    assert_eq!(
        v["data"]["key"]["fingerprint"],
        crate::agent::models::secrets::fingerprint(KEY)
    );
    let (_, raw) = h.call(h.request("GET", "/agent/api/voice", None)).await;
    assert!(!raw.contains(KEY), "the key never comes back");
    // Trading is effective only with both switches on.
    let (s, _) = h
        .json(
            "PUT",
            "/agent/api/settings",
            Some(json!({"trading_enabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let v = put_voice(&h, json!({"voice_trading_enabled": true})).await;
    assert_eq!(v["data"]["trading_enabled_master"], true);
    assert_eq!(v["data"]["trading_effective"], true);
}

#[tokio::test]
async fn an_update_is_checked_before_anything_is_written() {
    let (h, _fake) = voice_harness(vec![]).await;
    let refused = |body: serde_json::Value| {
        let h = &h;
        async move {
            let (s, v) = h.json("PUT", "/agent/api/voice", Some(body)).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
            v["message"].as_str().unwrap().to_string()
        }
    };
    assert_eq!(refused(json!({})).await, "Nothing to update");
    assert_eq!(
        refused(json!({"voice_order_phrase": "x", "api_key": "y"})).await,
        "Unknown voice settings: api_key, voice_order_phrase"
    );
    // A bad name refuses the whole request: the switch beside it stays off.
    assert_eq!(
        refused(json!({"voice_enabled": true, "voice_agent_name": "R2D2"})).await,
        NAME_REFUSED
    );
    assert_eq!(
        refused(json!({"voice_speaker": "nova"})).await,
        "Voice must be one of: marin, cedar, alloy, echo, shimmer"
    );
    assert_eq!(
        refused(json!({"voice_model": "  "})).await,
        "The voice model cannot be empty"
    );
    assert_eq!(
        refused(json!({"voice_provider": "azure"})).await,
        "Unknown voice provider 'azure'. Accepted: openai"
    );
    for idle in [json!(29), json!(3601), json!(0), json!(-1), json!("soon")] {
        refused(json!({ "voice_idle_timeout_seconds": idle })).await;
    }
    for window in [json!(4), json!(301)] {
        refused(json!({ "voice_confirm_window_seconds": window })).await;
    }
    let (_, v) = h.json("GET", "/agent/api/voice", None).await;
    assert_eq!(v["data"]["voice_enabled"], false, "nothing was written");
    let v = put_voice(
        &h,
        json!({
            "voice_agent_name": "  Tara   Rose ", "voice_speaker": "CEDAR",
            "voice_provider": "OpenAI", "voice_model": " gpt-live-2 ",
            "voice_idle_timeout_seconds": 600, "voice_confirm_window_seconds": "45",
        }),
    )
    .await;
    let d = &v["data"];
    assert_eq!(d["voice_agent_name"], "Tara Rose");
    assert_eq!(d["voice_speaker"], "cedar");
    assert_eq!(d["voice_provider"], "openai");
    assert_eq!(
        d["voice_model"], "gpt-live-2",
        "free text, not the known list"
    );
    assert_eq!(d["voice_idle_timeout_seconds"], 600);
    assert_eq!(d["voice_confirm_window_seconds"], 45);
    // An empty name restores the shipped one.
    let v = put_voice(&h, json!({"voice_agent_name": ""})).await;
    assert_eq!(v["data"]["voice_agent_name"], "Vega");
}

#[tokio::test]
async fn the_key_is_write_only_and_clearing_it_is_idempotent() {
    let (h, _fake) = voice_harness(vec![]).await;
    for bad in [
        json!({}),
        json!({"api_key": ""}),
        json!({"api_key": "short-key"}),
    ] {
        let (s, v) = h.json("PUT", "/agent/api/voice/key", Some(bad)).await;
        assert_eq!(
            (s, v["message"].as_str()),
            (StatusCode::BAD_REQUEST, Some(NOT_A_KEY))
        );
    }
    let (s, _) = h
        .json("PUT", "/agent/api/voice/key", Some(json!({"api_key": KEY})))
        .await;
    assert_eq!(s, StatusCode::OK);
    for _ in 0..2 {
        let (s, v) = h.json("DELETE", "/agent/api/voice/key", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["message"], "Voice key cleared");
        assert_eq!(v["data"]["key"]["has_value"], false);
    }
}

/// A fresh start or account reset drops the voice key with every other
/// Agent secret (it was sealed with the key that is gone).
#[tokio::test]
async fn a_wipe_takes_the_voice_key() {
    let (h, _fake) = voice_harness(vec![]).await;
    h.json("PUT", "/agent/api/voice/key", Some(json!({"api_key": KEY})))
        .await;
    let conn = h.ctx().sqlite.conn().unwrap();
    crate::agent::models::secrets::wipe(&conn).unwrap();
    drop(conn);
    let (_, v) = h.json("GET", "/agent/api/voice", None).await;
    assert_eq!(v["data"]["key"]["has_value"], false);
}
