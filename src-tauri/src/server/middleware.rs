//! Request middleware: Host check, browser session + CSRF, the signed-in
//! user guard, and the `/api/v1` rate limit.

use crate::server::envelope::{error, json_response, not_authenticated};
use crate::server::form::{csrf_rejected, tokens_match, FormData};
use crate::server::ratelimit::{Bucket, Claim, SignInSource};
use crate::session::web::{WebSession, COOKIE_NAME};
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{ConnectInfo, FromRequest, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

/// The browser session for this request, if the cookie named one.
#[derive(Clone, Debug, Default)]
pub struct SessionCtx(pub Option<WebSession>);

/// Inserted by the user guard for handlers behind it.
#[derive(Clone, Debug)]
pub struct AuthedUser {
    pub username: String,
    pub session_id: String,
}

/// Where a request comes from (security review S-02, S-03). Classified
/// once per request by [`peer_layer`], the outermost application layer, from
/// the socket peer and the request as it arrived, and stored in the
/// request. Every per-caller control (rate limits, sign-in budgets, bans,
/// the webhook allowlist, the Remote MCP switch, account setup) reads the
/// stored value through [`Src`], [`ClientIp`] or [`client_ip`]; nothing else
/// reads forwarding headers, so no two checks can see different callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A program on this computer: a loopback peer that sent no
    /// forwarding-type header and names the app's own loopback address.
    Local,
    /// A device on the network: a socket peer that is not loopback. Its
    /// headers are never read.
    Lan(IpAddr),
    /// A caller behind a tunnel or proxy on this computer: a loopback peer
    /// that sent a forwarding-type header or named any other host. Its
    /// address is unknown: no forwarding header is ever read as one, so
    /// every tunnel caller is one shared identity ([`PROXIED_CALLER`]).
    Tunnel,
}

impl Source {
    /// The address per-address controls count this caller under. Every
    /// tunnel caller shares [`PROXIED_CALLER`].
    pub fn ip(self) -> IpAddr {
        match self {
            Source::Local => IpAddr::V4(Ipv4Addr::LOCALHOST),
            Source::Lan(ip) => ip,
            Source::Tunnel => PROXIED_CALLER,
        }
    }

    /// The address bans and IP allowlists apply to: a device on the
    /// network only. This computer and tunnel callers have none.
    pub fn network_address(self) -> Option<IpAddr> {
        match self {
            Source::Lan(ip) => Some(ip),
            _ => None,
        }
    }

    pub fn is_local(self) -> bool {
        self == Source::Local
    }
}

/// Stand-in address for callers that reach the app through a proxy or
/// tunnel on this machine whose address is not known: one shared,
/// non-loopback identity, so they are never trusted as local and never
/// share limits with the trader's own programs. In the discard-only
/// `100::/64` block, like the MCP dispatcher's address.
pub const PROXIED_CALLER: IpAddr = IpAddr::V6(std::net::Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 2));

/// Header names a reverse proxy, tunnel or CDN adds (ngrok, cloudflared,
/// Tailscale Funnel, Fastly, Fly, nginx, Caddy, Envoy), besides every
/// `x-forwarded-*`. A loopback request carrying any of them, whatever its
/// value (empty and repeated ones included), came from somewhere else.
const FORWARDING_HEADERS: [&str; 13] = [
    "forwarded",
    "via",
    "x-real-ip",
    "x-client-ip",
    "x-cluster-client-ip",
    "x-original-forwarded-for",
    "true-client-ip",
    "cf-connecting-ip",
    "cf-ray",
    "fastly-client-ip",
    "fly-client-ip",
    "tailscale-funnel-request",
    "x-envoy-external-address",
];

/// Whether a forwarding-type header is present. Header names are matched
/// case-insensitively (they are stored in lower case).
fn forwarded(headers: &HeaderMap) -> bool {
    headers.keys().any(|k| {
        let n = k.as_str();
        n.starts_with("x-forwarded-") || FORWARDING_HEADERS.contains(&n)
    })
}

fn is_loopback(ip: IpAddr) -> bool {
    crate::server::addr::canonical(ip).is_loopback()
}

