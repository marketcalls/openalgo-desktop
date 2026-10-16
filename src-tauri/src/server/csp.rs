//! The Content Security Policy every response carries.
//!
//! `connect-src` admits the app's own origin and loopback. The market data
//! feed is on another port, and with other devices allowed (LAN) or a saved
//! feed address (a `wss://` tunnel) the pages connect to a host loopback does
//! not cover, so the policy also names exactly the feed origin that
//! `/api/websocket/config` hands the page (UI-07). Never a wildcard host.

use crate::config::ServerConfig;
use crate::server::routes::websocket_example::feed_url;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

const BEFORE_CONNECT: &str =
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; font-src 'self' data:; \
connect-src 'self' ws://127.0.0.1:* ws://localhost:* http://127.0.0.1:* http://localhost:*";

const AFTER_CONNECT: &str =
    "; worker-src 'self' blob:; frame-ancestors 'none'; object-src 'none'; \
base-uri 'self'; form-action 'self'";

/// The policy for a page loaded with this `Host` header.
pub(crate) fn policy(cfg: &ServerConfig, host_header: Option<&str>) -> String {
    match feed_origin(&feed_url(cfg, host_header)) {
        Some(origin) => format!("{BEFORE_CONNECT} {origin}{AFTER_CONNECT}"),
        None => format!("{BEFORE_CONNECT}{AFTER_CONNECT}"),
    }
}

/// `scheme://host[:port]` of a feed address that loopback does not already
/// cover, or `None`. Anything but a plain `ws`/`wss` host is left out rather
/// than written into the header: the address comes from a setting or from the
/// request's `Host`, and a policy is no place for a wildcard or a `;`.
fn feed_origin(address: &str) -> Option<String> {
    let url = url::Url::parse(address).ok()?;
    let scheme = url.scheme();
    if !matches!(scheme, "ws" | "wss") || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    let plain = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !plain || host.is_empty() {
        return None;
    }
    if scheme == "ws" && matches!(host, "127.0.0.1" | "localhost") {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Sets the policy on every response that has none of its own.
pub(crate) async fn layer(State(ctx): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let mut response = next.run(req).await;
    if !response
        .headers()
        .contains_key(header::CONTENT_SECURITY_POLICY)
    {
        let value = policy(&ctx.server_config(), host.as_deref());
        if let Ok(v) = HeaderValue::from_str(&value) {
            response
                .headers_mut()
                .insert(header::CONTENT_SECURITY_POLICY, v);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lan() -> ServerConfig {
        ServerConfig {
            bind_host: "0.0.0.0".into(),
            ws_port: 8765,
            ..ServerConfig::default()
        }
    }

    fn connect_src(policy: &str) -> &str {
        policy
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("connect-src"))
            .unwrap_or_default()
    }

    #[test]
    fn a_page_opened_from_another_device_may_reach_the_feed_on_its_host() {
        let p = policy(&lan(), Some("192.168.1.10:5000"));
        assert!(
            connect_src(&p).ends_with(" ws://192.168.1.10:8765"),
            "{}",
            p
        );
        assert!(!p.contains("ws://*"), "{}", p);
    }

    #[test]
    fn a_saved_feed_address_is_admitted_by_its_origin() {
        let cfg = ServerConfig {
            websocket_url: Some("wss://feed.example/ws".into()),
            ..ServerConfig::default()
        };
        let p = policy(&cfg, Some("127.0.0.1:5000"));
        assert!(connect_src(&p).ends_with(" wss://feed.example"), "{}", p);
    }

    #[test]
    fn the_loopback_default_is_unchanged() {
        let p = policy(&ServerConfig::default(), Some("127.0.0.1:5000"));
        assert_eq!(
            p,
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; font-src 'self' data:; \
             connect-src 'self' ws://127.0.0.1:* ws://localhost:* http://127.0.0.1:* \
             http://localhost:*; worker-src 'self' blob:; frame-ancestors 'none'; \
             object-src 'none'; base-uri 'self'; form-action 'self'"
        );
        // LAN on, but the page itself opened on loopback.
        assert_eq!(policy(&lan(), Some("127.0.0.1:5000")), p);
        assert_eq!(policy(&lan(), None), p);
    }

    #[test]
    fn a_host_that_is_not_a_plain_name_adds_nothing() {
        let base = policy(&ServerConfig::default(), None);
        for host in [
            "evil; script-src *",
            "*.example:5000",
            "a b:5000",
            "x\"y:5000",
        ] {
            assert_eq!(policy(&lan(), Some(host)), base, "{}", host);
        }
        for saved in [
            "wss://*.example/ws",
            "https://feed.example",
            "wss://u:p@feed.example",
        ] {
            let cfg = ServerConfig {
                websocket_url: Some(saved.into()),
                ..ServerConfig::default()
            };
            assert_eq!(policy(&cfg, None), base, "{}", saved);
        }
    }
}
