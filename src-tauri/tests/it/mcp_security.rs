//! MCP hardening: input limits on the research tools, bounded fan-out (the
//! `/api/v1` limiter, the per-token upstream budget, the research slots),
//! the Origin guard, constant-time token lookup, and a revoked token ending
//! its open event stream.

use crate::mcp_support::{LOCAL, M};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use futures_util::StreamExt;
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::brokers::types::Candle;
use openalgo_desktop_lib::mcp::dispatch::{MCP_CALLER, UPSTREAM_LIMIT};
use openalgo_desktop_lib::mcp::research::{MAX_SCREEN_SYMBOLS, RESEARCH_SLOTS};
use openalgo_desktop_lib::mcp::store::{self, TokenScope};
use openalgo_desktop_lib::server::ratelimit::Bucket;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;
use tower::ServiceExt;

fn history_calls(m: &M) -> usize {
    m.h.mock
        .calls
        .lock()
        .iter()
        .filter(|c| matches!(c, MockCall::History(_)))
        .count()
}

fn candles() -> Vec<Candle> {
    (0..40)
        .map(|i| Candle {
            timestamp: 1_767_205_800 + i * 86_400,
            open: 100.0,
            high: 101.0 + i as f64 % 3.0,
            low: 99.0,
            close: 100.0 + (i as f64 * 0.5).sin(),
            volume: 10,
            oi: 0,
        })
        .collect()
}