/// `host[:port]` split into its name (lower case, IPv6 without brackets)
/// and port. `None` for a malformed port or bracket.
fn split_authority(host: &str) -> Option<(String, Option<u16>)> {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        let (name, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        return Some((name.to_string(), port));
    }
    match host.rsplit_once(':') {
        Some((name, p)) => Some((name.to_string(), Some(p.parse::<u16>().ok()?))),
        None => Some((host, None)),
    }
}

/// Whether `port` is one the app's own pages are served on.
fn app_port(cfg: &crate::config::ServerConfig, listening: u16, port: Option<u16>) -> bool {
    match port {
        None => cfg.http_port == 80,
        Some(p) => p == cfg.http_port || p == listening || (cfg!(debug_assertions) && p == 5173),
    }
}

/// Whether `host` is the app's own address on this computer.
fn names_this_computer(cfg: &crate::config::ServerConfig, listening: u16, host: &str) -> bool {
    split_authority(host).is_some_and(|(name, port)| {
        matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1") && app_port(cfg, listening, port)
    })
}

/// Who is calling (security review S-02, S-03). `socket` is the connection's
/// peer (`None` for an in-process call with no connection, as in tests);
/// `authority` the request target's authority (HTTP/2 `:authority`, or an
/// absolute-form HTTP/1.1 target).
///
/// * A peer that is not loopback is [`Source::Lan`]; its headers are never
///   read (anyone can send them).
/// * A loopback peer (or no connection) is [`Source::Local`] only when no
///   forwarding-type header is present and every host it names (`Host`,
///   exactly once, and the target's authority) is the app's own loopback
///   address and port. A request with a connection must name one.
/// * Anything else is [`Source::Tunnel`]: never local, no address, one
///   shared identity.
pub fn classify(
    cfg: &crate::config::ServerConfig,
    listening: u16,
    socket: Option<IpAddr>,
    headers: &HeaderMap,
    authority: Option<&str>,
) -> Source {
    if let Some(peer) = socket.map(crate::server::addr::canonical) {
        if !peer.is_loopback() {
            return Source::Lan(peer);
        }
    }
    let mut hosts = headers.get_all(header::HOST).iter();
    let host_ok = match (hosts.next(), hosts.next()) {
        (Some(h), None) => h
            .to_str()
            .is_ok_and(|h| names_this_computer(cfg, listening, h)),
        (None, _) => authority.is_some() || socket.is_none(),
        _ => false,
    };
    let authority_ok = authority.is_none_or(|a| names_this_computer(cfg, listening, a));
    if forwarded(headers) || !host_ok || !authority_ok {
        Source::Tunnel
    } else {
        Source::Local
    }
}

/// The configured tunnel host (`host_server`), lower case.
fn tunnel_host(cfg: &crate::config::ServerConfig) -> Option<String> {
    cfg.host_server
        .as_deref()
        .and_then(|h| url::Url::parse(h).ok())
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

fn host_name(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    split_authority(host).map(|(name, _)| name)
}

/// Per-process random key for [`limiter_key`], so the limiter never holds an
/// unsalted digest of a credential.
fn limiter_secret() -> &'static [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        use rand::RngCore;
        let mut k = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut k);
        k
    })
}

/// The key a failure throttle or rate limit counts a caller under. A real
/// address is itself. The shared identity of tunnel and proxy callers
/// ([`PROXIED_CALLER`]) is never throttled as one caller for a credential
/// that is checked against a stored secret and stays the same from one
/// attempt to the next (a webhook address, a strategy token, an API key):
/// one stranger would lock out every alert coming through the tunnel. Those
/// attempts are counted per credential instead (`scope` names the kind), so
/// a stranger's bad attempts never block a correct credential (security
/// review S-03). Never used for sign-in, where the name and password vary
/// with every guess. The key is a prefix of an HMAC under a per-process
/// random key, in the discard-only `100:0:0:1::/64` block: a limiter key,
/// never an address anything is sent to or banned.
pub fn limiter_key(ip: IpAddr, scope: &str, credential: &str) -> IpAddr {
    if ip != PROXIED_CALLER {
        return ip;
    }
    use hmac::{Hmac, Mac};
    let Ok(mut mac) = <Hmac<sha2::Sha256> as Mac>::new_from_slice(limiter_secret()) else {
        return PROXIED_CALLER;
    };
    mac.update(scope.as_bytes());
    mac.update(&[0u8]);
    mac.update(credential.as_bytes());
    let d = mac.finalize().into_bytes();
    let w = |i: usize| u16::from_be_bytes([d[i], d[i + 1]]);
    IpAddr::V6(std::net::Ipv6Addr::new(
        0x100,
        0,
        0,
        1,
        w(0),
        w(2),
        w(4),
        w(6),
    ))
}

