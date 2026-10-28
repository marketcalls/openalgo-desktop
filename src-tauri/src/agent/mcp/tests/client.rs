//! The client: protocol, retry and deadline, circuit breaker, caches.

use super::client_for;
use crate::agent::mcp::client::McpClient;
use crate::agent::mcp::client::{user_agent, PROTOCOL_VERSION};
use crate::agent::mcp::errors::Kind;
use crate::agent::mcp::testing::{
    assert_trader_safe, reply_raw, respond, spec, FakeClock, FakeServer, Handler, Seam, RUPEE,
};
use crate::agent::mcp::transport::Failure;
use crate::agent::mcp::wire::parse_sse;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn names(tools: &[Map<String, Value>]) -> Vec<&str> {
    tools.iter().map(|t| t["name"].as_str().unwrap()).collect()
}

/// A handler that fails `n` times with `fail`, then answers as usual.
fn failing(
    n: usize,
    fail: impl Fn() -> Result<crate::agent::mcp::wire::Reply, Failure> + Send + Sync + 'static,
) -> Handler {
    let left = AtomicUsize::new(n);
    respond(move |server: &FakeServer, seen| {
        if left.load(Ordering::SeqCst) > 0 {
            left.fetch_sub(1, Ordering::SeqCst);
            return fail();
        }
        server.default_reply(seen)
    })
}

fn args() -> Map<String, Value> {
    json!({"symbol": "SBIN"}).as_object().cloned().unwrap()
}

// --------------------------------------------------------------- protocol

#[tokio::test]
async fn initialize_comes_first_and_the_session_header_follows() {
    let (client, server, _) = client_for(spec("demo_eod"));
    let tools = client.list_tools(false).await.unwrap();
    assert_eq!(
        server.methods(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
    let seen = server.seen.lock().clone();
    assert!(!seen[0].headers.contains_key("Mcp-Session-Id"));
    assert_eq!(
        seen[0].payload["params"]["protocolVersion"],
        PROTOCOL_VERSION
    );
    assert_eq!(
        seen[0].payload["params"]["clientInfo"]["name"],
        "openalgo-agent"
    );
    assert_eq!(seen[2].headers["Mcp-Session-Id"], "s1");
    assert_eq!(
        seen[2].headers["Accept"],
        "application/json, text/event-stream"
    );
    assert_eq!(seen[2].headers["MCP-Protocol-Version"], "2025-06-18");
    assert!(seen[2].headers["User-Agent"].starts_with("OpenAlgo-Agent/"));
    assert!(user_agent().contains("OpenAlgo-Desktop"));
    // The denied tool is filtered out.
    assert_eq!(names(&tools), ["get_stock_history", "get_market_mood"]);
}

#[tokio::test]
async fn a_second_request_reuses_the_session() {
    let (client, server, _) = client_for(spec("demo_eod"));
    client.list_tools(false).await.unwrap();
    client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap();
    assert_eq!(server.count("initialize"), 1);
}

#[tokio::test]
async fn an_event_stream_reply_is_read_past_notifications() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.sse.store(true, Ordering::SeqCst);
    let result = client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&result.text).unwrap(),
        json!({"symbol": "SBIN", "close": 812.5})
    );
}

#[test]
fn sse_parsing_joins_multi_line_data_and_skips_comments() {
    let body = ": comment\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\nevent: x\nid: 7\ndata: not json\n\ndata:{\"b\":2}\n";
    assert_eq!(parse_sse(body), vec![json!({"a": 1}), json!({"b": 2})]);
}

#[tokio::test]
async fn an_expired_session_is_initialised_once_more_and_retried() {
    let (client, server, _) = client_for(spec("demo_eod"));
    client.list_tools(false).await.unwrap();
    server.live.lock().clear();
    client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap();
    assert_eq!(server.count("initialize"), 2);
    let last = server.seen.lock().last().cloned().unwrap();
    assert_eq!(last.headers["Mcp-Session-Id"], "s2");
}

#[tokio::test]
async fn utf8_text_survives_the_round_trip() {
    let (client, _, _) = client_for(spec("demo_eod"));
    let tools = client.list_tools(false).await.unwrap();
    assert!(tools[0]["description"].as_str().unwrap().contains(RUPEE));
}

