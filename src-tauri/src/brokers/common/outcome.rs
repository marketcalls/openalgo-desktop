//! What a placement did at the broker: accepted, refused, or uncertain
//! (Codex LOG-08, design requirements 2 and 9).
//!
//! A placement has three outcomes, never two:
//!
//! * **Accepted**: the broker answered with an order id.
//! * **Refused**: the request provably never reached the broker (it could
//!   not be built, the connection could not be opened, the session or the
//!   request was refused before sending), or the broker answered that it
//!   did not place the order.
//! * **Uncertain**: the request was sent, or may have been, and no definite
//!   answer came back: a timeout or a dropped connection after sending, a
//!   server error (5xx), an unreadable reply, a success without an order id.
//!   The order may exist at the broker. Whoever placed it keeps its claim
//!   and never places it again blindly; the order reconciler looks it up in
//!   the broker's order book (by client tag where the broker carries one).
//!
//! **Inside a placement the default is uncertain.** An adapter error is a
//! refusal only when the adapter marks it as one: a readable answer from
//! the broker that says it did not place the order, or a refusal before
//! anything was sent ([`refusal`], which builds [`AppError::Refused`]), and
//! the pre-send errors that cannot follow a send (validation, a missing
//! session, a symbol not found). Every other broker error, every transport
//! failure after sending ([`after_send`]), an unreadable answer and a
//! success without an id are uncertain. An adapter that has not been
//! reviewed therefore errs on the safe side: its refusals read as
//! uncertain, never its uncertainties as refusals.
//!
//! Inside [`placing`], [`super::http::read_json`] notes a server error
//! (5xx); a [`refusal`] after one in the same placement stays uncertain, so
//! a broker's error body on a 5xx never reads as "not placed".
//!
//! The uncertainty never reaches the `/api/v1` wire: the order service
//! answers exactly as before (a broker's text with 400, a transport failure
//! with 500 and its internal-error text) and carries the outcome beside the
//! reply for in-process callers (strategies, scalping, OpenScript, MCP).
//!
//! Adapters that tag orders attach the tag to an uncertain answer with
//! [`with_client_tag`], so the reconciler can find the order by it.

use crate::brokers::types::OrderResponse;
use crate::error::{AppError, Result, UncertainOrder};
use std::cell::Cell;
use std::future::Future;

/// What a placement did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceOutcome {
    /// The broker acknowledged the order with this id.
    Accepted { order_id: String },
    /// The order was not placed. Trader-facing reason.
    Refused { reason: String },
    /// The order may have been placed. `client_tag` is the tag it was sent
    /// with, when the adapter tags orders. Trader-facing reason.
    Uncertain {
        client_tag: Option<String>,
        reason: String,
    },
}

impl PlaceOutcome {
    pub fn is_uncertain(&self) -> bool {
        matches!(self, PlaceOutcome::Uncertain { .. })
    }
}

/// What a trader reads for an uncertain placement (logs, events, alerts).
pub const UNCERTAIN_MESSAGE: &str = "The broker did not confirm whether this order was placed. \
It may have been placed: check the order book before placing it again.";

/// State of one placement.
#[derive(Default)]
struct Scope {
    /// A server error (5xx) was answered inside this placement.
    server_error: Cell<bool>,
}

tokio::task_local! {
    static PLACING: Scope;
}

/// Run `fut` as one order placement: inside it, any adapter error that is
/// not a marked refusal is uncertain.
pub async fn placing<F: Future>(fut: F) -> F::Output {
    PLACING.scope(Scope::default(), fut).await
}

/// Whether the current task is inside [`placing`].
pub fn in_placement() -> bool {
    PLACING.try_with(|_| ()).is_ok()
}

/// A server error was answered inside the current placement (called by
/// `read_json` and by adapters with their own transport).
pub fn note_server_error() {
    let _ = PLACING.try_with(|s| s.server_error.set(true));
}

fn saw_server_error() -> bool {
    PLACING.try_with(|s| s.server_error.get()).unwrap_or(false)
}

