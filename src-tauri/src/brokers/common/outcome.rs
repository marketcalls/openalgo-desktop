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
//! The rules live here so every adapter inherits them:
//!
//! * a transport error is classified by [`after_send`] (only a failure to
//!   connect or to build the request proves nothing was sent);
//! * inside a placement ([`placing`]), [`super::http::read_json`] turns a
//!   5xx or an unreadable body into [`AppError::Uncertain`] instead of an
//!   ordinary broker refusal;
//! * the order service classifies whatever the adapter returned with
//!   [`classify`].
//!
//! Adapters that tag orders attach the tag to an uncertain answer with
//! [`with_client_tag`], so the reconciler can find the order by it.

use crate::brokers::types::OrderResponse;
use crate::error::{AppError, Result};
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

/// What a trader or an API client reads for an uncertain placement.
pub const UNCERTAIN_MESSAGE: &str = "The broker did not confirm whether this order was placed. \
It may have been placed: check the order book before placing it again.";

tokio::task_local! {
    static PLACING: ();
}

/// Run `fut` as one order placement: transport and reply failures inside it
/// are classified as uncertain rather than refused.
pub async fn placing<F: Future>(fut: F) -> F::Output {
    PLACING.scope((), fut).await
}

/// Whether the current task is inside [`placing`].
pub fn in_placement() -> bool {
    PLACING.try_with(|_| ()).is_ok()
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
        // An answer that could not be parsed, a socket failure or an error
        // the adapter did not name: the order may have reached the broker.
        AppError::Serialization(_)
        | AppError::WebSocket(_)
        | AppError::Io(_)
        | AppError::Internal(_) => Some(None),
        // Refusals the adapter or the broker stated: nothing was placed.
        AppError::Broker(_)
        | AppError::Auth(_)
        | AppError::Validation(_)
        | AppError::NotFound(_)
        | AppError::Unsupported(_)
        | AppError::Config(_)
        | AppError::Locked
        | AppError::Keychain(_)
        | AppError::Encryption(_)
        | AppError::Database(_)
        | AppError::Pool(_)
        | AppError::DuckDb(_) => None,
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
                reason: match e {
                    AppError::Uncertain(u) => u.message.clone(),
                    _ => UNCERTAIN_MESSAGE.to_string(),
                },
            },
            None => PlaceOutcome::Refused {
                reason: e.client_message(),
            },
        },
    }
}

/// For an adapter that sent `client_tag` with the order: an answer whose
/// outcome is unknown becomes [`AppError::Uncertain`] carrying the tag, so
/// the order can be found by it. Other answers pass through.
pub fn with_client_tag(result: Result<OrderResponse>, client_tag: &str) -> Result<OrderResponse> {
    match result {
        Ok(r) if r.order_id.trim().is_empty() => {
            Err(AppError::uncertain(UNCERTAIN_MESSAGE, Some(client_tag.into())))
        }
        Ok(r) => Ok(r),
        Err(e) => match uncertain_cause(&e) {
            Some(None) => {
                tracing::warn!(
                    "Order with tag {} has no definite answer ({}); it will be looked up",
                    client_tag,
                    e.code()
                );
                Err(AppError::uncertain(
                    match &e {
                        AppError::Uncertain(u) => u.message.clone(),
                        _ => UNCERTAIN_MESSAGE.to_string(),
                    },
                    Some(client_tag.into()),
                ))
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

    async fn post(url: &str, timeout: Duration) -> Result<OrderResponse> {
        let resp = client(timeout)
            .post(url)
            .json(&serde_json::json!({"symbol": "SBIN"}))
            .send()
            .await?;
        let (_, v): (_, serde_json::Value) = super::super::http::read_json("test", resp).await?;
        Ok(OrderResponse {
            order_id: v["orderid"].as_str().unwrap_or_default().to_string(),
            message: None,
        })
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
    fn stated_refusals_stay_refused() {
        for e in [
            AppError::Broker("Insufficient funds".into()),
            AppError::Auth("session expired".into()),
            AppError::Validation("bad".into()),
        ] {
            assert!(
                matches!(classify(&Err(e)), PlaceOutcome::Refused { .. }),
                "a stated refusal is a refusal"
            );
        }
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
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
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
        let r = with_client_tag(Err(AppError::Broker("RMS".into())), "oaabc");
        assert!(matches!(r, Err(AppError::Broker(_))));
        let t = new_client_tag();
        assert_eq!(t.len(), 18);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