/// Failed-credential budget shared by every surface that checks an API key
/// or token (`/api/v1`, `/mcp`, the market data feed): ten failures a
/// minute, then the caller is refused without the credential being
/// checked (security review S-02, S-03). `caller` is [`Source::ip`] (the
/// feed passes [`feed_caller`]). This computer is never locked out; a
/// device on the network is counted by its address, whatever it presents;
/// tunnel callers by the credential presented ([`limiter_key`]), so a
/// stranger's failures never block a correct credential. `scope` names the
/// kind of credential.
pub fn credential_locked(ctx: &AppState, caller: IpAddr, scope: &str, presented: &str) -> bool {
    !is_loopback(caller)
        && ctx.limiter.is_exhausted(
            Bucket::ApiKeyFail,
            limiter_key(caller, scope, presented),
            ctx.limiter.now(),
        )
}

/// Count one failed credential check (see [`credential_locked`]).
pub fn credential_failed(ctx: &AppState, caller: IpAddr, scope: &str, presented: &str) {
    if !is_loopback(caller) {
        let _ = ctx.limiter.check(
            Bucket::ApiKeyFail,
            limiter_key(caller, scope, presented),
            ctx.limiter.now(),
        );
    }
}

/// The feed's caller for [`credential_locked`]: a device on the network by
/// its address; a loopback peer, which may be this computer or a tunnel
/// (the feed reads no headers), as a tunnel caller, counted per key.
pub fn feed_caller(peer: IpAddr) -> IpAddr {
    let peer = crate::server::addr::canonical(peer);
    if peer.is_loopback() {
        PROXIED_CALLER
    } else {
        peer
    }
}

/// Outermost application layer: classify the caller once ([`classify`])
/// and store it for every per-caller control further in.
pub async fn peer_layer(
    State(ctx): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let socket = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let cfg = ctx.server_config();
    let source = classify(
        &cfg,
        ctx.listening_port(),
        socket,
        req.headers(),
        req.uri().authority().map(|a| a.as_str()),
    );
    let https = served_over_https(&cfg, req.headers());
    req.extensions_mut().insert(source);
    let mut resp = next.run(req).await;
    if https {
        mark_cookies_secure(&mut resp);
    }
    resp
}

/// The page was served to the browser over HTTPS by the configured tunnel
/// (the request names the tunnel host and the tunnel address is https).
fn served_over_https(cfg: &crate::config::ServerConfig, headers: &HeaderMap) -> bool {
    let https_tunnel = cfg
        .host_server
        .as_deref()
        .and_then(|h| url::Url::parse(h).ok())
        .is_some_and(|u| u.scheme() == "https");
    https_tunnel
        && tunnel_host(cfg).is_some_and(|t| host_name(headers).as_deref() == Some(t.as_str()))
}

/// Add `Secure` to the session cookie of a response served through an
/// HTTPS tunnel, so the browser never sends it over plain HTTP (security
/// review S-16). Plain loopback keeps the cookie as it is.
fn mark_cookies_secure(resp: &mut Response) {
    let cookies: Vec<HeaderValue> = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    if cookies.is_empty() {
        return;
    }
    resp.headers_mut().remove(header::SET_COOKIE);
    for c in cookies {
        let v = match c.to_str() {
            Ok(s) if !s.to_ascii_lowercase().contains("; secure") => {
                HeaderValue::from_str(&format!("{}; Secure", s)).unwrap_or(c)
            }
            _ => c,
        };
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
}

/// The stored [`Source`]. A request that did not pass [`peer_layer`] (the
/// MCP dispatcher's internal calls) falls back on its socket peer alone,
/// never on its headers: a loopback peer whose headers were not classified
/// is not trusted as local.
pub fn source_of(ext: &axum::http::Extensions) -> Source {
    if let Some(s) = ext.get::<Source>() {
        return *s;
    }
    match ext
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| crate::server::addr::canonical(c.0.ip()))
    {
        Some(ip) if !ip.is_loopback() => Source::Lan(ip),
        Some(_) => Source::Tunnel,
        None => Source::Local,
    }
}