/// The broker's definite "not placed" (a readable refusal), or an
/// adapter's refusal before sending. Inside a placement this is
/// [`AppError::Refused`], unless a server error was answered in the same
/// placement (then the outcome stays unknown); outside one it is an
/// ordinary [`AppError::Broker`].
pub fn refusal(message: impl Into<String>) -> AppError {
    let m = message.into();
    if in_placement() && !saw_server_error() {
        AppError::Refused(m)
    } else {
        AppError::Broker(m)
    }
}

/// [`refusal`] for an answer with this HTTP status: a server error is never
/// a refusal.
pub fn refusal_with_status(status: u16, message: impl Into<String>) -> AppError {
    if (500..600).contains(&status) {
        note_server_error();
        return AppError::Broker(message.into());
    }
    refusal(message)
}

/// A placement answer that does not carry an order id, by HTTP status:
///
/// * a server error (5xx) leaves the order unknown;
/// * a client error (4xx) is the broker's refusal;
/// * a success status (2xx) is a refusal only when the body carries the
///   broker's explicit failure (`failed`: a parsed failure status or error
///   field). Anything else on a 2xx (`{}`, `null`, a body without the
///   success marker) may be a placed order: unknown, keeping the broker's
///   text for the wire.
pub fn not_accepted(status: u16, failed: bool, message: impl Into<String>) -> AppError {
    let message = message.into();
    match status {
        500..=599 => {
            note_server_error();
            AppError::Broker(message)
        }
        200..=299 if !failed => {
            tracing::warn!(
                status,
                "A placement answer carried neither an order id nor a failure; it may have been placed"
            );
            AppError::uncertain(message, None)
        }
        _ => refusal(message),
    }
}

/// [`not_accepted`] for an adapter's own error: its broker-text error is
/// judged by status and failure marker; anything else passes through.
pub fn not_accepted_err(status: u16, failed: bool, e: AppError) -> AppError {
    match e {
        AppError::Broker(m) => not_accepted(status, failed, m),
        other => other,
    }
}

/// Mark an adapter's own error as a definite refusal: its broker-text error
/// becomes [`refusal`]; anything else passes through.
pub fn definite(e: AppError) -> AppError {
    match e {
        AppError::Broker(m) => refusal(m),
        other => other,
    }
}

/// An error from a step that runs before the order is sent (a quote read
/// for market protection, a session check): the order was not sent, so
/// inside a placement it is a definite refusal. Errors that already say
/// "not placed" pass through.
pub fn not_sent(e: AppError) -> AppError {
    if in_placement() && uncertain_cause(&e).is_some() {
        tracing::warn!("A step before the order was sent failed: {}", e.code());
        AppError::Refused(e.client_message())
    } else {
        e
    }
}

/// Run a step that comes before the order is sent (a quote read, a
/// lookup). Its failure is [`not_sent`], and a server error it met does
/// not count against the order's own answer.
pub async fn before_send<T, F>(fut: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let before = saw_server_error();
    let r = fut.await;
    let _ = PLACING.try_with(|s| s.server_error.set(before));
    r.map_err(not_sent)
}

/// Whether a transport error may have happened after the request reached
/// the broker. Only a request that could not be built or a connection that
/// could not be opened (a refused, unreachable or timed-out connect, a TLS
/// failure) proves that no byte of the order was sent.
pub fn after_send(e: &reqwest::Error) -> bool {
    !(e.is_builder() || e.is_connect())
}

/// Whether this error, returned by a placement, leaves the order's outcome
/// unknown. `Some(tag)` carries the client tag when one is known.
pub fn uncertain_cause(e: &AppError) -> Option<Option<String>> {
    match e {
        AppError::Uncertain(u) => Some(u.client_tag.clone()),
        AppError::Http(re) if after_send(re) => Some(None),
        AppError::Http(_) => None,
        // The default: a broker error not marked as a refusal, an answer
        // that could not be parsed, a socket failure, an unnamed error.
        AppError::Broker(_)
        | AppError::Serialization(_)
        | AppError::WebSocket(_)
        | AppError::Io(_)
        | AppError::Internal(_) => Some(None),
        // A refusal the broker stated, or one that cannot follow a send.
        AppError::Refused(_)
        | AppError::Auth(_)
        | AppError::Validation(_)
        | AppError::NotFound(_)
        | AppError::Unsupported(_)
        | AppError::Config(_)
        | AppError::Locked
        | AppError::KeychainUnavailable
        | AppError::KeysDoNotMatch
        | AppError::Keychain(_)
        | AppError::Encryption(_)
        | AppError::Database(_)
        | AppError::Pool(_)
        | AppError::DuckDb(_) => None,
    }
}