#[tokio::test]
async fn a_json_rpc_argument_error_on_a_call_is_a_tool_error() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on("tools/call", respond(|_, seen| {
            let body = json!({"jsonrpc": "2.0", "id": seen.payload["id"], "error": {"code": -32602, "message": "symbol is required"}});
            Ok(reply_raw(200, &[("content-type", "application/json")], &body.to_string()))
        }),
    );
    let e = client
        .call_tool("get_stock_history", &Map::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, Kind::Tool("symbol is required".into()));
    assert_eq!(
        e.message,
        "Demo end-of-day data refused that tool name or its arguments."
    );
}

#[tokio::test]
async fn is_error_in_a_result_is_a_tool_error_carrying_the_server_text() {
    let (client, server, _) = client_for(spec("demo_eod"));
    *server.call_result.lock() =
        json!({"isError": true, "content": [{"type": "text", "text": "bad date"}]});
    let e = client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap_err();
    assert_eq!(e.kind, Kind::Tool("bad date".into()));
}

#[tokio::test]
async fn a_reply_that_is_not_json_is_a_protocol_error() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "tools/list",
        respond(|_, _| {
            Ok(reply_raw(
                200,
                &[("content-type", "text/html")],
                "<html>gateway</html>",
            ))
        }),
    );
    let e = client.list_tools(false).await.unwrap_err();
    assert_eq!(e.code(), "protocol_error");
    assert_trader_safe(&e.message);
}

#[tokio::test]
async fn content_items_are_joined_and_others_named() {
    let (client, server, _) = client_for(spec("demo_eod"));
    *server.call_result.lock() = json!({"content": [
        {"type": "text", "text": "one"}, {"type": "image", "data": "x"},
        {"type": "resource", "resource": {"text": "two"}}, {"type": "resource", "resource": {"blob": "x"}}
    ], "structuredContent": {"a": 1}});
    let r = client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap();
    assert_eq!(
        r.text,
        "one\n[image content was omitted]\ntwo\n[a binary resource was omitted]"
    );
    assert_eq!(r.structured, Some(json!({"a": 1})));
}

// ------------------------------------------------- retry and the deadline

#[tokio::test]
async fn a_timeout_is_retried_once_and_the_retry_answers() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "tools/list",
        failing(1, || Err(Failure::TimedOut("timeout".into()))),
    );
    client.list_tools(false).await.unwrap();
    assert_eq!(server.count("tools/list"), 2);
}

#[tokio::test]
async fn a_gateway_504_is_retried_once() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "tools/list",
        failing(1, || Ok(reply_raw(504, &[], "Gateway Timeout"))),
    );
    client.list_tools(false).await.unwrap();
    assert_eq!(server.count("tools/list"), 2);
}

#[tokio::test]
async fn two_timeouts_give_up_with_a_sentence_a_trader_can_read() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "tools/list",
        failing(9, || Err(Failure::TimedOut("ReadTimeout".into()))),
    );
    let e = client.list_tools(false).await.unwrap_err();
    assert_eq!(e.kind, Kind::TimedOut);
    assert_eq!(server.count("tools/list"), 2);
    assert!(e.message.contains("Example Data's side"), "{}", e.message);
    assert_trader_safe(&e.message);
}

#[tokio::test]
async fn a_connection_failure_is_unreachable_and_trader_safe() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "initialize",
        failing(9, || Err(Failure::Unreachable("ConnectError".into()))),
    );
    let e = client.list_tools(false).await.unwrap_err();
    assert_eq!((e.code(), server.count("initialize")), ("unavailable", 2));
    assert_trader_safe(&e.message);
}

#[tokio::test]
async fn no_retry_is_attempted_past_the_deadline() {
    let clock = Arc::new(FakeClock::default());
    let server = Arc::new(FakeServer {
        clock: Some((clock.clone(), Duration::from_secs(24))),
        ..FakeServer::default()
    });
    let client = McpClient::new(
        Arc::new(spec("demo_eod")),
        Arc::new(Seam(server.clone())),
        clock.clone(),
    );
    server.on(
        "initialize",
        failing(9, || Err(Failure::TimedOut("timeout".into()))),
    );
    let e = client.list_tools(false).await.unwrap_err();
    assert_eq!((e.kind, server.count("initialize")), (Kind::TimedOut, 1));
}