pub fn client_ip(req: &Request) -> IpAddr {
    source_of(req.extensions()).ip()
}

/// The stored caller class ([`Source`]).
pub struct Src(pub Source);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Src {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Src(source_of(&parts.extensions)))
    }
}

/// The address per-address controls count the caller under
/// ([`Source::ip`]).
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(source_of(&parts.extensions).ip()))
    }
}

/// Session for this request (always present behind `session_layer`).
pub struct Sess(pub Option<WebSession>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Sess {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Sess(
            parts
                .extensions
                .get::<SessionCtx>()
                .and_then(|s| s.0.clone()),
        ))
    }
}

/// The signed-in user (only on routes behind `require_user`).
pub struct User(pub AuthedUser);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for User {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthedUser>()
            .cloned()
            .map(User)
            .ok_or_else(not_authenticated)
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| {
            let mut it = kv.trim().splitn(2, '=');
            match (it.next(), it.next()) {
                (Some(k), Some(v)) if k == name => Some(v.to_string()),
                _ => None,
            }
        })
        .next()
}

pub fn session_cookie(id: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/",
        COOKIE_NAME, id
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("session=; Path=/"))
}

pub fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static("session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

// ------------------------------------------------------------------ Host check

/// Hosts the server answers to. Blocks DNS rebinding: a page on
/// `evil.example` resolving to 127.0.0.1 sends `Host: evil.example`.
pub fn host_allowed(ctx: &AppState, host: &str) -> bool {
    let cfg = ctx.server_config();
    let host = host.to_ascii_lowercase();
    let (name, port) = if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        match rest.split_once(']') {
            Some((n, p)) => (
                n.to_string(),
                p.strip_prefix(':').and_then(|p| p.parse::<u16>().ok()),
            ),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((n, p)) => (n.to_string(), Some(p.parse::<u16>().unwrap_or(0))),
            None => (host.clone(), None),
        }
    };
    if let Some(public) = cfg
        .host_server
        .as_deref()
        .and_then(|h| url::Url::parse(h).ok())
    {
        if public
            .host_str()
            .map(|h| h.eq_ignore_ascii_case(&name))
            .unwrap_or(false)
        {
            return true;
        }
    }
    let port_ok = match port {
        None => cfg.http_port == 80,
        Some(p) => {
            p == cfg.http_port || p == ctx.listening_port() || (cfg!(debug_assertions) && p == 5173)
        }
    };
    if !port_ok {
        return false;
    }
    if matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return true;
    }
    // LAN access by IP address is allowed when the trader bound beyond
    // loopback; hostnames other than localhost never are.
    !cfg.is_loopback() && name.parse::<IpAddr>().is_ok()
}

pub async fn host_check(State(ctx): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    // HTTP/1.1 names the host in `Host`; HTTP/2 in `:authority`, which
    // arrives as the URI's authority.
    let named = match req.headers().get(header::HOST) {
        Some(h) => Some(h.to_str().ok()),
        None => req.uri().authority().map(|a| Some(a.as_str())),
    };
    match named {
        Some(Some(host)) if host_allowed(&ctx, host) => {}
        // A request over a real connection must name the host (security
        // review S-13); in-process callers (the tests) have no connection
        // and no `Host`.
        None if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none() => {}
        _ => {
            tracing::warn!("Rejected request with a missing or unexpected Host header");
            return error(StatusCode::BAD_REQUEST, "Request blocked.");
        }
    }
    next.run(req).await
}

// --------------------------------------------------------- session and CSRF

