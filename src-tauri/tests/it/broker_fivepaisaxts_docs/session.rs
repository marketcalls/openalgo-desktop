//! One market-data session for 5paisa XTS's feed, quotes and history (web
//! 5749d4126, marketcalls/openalgo#2484): the feed connects with the
//! session stored at sign-in, or the one quotes and history renewed, read
//! again at every connect, and logs in itself only where history does. The
//! renewed session is a cache under the app's rules. Ported from web
//! test/test_fivepaisaxts_xts_docs.py.
//!
//! Where the desktop keeps a rule of its own the test says so: a feed the
//! broker refuses is held until the next sign-in, where the web retries
//! the same token.

use super::fake::{broker, creds, history, md_token, token, until};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{handshake, FeedConfig, WebSocketManager};
use parking_lot::Mutex;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

/// Web `test_history_logs_in_again_once_on_an_invalid_token` (#2484).
#[tokio::test]
async fn history_logs_in_again_once_on_an_invalid_token() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    let old = md_token(1);
    fake.expired.lock().push(old.clone());
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        ..history("D", (2025, 1, 20), (2025, 1, 20))
    };
    let candles = b.get_history(&token(), &req).await.unwrap();
    assert_eq!(candles.len(), 1);
    let used: Vec<String> = fake
        .lines("ohlc|")
        .iter()
        .map(|l| l.split('|').nth(1).unwrap_or("").to_string())
        .collect();
    assert_eq!(used, [old, md_token(2)]);
}

// ---------------------------------------------------------------------------
// One market-data session for feed, quotes and history (#2484)
// ---------------------------------------------------------------------------

/// A fake market-data socket: records each connection's path and query,
/// opens Engine.IO, and accepts (or refuses) the Socket.IO connect.
async fn socket(log: Arc<Mutex<Vec<String>>>, refuse: bool) -> String {
    use tokio_tungstenite::tungstenite::Message as Frame;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let seen = log.clone();
                let cb = handshake::check(move |req: &Request, resp: Response| {
                    seen.lock().push(format!(
                        "ws|{}|{}",
                        req.uri().path(),
                        req.uri().query().unwrap_or("")
                    ));
                    Ok(resp)
                });
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let open = "0{\"sid\":\"s1\",\"upgrades\":[],\"pingInterval\":25000,\"pingTimeout\":20000,\"maxPayload\":1000000}";
                let _ = ws.send(Frame::Text(open.into())).await;
                loop {
                    match ws.next().await {
                        Some(Ok(Frame::Text(t))) if t == "40" => break,
                        Some(Ok(_)) => continue,
                        _ => return,
                    }
                }
                let answer = if refuse {
                    "44{\"message\":\"Invalid Token\"}"
                } else {
                    "40{\"sid\":\"n1\"}"
                };
                let _ = ws.send(Frame::Text(answer.into())).await;
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    url
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    })
}

fn connections(log: &Mutex<Vec<String>>) -> Vec<String> {
    log.lock()
        .iter()
        .filter(|l| l.starts_with("ws|"))
        .cloned()
        .collect()
}

/// Web `test_feed_connects_with_the_stored_token_and_never_logs_in` and
/// `test_market_data_user_id_is_the_claim_before_the_underscore`: the
/// address carries the token stored at sign-in and the user id its claim
/// names; the only market-data login is the sign-in's.
#[tokio::test]
async fn feed_connects_with_the_stored_token_and_never_logs_in() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    let signed_in = b.authenticate(creds()).await.unwrap();
    let stored = signed_in.feed_token.clone().unwrap();
    let auth = AuthToken::new(signed_in.auth_token.as_str())
        .with_feed(Some(stored.as_str()))
        .with_user_id(signed_in.user_id.as_str());
    let m = manager();
    m.connect(b.create_feed(&auth).unwrap()).await.unwrap();
    until("the socket connect", || !connections(&log).is_empty()).await;
    let first = &connections(&log)[0];
    assert!(
        first.starts_with("ws|/apimarketdata/socket.io/|"),
        "{first}"
    );
    assert!(first.contains(&format!("token={stored}&")), "{first}");
    assert!(first.contains("userID=1048131&"), "{first}");
    assert_eq!(fake.lines("login|"), ["login|/apimarketdata/auth/login"]);
    m.disconnect().await.unwrap();
}

