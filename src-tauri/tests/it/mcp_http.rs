//! `/mcp` over HTTP: authentication challenges, JSON-RPC handling, scopes,
//! the kill switch, per-token rate limits, the audit trail, analyzer
//! (sandbox) routing of order tools, Remote MCP reachability, and the admin
//! and token routes.

use crate::mcp_support::{LOCAL, M};
use axum::http::{header, Method, StatusCode};
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::mcp::store::TokenScope;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};

const LAN: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

fn order() -> Value {
    json!({"symbol": "sbin", "quantity": 1, "action": "buy", "exchange": "nse"})
}

fn place_calls(m: &M) -> usize {
    m.h.mock
        .calls
        .lock()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
        .count()
}

/// The tool's enveloped output, whether or not it reports an error.
async fn tool_text(m: &M, t: &str, tool: &str, args: Value) -> Value {
    let v = m.call(t, tool, args).await;
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{} gave no text: {}", tool, v));
    serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }))
}

fn assert_verify_first(out: &Value) {
    let e = &out["data"]["error"];
    assert_eq!(e["retry_safe"], false, "{}", out);
    assert_eq!(e["verify_first"], true, "{}", out);
    assert!(
        !out.to_string().contains("never submitted"),
        "an order that may exist is never called unsent: {}",
        out
    );
}

/// MCP-01 and LOG-08: the broker took the order and the answer was lost on
/// the way back. The AI client is told to check before retrying, never that
/// retrying is safe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_broker_answer_is_never_reported_as_safe_to_retry() {
    use openalgo_desktop_lib::brokers::mock::AfterSend;
    let m = M::new().await;
    m.h.analyze(false);
    let t = m.token(TokenScope::ReadWrite);
    for kind in [AfterSend::Dropped, AfterSend::TimedOut] {
        m.h.mock.after_send.lock().push_back((kind, "open"));
        let out = tool_text(&m, &t, "place_order", order()).await;
        assert_verify_first(&out);
        assert_eq!(out["data"]["error"]["error_type"], "unknown_outcome");
    }
    assert_eq!(place_calls(&m), 2, "each order reached the broker once");
    m.h.shutdown().await;
}