fn is_mutating(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Routes that never use the browser session's CSRF token.
fn csrf_exempt(path: &str) -> bool {
    path.starts_with("/api/v1/")
        || path == "/api/v1"
        || path.starts_with("/socket.io")
        // `/setup` is not exempt: it needs the session's token and a
        // same-origin request like every other write (security review S-05).
        || path == "/auth/login"
        || path.starts_with("/strategy/webhook/")
        // The OpenScript runner page: each call carries its run's secret.
        || path.starts_with("/openscript/runner/host/")
        || path.starts_with("/chartink/webhook/")
        // MCP: bearer token, never the browser session.
        || path == "/mcp"
        // Broker form-POST redirects (state-verified in the handler).
        || path
            .strip_suffix("/callback")
            .and_then(|p| p.strip_prefix('/'))
            .is_some_and(crate::brokers::catalog::posts_callback)
}

/// The CSRF and same-origin checks the session layer applies to a write,
/// for a handler on a path the layer exempts that still serves a
/// cookie-authenticated form for some callers (`/<broker>/callback`):
/// same-origin, and the session's token in the `X-CSRFToken` header or the
/// `csrf_token` form field.
pub fn write_allowed(
    ctx: &AppState,
    headers: &HeaderMap,
    expected: &str,
    form_token: Option<&str>,
) -> bool {
    if foreign_origin(ctx, headers) {
        return false;
    }
    let header = headers
        .get("x-csrftoken")
        .or_else(|| headers.get("x-csrf-token"))
        .and_then(|v| v.to_str().ok());
    header
        .or(form_token)
        .is_some_and(|t| tokens_match(t, expected))
}

/// A page navigation the app itself made, or one the trader typed: the
/// only GETs allowed to trigger a side effect such as a broker OTP send.
/// `Sec-Fetch-Site` decides when present (`same-origin` or `none`);
/// without it (older webviews) the `Origin`, else the `Referer`, must name
/// this app. Anything else (cross-site, or no evidence at all) is not.
pub fn same_origin_navigation(ctx: &AppState, headers: &HeaderMap) -> bool {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = get("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }
    if let Some(origin) = get("origin") {
        if origin == "null" {
            return false;
        }
        let host = origin
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        return host_allowed(ctx, host);
    }
    let Some(url) = get("referer").and_then(|r| url::Url::parse(r).ok()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = match url.port() {
        Some(p) => format!("{}:{}", host, p),
        None => host.to_string(),
    };
    host_allowed(ctx, &host)
}

/// Same-origin check for cookie-authenticated writes (web `logout` uses
/// `Sec-Fetch-Site`; this applies it to every write). Also refuses
/// cross-site browser requests before they reach a failure counter, so a
/// web page cannot lock the trader out (security review S-02). Programs
/// that are not browsers send neither header and are unaffected.
pub fn foreign_origin(ctx: &AppState, headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" && site != "none" {
            return true;
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin == "null" {
            return true;
        }
        let host = origin
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        return !host_allowed(ctx, host);
    }
    false
}

/// A browser request from another site that is not a top-level page load:
/// an image, script, frame or `fetch`. A broker's sign-in redirect is a
/// top-level navigation and passes; a web page firing requests at a
/// callback to use up the sign-in limit does not (security review S-02).
pub fn cross_site_subresource(headers: &HeaderMap) -> bool {
    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let cross = matches!(
        get("sec-fetch-site"),
        Some("cross-site") | Some("same-site")
    );
    // A client that does not say the mode cannot be told apart (older
    // browsers send none of these headers): let it through, as before.
    let navigation = get("sec-fetch-mode").is_none_or(|m| m == "navigate")
        && get("sec-fetch-dest").unwrap_or("document") == "document";
    cross && !navigation
}

pub async fn session_layer(
    State(ctx): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let now = ctx.now();
    let session =
        cookie_value(req.headers(), COOKIE_NAME).and_then(|id| ctx.sessions.get(&id, now));
    let path = req.uri().path().to_string();

    if is_mutating(req.method()) && !csrf_exempt(&path) {
        if foreign_origin(&ctx, req.headers()) {
            return error(StatusCode::FORBIDDEN, "Request blocked.");
        }
        let Some(s) = session.as_ref() else {
            return csrf_rejected();
        };
        let header_token = req
            .headers()
            .get("x-csrftoken")
            .or_else(|| req.headers().get("x-csrf-token"))
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        match header_token {
            Some(t) => {
                if !tokens_match(&t, &s.csrf_token) {
                    return csrf_rejected();
                }
            }
            None => {
                // Look for the `csrf_token` form field, then hand the body on.
                let (parts, body) = req.into_parts();
                let bytes = match axum::body::to_bytes(body, crate::config::BODY_LIMIT_BYTES).await
                {
                    Ok(b) => b,
                    Err(_) => {
                        return error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large.")
                    }
                };
                let probe = Request::from_parts(parts.clone(), Body::from(bytes.clone()));
                let ok = match FormData::from_request(probe, &()).await {
                    Ok(f) => tokens_match(f.get("csrf_token").unwrap_or(""), &s.csrf_token),
                    Err(_) => false,
                };
                if !ok {
                    return csrf_rejected();
                }
                req = Request::from_parts(parts, Body::from(bytes));
            }
        }
    }

    req.extensions_mut().insert(SessionCtx(session));
    next.run(req).await
}

/// Guard for every non-public session route.
pub async fn require_user(req: Request, next: Next) -> Response {
    let user = req
        .extensions()
        .get::<SessionCtx>()
        .and_then(|s| s.0.clone())
        .and_then(|s| s.user.clone().map(|u| (u, s.id.clone())));
    match user {
        Some((username, session_id)) => {
            let mut req = req;
            req.extensions_mut().insert(AuthedUser {
                username,
                session_id,
            });
            next.run(req).await
        }
        None => not_authenticated(),
    }
}

/// Pages that are also JSON endpoints (`/apikey`): a browser navigation gets
/// the SPA, a JSON request needs the signed-in user.
pub async fn require_user_for_json(req: Request, next: Next) -> Response {
    let wants_json = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false);
    if req.method() == Method::GET && !wants_json {
        return crate::server::spa::index_response(StatusCode::OK);
    }
    require_user(req, next).await
}

// ----------------------------------------------------------- rate limiting

/// `/api/v1` per-IP moving window. 429 body is Flask-Limiter's
/// `{"message": "100 per 1 second"}`; no rate-limit headers.
pub async fn api_rate_limit(
    State(ctx): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    // A web page in the trader's browser (an `<img>` pointed at
    // /api/v1/ticker, a form post) gets the invalid-key answer without the
    // key being checked or anything being counted, so it can neither lock
    // local programs out nor use up their rate limit (security review S-02).
    // Its response was unreadable to it anyway.
    if foreign_origin(&ctx, req.headers()) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({"status": "error", "message": crate::services::core::INVALID_API_KEY}),
        );
    }
    let bucket = Bucket::for_api_path(req.uri().path());
    if ctx
        .limiter
        .check(bucket, client_ip(&req), ctx.limiter.now())
        .is_err()
    {
        return json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"message": bucket.describe()}),
        );
    }
    next.run(req).await
}

