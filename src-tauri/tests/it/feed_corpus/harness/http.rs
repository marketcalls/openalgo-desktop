//! The fake broker REST API, on an ephemeral loopback port, for what a feed
//! or an order poller reads over HTTP: Upstox's authorize call before every
//! connect, Groww's socket token, the order book 5paisa, Samco and Groww
//! poll for order updates, XTS's REST subscriptions.
//!
//! A path answers with its route's current answer: one fixed answer
//! (`Env::serve_json`, or a route in `case.json`), or a scripted sequence,
//! each `http_serve` step replacing the answer from then on and
//! `http_polled` waiting until the new one has been read. A route's
//! requests are counted, not kept, unless the case asks to compare them
//! (`record`); a request no route answers gets 404 and is kept, so the end
//! of the case reports it. The server task is owned by the `HttpFake` and
//! aborted when it is dropped.

use super::case::{json_matches, HttpBody};
use super::json_text;
use super::upstream::until;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// What one path answers.
#[derive(Debug, Clone)]
pub(crate) struct Route {
    /// `None`: any method.
    pub(crate) method: Option<String>,
    pub(crate) path: String,
    pub(crate) status: u16,
    pub(crate) body: String,
    /// Keep its requests for `http_receives`.
    pub(crate) record: bool,
    /// Requests answered since the answer was last set.
    hits: usize,
}

impl Route {
    pub(crate) fn new(method: Option<&str>, path: &str, status: u16, body: String) -> Route {
        Route {
            method: method.map(str::to_ascii_uppercase),
            path: path.to_string(),
            status,
            body,
            record: false,
            hits: 0,
        }
    }

    /// Whether this route answers a request: a route whose path has a
    /// query answers exactly that path and query (Groww reads one path
    /// per segment, `?segment=CASH&..` then `?segment=FNO&..`); one
    /// without answers its path with any query.
    fn answers(&self, method: &Method, uri: &Uri) -> bool {
        let wanted = if self.path.contains('?') {
            uri.path_and_query()
                .map_or_else(|| uri.path(), |p| p.as_str())
        } else {
            uri.path()
        };
        self.path == wanted && self.method.as_deref().is_none_or(|m| m == method.as_str())
    }

    /// Whether this route and one for `method` and `path` answer the same
    /// requests (a route for any method covers every method).
    fn covers(&self, method: Option<&str>, path: &str) -> bool {
        self.path == path
            && (self.method.is_none() || method.is_none() || self.method.as_deref() == method)
    }
}

/// A request the fake kept.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// Path and query.
    pub(crate) path: String,
    pub(crate) body: String,
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.method, self.path, self.body)
    }
}

#[derive(Default)]
struct HttpState {
    routes: Vec<Route>,
    kept: Vec<Request>,
    taken: usize,
}

pub(crate) struct HttpFake {
    url: String,
    state: Arc<Mutex<HttpState>>,
    task: JoinHandle<()>,
}

impl HttpFake {
    pub(crate) async fn bind() -> HttpFake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake broker API binds an ephemeral port");
        let url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("the fake broker API's address")
        );
        let state = Arc::new(Mutex::new(HttpState::default()));
        let app = Router::new().fallback(answer).with_state(state.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        HttpFake { url, state, task }
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Answer requests to the route's method and path with it from now on,
    /// in place of an earlier answer for them.
    pub(crate) fn serve(&self, route: Route) {
        let mut st = self.state.lock();
        st.routes
            .retain(|r| !r.covers(route.method.as_deref(), &route.path));
        st.routes.push(route);
    }

    /// Once the route for `method` and `path` has answered `times` times
    /// since its answer was set; `false` if it does not within the
    /// deadline.
    pub(crate) async fn polled(&self, method: Option<&str>, path: &str, times: usize) -> bool {
        let method = method.map(str::to_ascii_uppercase);
        until(|| {
            let st = self.state.lock();
            st.routes
                .iter()
                .any(|r| r.covers(method.as_deref(), path) && r.hits >= times)
                .then_some(())
        })
        .await
        .is_some()
    }

    /// The next `count` kept requests, once they have come; `Err` with what
    /// did come if they do not.
    pub(crate) async fn take(&self, count: usize) -> Result<Vec<Request>, Vec<Request>> {
        let ready = until(|| {
            let st = self.state.lock();
            (st.kept.len() - st.taken >= count).then_some(())
        })
        .await;
        let mut st = self.state.lock();
        let from = st.taken;
        match ready {
            Some(()) => {
                st.taken += count;
                Ok(st.kept[from..from + count].to_vec())
            }
            None => Err(st.kept[from..].to_vec()),
        }
    }

    /// Kept requests no step has taken.
    pub(crate) fn leftover(&self) -> Vec<Request> {
        let st = self.state.lock();
        st.kept[st.taken..].to_vec()
    }
}

