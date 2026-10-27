//! What the app window needs for voice, pinned where CI can see it: the
//! page's security policy, the macOS bundle's microphone entries, and the
//! window's microphone rule.

use super::{put_voice, voice_harness};
use crate::agent::voice::window_may_listen;
use serde_json::{json, Value};

/// No host is added for voice, and none may be. The page talks only to
/// this app (the offer goes to `/agent/api/voice/session`, which mints the
/// session server side); the audio and the data channel are WebRTC, which
/// `connect-src` does not govern; the reply plays from a `MediaStream`
/// (`srcObject`), which `media-src` does not govern. Measured in Chromium
/// (WebView2's engine) under this exact policy: microphone, a WebRTC call
/// with an `oai-events` channel and remote audio playback raised no
/// violation.
#[tokio::test]
async fn the_page_policy_admits_no_speech_provider() {
    let (h, _fake) = voice_harness(vec![]).await;
    let resp = tower::ServiceExt::oneshot(
        crate::server::app(h.ctx().clone()),
        h.request("GET", "/agent", None),
    )
    .await
    .unwrap();
    let policy = resp.headers()[axum::http::header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert!(!policy.to_lowercase().contains("openai"), "{}", policy);
    let connect = policy
        .split(';')
        .map(str::trim)
        .find(|d| d.starts_with("connect-src"))
        .unwrap();
    assert_eq!(
        connect,
        "connect-src 'self' ws://127.0.0.1:* ws://localhost:* http://127.0.0.1:* http://localhost:*"
    );
    assert!(
        !policy.contains("media-src"),
        "media follows default-src 'self'"
    );
}

fn plist_value<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let after = plist.split(&format!("<key>{}</key>", key)).nth(1)?;
    let start = after.find('<')?;
    let end = after[start..].find('\n').map_or(after.len(), |e| start + e);
    Some(after[start..end].trim())
}

/// macOS refuses a microphone to an app whose bundle does not say why (it
/// is killed on first use), and a signed app with the hardened runtime also
/// needs the audio-input entitlement. Both are merged by the bundler.
#[test]
fn the_macos_bundle_says_why_it_listens_and_may() {
    let info = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Info.plist"));
    let why = plist_value(info, "NSMicrophoneUsageDescription").unwrap();
    assert!(
        why.starts_with("<string>") && why.ends_with("</string>"),
        "{}",
        why
    );
    assert!(
        why.contains("microphone") && why.contains("voice"),
        "{}",
        why
    );
    let entitlements = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/entitlements.plist"));
    assert_eq!(
        plist_value(entitlements, "com.apple.security.device.audio-input"),
        Some("<true/>")
    );
    let conf: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tauri.conf.json"
    )))
    .unwrap();
    assert_eq!(
        conf["bundle"]["macOS"]["entitlements"],
        "entitlements.plist"
    );
}

/// The window grants the microphone itself only while voice is on.
#[tokio::test]
async fn the_window_listens_only_while_voice_is_on() {
    let (h, _fake) = voice_harness(vec![]).await;
    assert!(!window_may_listen(h.ctx()));
    put_voice(&h, json!({"voice_enabled": true})).await;
    assert!(window_may_listen(h.ctx()));
    put_voice(&h, json!({"voice_enabled": false})).await;
    assert!(!window_may_listen(h.ctx()));
}
