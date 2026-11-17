//! Credentials never leak: every sign-in, REST and feed error path runs
//! with sentinel credentials against a closed port (transport errors carry
//! the URL, and Nuvama's vendor login puts the API key in its path) and a
//! fake that refuses everything; the sentinel must appear in no log line
//! at any level, no error `Display` or `Debug`, and no trader-facing
//! message.

use super::master;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::outcome::placing;
use openalgo_desktop_lib::brokers::common::streaming::{FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::nuvama::session::NuvamaSession;
use openalgo_desktop_lib::brokers::nuvama::streaming::{NuvamaFeed, NuvamaOrderFeed};
use openalgo_desktop_lib::brokers::nuvama::{NuvamaBroker, NuvamaUrls};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const S: &str = "SENTINELq7Zx";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let c = Captured::default();
    let w = c.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    (c, tracing::subscriber::set_default(sub))
}

fn clean(what: &str, text: &str) {
    assert!(!text.contains(S), "{} leaked the sentinel: {}", what, text);
}

fn check<T>(what: &str, r: openalgo_desktop_lib::error::Result<T>) {
    if let Err(e) = r {
        check_err(what, &e);
    }
}

fn check_err(what: &str, e: &AppError) {
    clean(what, &format!("{} | {:?} | {}", e, e, e.client_message()));
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// Refuses everything, repeating nothing of the request.
async fn refusing_http() -> String {
    let app = Router::new().fallback(|| async {
        (
            StatusCode::from_u16(222).unwrap(),
            [("content-type", "application/json")],
            r#"{"error":{"actCd":"54","errCd":"EGN0003","errMsg":"Invalid request"}}"#,
        )
            .into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: S.into(),
        api_secret: Some(S.into()),
        client_id: Some(S.into()),
        auth_code: Some(S.into()),
        request_token: Some(S.into()),
        ..Default::default()
    }
}

/// A session every part of which is the sentinel, the account included.
fn token() -> String {
    [S, S, S, S, "EQ", S].join(":::")
}

async fn drive(host: &str) {
    let b = NuvamaBroker::with_base_url(master(), NuvamaUrls::rebased(host, closed_port()));
    check("authenticate", b.authenticate(creds()).await);
    let auth = AuthToken::new(token());
    let o = ResolvedOrder::resolve(
        &OrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 1,
            price: 0.0,
            order_type: "MARKET".into(),
            product: "MIS".into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        },
        &master(),
    )
    .unwrap();
    check("place", placing(b.place_order(&auth, &o)).await);
    check("cancel", b.cancel_order(&auth, S).await);
    check("cancel all", b.cancel_all_orders(&auth).await);
    check("orders", b.get_order_book(&auth).await);
    check("trades", b.get_trade_book(&auth).await);
    check("positions", b.get_positions(&auth).await);
    check("holdings", b.get_holdings(&auth).await);
    check("funds", b.get_funds(&auth).await);
    let key = QuoteKey::new("NSE", "RELIANCE");
    check("quote", b.get_quote(&auth, &key).await);
    check("depth", b.get_market_depth(&auth, &key).await);
    check(
        "history",
        b.get_history(
            &auth,
            &HistoryRequest {
                key,
                interval: "1m".into(),
                start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            },
        )
        .await,
    );
    check("master", b.download_master_contract(&auth).await);
}

async fn feed_once(
    manager: &WebSocketManager,
    feed: Box<dyn openalgo_desktop_lib::brokers::common::streaming::BrokerFeed>,
) {
    check("feed connect", manager.connect(feed).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    check("feed disconnect", manager.disconnect().await);
}

#[tokio::test]
async fn no_credential_reaches_a_log_or_an_error() {
    let (log, _guard) = capture();
    drive(&format!("http://127.0.0.1:{}", closed_port())).await;
    drive(&refusing_http().await).await;

    // The feeds' relays reach a closed streaming port.
    let manager = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        connect_timeout: Duration::from_millis(500),
        ..FeedConfig::default()
    });
    manager
        .subscribe(vec![FeedSubscription {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            token: "2885_NSE".into(),
            brsymbol: "INE002A01018".into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Quote,
            depth: 5,
        }])
        .await
        .unwrap();
    let urls = NuvamaUrls::rebased("http://127.0.0.1:1", closed_port());
    let session = NuvamaSession::parse(&token()).unwrap();
    feed_once(&manager, Box::new(NuvamaFeed::new(&urls, &session))).await;
    feed_once(
        &manager,
        Box::new(NuvamaOrderFeed::new(&urls, &session, master())),
    )
    .await;

    let text = String::from_utf8_lossy(&log.0.lock().unwrap()).into_owned();
    assert!(!text.is_empty(), "nothing was captured");
    assert!(
        text.contains("Nuvama"),
        "the adapter's logging was not captured"
    );
    clean("logs", &text);
}