#[tokio::test]
async fn each_request_carries_the_servers_timeouts() {
    let (client, server, _) = client_for(spec("demo_eod"));
    client.list_tools(false).await.unwrap();
    let t = server.seen.lock()[0].timeouts;
    assert_eq!(
        (t.connect, t.read, t.total),
        (
            Duration::from_secs(5),
            Duration::from_secs(10),
            Duration::from_secs(25)
        )
    );
}

#[tokio::test]
async fn a_slow_down_is_not_retried() {
    let (client, server, _) = client_for(spec("demo_eod"));
    server.on(
        "tools/list",
        failing(9, || Ok(reply_raw(429, &[], "slow down"))),
    );
    let e = client.list_tools(false).await.unwrap_err();
    assert_eq!(server.count("tools/list"), 1);
    assert_eq!(
        e.message,
        "Demo end-of-day data asked this platform to slow down. Wait a minute and try again."
    );
}

// -------------------------------------------------------- circuit breaker

fn breaking(threshold: u32) -> (Arc<McpClient>, Arc<FakeServer>, Arc<FakeClock>) {
    let mut s = spec("demo_eod");
    s.breaker_threshold = threshold;
    s.max_retries = 0;
    client_for(s)
}

#[tokio::test]
async fn it_opens_after_the_threshold_and_answers_without_calling() {
    let (client, server, _) = breaking(2);
    server.on(
        "initialize",
        failing(2, || Ok(reply_raw(502, &[], "bad gateway"))),
    );
    for _ in 0..2 {
        assert_eq!(
            client.list_tools(true).await.unwrap_err().code(),
            "unavailable"
        );
    }
    let sent = server.seen.lock().len();
    let e = client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap_err();
    assert!(matches!(e.kind, Kind::Paused(after) if after > Duration::ZERO));
    assert_eq!(
        server.seen.lock().len(),
        sent,
        "a paused server is not called"
    );
    assert_trader_safe(&e.message);
    assert!(client.breaker().1);
}

#[tokio::test]
async fn it_lets_one_call_through_after_the_cooldown_and_closes_on_success() {
    let (client, server, clock) = breaking(2);
    server.on("initialize", failing(2, || Ok(reply_raw(502, &[], "x"))));
    for _ in 0..2 {
        let _ = client.list_tools(true).await;
    }
    clock.advance(Duration::from_secs(61));
    client
        .call_tool("get_stock_history", &args())
        .await
        .unwrap();
    assert_eq!(client.breaker(), (0, false, Duration::ZERO));
}

#[tokio::test]
async fn a_failed_trial_opens_it_again_for_a_full_cooldown() {
    let (client, server, clock) = breaking(2);
    server.on("initialize", failing(3, || Ok(reply_raw(502, &[], "x"))));
    for _ in 0..2 {
        let _ = client.list_tools(true).await;
    }
    clock.advance(Duration::from_secs(61));
    assert_eq!(
        client
            .call_tool("get_stock_history", &args())
            .await
            .unwrap_err()
            .code(),
        "unavailable"
    );
    assert_eq!(
        client
            .call_tool("get_stock_history", &args())
            .await
            .unwrap_err()
            .code(),
        "paused"
    );
}

#[tokio::test]
async fn a_tool_error_is_an_answer_and_does_not_count_as_a_failure() {
    let (client, server, _) = breaking(1);
    *server.call_result.lock() =
        json!({"isError": true, "content": [{"type": "text", "text": "no"}]});
    for _ in 0..3 {
        assert_eq!(
            client
                .call_tool("get_stock_history", &args())
                .await
                .unwrap_err()
                .code(),
            "tool_error"
        );
    }
    assert_eq!(client.breaker().0, 0);
}

#[tokio::test]
async fn a_refusal_at_the_door_counts_as_a_failure() {
    for status in [401u16, 403, 400] {
        let (client, server, _) = breaking(2);
        server.on(
            "initialize",
            respond(move |_, _| {
                Ok(reply_raw(
                    status,
                    &[("content-type", "text/html")],
                    "<html>denied</html>",
                ))
            }),
        );
        for _ in 0..2 {
            let e = client.list_tools(true).await.unwrap_err();
            assert_eq!(e.code(), "protocol_error");
            assert_trader_safe(&e.message);
        }
        let sent = server.seen.lock().len();
        assert_eq!(
            client
                .call_tool("get_stock_history", &args())
                .await
                .unwrap_err()
                .code(),
            "paused"
        );
        assert_eq!(server.seen.lock().len(), sent);
    }
}