/// Web `test_reconnect_reads_the_stored_token_again`: once quotes renewed
/// the market-data session, the feed's next connect uses it, and logs in
/// for nothing of its own.
#[tokio::test]
async fn reconnect_takes_the_session_quotes_renewed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    m.connect(b.create_feed(&token()).unwrap()).await.unwrap();
    until("the first connect", || connections(&log).len() == 1).await;
    assert!(connections(&log)[0].contains(&format!("token={}&", md_token(1))));

    fake.expired.lock().push(md_token(1));
    b.get_quote(&token(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    m.reconnect();
    until("the second connect", || connections(&log).len() >= 2).await;
    assert!(
        connections(&log)[1].contains(&format!("token={}&", md_token(2))),
        "{:?}",
        connections(&log)
    );
    assert_eq!(fake.logins.load(Ordering::SeqCst), 2, "sign-in and renewal");
    m.disconnect().await.unwrap();
}

/// Web `test_without_a_stored_token_the_feed_logs_in_where_history_does`.
#[tokio::test]
async fn without_a_stored_token_the_feed_logs_in_where_history_does() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    m.connect(b.create_feed(&AuthToken::new("itok")).unwrap())
        .await
        .unwrap();
    until("the socket connect", || !connections(&log).is_empty()).await;
    assert_eq!(
        fake.lines("login|"),
        [
            "login|/apimarketdata/auth/login",
            "login|/apimarketdata/auth/login"
        ]
    );
    assert!(connections(&log)[0].contains(&format!("token={}&", md_token(2))));
    m.disconnect().await.unwrap();
}

/// Web `test_a_failed_connect_with_a_stored_token_raises_instead_of_logging_in`:
/// a refused stored session logs in for nothing. The desktop then holds
/// the feed until the next sign-in, as it does every refused feed, where
/// the web retries the same token.
#[tokio::test]
async fn a_refused_stored_session_is_not_followed_by_a_login() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), true).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    let status = m.watch_status();
    m.connect(b.create_feed(&token()).unwrap()).await.unwrap();
    until("the refusal", || {
        format!("{:?}", *status.borrow()).contains("AuthFailed")
    })
    .await;
    assert_eq!(fake.lines("login|"), ["login|/apimarketdata/auth/login"]);
    m.disconnect().await.unwrap();
}

/// The renewed session is a cache under the app's rules: a renewal still
/// logging in when the trader signs out answers its own call but is not
/// kept, so nothing after the sign-out uses it (CLAUDE.md, TtlCache).
#[tokio::test]
async fn a_renewal_across_a_sign_out_is_not_kept() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    let old = md_token(1);
    fake.expired.lock().push(old.clone());
    let gate = Arc::new(Notify::new());
    *fake.hold_login.lock() = Some(gate.clone());
    let key = QuoteKey::new("NSE", "RELIANCE");
    let t = token();
    let (quote, ()) = tokio::join!(b.get_quote(&t, &key), async {
        until("the renewal's login", || fake.lines("login|").len() == 2).await;
        b.on_logout().await;
        gate.notify_one();
    });
    assert_eq!(quote.unwrap().ltp, 2500.25, "its own call is answered");
    *fake.hold_login.lock() = None;
    fake.expired.lock().clear();
    b.get_quote(&t, &key).await.unwrap();
    let last = fake.lines("quotes|").pop().unwrap_or_default();
    assert_eq!(last, format!("quotes|{old}"), "the renewal was not kept");
}

/// A sign-out forgets a renewed session, and the next session starts from
/// its own stored token.
#[tokio::test]
async fn a_sign_out_forgets_the_renewed_session() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    fake.expired.lock().push(md_token(1));
    let key = QuoteKey::new("NSE", "RELIANCE");
    b.get_quote(&token(), &key).await.unwrap();
    b.on_logout().await;
    b.restore_session(&creds());
    fake.expired.lock().clear();
    b.get_quote(&token(), &key).await.unwrap();
    let last = fake.lines("quotes|").pop().unwrap_or_default();
    assert_eq!(last, format!("quotes|{}", md_token(1)));
}