fn watchlist(n: usize) -> Value {
    json!((0..n)
        .map(|_| json!({"symbol": "SBIN", "exchange": "NSE"}))
        .collect::<Vec<_>>())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_research_requests_are_refused_before_any_fetch() {
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Ok(candles()));
    let t = m.token(TokenScope::Read);
    let cases = [
        (
            "screen_instruments",
            json!({"symbols": watchlist(MAX_SCREEN_SYMBOLS + 1)}),
        ),
        (
            "multi_timeframe_analysis",
            json!({"symbol": "SBIN", "exchange": "NSE",
                   "intervals": ["1m", "3m", "5m", "10m", "15m", "30m", "1h", "D", "W"]}),
        ),
        (
            "get_historical_data",
            json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D", "bars": 5001}),
        ),
        (
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE", "indicator": "rsi", "lookback_bars": 100000}),
        ),
        (
            "get_trend_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE", "lookback_days": 100000}),
        ),
        (
            "get_momentum_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE", "start_date": "2000-01-01", "end_date": "2026-01-01"}),
        ),
        (
            "detect_signals",
            json!({"symbol": "SBIN", "exchange": "NSE", "limit": 1000000}),
        ),
        ("get_instruments", json!({"limit": 1000000})),
    ];
    for (tool, args) in cases {
        let out = m.output(&t, tool, args).await;
        assert_eq!(
            out["data"]["error"]["error_type"], "limit_exceeded",
            "{}: {}",
            tool, out
        );
    }
    assert_eq!(history_calls(&m), 0, "nothing was fetched");
    // At the limit is fine.
    let ok = m
        .output(
            &t,
            "screen_instruments",
            json!({"symbols": watchlist(MAX_SCREEN_SYMBOLS)}),
        )
        .await;
    assert_eq!(ok["data"]["scanned"], MAX_SCREEN_SYMBOLS);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_tokens_fan_out_is_capped_by_its_upstream_budget() {
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Ok(candles()));
    let t = m.token(TokenScope::Read);
    let budget = UPSTREAM_LIMIT.0;
    let mut refused = 0;
    for _ in 0..(budget / MAX_SCREEN_SYMBOLS + 2) {
        let out = m
            .output(
                &t,
                "screen_instruments",
                json!({"symbols": watchlist(MAX_SCREEN_SYMBOLS)}),
            )
            .await;
        // A call whose fan-out does not fit is refused whole, unfetched.
        if out["data"]["error"]["error_type"] == "rate_limited" {
            refused += 1;
        }
    }
    assert!(
        history_calls(&m) <= budget,
        "{} broker calls",
        history_calls(&m)
    );
    assert!(refused > 0);
    // Another token has its own budget (once the shared per-address API
    // window of one second, which the burst above also used, has passed).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let other = m.token(TokenScope::Read);
    let out = m
        .output(
            &other,
            "get_trend_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    assert!(out["data"]["indicators"].is_object(), "{}", out);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn internal_calls_pass_the_api_rate_limiter() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    // Spend the API bucket of the address MCP calls carry (1 s window).
    while m
        .h
        .ctx
        .limiter
        .check(Bucket::Api, MCP_CALLER, m.h.ctx.limiter.now())
        .is_ok()
    {}
    let out = m.output(&t, "get_funds", json!({})).await;
    assert_eq!(out["data"]["code"], 429, "{}", out);
    assert_eq!(out["data"]["error_type"], "http_error");
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_calls_wait_for_a_slot() {
    let m = std::sync::Arc::new(M::new().await);
    *m.h.mock.history.lock() = Some(Ok(candles()));
    let t = m.token(TokenScope::Read);
    let slots = m.h.ctx.mcp.research_slots();
    let held: Vec<_> = (0..RESEARCH_SLOTS)
        .map(|_| slots.clone().try_acquire_owned().unwrap())
        .collect();
    let (m2, t2) = (m.clone(), t.clone());
    let call = tokio::spawn(async move {
        m2.output(
            &t2,
            "get_trend_snapshot",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!call.is_finished(), "ran without a slot");
    assert_eq!(history_calls(&m), 0);
    drop(held);
    let out = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap();
    assert!(out["data"]["indicators"].is_object(), "{}", out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_origins_are_refused() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    let port = m.h.ctx.server_config().http_port;
    let send = |origin: Option<String>, method: &str| {
        let mut b = Request::builder()
            .method(method)
            .uri("/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {}", t))
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        let body = if method == "POST" {
            Body::from(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string())
        } else {
            Body::empty()
        };
        b.body(body).unwrap()
    };
    for o in [
        "http://evil.example",
        "null",
        "https://127.0.0.1.evil.example",
    ] {
        for method in ["POST", "GET"] {
            let (s, _, b) = m.raw(send(Some(o.into()), method), LOCAL).await;
            assert_eq!(
                s,
                StatusCode::FORBIDDEN,
                "{} {}: {}",
                method,
                o,
                String::from_utf8_lossy(&b)
            );
        }
    }
    let (s, _, _) = m
        .raw(
            send(Some(format!("http://127.0.0.1:{}", port)), "POST"),
            LOCAL,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = m.raw(send(None, "POST"), LOCAL).await;
    assert_eq!(s, StatusCode::OK);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_lookup_rejects_near_misses() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    let c = m.h.ctx.sqlite.conn().unwrap();
    assert!(store::find_token(&c, &t).unwrap().is_some());
    // Same display prefix, different secret part.
    let mut near = t.clone();
    let last = near.pop().unwrap();
    near.push(if last == '0' { '1' } else { '0' });
    assert!(store::find_token(&c, &near).unwrap().is_none());
    assert!(store::find_token(&c, &t[..t.len() - 1]).unwrap().is_none());
    drop(c);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_a_token_ends_its_open_event_stream() {
    let m = M::new().await;
    m.h.ctx
        .mcp
        .set_stream_timing(Duration::from_secs(60), Duration::from_millis(100));
    let t = m.token(TokenScope::Read);
    let mut req = Request::builder()
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {}", t))
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(LOCAL, 40000)));
    crate::with_host(&mut req, &m.h.ctx);
    let resp = openalgo_desktop_lib::server::app(m.h.ctx.clone())
        .oneshot(req)
        .await
        .unwrap();
    let mut body = resp.into_body().into_data_stream();
    body.next().await.unwrap().unwrap();
    let id = store::find_token(&m.h.ctx.sqlite.conn().unwrap(), &t)
        .unwrap()
        .unwrap()
        .id;
    store::revoke_token(&m.h.ctx.sqlite.conn().unwrap(), id, m.h.ctx.now()).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while body.next().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "stream outlived its token");
    drop(body);
    assert_eq!(m.h.ctx.mcp.open_streams(), 0);
    m.h.shutdown().await;
}