impl Drop for HttpFake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(
    State(state): State<Arc<Mutex<HttpState>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request = Request {
        method: method.to_string(),
        path: uri
            .path_and_query()
            .map_or_else(|| uri.path().to_string(), |p| p.as_str().to_string()),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let reply = {
        let mut st = state.lock();
        let route = st
            .routes
            .iter_mut()
            .rev()
            .find(|r| r.answers(&method, &uri));
        let reply = route.map(|r| {
            r.hits += 1;
            (r.status, r.body.clone(), r.record)
        });
        if reply.as_ref().is_none_or(|(_, _, record)| *record) {
            st.kept.push(request);
        }
        reply
    };
    match reply {
        Some((status, body, _)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no route in the case for this path").into_response(),
    }
}

/// Whether a kept request is the one a step expects: method, path and
/// query exactly, and the body as JSON, as text, or empty.
pub(crate) fn matches(want: &HttpRequestExpect, got: &Request) -> bool {
    want.method.eq_ignore_ascii_case(&got.method)
        && want.path == got.path
        && match &want.body {
            None => got.body.is_empty(),
            Some(HttpBody::Text(t)) => *t == got.body,
            Some(HttpBody::Json(v)) => {
                json_text::parse(&got.body).is_ok_and(|g| json_matches(v, &g))
            }
        }
}

/// A request an `http_receives` step expects.
#[derive(Debug, Clone)]
pub(crate) struct HttpRequestExpect {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) body: Option<HttpBody>,
}

impl std::fmt::Display for HttpRequestExpect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.body {
            None => write!(f, "{} {}", self.method, self.path),
            Some(HttpBody::Text(t)) => write!(f, "{} {} {}", self.method, self.path, t),
            Some(HttpBody::Json(v)) => write!(f, "{} {} {}", self.method, self.path, v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(fake: &HttpFake, path_and_query: &str) -> (u16, String) {
        let resp = reqwest::Client::new()
            .get(format!("{}{}", fake.url(), path_and_query))
            .send()
            .await
            .expect("the fake broker API answers");
        let status = resp.status().as_u16();
        (status, resp.text().await.unwrap_or_default())
    }

    /// Groww reads one path once per segment: a route with a query answers
    /// that query alone, so each segment gets its own book; a route without
    /// one answers its path whatever the query.
    #[tokio::test]
    async fn two_queries_of_one_path_get_their_own_answers() {
        let fake = HttpFake::bind().await;
        let cash = "/v1/order/list?segment=CASH&page=0&page_size=100";
        let fno = "/v1/order/list?segment=FNO&page=0&page_size=100";
        fake.serve(Route::new(Some("GET"), cash, 200, "cash".into()));
        fake.serve(Route::new(Some("GET"), fno, 200, "fno".into()));
        fake.serve(Route::new(None, "/v1/token", 200, "token".into()));
        assert_eq!(get(&fake, cash).await, (200, "cash".to_string()));
        assert_eq!(get(&fake, fno).await, (200, "fno".to_string()));
        assert_eq!(
            get(&fake, "/v1/token?any=1").await,
            (200, "token".to_string())
        );
        assert!(fake.polled(Some("GET"), cash, 1).await);
        assert!(fake.polled(Some("GET"), fno, 1).await);
        assert!(fake.leftover().is_empty());
        let other = "/v1/order/list?segment=COMMODITY&page=0&page_size=100";
        assert_eq!(get(&fake, other).await.0, 404);
        let kept: Vec<String> = fake.leftover().iter().map(|r| r.path.clone()).collect();
        assert_eq!(kept, vec![other.to_string()]);
    }
}