/// MCP-01: a handler that fails after the order reached the broker is an
/// unknown outcome, not a connection failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_after_dispatch_is_an_unknown_outcome() {
    let m = M::new().await;
    m.h.analyze(false);
    let t = m.token(TokenScope::ReadWrite);
    *m.h.mock.panic_after_place.lock() = true;
    let out = tool_text(&m, &t, "place_order", order()).await;
    *m.h.mock.panic_after_place.lock() = false;
    assert_verify_first(&out);
    assert_eq!(place_calls(&m), 1);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_or_revoked_tokens_get_the_bearer_challenge() {
    let m = M::new().await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let (s, h, v) = m.post(None, ping.clone(), LOCAL).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        v,
        json!({"error": "invalid_token", "error_description": "Missing Bearer token."})
    );
    let www = h[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(
        www.starts_with("Bearer realm=\"openalgo-mcp\", error=\"invalid_token\""),
        "{}",
        www
    );
    assert!(www.contains("resource_metadata=\"/.well-known/oauth-protected-resource\""));

    let (s, _, v) = m.post(Some("oamcp_not_a_token"), ping.clone(), LOCAL).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"], "invalid_token");

    let t = m.token(TokenScope::Read);
    assert_eq!(m.rpc(&t, "ping", json!({})).await["result"], json!({}));
    let (_, list) = m.session_json(Method::GET, "/api/mcp/tokens", None).await;
    let id = list["data"][0]["id"].as_i64().unwrap();
    let (s, _) = m
        .session_json(Method::DELETE, &format!("/api/mcp/tokens/{}", id), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = m.post(Some(&t), ping, LOCAL).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_rpc_envelope_and_methods() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    let init = m.rpc(&t, "initialize", json!({})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "openalgo");
    assert_eq!(
        init["result"]["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    assert_eq!(init["id"], 7);

    let (s, _, v) = m.post(Some(&t), json!([1, 2]), LOCAL).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["error"]["code"], -32700);
    let (_, _, v) = m
        .post(
            Some(&t),
            json!({"jsonrpc": "1.0", "id": 3, "method": "ping"}),
            LOCAL,
        )
        .await;
    assert_eq!(
        (v["error"]["code"].clone(), v["id"].clone()),
        (json!(-32600), json!(3))
    );
    let v = m.rpc(&t, "resources/list", json!({})).await;
    assert_eq!(v["error"]["code"], -32601);
    let (s, _, _) = m
        .post(
            Some(&t),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            LOCAL,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);

    let v = m.rpc(&t, "tools/call", json!([1])).await;
    assert_eq!(v["error"]["code"], -32602);
    let v = m.rpc(&t, "tools/call", json!({"arguments": {}})).await;
    assert_eq!(v["error"]["message"], "Invalid params: 'name' is required.");
    let v = m.call(&t, "no_such_tool", json!({})).await;
    assert_eq!(
        v["error"],
        json!({"code": -32601, "message": "Unknown tool: no_such_tool"})
    );
    let v = m.call(&t, "get_quote", json!({"exchange": "NSE"})).await;
    assert_eq!(v["error"]["code"], -32603);
    assert_eq!(
        v["error"]["data"]["reason"],
        "Invalid arguments. Check the tool schema."
    );
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_token_cannot_call_write_tools_and_is_audited() {
    let m = M::new().await;
    m.h.analyze(false);
    let t = m.token(TokenScope::Read);
    let v = m.call(&t, "place_order", order()).await;
    assert_eq!(
        v["error"],
        json!({"code": -32000, "message": "insufficient_scope", "data": {"required_scope": "write:orders"}})
    );
    assert_eq!(place_calls(&m), 0);
    let a = m.audit();
    let last = a.last().unwrap();
    assert_eq!(
        (last.tool.as_str(), last.outcome.as_str()),
        ("place_order", "insufficient_scope")
    );
    assert_eq!(last.scope, "write:orders");
    assert_eq!(last.params_hash.len(), 16);
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_tools_follow_analyzer_mode_like_the_api() {
    let m = M::new().await;
    let t = m.token(TokenScope::ReadWrite);

    m.h.analyze(false);
    let out = m.output(&t, "place_order", order()).await;
    assert_eq!(out["_openalgo_mcp_security"]["tool"], "place_order");
    assert_eq!(out["_openalgo_mcp_security"]["risk"], "broker_structured");
    assert_eq!(
        out["data"],
        json!({"status": "success", "orderid": "MOCK-1"})
    );
    assert_eq!(place_calls(&m), 1, "live mode reaches the broker");

    m.h.analyze(true);
    let out = m.output(&t, "place_order", order()).await;
    assert_eq!(out["data"]["status"], "success", "{}", out);
    assert_eq!(out["data"]["mode"], "analyze", "sandbox reply: {}", out);
    assert_eq!(place_calls(&m), 1, "analyzer mode never reaches the broker");

    let status = m.output(&t, "analyzer_status", json!({})).await;
    assert_eq!(status["data"]["data"]["analyze_mode"], true);

    let audit = m.audit();
    let calls: Vec<_> = audit.iter().filter(|e| e.tool == "place_order").collect();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|e| e.outcome == "success" && e.client_id == "test client"));
    assert!(calls.iter().all(|e| e.request_ip == "127.0.0.1"));
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_errors_are_shaped_like_the_sdk() {
    let m = M::new().await;
    m.h.analyze(false);
    let t = m.token(TokenScope::ReadWrite);
    m.h.mock
        .order_ids
        .lock()
        .push_back(Err("Insufficient margin".into()));
    let out = m.output(&t, "place_order", order()).await;
    assert_eq!(out["data"]["status"], "error");
    assert_eq!(out["data"]["code"], 400);
    assert_eq!(out["data"]["error_type"], "http_error");
    assert!(out["data"]["message"]
        .as_str()
        .unwrap()
        .starts_with("HTTP 400: "));
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_revokes_tokens_and_refuses_writes() {
    let m = M::new().await;
    m.h.analyze(true);
    let t = m.token(TokenScope::ReadWrite);
    let (s, v) = m
        .session_json(
            Method::POST,
            "/admin/api/mcp/kill-switch",
            Some(json!({"confirm": "yes"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    let (s, v) = m
        .session_json(
            Method::POST,
            "/admin/api/mcp/kill-switch",
            Some(json!({"confirm": "REVOKE_ALL_MCP_TOKENS"})),
        )
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "tokens_revoked": 1})
        )
    );
    let (s, _, _) = m
        .post(
            Some(&t),
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            crate::mcp_support::LOCAL,
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(!m.settings().write_scope_enabled);

    // A new token can read but not trade while the switch is on.
    let t2 = m.token(TokenScope::ReadWrite);
    assert_eq!(
        m.output(&t2, "get_funds", json!({})).await["data"]["status"],
        "success"
    );
    let (s, h, v) = m
        .post(
            Some(&t2),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "place_order", "arguments": order()}}),
            LOCAL,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"], "insufficient_scope");
    assert!(h[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .contains("error=\"insufficient_scope\""));
    assert_eq!(m.audit().last().unwrap().outcome, "writes_disabled");

    let (s, v) = m
        .session_json(
            Method::PUT,
            "/admin/api/mcp/settings",
            Some(json!({"write_scope_enabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["settings_pending"]["write_scope_enabled"], true);
    assert_eq!(
        m.output(&t2, "place_order", order()).await["data"]["mode"],
        "analyze"
    );
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_token_rate_limits() {
    let m = M::new().await;
    m.h.analyze(true);
    let t = m.token(TokenScope::ReadWrite);
    for _ in 0..5 {
        assert_eq!(
            m.output(&t, "cancel_all_orders", json!({})).await["data"]["status"],
            "success"
        );
    }
    let v = m.call(&t, "cancel_all_orders", json!({})).await;
    assert_eq!(
        v["error"],
        json!({"code": -32000, "message": "rate_limited",
               "data": {"scope": "write:orders", "limit": "5 per minute"}})
    );
    assert_eq!(m.audit().last().unwrap().outcome, "rate_limited");
    // Reads have their own window; another token has its own budget.
    assert!(m
        .call(&t, "get_index_symbols", json!({}))
        .await
        .get("result")
        .is_some());
    let other = m.token(TokenScope::ReadWrite);
    assert!(m
        .call(&other, "cancel_all_orders", json!({}))
        .await
        .get("result")
        .is_some());
    for _ in 0..59 {
        m.call(&t, "validate_order_constants", json!({})).await;
    }
    let v = m.call(&t, "validate_order_constants", json!({})).await;
    assert_eq!(v["error"]["data"]["limit"], "60 per minute");
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_access_needs_remote_mcp_on() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    let (s, _, _) = m.post(Some(&t), ping.clone(), LAN).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let mut st = m.settings();
    st.http_enabled = true;
    m.save_settings(&st);
    let (s, _, v) = m.post(Some(&t), ping, LAN).await;
    assert_eq!((s, v["result"].clone()), (StatusCode::OK, json!({})));
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_tools_and_the_trust_envelope() {
    let m = M::new().await;
    let t = m.token(TokenScope::Read);
    let out = m
        .output(&t, "get_index_symbols", json!({"exchange": "bse"}))
        .await;
    assert_eq!(out["data"]["exchange_code"], "BSE_INDEX");
    assert_eq!(
        out["_openalgo_mcp_security"]["trust"],
        "untrusted_tool_output"
    );
    let out = m
        .output(&t, "get_index_symbols", json!({"exchange": "MCX"}))
        .await;
    assert_eq!(
        out["data"]["error"]["message"],
        "Unknown exchange: MCX. Use NSE or BSE."
    );
    let out = m.output(&t, "get_openalgo_version", json!({})).await;
    assert!(out["data"]["text"]
        .as_str()
        .unwrap()
        .starts_with("OpenAlgo version: "));
    let out = m
        .output(&t, "search_instruments", json!({"query": "SBIN"}))
        .await;
    assert_eq!(out["_openalgo_mcp_security"]["risk"], "external_text");
    let out = m
        .output(
            &t,
            "check_holiday",
            json!({"date": "2026-10-04", "exchange": "nse"}),
        )
        .await;
    assert_eq!(
        out["data"],
        json!({"status": "success",
        "data": {"date": "2026-10-04", "exchange": "NSE", "is_holiday": true}})
    );
    let out = m
        .output(&t, "check_holiday", json!({"date": "04-10-2026"}))
        .await;
    assert_eq!(
        out["data"]["message"],
        "Invalid date format. Use YYYY-MM-DD"
    );
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_and_token_routes() {
    let m = M::new().await;
    // Sign-in required.
    let (s, _, _) = m
        .raw(
            axum::http::Request::builder()
                .uri("/admin/api/mcp/settings")
                .header(header::ACCEPT, "application/json")
                .body(axum::body::Body::empty())
                .unwrap(),
            LOCAL,
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, v) = m
        .session_json(Method::GET, "/admin/api/mcp/settings", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["settings"],
        json!({"http_enabled": false, "public_url": "", "mcp_url": "",
               "require_approval": false, "write_scope_enabled": true})
    );
    let (s, v) = m
        .session_json(
            Method::PUT,
            "/admin/api/mcp/settings",
            Some(json!({"public_url": "http://x"})),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("public_url must be HTTPS (e.g. https://yourdomain.com).")
        )
    );
    let (s, v) = m
        .session_json(
            Method::PUT,
            "/admin/api/mcp/settings",
            Some(json!({"http_enabled": "yes"})),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("http_enabled must be boolean.")
        )
    );

    let (_, v) = m
        .session_json(Method::GET, "/admin/api/oauth/clients", None)
        .await;
    assert_eq!(v["clients"], json!([]));

    let (s, v) = m
        .session_json(
            Method::POST,
            "/api/mcp/tokens",
            Some(json!({"name": "Claude", "scope": "read"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("oamcp_"));
    // The token is only in the client's env, never in the arguments.
    let cfg = &v["client_config"];
    let server = &cfg["claude_desktop"]["mcpServers"]["openalgo"];
    assert_eq!(server["env"]["OPENALGO_MCP_TOKEN"], token);
    assert!(!server["args"].to_string().contains(&token));
    assert_eq!(server["args"][0], "mcp");
    let code = cfg["claude_code"].as_str().unwrap();
    assert!(code.starts_with("claude mcp add openalgo -e "));
    assert!(!code.split(" -- ").nth(1).unwrap().contains(&token));
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(server["command"], exe);

    let (_, list) = m.session_json(Method::GET, "/api/mcp/tokens", None).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert!(
        !list.to_string().contains(&token),
        "token listed after creation"
    );
    let (s, v) = m
        .session_json(
            Method::POST,
            "/api/mcp/tokens",
            Some(json!({"scope": "admin"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);

    let (_, cfg) = m
        .session_json(Method::GET, "/api/mcp/client-config", None)
        .await;
    assert_eq!(
        cfg["claude_desktop"]["mcpServers"]["openalgo"]["env"]["OPENALGO_MCP_TOKEN"],
        "<MCP_TOKEN>"
    );

    // Audit route shape and filters.
    m.rpc(
        &token,
        "tools/call",
        json!({"name": "get_index_symbols", "arguments": {}}),
    )
    .await;
    m.rpc(
        &token,
        "tools/call",
        json!({"name": "place_order", "arguments": order()}),
    )
    .await;
    let (s, v) = m
        .session_json(Method::GET, "/admin/api/mcp/audit?tool=INDEX", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["count"], 1);
    assert_eq!(v["data"][0]["tool"], "get_index_symbols");
    assert_eq!(v["total_in_window"], 2);
    assert_eq!(v["mcp_enabled"], true);
    let (_, v) = m
        .session_json(
            Method::GET,
            "/admin/api/mcp/audit?outcome=insufficient_scope",
            None,
        )
        .await;
    assert_eq!(v["data"][0]["tool"], "place_order");
    assert!(!v.to_string().contains(&token));
    m.h.shutdown().await;
}