/// The trader text an uncertain error carries.
fn uncertain_reason(e: &AppError) -> String {
    match e {
        AppError::Uncertain(u) => u.message.clone(),
        AppError::Broker(m) => m.clone(),
        _ => UNCERTAIN_MESSAGE.to_string(),
    }
}

/// Classify what an adapter's `place_order` returned.
pub fn classify(result: &Result<OrderResponse>) -> PlaceOutcome {
    match result {
        Ok(r) if !r.order_id.trim().is_empty() => PlaceOutcome::Accepted {
            order_id: r.order_id.trim().to_string(),
        },
        // A success without an order id: the broker may hold the order.
        Ok(_) => PlaceOutcome::Uncertain {
            client_tag: None,
            reason: UNCERTAIN_MESSAGE.to_string(),
        },
        Err(e) => match uncertain_cause(e) {
            Some(client_tag) => PlaceOutcome::Uncertain {
                client_tag,
                reason: uncertain_reason(e),
            },
            None => PlaceOutcome::Refused {
                reason: e.client_message(),
            },
        },
    }
}

/// For an adapter that sent `client_tag` with the order: an answer whose
/// outcome is unknown becomes [`AppError::Uncertain`] carrying the tag (and
/// the wire answer it had), so the order can be found by it. Other answers
/// pass through.
pub fn with_client_tag(result: Result<OrderResponse>, client_tag: &str) -> Result<OrderResponse> {
    match result {
        Ok(r) if r.order_id.trim().is_empty() => Err(AppError::uncertain(
            UNCERTAIN_MESSAGE,
            Some(client_tag.into()),
        )),
        Ok(r) => Ok(r),
        Err(e) => match uncertain_cause(&e) {
            Some(None) => {
                tracing::warn!(
                    "Order with tag {} has no definite answer ({}); it will be looked up",
                    client_tag,
                    e.code()
                );
                let internal = match &e {
                    AppError::Broker(_) => false,
                    AppError::Uncertain(u) => u.internal,
                    _ => true,
                };
                Err(AppError::Uncertain(Box::new(UncertainOrder {
                    message: uncertain_reason(&e),
                    client_tag: Some(client_tag.into()),
                    internal,
                })))
            }
            _ => Err(e),
        },
    }
}