#[tokio::test]
async fn a_404_without_a_session_does_not_count_as_a_failure() {
    let (client, server, _) = breaking(1);
    server.on("initialize", respond(|_, _| Ok(reply_raw(404, &[], "no"))));
    let e = client.list_tools(true).await.unwrap_err();
    assert_eq!(e.code(), "protocol_error");
    assert_eq!(client.breaker().0, 0);
}

#[tokio::test]
async fn the_connection_test_ignores_an_open_breaker() {
    let (client, server, _) = breaking(1);
    server.on("initialize", failing(1, || Ok(reply_raw(502, &[], "x"))));
    let _ = client.list_tools(true).await;
    assert!(client.breaker().1);
    let check = client.check().await;
    assert!(check.ok, "{:?}", check);
    assert_eq!(
        (check.tool_count, check.server_name.as_str()),
        (2, "fake-server")
    );
    assert_eq!(
        check.message,
        "Demo end-of-day data answered and offers 2 tools."
    );
    assert!(!client.breaker().1);
}

/// The first `n` requests answer 502, every later one never answers.
struct FailThenHang(AtomicUsize);

#[async_trait::async_trait]
impl crate::agent::mcp::transport::Transport for FailThenHang {
    async fn post(
        &self,
        _: &str,
        _: &[(&'static str, String)],
        _: Vec<u8>,
        _: crate::agent::mcp::transport::Timeouts,
    ) -> Result<crate::agent::mcp::wire::Reply, Failure> {
        if self.0.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Ok(reply_raw(502, &[], "x"));
        }
        std::future::pending().await
    }
}

/// A trial whose future is dropped (the trader stopped the turn) frees its
/// slot, so the breaker never sticks paused.
#[tokio::test]
async fn a_dropped_trial_frees_its_slot() {
    let mut s = spec("demo_eod");
    s.breaker_threshold = 1;
    s.max_retries = 0;
    let clock = Arc::new(FakeClock::default());
    let client = McpClient::new(
        Arc::new(s),
        Arc::new(FailThenHang(AtomicUsize::new(1))),
        clock.clone(),
    );
    let _ = client.list_tools(true).await;
    assert!(client.breaker().1);
    clock.advance(Duration::from_secs(61));
    let a = args();
    let mut trial = Box::pin(client.call_tool("get_stock_history", &a));
    assert!(futures_util::poll!(trial.as_mut()).is_pending());
    assert!(client.probing(), "the trial is in flight");
    drop(trial);
    assert!(!client.probing(), "a dropped trial frees its slot");
}

// ------------------------------------------------------------------ caches

#[tokio::test]
async fn the_list_is_cached_until_its_ttl_runs_out() {
    let (client, server, clock) = client_for(spec("demo_eod"));
    client.list_tools(false).await.unwrap();
    client.list_tools(false).await.unwrap();
    assert_eq!(server.count("tools/list"), 1);
    clock.advance(Duration::from_secs(3601));
    client.list_tools(false).await.unwrap();
    assert_eq!(server.count("tools/list"), 2);
}

#[tokio::test]
async fn a_failed_fetch_is_remembered_briefly_then_tried_again() {
    let (client, server, clock) = client_for(spec("demo_eod"));
    server.on("tools/list", failing(2, || Ok(reply_raw(503, &[], "x"))));
    assert!(client.list_tools(false).await.is_err());
    let sent = server.seen.lock().len();
    assert!(client.list_tools(false).await.is_err());
    assert_eq!(server.seen.lock().len(), sent, "answered from memory");
    clock.advance(Duration::from_secs(31));
    client.list_tools(false).await.unwrap();
}

#[tokio::test]
async fn a_denied_tool_is_neither_listed_nor_callable() {
    let (client, server, _) = client_for(spec("demo_eod"));
    assert!(!names(&client.list_tools(false).await.unwrap()).contains(&"admin_purge"));
    let e = client
        .call_tool("admin_purge", &Map::new())
        .await
        .unwrap_err();
    assert_eq!(e.code(), "tool_error");
    assert_eq!(server.count("tools/call"), 0);
}

#[tokio::test]
async fn an_allowlist_offers_only_what_it_names() {
    let mut s = spec("demo_eod");
    s.allow_tools = vec!["get_market_mood".into()];
    let (client, _, _) = client_for(s);
    assert_eq!(
        names(&client.list_tools(false).await.unwrap()),
        ["get_market_mood"]
    );
}
