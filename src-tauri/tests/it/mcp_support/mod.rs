//! Harness for the MCP tests: the `/api/v1` harness (mock broker connected,
//! API key, symbol master) plus MCP tokens, a signed-in browser session and
//! JSON-RPC helpers that call `/mcp` in process from loopback.

#![allow(dead_code)]

use crate::api_v1_support::H;
use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use openalgo_desktop_lib::mcp::store::{self, TokenScope};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};

pub const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

pub struct M {
    pub h: H,
}

impl M {
    pub async fn new() -> Self {
        Self { h: H::new().await }
    }

    /// A live token with this scope.
    pub fn token(&self, scope: TokenScope) -> String {
        let c = self.h.ctx.sqlite.conn().unwrap();
        store::create_token(&c, "test client", scope, self.h.ctx.now())
            .unwrap()
            .1
    }

    pub fn settings(&self) -> store::Settings {
        store::settings(&self.h.ctx.sqlite.conn().unwrap()).unwrap()
    }

    pub fn save_settings(&self, s: &store::Settings) {
        store::save_settings(&self.h.ctx.sqlite.conn().unwrap(), s).unwrap()
    }

    /// Signed-in browser session: (cookie, csrf).
    pub fn session(&self) -> (String, String) {
        let s = self.h.ctx.sessions.create(self.h.ctx.now());
        self.h
            .ctx
            .sessions
            .update(&s.id, |x| x.user = Some("trader".into()));
        (format!("session={}", s.id), s.csrf_token)
    }

    pub async fn raw(&self, req: Request<Body>, ip: IpAddr) -> (StatusCode, HeaderMap, Vec<u8>) {
        self.h.send_from(req, ip).await
    }

    /// POST /mcp with a JSON body.
    pub async fn post(
        &self,
        token: Option<&str>,
        body: Value,
        ip: IpAddr,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {}", t));
        }
        let (s, h, bytes) = self
            .raw(b.body(Body::from(body.to_string())).unwrap(), ip)
            .await;
        (s, h, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    pub async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let (s, _, v) = self
            .post(
                Some(token),
                json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
                LOCAL,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{} -> {}", method, v);
        v
    }

    /// `tools/call`: the JSON-RPC reply.
    pub async fn call(&self, token: &str, tool: &str, args: Value) -> Value {
        self.rpc(
            token,
            "tools/call",
            json!({"name": tool, "arguments": args}),
        )
        .await
    }

    /// `tools/call` that succeeded: the enveloped tool output, parsed.
    pub async fn output(&self, token: &str, tool: &str, args: Value) -> Value {
        let v = self.call(token, tool, args).await;
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{} gave no text: {}", tool, v));
        assert_eq!(v["result"]["isError"], false);
        serde_json::from_str(text).unwrap()
    }

    /// Session request with cookie and CSRF.
    pub async fn session_json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (cookie, csrf) = self.session();
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, cookie)
            .header("x-csrftoken", csrf);
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let (s, _, bytes) = self.raw(b.body(body).unwrap(), LOCAL).await;
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Audit rows, oldest first.
    pub fn audit(&self) -> Vec<store::AuditEntry> {
        let c = self.h.ctx.logs.conn().unwrap();
        store::audit_tail(
            &c,
            &store::AuditQuery {
                limit: 500,
                ..Default::default()
            },
        )
        .unwrap()
        .0
    }
}
