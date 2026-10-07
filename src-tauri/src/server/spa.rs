//! The built React app (`dist/`), embedded in release builds with
//! `rust-embed` and read from disk in debug builds. In development the Tauri
//! window points at Vite, which proxies API calls here.
//!
//! Known frontend routes get `index.html` with 200; any other path gets
//! `index.html` with 404 (the web does the same, and React shows its
//! NotFound page). The route list must follow the frontend router: adding a
//! page means adding its path here in the same change.

use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
// Relative to the crate root. rust-embed expands `$VAR` in this path only
// with its `interpolate-folder-path` feature; without it a `$CARGO_MANIFEST_DIR`
// prefix was taken literally, so debug builds looked in a folder that does not
// exist and release builds embedded nothing (allowed by `allow_missing`).
#[folder = "../dist"]
#[allow_missing = true]
struct Assets;

/// Frontend routes (web `App.tsx`), minus the out-of-scope Python strategy
/// host, Flow and pandas backtesters. `:x` matches one path segment.
pub const SPA_ROUTES: &[&str] = &[
    "/",
    "/:broker/auth",
    "/action-center",
    "/admin",
    "/admin/diagnostics",
    "/admin/freeze",
    "/admin/holidays",
    "/admin/remote-mcp",
    "/admin/timings",
    "/agent",
    "/agent/config",
    "/analyzer",
    "/apikey",
    "/arbitrage",
    "/broker",
    "/broker/:broker/totp",
    "/broker/samco/auth",
    "/chart/test",
    "/chartink",
    "/chartink/:strategyId",
    "/chartink/:strategyId/configure",
    "/chartink/new",
    "/dashboard",
    "/download",
    "/error",
    "/faq",
    "/gammadensity",
    "/gex",
    "/gocharting",
    "/health",
    "/historify",
    "/historify/charts",
    "/historify/charts/:symbol",
    "/holdings",
    "/ivchart",
    "/ivsmile",
    "/leverage",
    "/login",
    "/logs",
    "/logs/latency",
    "/logs/live",
    "/logs/sandbox",
    "/logs/security",
    "/logs/traffic",
    "/master-contract",
    "/maxpain",
    "/oiprofile",
    "/oirange",
    "/oitracker",
    "/optionchain",
    "/orderbook",
    "/platforms",
    "/playground",
    "/pnl-tracker",
    "/positions",
    "/profile",
    "/rate-limited",
    "/reset-password",
    "/sandbox",
    "/sandbox/mypnl",
    "/scalping",
    "/search",
    "/search/token",
    "/setup",
    "/straddle",
    "/straddlepnl",
    "/strategy",
    "/strategy/:strategyId",
    "/strategy/:strategyId/edit",
    "/strategy/new",
    "/strategybuilder",
    "/strategybuilder/portfolio",
    "/telegram",
    "/telegram/analytics",
    "/telegram/config",
    "/telegram/users",
    "/tools",
    "/tools/strategy",
    "/tools/strategy/portfolio",
    "/tradebook",
    "/trading",
    "/tradingview",
    "/volsurface",
    "/websocket/order",
    "/websocket/test",
    "/websocket/test/20",
    "/websocket/test/30",
    "/websocket/test/50",
    "/whatsapp",
];

pub fn is_spa_route(path: &str) -> bool {
    let path = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    let segs: Vec<&str> = path.split('/').collect();
    SPA_ROUTES.iter().any(|r| {
        let rs: Vec<&str> = r.split('/').collect();
        rs.len() == segs.len()
            && rs
                .iter()
                .zip(segs.iter())
                .all(|(a, b)| a.starts_with(':') && !b.is_empty() || a == b)
    })
}

const FALLBACK_INDEX: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"UTF-8\"><title>OpenAlgo</title></head>\
<body><p>The OpenAlgo interface is not built in this copy of the app. Reinstall OpenAlgo Desktop.</p></body></html>";

pub fn index_response(status: StatusCode) -> Response {
    let body = Assets::get("index.html")
        .map(|f| Body::from(f.data.into_owned()))
        .unwrap_or_else(|| Body::from(FALLBACK_INDEX));
    let mut r = (status, body).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// Static asset or SPA page for a GET that matched no API route.
pub fn serve(uri: &Uri) -> Response {
    let path = uri.path();
    let rel = path.trim_start_matches('/');
    if !rel.is_empty() && !rel.ends_with('/') && !rel.contains("..") {
        if let Some(f) = Assets::get(rel) {
            let mime = mime_guess::from_path(rel).first_or_octet_stream();
            let mut r = Body::from(f.data.into_owned()).into_response();
            if let Ok(v) = HeaderValue::from_str(mime.as_ref()) {
                r.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            let cache = if rel.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            return r;
        }
    }
    if is_spa_route(path) {
        index_response(StatusCode::OK)
    } else {
        index_response(StatusCode::NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    /// The interface ships inside the app. When the frontend has been built
    /// (CI builds it before the Tauri bundle), index.html must be found;
    /// a wrong folder path would otherwise only show up as a blank app.
    #[test]
    fn built_interface_is_found_when_present() {
        let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("dist")
            .join("index.html");
        if dist.exists() {
            assert!(
                super::Assets::get("index.html").is_some(),
                "dist/index.html exists but the server cannot see it"
            );
        }
    }

    use super::*;

    #[test]
    fn route_matching() {
        assert!(is_spa_route("/"));
        assert!(is_spa_route("/dashboard"));
        assert!(is_spa_route("/dashboard/"));
        assert!(is_spa_route("/broker/angel/totp"));
        assert!(is_spa_route("/strategy/42/edit"));
        assert!(!is_spa_route("/doesnotexist"));
        assert!(!is_spa_route("/python"));
        assert!(!is_spa_route("/broker/angel/totp/x"));
    }
}
