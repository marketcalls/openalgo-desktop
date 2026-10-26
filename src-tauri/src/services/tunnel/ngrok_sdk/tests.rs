use super::*;
use crate::services::tunnel::fake::capture_logs;
use tokio::net::TcpListener;

const TOKEN: &str = "2abcDEFghiJKLmnoPQRstu_vwxYZ0123456789";

fn request() -> OpenRequest {
    OpenRequest {
        authtoken: crate::security::Secret::new(TOKEN),
        domain: Some("desk.ngrok-free.app".into()),
    }
}

#[test]
fn ngrok_error_codes_are_sorted_by_what_the_trader_can_do() {
    let code = |c: &str| by_code(Some(c));
    for c in [
        "ERR_NGROK_105",
        "ERR_NGROK_106",
        "ERR_NGROK_107",
        "ERR_NGROK_300",
        "ERR_NGROK_4018",
    ] {
        assert_eq!(code(c), OpenError::Authtoken, "{}", c);
    }
    for c in [
        "ERR_NGROK_313",
        "ERR_NGROK_319",
        "ERR_NGROK_320",
        "ERR_NGROK_326",
    ] {
        assert_eq!(code(c), OpenError::Domain, "{}", c);
    }
    for c in ["ERR_NGROK_108", "ERR_NGROK_334", "ERR_NGROK_348"] {
        assert_eq!(code(c), OpenError::InUse, "{}", c);
    }
    for c in ["ERR_NGROK_103", "ERR_NGROK_123", "ERR_NGROK_247"] {
        assert_eq!(code(c), OpenError::Account, "{}", c);
    }
    for c in ["ERR_NGROK_9999", "ERR_NGROK_", "ngrok said no", ""] {
        assert_eq!(code(c), OpenError::Other, "{}", c);
    }
    assert_eq!(by_code(None), OpenError::Other);
    assert_eq!(code(" ERR_NGROK_107 "), OpenError::Authtoken);
}

#[test]
fn only_the_problems_time_can_fix_are_retried() {
    assert!(!OpenError::Authtoken.retries());
    assert!(!OpenError::Domain.retries());
    assert!(!OpenError::Account.retries());
    assert!(OpenError::InUse.retries());
    assert!(OpenError::Unreachable.retries());
    assert!(OpenError::Other.retries());
}

/// ngrok's answer to a malformed authtoken repeats it ("Your authtoken:
/// ..."); what reaches the log does not.
#[test]
fn an_answer_from_ngrok_is_logged_without_the_authtoken() {
    let msg = format!(
        "The authtoken you specified does not look like a proper ngrok authtoken.\n\
         Your authtoken: {}\nInstructions are on your dashboard. Got {} again.",
        TOKEN, TOKEN
    );
    let out = redacted(&msg, TOKEN);
    assert!(!out.contains(TOKEN), "{}", out);
    assert!(
        !out.to_ascii_lowercase().contains("your authtoken"),
        "{}",
        out
    );
    assert!(out.contains("Instructions are on your dashboard"));
    assert_eq!(redacted("plain", ""), "plain");
}

/// The real SDK, offline: its server is a local socket that hangs up.
/// Building its TLS client must not panic (rustls picks its crypto
/// provider from the features built, and two would leave it none), and a
/// server that cannot be reached is "unreachable", with nothing about the
/// authtoken in the log.
#[tokio::test(flavor = "current_thread")]
async fn the_sdk_against_a_server_that_hangs_up_is_unreachable() {
    let logs = capture_logs();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            drop(sock);
        }
    });
    let provider = NgrokProvider::at(addr.to_string());
    let outcome = provider.open(&request()).await;
    assert!(matches!(outcome, Err(OpenError::Unreachable)));
    // Nobody listening at all.
    server.abort();
    let _ = server.await;
    let provider = NgrokProvider::at(addr.to_string());
    assert!(matches!(
        provider.open(&request()).await,
        Err(OpenError::Unreachable)
    ));
    let text = logs.text();
    assert!(text.contains("Could not reach ngrok"), "{}", text);
    assert!(!text.contains(TOKEN), "{}", text);
}