/// Login-type limits (5 per minute and 25 per hour per address). Counted
/// per address only, never per name, state or other value that changes
/// with every attempt; the tunnel's shared identity is one address here.
pub fn login_limited(ctx: &AppState, ip: IpAddr) -> Option<Response> {
    let now = ctx.limiter.now();
    let over = ctx.limiter.check(Bucket::LoginMinute, ip, now).is_err()
        || ctx.limiter.check(Bucket::LoginHour, ip, now).is_err();
    over.then(|| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many login attempts. Please wait a minute and try again.",
        )
    })
}

/// Whether `url` (an `Origin` or `Referer` value) is a page of this app.
fn app_page(ctx: &AppState, url: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    let authority = match u.port() {
        Some(p) => format!("{}:{}", host, p),
        None => host.to_string(),
    };
    host_allowed(ctx, &authority)
}

/// The same-origin check every sign-in attempt (a password, an
/// authenticator code, the current password) must pass before anything is
/// counted or checked (security review S-02):
///
/// * `Sec-Fetch-Site`, when sent, says `same-origin` or `none`;
/// * `Origin`, when sent, is this app's page; else `Referer`, when sent;
/// * a request with none of the three (a program, not a browser) is
///   accepted only from this computer, where it still spends the local
///   budget.
pub fn sign_in_origin_ok(ctx: &AppState, headers: &HeaderMap, source: Source) -> bool {
    let site = headers
        .get("sec-fetch-site")
        .map(|v| v.to_str().unwrap_or(""));
    if site.is_some_and(|s| s != "same-origin" && s != "none") {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        return origin.to_str().is_ok_and(|o| app_page(ctx, o));
    }
    if let Some(referer) = headers.get(header::REFERER) {
        return referer.to_str().is_ok_and(|r| app_page(ctx, r));
    }
    site.is_some() || source.is_local()
}