/// A fresh client tag: `oa` and 16 hex digits (18 characters, letters and
/// digits only), inside every tagging broker's limits (Kite and Fyers 20,
/// Dhan 25, Angel 20, Upstox 40).
pub fn new_client_tag() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("oa{}", &hex[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn ok(id: &str) -> Result<OrderResponse> {
        Ok(OrderResponse {
            order_id: id.into(),
            message: None,
        })
    }

    /// A server that reads the request and then does `then` with the socket.
    async fn server<F, Fut>(then: F) -> String
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            // The whole request has arrived: the order was sent.
            let _ = s.read(&mut buf).await;
            then(s).await;
        });
        format!("http://{}/order", addr)
    }

    fn client(timeout: Duration) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .build()
            .unwrap()
    }

    /// An adapter's placement as most are written: send, read the JSON, and
    /// mark a non-2xx answer as the broker's refusal.
    async fn post(url: &str, timeout: Duration) -> Result<OrderResponse> {
        let resp = client(timeout)
            .post(url)
            .json(&serde_json::json!({"symbol": "SBIN"}))
            .send()
            .await?;
        let (status, v): (_, serde_json::Value) =
            super::super::http::read_json("test", resp).await?;
        if !status.is_success() {
            return Err(refusal_with_status(
                status.as_u16(),
                v["message"].as_str().unwrap_or("refused").to_string(),
            ));
        }
        Ok(OrderResponse {
            order_id: v["orderid"].as_str().unwrap_or("X1").to_string(),
            message: None,
        })
    }

    /// Like [`post`], but an adapter that marks every error body with
    /// [`refusal`] without looking at the status.
    async fn post_unaware(url: &str) -> Result<OrderResponse> {
        let resp = client(Duration::from_secs(5))
            .post(url)
            .json(&serde_json::json!({"symbol": "SBIN"}))
            .send()
            .await?;
        let (_, v): (_, serde_json::Value) = super::super::http::read_json("test", resp).await?;
        if v["status"] != "success" {
            return Err(refusal(
                v["message"].as_str().unwrap_or("refused").to_string(),
            ));
        }
        Ok(OrderResponse {
            order_id: v["orderid"].as_str().unwrap_or("X1").to_string(),
            message: None,
        })
    }

    async fn answer(status_line: &'static str, body: &'static str) -> String {
        server(move |mut s| async move {
            let _ = s
                .write_all(
                    format!(
                        "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        status_line,
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await;
        })
        .await
    }

    #[tokio::test]
    async fn a_readable_refusal_is_refused_inside_a_placement() {
        let url = answer(
            "400 Bad Request",
            r#"{"status":"error","message":"Insufficient funds"}"#,
        )
        .await;
        let r = placing(post(&url, Duration::from_secs(5))).await;
        assert_eq!(
            classify(&r),
            PlaceOutcome::Refused {
                reason: "Insufficient funds".into()
            }
        );
        // The wire keeps the broker error code and text.
        let e = r.unwrap_err();
        assert_eq!(e.code(), "BROKER_ERROR");
        assert_eq!(e.client_message(), "Insufficient funds");
    }

    #[tokio::test]
    async fn a_refusal_after_a_server_error_stays_uncertain() {
        // The adapter marks the error body as a refusal without looking at
        // the status: the 503 that read_json noted keeps it unknown.
        let url = answer(
            "503 Service Unavailable",
            r#"{"status":"error","message":"Order rejected"}"#,
        )
        .await;
        let r = placing(post_unaware(&url)).await;
        assert!(classify(&r).is_uncertain(), "{:?}", r);
        // The same body on a 200 is a refusal.
        let url = answer("200 OK", r#"{"status":"error","message":"Order rejected"}"#).await;
        let r = placing(post_unaware(&url)).await;
        assert!(
            matches!(classify(&r), PlaceOutcome::Refused { .. }),
            "{:?}",
            r
        );
    }

    #[tokio::test]
    async fn a_refusal_outside_a_placement_is_an_ordinary_broker_error() {
        assert!(matches!(refusal("x"), AppError::Broker(_)));
        assert!(matches!(
            placing(async { refusal("x") }).await,
            AppError::Refused(_)
        ));
        assert!(matches!(
            placing(async { refusal_with_status(502, "x") }).await,
            AppError::Broker(_)
        ));
    }

    #[tokio::test]
    async fn a_step_before_sending_never_makes_the_order_uncertain() {
        // Its failure is "not placed".
        let r: Result<f64> = placing(before_send(async {
            Err(AppError::Broker("quote failed".into()))
        }))
        .await;
        assert!(matches!(r, Err(AppError::Refused(_))), "{:?}", r);
        // A server error it met does not taint the order's own refusal.
        let e = placing(async {
            let _ = before_send(async {
                note_server_error();
                Ok::<_, AppError>(1.0)
            })
            .await;
            refusal("RMS: margin exceeds")
        })
        .await;
        assert!(matches!(e, AppError::Refused(_)), "{:?}", e);
        // Outside a placement nothing changes.
        let r: Result<f64> = before_send(async { Err(AppError::Broker("q".into())) }).await;
        assert!(matches!(r, Err(AppError::Broker(_))));
    }

    #[test]
    fn an_order_id_is_accepted_and_an_empty_one_is_uncertain() {
        assert_eq!(
            classify(&ok(" 42 ")),
            PlaceOutcome::Accepted {
                order_id: "42".into()
            }
        );
        assert!(classify(&ok("")).is_uncertain());
    }

    #[test]
    fn only_marked_refusals_are_refused() {
        for e in [
            AppError::Refused("Insufficient funds".into()),
            AppError::Auth("session expired".into()),
            AppError::Validation("bad".into()),
        ] {
            assert!(
                matches!(classify(&Err(e)), PlaceOutcome::Refused { .. }),
                "a stated refusal is a refusal"
            );
        }
        // An unmarked broker error is uncertain: an adapter that was not
        // reviewed errs on the safe side (P0-1).
        assert!(classify(&Err(AppError::Broker("Insufficient funds".into()))).is_uncertain());
        assert!(classify(&Err(AppError::Internal("x".into()))).is_uncertain());
        assert!(classify(&Err(AppError::uncertain("m", Some("oa1".into())))).is_uncertain());
    }

    #[tokio::test]
    async fn a_dropped_connection_after_sending_is_uncertain() {
        let url = server(|s| async move { drop(s) }).await;
        let r = placing(post(&url, Duration::from_secs(5))).await;
        assert!(classify(&r).is_uncertain(), "{:?}", r);
    }

    #[tokio::test]
    async fn a_timeout_after_sending_is_uncertain() {
        let url = server(|s| async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(s);
        })
        .await;
        let r = placing(post(&url, Duration::from_millis(200))).await;
        assert!(classify(&r).is_uncertain(), "{:?}", r);
    }

    #[tokio::test]
    async fn a_server_error_after_sending_is_uncertain_even_with_a_json_body() {
        let url = server(|mut s| async move {
            let body = r#"{"status":"error","message":"Internal server error"}"#;
            let _ = s
                .write_all(
                    format!(
                        "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await;
        })
        .await;
        let r = placing(post(&url, Duration::from_secs(5))).await;
        assert!(classify(&r).is_uncertain(), "{:?}", r);
    }

    #[tokio::test]
    async fn an_unreadable_success_body_is_uncertain() {
        let url = server(|mut s| async move {
            let body = "<html>gateway</html>";
            let _ = s
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await;
        })
        .await;
        let r = placing(post(&url, Duration::from_secs(5))).await;
        assert!(classify(&r).is_uncertain(), "{:?}", r);
    }

    #[tokio::test]
    async fn a_connection_that_never_opened_is_refused() {
        // Bind then drop: nothing listens on the port.
        let addr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let r = placing(post(
            &format!("http://{}/order", addr),
            Duration::from_secs(5),
        ))
        .await;
        assert!(
            matches!(classify(&r), PlaceOutcome::Refused { .. }),
            "{:?}",
            r
        );
    }

    #[tokio::test]
    async fn outside_a_placement_a_server_error_is_an_ordinary_broker_error() {
        let url = server(|mut s| async move {
            let _ = s
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        })
        .await;
        let r = post(&url, Duration::from_secs(5)).await;
        assert!(matches!(r, Err(AppError::Broker(_))), "{:?}", r);
    }

    #[test]
    fn a_tag_travels_with_an_uncertain_answer() {
        let r = with_client_tag(Err(AppError::Internal("cut".into())), "oaabc");
        match r {
            Err(AppError::Uncertain(u)) => assert_eq!(u.client_tag.as_deref(), Some("oaabc")),
            other => panic!("{:?}", other),
        }
        // A marked refusal stays one.
        let r = with_client_tag(Err(AppError::Refused("RMS".into())), "oaabc");
        assert!(matches!(r, Err(AppError::Refused(_))));
        // An unmarked broker error is unknown, keeps its text for the wire
        // and is not an internal failure.
        match with_client_tag(Err(AppError::Broker("Gateway busy".into())), "oaabc") {
            Err(AppError::Uncertain(u)) => {
                assert_eq!(u.client_tag.as_deref(), Some("oaabc"));
                assert_eq!(u.message, "Gateway busy");
                assert!(!u.internal);
            }
            other => panic!("{:?}", other),
        }
        let t = new_client_tag();
        assert_eq!(t.len(), 18);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
