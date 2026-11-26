//! Pocketful REST tests, ported from the web's `test/test_pocketful_*.py`
//! (marketcalls/openalgo#2467) with the same inputs and expectations,
//! against a local fake Pocketful that answers the documentation's own
//! examples (no account data; `<USER_ID>`-style placeholders).

mod auth;
mod client;
mod funds_margin;
mod orders;

use super::PocketfulBroker;
use crate::brokers::common::symbols::SymbolResolver;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

/// The trading client id the fixtures use (the docs' own example account).
pub(super) const CLIENT: &str = "DEMO1";

/// One request the fake received.
#[derive(Debug, Clone)]
pub(super) struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or("")
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// The query as decoded pairs, in order.
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .into_owned()
            .collect()
    }
}

/// One scripted answer.
#[derive(Debug, Clone)]
pub(super) enum Reply {
    /// A status and a JSON body.
    Json(u16, Value),
    /// A status and a body that is not JSON.
    Text(u16, &'static str),
    /// Headers, then the connection breaks inside the body: a request that
    /// was sent and whose answer was lost.
    Cut,

    /// The reply, after a wait: a Pocketful that is slow to answer.
    Late(Duration, Box<Reply>),
}

/// A path, what its query must contain (empty for anything), and the
/// replies left.
type Routes = Arc<Mutex<Vec<(String, String, Vec<Reply>)>>>;

/// A local fake Pocketful. Each path answers its scripted replies in order,
/// the last one repeating; an unscripted path answers 404.
pub(super) struct Fake {
    pub base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    routes: Routes,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn respond(reply: Reply) -> Response {
    match reply {
        Reply::Json(status, body) => (
            StatusCode::from_u16(status).unwrap(),
            [("content-type", "application/json")],
            body.to_string(),
        )
            .into_response(),
        Reply::Text(status, body) => (StatusCode::from_u16(status).unwrap(), body).into_response(),
        Reply::Cut => {
            let broken = futures_util::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("connection cut"))
            });
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(broken))
                .unwrap()
        }

        // The wait is the handler's (`Fake::start`).
        Reply::Late(_, reply) => respond(*reply),
    }
}

impl Fake {
    pub async fn start() -> Self {
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let routes: Routes = Arc::default();
        let (s, r) = (seen.clone(), routes.clone());
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let (s, r) = (s.clone(), r.clone());
                async move {
                    let path = uri.path().to_string();
                    let query = uri.query().unwrap_or("").to_string();
                    s.lock().push(Seen {
                        method: method.to_string(),
                        path: path.clone(),
                        query: query.clone(),
                        headers: headers
                            .iter()
                            .map(|(k, v)| {
                                (k.to_string(), v.to_str().unwrap_or_default().to_string())
                            })
                            .collect(),
                        body: String::from_utf8_lossy(&body).into_owned(),
                    });
                    let reply = {
                        let mut routes = r.lock();
                        routes
                            .iter_mut()
                            .find(|(p, q, _)| *p == path && query.contains(q.as_str()))
                            .map(|(_, _, replies)| {
                                if replies.len() > 1 {
                                    replies.remove(0)
                                } else {
                                    replies[0].clone()
                                }
                            })
                    };
                    let reply = reply.unwrap_or(Reply::Json(
                        404,
                        serde_json::json!({"status": "error", "message": "not mocked"}),
                    ));
                    if let Reply::Late(wait, _) = &reply {
                        tokio::time::sleep(*wait).await;
                    }
                    respond(reply)
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base,
            seen,
            routes,
            task,
        }
    }

    /// Answer `path` with `replies`, in order, the last one repeating.
    pub fn on(&self, path: &str, replies: Vec<Reply>) {
        self.on_query(path, "", replies);
    }

    /// [`Self::on`] for the requests to `path` whose query contains
    /// `query`; a route with a query is tried before one without.
    pub fn on_query(&self, path: &str, query: &str, replies: Vec<Reply>) {
        assert!(!replies.is_empty());
        let mut routes = self.routes.lock();
        routes.retain(|(p, q, _)| !(p == path && q == query));
        routes.push((path.to_string(), query.to_string(), replies));
        // The most specific route first.
        routes.sort_by_key(|(_, q, _)| std::cmp::Reverse(q.len()));
    }

    /// Every request so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().clone()
    }

    /// The requests to `path`.
    pub fn to(&self, path: &str) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.path == path).collect()
    }

    /// A broker on this fake with `symbols`, retrying a rate limit after a
    /// millisecond instead of a second.
    pub fn broker(&self, symbols: SymbolResolver) -> PocketfulBroker {
        PocketfulBroker::with_base_url(
            symbols,
            &self.base,
            "ws://127.0.0.1:9",
            format!("{}/contract", self.base),
        )
        .with_rate_limit_backoff(Duration::from_millis(1))
    }
}

/// `{"status": "success", "message": "", "data": data}`.
pub(super) fn success(data: Value) -> Reply {
    Reply::Json(
        200,
        serde_json::json!({"status": "success", "message": "", "data": data}),
    )
}

/// Every tracing event at every level, written while the guard lives.
#[derive(Clone, Default)]
pub(super) struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

pub(super) fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let c = Captured::default();
    let w = c.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    (c, tracing::subscriber::set_default(sub))
}