/// [`sign_in_origin_ok`] as the refusal, before anything is counted.
pub fn sign_in_refused(ctx: &AppState, headers: &HeaderMap, source: Source) -> Option<Response> {
    (!sign_in_origin_ok(ctx, headers, source))
        .then(|| error(StatusCode::FORBIDDEN, "Request blocked."))
}

/// The sign-in budget a caller spends (see
/// [`crate::server::ratelimit::LoginBackoff`]): this computer, every
/// tunnel caller together (they cannot be told apart reliably), or the
/// network address (IPv6 by its /64, which one device can rotate through).
pub fn sign_in_source(source: Source) -> SignInSource {
    match source {
        Source::Local => SignInSource::Local,
        Source::Tunnel => SignInSource::Tunnel,
        Source::Lan(ip) => match crate::server::addr::canonical(ip) {
            IpAddr::V6(v6) => {
                let s = v6.segments();
                SignInSource::Network(IpAddr::V6(std::net::Ipv6Addr::new(
                    s[0], s[1], s[2], s[3], 0, 0, 0, 0,
                )))
            }
            v4 => SignInSource::Network(v4),
        },
    }
}

/// A sign-in attempt claimed against its budget. Give it an outcome with
/// [`Self::succeeded`], [`Self::released`] or [`Self::failed`]; dropped
/// without one, it stays counted as a failure.
#[must_use]
pub struct SignInAttempt<'a> {
    ctx: &'a AppState,
    claim: Claim,
}

impl SignInAttempt<'_> {
    /// Signed in completely: the budget starts over.
    pub fn succeeded(self) {
        self.ctx.limiter.backoff.succeed(&self.claim);
    }

    /// Nothing was checked, or the password was right and a code is still
    /// due: the attempt is given back.
    pub fn released(self) {
        self.ctx.limiter.backoff.release(&self.claim);
    }

    /// The password or code was wrong (it is already counted).
    pub fn failed(self) {}
}

/// Claim a sign-in attempt (password or authenticator code) from `source`
/// before anything is checked: the refusal while that source's wait runs,
/// otherwise the attempt, already counted as a failure. One budget per
/// source whatever name is typed, so the delays never tell which name is
/// the account's.
/// The wait check and the count are one step under one lock, so parallel
/// requests cannot all slip through. Call [`sign_in_refused`] first.
#[allow(clippy::result_large_err)]
pub fn claim_sign_in(ctx: &AppState, source: Source) -> Result<SignInAttempt<'_>, Response> {
    match ctx
        .limiter
        .backoff
        .claim(sign_in_source(source), ctx.limiter.now())
    {
        Ok(claim) => Ok(SignInAttempt { ctx, claim }),
        Err(wait) => Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Too many failed sign-in attempts. Wait {} seconds and try again.",
                wait.as_millis().div_ceil(1000).max(1)
            ),
        )),
    }
}

/// Adds the session cookie to a response.
pub fn with_cookie(mut resp: Response, id: &str) -> Response {
    resp.headers_mut()
        .append(header::SET_COOKIE, session_cookie(id));
    resp
}

pub fn redirect(to: &str) -> Response {
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            HeaderValue::from_str(to).unwrap_or(HeaderValue::from_static("/")),
        )],
    )
        .into_response()
}
