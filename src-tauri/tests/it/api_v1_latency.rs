//! The latency log's values (OBS-03, OBS-04, OBS-06): an order's broker
//! time is the time it spent with the broker and the overhead the rest, a
//! sandbox order has no broker time, the broker is the one the order went
//! to, and the orders a trader sends from a page are recorded like
//! `/api/v1` calls.

use crate::api_v1_support;

use api_v1_support::H;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openalgo_desktop_lib::brokers::mock::PlaceGate;
use openalgo_desktop_lib::db::sqlite::monitor::{self as store, LatencyRow};
use openalgo_desktop_lib::state::BrokerSession;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

fn order_body() -> Value {
    json!({"strategy": "t", "exchange": "NSE", "symbol": "SBIN", "action": "BUY",
        "quantity": 1, "pricetype": "MARKET", "product": "MIS"})
}

/// Every latency row, oldest first.
fn rows(h: &H) -> Vec<LatencyRow> {
    h.ctx.monitor.drain_now(&h.ctx);
    let mut v = store::recent_latency(&h.ctx.logs.conn().unwrap(), None).unwrap();
    v.reverse();
    v
}

fn adds_up(r: &LatencyRow) -> bool {
    (r.rtt_ms + r.overhead_ms - r.total_latency_ms).abs() < 1e-6
}

/// OBS-03, OBS-06: a live order held 100 ms at the broker records about
/// that as its broker time and the rest as overhead, under the broker it
/// was sent to, although the session moved to another broker before the
/// reply was written.
#[tokio::test]
async fn a_live_order_records_its_broker_time_under_the_broker_it_went_to() {
    let h = H::new().await;
    let gate = Arc::new(PlaceGate::default());
    *h.mock.place_gate.lock() = Some(gate.clone());
    let at_broker = async {
        gate.entered.notified().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        *h.ctx.broker_session.write() = Some(BrokerSession {
            broker_id: "dhan".into(),
            auth_token: "other-token".into(),
            feed_token: None,
            user_id: "CD5678".into(),
            user_name: None,
            authenticated_at: h.ctx.now(),
        });
        gate.release.notify_one();
    };
    let ((s, v), ()) = tokio::join!(
        h.post("/api/v1/placeorder", h.with_key(order_body())),
        at_broker
    );
    assert_eq!(s, StatusCode::OK, "{}", v);
    let all = rows(&h);
    assert_eq!(all.len(), 1);
    let r = &all[0];
    assert_eq!(r.order_type, "PLACE");
    assert_eq!(
        r.broker.as_deref(),
        Some("zerodha"),
        "the broker that sent it"
    );
    assert!(r.rtt_ms >= 100.0, "{:?}", r);
    assert!(
        r.overhead_ms > 0.0 && r.rtt_ms < r.total_latency_ms,
        "{:?}",
        r
    );
    assert!(adds_up(r), "{:?}", r);
}

/// OBS-03: a sandbox order reached no broker: no broker time, the whole
/// time is the platform's, and the Latency page's average overhead says so.
#[tokio::test]
async fn a_sandbox_order_has_no_broker_time() {
    let h = H::new().await;
    h.analyze(true);
    let (s, v) = h.post("/api/v1/placeorder", h.with_key(order_body())).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let all = rows(&h);
    assert_eq!(all.len(), 1);
    let r = &all[0];
    assert_eq!(r.rtt_ms, 0.0, "{:?}", r);
    assert!(
        r.overhead_ms > 0.0 && r.overhead_ms == r.total_latency_ms,
        "{:?}",
        r
    );
    let (cookie, _) = session(&h);
    let (s, stats) = call(
        &h,
        req(Method::GET, "/latency/api/stats", None, &cookie, None),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(stats["avg_overhead"].as_f64().unwrap() > 0.0, "{}", stats);
}

fn session(h: &H) -> (String, String) {
    let s = h.ctx.sessions.create(h.ctx.now());
    h.ctx
        .sessions
        .update(&s.id, |x| x.user = Some("trader".into()));
    (format!("session={}", s.id), s.csrf_token)
}

fn req(
    method: Method,
    path: &str,
    body: Option<Value>,
    cookie: &str,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("accept", "application/json")
        .header("cookie", cookie);
    if let Some(t) = csrf {
        b = b.header("x-csrftoken", t);
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn call(h: &H, r: Request<Body>) -> (StatusCode, Value) {
    let (s, _, b) = h.send_from(r, IpAddr::V4(Ipv4Addr::LOCALHOST)).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

/// OBS-04: each order a trader sends from the Scalping or Positions page
/// is one latency row with the web's order type; one refused before it
/// reached the order (the CSRF check, no signed-in user) is none, and an
/// `/api/v1` order is still one.
#[tokio::test]
async fn page_orders_are_in_the_latency_log() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let leg = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS"});
    let position = json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"});
    let page: [(&str, Option<Value>, &str); 6] = [
        ("/scalping/api/order", Some(leg.clone()), "PLACE"),
        ("/scalping/api/close_leg", Some(leg), "CLOSE"),
        ("/scalping/api/close_all", None, "CLOSE"),
        ("/scalping/api/cancel_all", None, "CANCEL_ALL"),
        ("/close_position", Some(position), "CLOSE"),
        ("/close_all_positions", None, "CLOSE"),
    ];
    let mut want = Vec::new();
    for (path, body, kind) in page {
        let (s, v) = call(&h, req(Method::POST, path, body, &cookie, Some(&csrf))).await;
        assert!(
            s != StatusCode::UNAUTHORIZED && s != StatusCode::FORBIDDEN,
            "{} {} {}",
            path,
            s,
            v
        );
        want.push(kind);
        let got: Vec<String> = rows(&h).into_iter().map(|r| r.order_type).collect();
        assert_eq!(got, want, "after {}", path);
    }
    // Refused before the order, not recorded: no CSRF token, and a
    // session nobody signed in to.
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/close_all_positions",
            None,
            "session=none",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let anon = h.ctx.sessions.create(h.ctx.now());
    let anon_cookie = format!("session={}", anon.id);
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/close_all_positions",
            None,
            &anon_cookie,
            Some(&anon.csrf_token),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(rows(&h).len(), want.len());
    let (s, v) = h.post("/api/v1/placeorder", h.with_key(order_body())).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let all = rows(&h);
    assert_eq!(all.len(), want.len() + 1);
    assert_eq!(all.last().map(|r| r.order_type.as_str()), Some("PLACE"));
    assert!(all.iter().all(adds_up));
    assert!(all.iter().all(|r| r.broker.as_deref() == Some("zerodha")));
}
