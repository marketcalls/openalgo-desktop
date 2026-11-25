//! One market-data session for 5paisa XTS's feed, quotes and history (web
//! 5749d4126, marketcalls/openalgo#2484): the user id the socket address
//! takes from the token, the renewed session's cache rule, and which
//! session the feed connects with. End to end:
//! `tests/it/broker_fivepaisaxts_docs/session.rs`.

use super::session::{market_data_user_id, MarketSessions, SharedSession};
use super::streaming::{FeedSource, XtsFeed};
use super::XtsConfig;
use crate::brokers::common::streaming::BrokerFeed;
use crate::security::Secret;
use base64::Engine;
use std::sync::Arc;

fn five() -> &'static XtsConfig {
    &crate::brokers::fivepaisaxts::CONFIG
}

/// A market-data token whose `userID` claim is `claim`, as XTS issues it.
fn jwt(claim: &str) -> String {
    let part = |s: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!(
        "{}.{}.sig",
        part("{\"alg\":\"HS256\"}".into()),
        part(format!("{{\"userID\":\"{claim}\"}}"))
    )
}

/// Only 5paisa XTS's feed connects with the stored session (web #2484),
/// and its own login, when it needs one, goes where history's does.
#[test]
fn only_fivepaisaxts_feeds_on_the_stored_session() {
    for cfg in [
        five(),
        &crate::brokers::jainamxts::CONFIG,
        &crate::brokers::compositedge::CONFIG,
        &crate::brokers::rmoney::CONFIG,
        &crate::brokers::ibulls::CONFIG,
        &crate::brokers::wisdom::CONFIG,
        &crate::brokers::iifl::CONFIG,
    ] {
        let expected = cfg.id == "fivepaisaxts";
        assert_eq!(cfg.hooks.feed_on_stored_session, expected, "{}", cfg.id);
    }
    assert_eq!(five().socket_login_path, "/apimarketdata/auth/login");
}

/// Web `test_market_data_user_id_is_the_claim_before_the_underscore`.
#[test]
fn market_data_user_id_is_the_claim_before_the_underscore() {
    assert_eq!(
        market_data_user_id(&jwt("1048131_856F2F2AF32542B762129")).as_deref(),
        Some("1048131")
    );
    assert_eq!(
        market_data_user_id(&jwt("1048131")).as_deref(),
        Some("1048131")
    );
    assert_eq!(market_data_user_id("not-a-jwt"), None);
    assert_eq!(market_data_user_id(&jwt("_APP")), None);
}

fn shared(token: &str) -> SharedSession {
    SharedSession {
        token: Secret::new(token),
        user_id: "1048131".into(),
    }
}

/// The renewed session follows the cache rules: kept when nothing cleared
/// the slot since its generation was read, never when a sign-in, resume or
/// sign-out did (the renewal belongs to the session that ended).
#[test]
fn a_renewal_is_kept_only_when_nothing_cleared_the_slot() {
    let s = MarketSessions::default();
    let generation = s.generation();
    assert!(s.renewed(shared("t1"), generation));
    assert_eq!(
        s.current().map(|x| x.token.expose().to_string()).as_deref(),
        Some("t1")
    );
    let generation = s.generation();
    s.clear();
    assert!(!s.renewed(shared("stale"), generation));
    assert!(s.current().is_none());
    assert!(s.renewed(shared("t2"), s.generation()));
    assert_eq!(s.current().map(|x| x.user_id).as_deref(), Some("1048131"));
}

/// The address the feed opens after `prepare`.
async fn address(cfg: &'static XtsConfig, source: FeedSource) -> String {
    let mut feed = XtsFeed::new(
        cfg,
        crate::brokers::common::http::client(),
        "https://xts.invalid".into(),
        None,
        source,
    );
    assert!(feed.prepare().await.is_ok(), "{}", cfg.id);
    feed.ws_request()
        .map(|r| r.uri().to_string())
        .unwrap_or_default()
}

/// 5paisa XTS's feed connects with the session quotes renewed before the
/// one stored at sign-in, and takes the user id from the token's claim,
/// else the one stored with it; it logs in for neither.
#[tokio::test]
async fn fivepaisaxts_feed_prefers_the_renewed_session() {
    let stored = FeedSource::stored("tok", "U1");
    let url = address(five(), stored.clone()).await;
    assert!(url.contains("token=tok&userID=U1&"), "{url}");

    let slot = Arc::new(MarketSessions::default());
    assert!(slot.renewed(shared(&jwt("1048131_APP")), slot.generation()));
    let source = FeedSource {
        shared: Some(slot.clone()),
        ..stored
    };
    let url = address(five(), source.clone()).await;
    assert!(
        url.contains(&format!("token={}&", jwt("1048131_APP"))),
        "{url}"
    );
    assert!(url.contains("userID=1048131&"), "{url}");

    // A template member keeps its stored token whatever was renewed.
    let url = address(&crate::brokers::iifl::CONFIG, source).await;
    assert!(url.contains("token=tok&userID=U1&"), "{url}");
}
