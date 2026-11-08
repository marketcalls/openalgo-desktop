//! Select Broker never shows text taken from its address (web eec700046,
//! `BrokerSelect.test.tsx` "never shows text from the address"). A failed
//! sign-in sends the browser back to `/broker?error=<id>&broker=<broker>`;
//! the sentence stays in OpenAlgo and is read once, by the browser session
//! it came back with (`session::sign_in_failures`).

use super::*;

const NOT_STARTED: &str = "This broker sign-in was not started from OpenAlgo or has expired. Start the broker login again from OpenAlgo.";

/// A Zerodha callback with a state OpenAlgo never issued: it fails with
/// `NOT_STARTED`. Returns where it sends the browser.
async fn forged_callback(h: &H, cookie: Option<&str>) -> String {
    let req = get("/zerodha/callback?status=success&request_token=rt1&state=forged");
    let req = match cookie {
        Some(c) => with_session(req, c, None),
        None => req,
    };
    let (s, headers, _) = h.send(req).await;
    assert_eq!(s, StatusCode::FOUND);
    location(&headers)
}

async fn read_reason(h: &H, cookie: &str, id: &str) -> (StatusCode, Value) {
    let path = format!("/api/broker/sign-in-failure/{}", urlencoding::encode(id));
    h.json(with_session(get(&path), cookie, None)).await
}

fn harness() -> H {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    h
}

/// The address carries an opaque id and no word of the sentence; the page
/// reads the sentence from OpenAlgo, once.
#[tokio::test]
async fn a_failed_sign_in_puts_no_sentence_in_the_address() {
    let h = harness();
    let (cookie, _) = h.session(true);
    let loc = forged_callback(&h, Some(&cookie)).await;
    let id = sign_in_failure_id(&loc);
    assert!(
        !loc.contains("started"),
        "the sentence is in the address: {}",
        loc
    );
    assert!(
        id.len() >= 43
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "not an opaque id: {}",
        id
    );

    let (s, body) = read_reason(&h, &cookie, &id).await;
    assert_eq!(s, StatusCode::OK, "{}", body);
    assert_eq!(body["data"]["message"], NOT_STARTED);
    assert_eq!(body["data"]["broker"], "zerodha");

    let (s, body) = read_reason(&h, &cookie, &id).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "read once: {}", body);
    assert_eq!(
        body["message"],
        "The broker login did not complete. Try again."
    );
}

/// Another browser session cannot read it, and an id that was never
/// issued (a crafted link's text) reads as the fixed sentence.
#[tokio::test]
async fn a_sign_in_reason_belongs_to_the_session_it_came_back_with() {
    let h = harness();
    let (mine, _) = h.session(true);
    let (other, _) = h.session(true);
    let id = sign_in_failure_id(&forged_callback(&h, Some(&mine)).await);

    let (s, _) = read_reason(&h, &other, &id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, body) = read_reason(&h, &mine, &id).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "another session's read used it up: {}",
        body
    );
    assert_eq!(body["data"]["message"], NOT_STARTED);

    let (s, body) = read_reason(&h, &mine, "<b>INJECTED-TEXT</b>").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!body.to_string().contains("INJECTED-TEXT"), "{}", body);
}

/// A callback that came without a cookie (the XTS brokers post back across
/// sites) keeps its reason for the next signed-in session, once.
#[tokio::test]
async fn a_reason_without_a_session_is_read_once_by_the_signed_in_trader() {
    let h = harness();
    let id = sign_in_failure_id(&forged_callback(&h, None).await);
    let (cookie, _) = h.session(true);
    let (s, body) = read_reason(&h, &cookie, &id).await;
    assert_eq!(s, StatusCode::OK, "{}", body);
    assert_eq!(body["data"]["message"], NOT_STARTED);
    let (s, _) = read_reason(&h, &cookie, &id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// The reason is for the signed-in trader only, and logging out drops it.
#[tokio::test]
async fn a_sign_in_reason_needs_the_signed_in_user_and_ends_at_logout() {
    let h = harness();
    let (cookie, _) = h.session(true);
    let id = sign_in_failure_id(&forged_callback(&h, Some(&cookie)).await);

    let (anon, _) = h.session(false);
    let (s, body) = read_reason(&h, &anon, &id).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", body);
    assert!(!body.to_string().contains("started"), "{}", body);

    h.ctx().sessions.clear(); // what a logout does to every browser session
    let (cookie, _) = h.session(true);
    let (s, _) = read_reason(&h, &cookie, &id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
