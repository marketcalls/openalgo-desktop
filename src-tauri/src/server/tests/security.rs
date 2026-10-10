//! Regression tests for the 2026-10-09 security review
//! (`docs/audit/2026-10-09/01-security.md`). Each test names its finding.

use super::*;

fn count(h: &H, sql: &str) -> i64 {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

// ------------------------------------------------- S-01 account reset

/// S-01: the account cannot be reset over HTTP any more, whatever the
/// caller sends (a session, its CSRF token and the confirmation).
#[tokio::test]
async fn s01_account_reset_is_not_reachable_over_http() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let (cookie, csrf) = h.session(false);
    let (s, _) = h
        .json(with_session(
            post_json("/auth/reset-account", json!({"confirm": "RESET"})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!AuthService::needs_setup(h.ctx()).unwrap(), "account kept");
    assert!(h.ctx().is_broker_connected(), "broker session kept");
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "API key kept");
}

/// S-07: a reset leaves nothing usable behind: MCP tokens, strategy and
/// Chartink webhook secrets, Telegram and WhatsApp links.
#[tokio::test]
async fn s07_account_reset_revokes_every_outside_credential() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let now = h.ctx().now();
    let (mcp_token, strategy_hash) = {
        let c = h.ctx().sqlite.conn().unwrap();
        let (_, mcp_token) = crate::mcp::store::create_token(
            &c,
            "claude",
            crate::mcp::store::TokenScope::ReadWrite,
            now,
        )
        .unwrap();
        let token = crate::strategy::store::generate_webhook_token();
        let strategy_hash = crate::strategy::store::hash_webhook_token(&token);
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [&strategy_hash],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id)
             VALUES ('scan', '11111111-1111-4111-8111-111111111111')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO telegram_users (telegram_id, openalgo_username) VALUES (42, 'trader')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username)
             VALUES ('919@s.whatsapp.net', '919', 'trader')",
            [],
        )
        .unwrap();
        (mcp_token, strategy_hash)
    };

    AuthService::reset_account_everywhere(h.ctx())
        .await
        .unwrap();

    let c = h.ctx().sqlite.conn().unwrap();
    assert!(crate::mcp::store::find_token(&c, &mcp_token)
        .unwrap()
        .is_none());
    let hash: String = c
        .query_row("SELECT webhook_token_hash FROM sm_strategy", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(hash, strategy_hash, "strategy webhook rotated");
    let id: String = c
        .query_row("SELECT webhook_id FROM chartink_strategies", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(id, "11111111-1111-4111-8111-111111111111");
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    drop(c);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM telegram_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM whatsapp_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM users"), 0);
    assert!(!h.ctx().is_broker_connected());
    assert!(AuthService::needs_setup(h.ctx()).unwrap());
    // Strategy and Chartink definitions are kept.
    assert_eq!(count(&h, "SELECT COUNT(*) FROM sm_strategy"), 1);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM chartink_strategies"), 1);
}

// ---------------------------------------------- S-02 cross-site lockouts

const LAN: &str = "192.168.1.50";

/// The port the test server's configuration names (development ports in
/// debug builds).
fn app_port() -> u16 {
    if crate::config::dev_ports_enabled() {
        crate::config::DEV_HTTP_PORT
    } else {
        crate::config::DEFAULT_HTTP_PORT
    }
}

/// The request as if it came over a socket from `ip`, naming the app.
fn from_peer(mut r: Request<Body>, ip: &str) -> Request<Body> {
    r.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
            ip.parse().unwrap(),
            50000,
        )));
    r.headers_mut().insert(
        header::HOST,
        format!("127.0.0.1:{}", app_port()).parse().unwrap(),
    );
    r
}

/// Headers a browser adds to a request a page on another site fires.
fn cross_site(mut r: Request<Body>, dest: &str) -> Request<Body> {
    let h = r.headers_mut();
    h.insert("sec-fetch-site", "cross-site".parse().unwrap());
    h.insert("sec-fetch-mode", "no-cors".parse().unwrap());
    h.insert("sec-fetch-dest", dest.parse().unwrap());
    h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    r
}

fn ticker(key: &str) -> Request<Body> {
    get(&format!(
        "/api/v1/ticker/NSE:SBIN?apikey={}&interval=D&from=2026-01-01&to=2026-01-02",
        key
    ))
}

/// S-02: a web page's form posts to /auth/login are refused before they
/// count, so the trader can still sign in.
#[tokio::test]
async fn s02_cross_site_login_posts_do_not_lock_out_sign_in() {
    let h = H::new();
    h.setup();
    for _ in 0..30 {
        let r = cross_site(
            multipart(
                "/auth/login",
                &[("username", USER), ("password", "Wrong@123")],
            ),
            "empty",
        );
        let (s, _) = h.json(r).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// S-02: image requests with a bad key in the URL do not lock local
/// programs out of /api/v1; the answer keeps the web's 403 and message.
#[tokio::test]
async fn s02_image_requests_with_a_bad_key_do_not_lock_out_local_programs() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for _ in 0..20 {
        let (s, v) = h.json(cross_site(ticker("x"), "image")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["message"], "Invalid openalgo apikey");
        let (s, _) = h
            .json(from_peer(cross_site(ticker("x"), "image"), LAN))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            LAN,
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "refused before the key was checked");
}

/// S-02: a key sent in the URL counts like one in the body: ten bad keys in
/// `GET /api/v1/ticker` spend the device's failure budget. A valid key from
/// that device still works, in the URL or in the body.
#[tokio::test]
async fn s02_a_key_in_the_url_counts_like_one_in_the_body() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let lan: std::net::IpAddr = LAN.parse().unwrap();
    for i in 0..10 {
        let (s, _, _) = h.send(from_peer(ticker(&format!("guess{}", i)), LAN)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let now = h.ctx().limiter.now();
    assert!(h.ctx().limiter.is_exhausted(Bucket::ApiKeyFail, lan, now));
    let (s, _, _) = h.send(from_peer(ticker(&key), LAN)).await;
    assert_ne!(s, StatusCode::FORBIDDEN, "a valid key in the URL works");
    let (s, _) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            LAN,
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "a valid key in the body works");
}

/// Remote MCP on and a token for it.
fn remote_mcp(h: &H) -> String {
    let c = h.ctx().sqlite.conn().unwrap();
    let mut st = crate::mcp::store::settings(&c).unwrap();
    st.http_enabled = true;
    crate::mcp::store::save_settings(&c, &st).unwrap();
    crate::mcp::store::create_token(
        &c,
        "claude",
        crate::mcp::store::TokenScope::Read,
        h.ctx().now(),
    )
    .unwrap()
    .1
}

fn mcp_ping(token: &str) -> Request<Body> {
    let mut r = post_json("/mcp", json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}));
    r.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", token).parse().unwrap(),
    );
    r
}

/// S-02: a thousand bad MCP tokens through the tunnel spend the tunnel's
/// failure budget and nothing else: a valid token through the same tunnel
/// works at once, and later bad tokens are refused straight away (429).
#[tokio::test]
async fn s02_tunnel_failures_never_refuse_a_valid_mcp_token() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    h.setup();
    let token = remote_mcp(&h);
    let t0 = h.ctx().limiter.now();
    let mut unauthorized = 0;
    for i in 0..1000u64 {
        // Five hundred a second, under the resource guard.
        h.ctx()
            .limiter
            .freeze(Some(t0 + std::time::Duration::from_millis(2 * i)));
        let (s, _) = h
            .json(tunnelled(mcp_ping(&format!("oamcp_guess{}", i))))
            .await;
        match s {
            StatusCode::UNAUTHORIZED => unauthorized += 1,
            StatusCode::TOO_MANY_REQUESTS => {}
            other => panic!("{}", other),
        }
    }
    assert_eq!(unauthorized, 10, "ten failures, then refused at once");
    let now = h.ctx().limiter.now();
    assert!(h
        .ctx()
        .limiter
        .is_exhausted(Bucket::ApiKeyFail, PROXIED_CALLER, now));
    let (s, v) = h.json(tunnelled(mcp_ping(&token))).await;
    assert_eq!(s, StatusCode::OK, "the valid token works at once: {}", v);
    let (s, v) = h.json(tunnelled(mcp_ping("oamcp_another"))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_eq!(v["error"], "rate_limited");
}

/// S-02: bad MCP tokens and bad API keys spend one failure budget per
/// caller. Once a device has spent it, its bad tokens are refused at once
/// (429) while its valid token still works; this computer has its own.
#[tokio::test]
async fn s02_bad_mcp_tokens_share_the_failure_budget_of_bad_keys() {
    let h = H::new();
    h.setup();
    let token = remote_mcp(&h);
    for i in 0..5 {
        let (s, _) = h
            .json(from_peer(mcp_ping(&format!("oamcp_guess{}", i)), LAN))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = h
            .json(from_peer(
                post_json("/api/v1/ping", json!({"apikey": format!("bad{}", i)})),
                LAN,
            ))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, v) = h.json(from_peer(mcp_ping("oamcp_more"), LAN)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    let (s, v) = h.json(from_peer(mcp_ping(&token), LAN)).await;
    assert_eq!(s, StatusCode::OK, "the device's valid token: {}", v);
    let (s, _) = h.json(mcp_ping("oamcp_more")).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "this computer has its own budget"
    );
    let (s, v) = h.json(mcp_ping(&token)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// S-02: a web page firing requests at /api/v1 does not use up the
/// 100-per-second limit this computer's programs share.
#[tokio::test]
async fn s02_cross_site_requests_do_not_use_up_the_local_rate_limit() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for _ in 0..150 {
        let (s, _) = h.json(cross_site(ticker("x"), "image")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// S-02: bad keys never refuse a valid key, from this computer or from a
/// device on the network: they spend only the caller's failure budget.
#[tokio::test]
async fn s02_bad_keys_never_refuse_a_valid_key() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    for _ in 0..12 {
        let (s, v) = h
            .json(post_json("/api/v1/ping", json!({"apikey": "wrong"})))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["message"], "Invalid openalgo apikey");
        let (s, _) = h
            .json(from_peer(
                post_json("/api/v1/ping", json!({"apikey": "wrong"})),
                LAN,
            ))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let now = h.ctx().limiter.now();
    for caller in ["127.0.0.1", LAN] {
        let ip: std::net::IpAddr = caller.parse().unwrap();
        assert!(h.ctx().limiter.is_exhausted(Bucket::ApiKeyFail, ip, now));
    }
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "this computer");
    let (s, _) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            LAN,
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "the device");
}

/// S-02: a valid key before the broker login is refused as on the web but
/// does not count as a bad key.
#[tokio::test]
async fn s02_valid_key_without_a_broker_session_is_not_a_failure() {
    let h = H::new();
    let key = h.setup();
    for _ in 0..12 {
        let (s, _) = h
            .json(from_peer(
                post_json("/api/v1/ping", json!({"apikey": key})),
                LAN,
            ))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    h.connect_broker();
    let (s, _) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            LAN,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
}

/// S-02: images and frames aimed at a broker callback from another site do
/// not use up the sign-in limit.
#[tokio::test]
async fn s02_cross_site_callback_images_do_not_use_up_the_sign_in_limit() {
    let h = H::new();
    h.setup();
    for _ in 0..10 {
        let (s, _, _) = h
            .send(cross_site(
                get("/zerodha/callback?request_token=x&state=y"),
                "image",
            ))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    assert!(!h.ctx().limiter.is_exhausted(
        crate::server::ratelimit::Bucket::LoginMinute,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        h.ctx().limiter.now()
    ));
    let (s, _) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
}

// ------------------------------------------------ S-03 tunnel callers

use crate::server::middleware::{classify, Source, PROXIED_CALLER};

fn hdrs(pairs: &[(&str, &str)]) -> axum::http::HeaderMap {
    let mut m = axum::http::HeaderMap::new();
    for (k, v) in pairs {
        m.append(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    m
}

fn ip(s: &str) -> std::net::IpAddr {
    s.parse().unwrap()
}

/// The app's own loopback `Host` for `cfg`.
fn own_host(cfg: &crate::config::ServerConfig) -> String {
    format!("127.0.0.1:{}", cfg.http_port)
}

/// `classify` for a connection from `peer` with `pairs` as headers.
fn class(cfg: &crate::config::ServerConfig, peer: &str, pairs: &[(&str, &str)]) -> Source {
    classify(cfg, cfg.http_port, Some(ip(peer)), &hdrs(pairs), None)
}

/// S-03: who a request is from, by socket peer and forwarding headers. No
/// forwarding header is ever read as an address.
#[test]
fn s03_no_forwarding_header_is_read_as_an_address() {
    let mut cfg = crate::config::ServerConfig::default();
    let host = own_host(&cfg);
    // The trader's own programs.
    assert_eq!(class(&cfg, "127.0.0.1", &[("host", &host)]), Source::Local);
    assert_eq!(class(&cfg, "::1", &[("host", &host)]), Source::Local);
    // Through a tunnel, a forwarded address is not believed: the caller is
    // the shared tunnel identity, never local, with no address an
    // allowlist or a ban could match.
    for (name, value) in [
        ("x-forwarded-for", "203.0.113.7"),
        ("cf-connecting-ip", "203.0.113.7"),
        ("x-real-ip", "203.0.113.7"),
        ("forwarded", "for=203.0.113.7"),
    ] {
        let s = class(&cfg, "127.0.0.1", &[("host", &host), (name, value)]);
        assert_eq!(s, Source::Tunnel, "{}", name);
        assert_eq!(s.ip(), PROXIED_CALLER);
        assert_eq!(s.network_address(), None);
    }
    // A request naming the tunnel host is a tunnel request even bare.
    cfg.host_server = Some("https://abc.ngrok.app".into());
    assert_eq!(
        class(&cfg, "127.0.0.1", &[("host", "abc.ngrok.app")]),
        Source::Tunnel
    );
    // A device on the network is itself; its headers are never read.
    for (name, value) in [
        ("x-forwarded-for", "203.0.113.7"),
        ("x-real-ip", "192.168.1.51"),
        ("cf-connecting-ip", "::1"),
    ] {
        assert_eq!(
            class(&cfg, LAN, &[(name, value)]),
            Source::Lan(ip(LAN)),
            "{}",
            name
        );
    }
    let lan = class(&cfg, LAN, &[("x-forwarded-for", "127.0.0.1")]);
    assert_eq!(lan, Source::Lan(ip(LAN)));
    assert_eq!(lan.network_address(), Some(ip(LAN)));
    // An IPv4 caller seen by a dual-stack listener is the IPv4 address.
    assert_eq!(
        class(&cfg, "::ffff:192.168.1.50", &[]),
        Source::Lan(ip(LAN))
    );
}

/// S-03: every way of saying "forwarded", and any host but the app's own,
/// makes a loopback request a tunnel request.
#[test]
fn s03_header_variants_and_foreign_hosts_are_tunnel_requests() {
    let cfg = crate::config::ServerConfig::default();
    let host = own_host(&cfg);
    for pairs in [
        vec![("host", host.as_str()), ("x-FORWARDED-for", "1.2.3.4")],
        vec![
            ("host", host.as_str()),
            ("X-Forwarded-Host", "evil.example"),
        ],
        vec![("host", host.as_str()), ("x-forwarded-for", "")],
        vec![
            ("host", host.as_str()),
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-for", "5.6.7.8"),
        ],
        vec![("host", host.as_str()), ("Forwarded", "for=1.2.3.4")],
        vec![("host", host.as_str()), ("X-Real-IP", "1.2.3.4")],
        vec![("host", host.as_str()), ("CF-Connecting-IP", "1.2.3.4")],
        vec![("host", host.as_str()), ("True-Client-IP", "1.2.3.4")],
        vec![("host", host.as_str()), ("X-Client-IP", "1.2.3.4")],
        vec![("host", host.as_str()), ("Fastly-Client-IP", "1.2.3.4")],
        vec![
            ("host", host.as_str()),
            ("X-Original-Forwarded-For", "1.2.3.4"),
        ],
        vec![("host", host.as_str()), ("Via", "1.1 proxy")],
        vec![("host", "evil.example")],
        vec![("host", "127.0.0.1:1")],
        vec![("host", "localhost.evil.example")],
        vec![("host", host.as_str()), ("host", host.as_str())],
        vec![],
    ] {
        assert_eq!(
            class(&cfg, "127.0.0.1", &pairs),
            Source::Tunnel,
            "{:?}",
            pairs
        );
    }
    for own in [
        host.clone(),
        format!("LOCALHOST:{}", cfg.http_port),
        format!("[::1]:{}", cfg.http_port),
    ] {
        assert_eq!(
            class(&cfg, "127.0.0.1", &[("host", &own)]),
            Source::Local,
            "{}",
            own
        );
    }
    // HTTP/2 names the host in `:authority`.
    assert_eq!(
        classify(
            &cfg,
            cfg.http_port,
            Some(ip("127.0.0.1")),
            &hdrs(&[]),
            Some(&host)
        ),
        Source::Local
    );
    assert_eq!(
        classify(
            &cfg,
            cfg.http_port,
            Some(ip("127.0.0.1")),
            &hdrs(&[]),
            Some("evil.example")
        ),
        Source::Tunnel
    );
}

/// S-03: the caller is classified once, by the outermost layer; every
/// control reads the stored value. A header added to the request after
/// classification changes nothing.
#[tokio::test]
async fn s03_every_control_reads_the_stored_source() {
    use crate::server::middleware::{peer_layer, ClientIp, Src};
    let h = H::new();
    let ctx = h.ctx().clone();
    let inject = |mut req: Request<Body>, next: axum::middleware::Next| async move {
        req.headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        req.headers_mut()
            .insert(header::HOST, "evil.example".parse().unwrap());
        next.run(req).await
    };
    let app = axum::Router::new()
        .route(
            "/probe",
            axum::routing::get(|Src(s): Src, ClientIp(ip): ClientIp| async move {
                format!("{:?} {}", s, ip)
            }),
        )
        .layer(axum::middleware::from_fn(inject))
        .layer(axum::middleware::from_fn_with_state(
            ctx.clone(),
            peer_layer,
        ))
        .with_state(ctx.clone());
    let resp = tower::ServiceExt::oneshot(app, from_peer(get("/probe"), "127.0.0.1"))
        .await
        .unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), "Local 127.0.0.1");
}

/// S-03: Remote MCP off refuses a tunnel caller although its socket peer
/// is loopback.
#[tokio::test]
async fn s03_remote_mcp_switch_applies_to_tunnel_callers() {
    let h = H::new();
    h.setup();
    let mut r = get("/mcp");
    r.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    let (s, v) = h.json(r).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{}", v);
}

fn tunnelled(mut r: Request<Body>) -> Request<Body> {
    r.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    r
}

/// S-03: a thousand bad keys through the tunnel spend the tunnel's failure
/// budget and nothing else: a valid key through the same tunnel works at
/// once, local programs work, and no bad key costs an Argon2 check (the
/// HMAC index misses first).
#[tokio::test]
async fn s03_bad_keys_through_a_tunnel_do_not_block_a_valid_key() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let t0 = h.ctx().limiter.now();
    let hashes = h.ctx().api_keys.hash_checks();
    for i in 0..1000u64 {
        // Five hundred a second, under the resource guard.
        h.ctx()
            .limiter
            .freeze(Some(t0 + std::time::Duration::from_millis(2 * i)));
        let (s, v) = h
            .json(tunnelled(post_json(
                "/api/v1/ping",
                json!({"apikey": format!("wrong{}", i)}),
            )))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["message"], "Invalid openalgo apikey");
    }
    let now = h.ctx().limiter.now();
    assert!(h
        .ctx()
        .limiter
        .is_exhausted(Bucket::ApiKeyFail, PROXIED_CALLER, now));
    assert_eq!(h.ctx().api_keys.hash_checks(), hashes, "no Argon2 check");
    let (s, v) = h
        .json(tunnelled(post_json("/api/v1/ping", json!({"apikey": key}))))
        .await;
    assert_eq!(s, StatusCode::OK, "a valid key through the tunnel: {}", v);
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "local programs work");
}

/// S-03: the identity every tunnel caller shares is never banned, however
/// many bad keys and unknown pages come through the tunnel.
#[tokio::test]
async fn s03_the_shared_tunnel_identity_is_never_banned() {
    let h = H::new();
    h.setup();
    for i in 0..60 {
        let _ = h
            .send(tunnelled(post_json(
                "/api/v1/ping",
                json!({"apikey": format!("wrong{}", i)}),
            )))
            .await;
        let _ = h
            .send(tunnelled(get(&format!("/no-such-page-{}.php", i))))
            .await;
    }
    h.ctx().monitor.drain_now(h.ctx());
    let proxied = PROXIED_CALLER.to_string();
    assert!(!h.ctx().monitor.is_banned(&proxied, h.ctx().now()));
    let n: i64 = h
        .ctx()
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ip_bans WHERE ip_address = ?1",
            [&proxied],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    // Nor by hand.
    let c = h.ctx().logs.conn().unwrap();
    assert!(!crate::db::sqlite::monitor::ban_ip(
        &c,
        &proxied,
        "manual",
        Some(24),
        false,
        "test",
        h.ctx().now(),
        5
    )
    .unwrap());
}

/// S-03: fifty bad webhook calls through the tunnel do not stop a good
/// one through the same tunnel.
#[tokio::test]
async fn s03_bad_webhook_calls_through_a_tunnel_do_not_block_a_good_one() {
    let h = H::new();
    h.setup();
    let token = crate::strategy::store::generate_webhook_token();
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [crate::strategy::store::hash_webhook_token(&token)],
        )
        .unwrap();
    for i in 0..50 {
        let bad = format!("oaws_{:0>43}", i);
        let (s, _) = h
            .json(tunnelled(post_json(
                &format!("/strategy/webhook/{}", bad),
                json!({}),
            )))
            .await;
        assert_ne!(s, StatusCode::OK);
    }
    let (s, v) = h
        .json(tunnelled(post_json(
            &format!("/strategy/webhook/{}", token),
            json!({}),
        )))
        .await;
    assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_ne!(v["result"], "rejected_token", "{}", v);
    assert_ne!(v["result"], "rate_limited", "{}", v);
}

/// S-03: a thousand invented webhook addresses through the tunnel spend the
/// tunnel's failure budget and nothing else: the real strategy and Chartink
/// webhooks then work through the same tunnel at once, and later invented
/// addresses are refused straight away (429) with no audit row.
#[tokio::test]
async fn s03_tunnel_failures_never_refuse_a_valid_webhook() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    h.setup();
    let token = crate::strategy::store::generate_webhook_token();
    let good = "22222222-2222-4222-8222-222222222222";
    {
        let c = h.ctx().sqlite.conn().unwrap();
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [crate::strategy::store::hash_webhook_token(&token)],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id) VALUES ('scan', ?1)",
            [good],
        )
        .unwrap();
    }
    let t0 = h.ctx().limiter.now();
    let mut refused_at_once = 0;
    for i in 0..1000u64 {
        // Five hundred a second, under the resource guard.
        h.ctx()
            .limiter
            .freeze(Some(t0 + std::time::Duration::from_millis(2 * i)));
        let path = if i % 2 == 0 {
            format!("/strategy/webhook/oaws_{:0>43}", i)
        } else {
            format!("/chartink/webhook/{}", uuid::Uuid::new_v4())
        };
        let (s, v) = h.json(tunnelled(post_json(&path, json!({})))).await;
        match s {
            StatusCode::NOT_FOUND => {}
            StatusCode::TOO_MANY_REQUESTS => refused_at_once += 1,
            other => panic!("{} {}", other, v),
        }
    }
    assert_eq!(refused_at_once, 990, "ten failures, then refused at once");
    let now = h.ctx().limiter.now();
    assert!(h
        .ctx()
        .limiter
        .is_exhausted(Bucket::WebhookFail, PROXIED_CALLER, now));
    let audit = count(&h, "SELECT COUNT(*) FROM sm_webhook_event");
    let (s, _) = h
        .json(tunnelled(post_json(
            &format!("/strategy/webhook/oaws_{:0>43}", 5000),
            json!({}),
        )))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        count(&h, "SELECT COUNT(*) FROM sm_webhook_event"),
        audit,
        "no audit row"
    );
    let (s, v) = h
        .json(tunnelled(post_json(
            &format!("/strategy/webhook/{}", token),
            json!({}),
        )))
        .await;
    assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_ne!(v["result"], "rejected_token", "{}", v);
    let (s, v) = h
        .json(tunnelled(post_json(
            &format!("/chartink/webhook/{}", good),
            json!({}),
        )))
        .await;
    assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_ne!(
        v["error"],
        crate::chartink::webhook::INVALID_WEBHOOK,
        "{}",
        v
    );
}

/// S-03: fifty Chartink alerts with wrong addresses through the tunnel do
/// not stop the real webhook's alert through the same tunnel.
#[tokio::test]
async fn s03_bad_chartink_calls_through_a_tunnel_do_not_block_a_good_one() {
    let h = H::new();
    h.setup();
    let good = "22222222-2222-4222-8222-222222222222";
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO chartink_strategies (name, webhook_id) VALUES ('scan', ?1)",
            [good],
        )
        .unwrap();
    for _ in 0..50 {
        let bad = uuid::Uuid::new_v4().to_string();
        let (s, _) = h
            .json(tunnelled(post_json(
                &format!("/chartink/webhook/{}", bad),
                json!({}),
            )))
            .await;
        assert_ne!(s, StatusCode::OK);
    }
    let (s, v) = h
        .json(tunnelled(post_json(
            &format!("/chartink/webhook/{}", good),
            json!({}),
        )))
        .await;
    assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert_ne!(
        v["error"],
        crate::chartink::webhook::INVALID_WEBHOOK,
        "{}",
        v
    );
}

/// S-06: a strategy webhook and a Chartink alert sent through the app leave
/// no secret in the traffic log or the unknown-page tracker.
#[tokio::test]
async fn s06_webhook_secrets_never_reach_the_traffic_log() {
    let h = H::new();
    h.setup();
    let token = crate::strategy::store::generate_webhook_token();
    let chartink = "33333333-3333-4333-8333-333333333333";
    let _ = h
        .send(from_peer(
            post_json(&format!("/strategy/webhook/{}", token), json!({})),
            LAN,
        ))
        .await;
    let _ = h
        .send(from_peer(
            post_json(&format!("/chartink/webhook/{}", chartink), json!({})),
            LAN,
        ))
        .await;
    h.ctx().monitor.drain_now(h.ctx());
    let c = h.ctx().logs.conn().unwrap();
    let paths: Vec<String> = c
        .prepare("SELECT path FROM traffic_logs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        paths.iter().any(|p| p == "/strategy/webhook/<redacted>"),
        "the call was logged: {:?}",
        paths
    );
    let tried: Vec<String> = c
        .prepare("SELECT paths_attempted FROM error_404_tracker")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for text in paths.iter().chain(tried.iter()) {
        assert!(!text.contains(&token), "{}", text);
        assert!(!text.contains(chartink), "{}", text);
    }
}

/// S-03: a forged X-Forwarded-For through a tunnel never satisfies a
/// strategy webhook IP allowlist, even one that lists that address and
/// every IPv6 address; a device on the network that is listed does.
#[tokio::test]
async fn s03_spoofed_forwarded_address_does_not_pass_the_webhook_allowlist() {
    let h = H::new();
    h.setup();
    let token = crate::strategy::store::generate_webhook_token();
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, webhook_ip_allowlist,
                 created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1,
                 '[\"203.0.113.7\", \"::/0\"]', 'x', 'x')",
            [crate::strategy::store::hash_webhook_token(&token)],
        )
        .unwrap();
    let call = |pairs: &[(&str, &str)]| {
        let mut r = post_json(&format!("/strategy/webhook/{}", token), json!({}));
        for (k, v) in pairs {
            r.headers_mut().insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        r
    };
    for (name, value) in [
        ("x-forwarded-for", "203.0.113.7"),
        ("cf-connecting-ip", "203.0.113.7"),
        ("x-real-ip", "203.0.113.7"),
    ] {
        let (_, v) = h.json(call(&[(name, value)])).await;
        assert_eq!(v["result"], "rejected_ip", "{}: {}", name, v);
    }
    // The listed address itself, as a device on the network, passes.
    let (_, v) = h
        .json(from_peer(
            post_json(&format!("/strategy/webhook/{}", token), json!({})),
            "203.0.113.7",
        ))
        .await;
    assert_ne!(v["result"], "rejected_ip", "{}", v);
}

// ------------------------------------------- S-09 live updates after logout

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The app served on an ephemeral loopback port, as the trader's browser
/// reaches it. Dropping the handle stops it.
struct Served {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(h: &H) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    *h.ctx().server_status.write() = crate::state::ServerStatus::Running {
        host: "127.0.0.1".into(),
        port: addr.port(),
    };
    let svc = crate::server::app(h.ctx().clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<Request<Body>>::into_make_service_with_connect_info::<
                std::net::SocketAddr,
            >(svc),
        )
        .await;
    });
    Served { addr, task }
}

/// A live update connection for the browser session `cookie`, joined to
/// the default namespace. `None` when the server refuses it.
async fn live_updates(s: &Served, cookie: &str) -> Option<Ws> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let mut req = format!(
        "ws://127.0.0.1:{}/socket.io/?EIO=4&transport=websocket",
        s.addr.port()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut().insert(
        header::HOST,
        format!("127.0.0.1:{}", s.addr.port()).parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.ok()?;
    let open = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .ok()??
        .ok()?;
    assert!(open.to_text().unwrap().starts_with('0'), "{:?}", open);
    ws.send(Message::Text("40".into())).await.ok()?;
    // The namespace join, answering Engine.IO pings ("2") that may arrive
    // first on a slow runner.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next())
            .await
            .ok()??
            .ok()?;
        let text = m.to_text().ok()?.to_string();
        if text == "2" {
            ws.send(Message::Text("3".into())).await.ok()?;
            continue;
        }
        return text.starts_with("40").then_some(ws);
    }
}

/// Whether the server closes the connection (a namespace disconnect or a
/// closed socket) within `secs`.
async fn closed_within(ws: &mut Ws, secs: u64) -> bool {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.as_str() == "41" => return true,
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(_))) => continue,
            Err(_) => return false,
        }
    }
}

/// Whether `event` arrives on the connection within `secs`.
async fn receives(ws: &mut Ws, event: &str, secs: u64) -> bool {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let want = format!("42[\"{}\"", event);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.as_str().starts_with(&want) => return true,
            Ok(Some(Ok(_))) => continue,
            _ => return false,
        }
    }
}

fn session_id(cookie: &str) -> String {
    cookie
        .split(';')
        .find_map(|kv| kv.trim().strip_prefix("session="))
        .unwrap()
        .to_string()
}

/// S-09: a signed-in page's Socket.IO connection is closed by the server
/// when the session ends, not left receiving order and position pushes.
#[tokio::test]
async fn s09_live_update_connection_is_closed_on_sign_out() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let s = serve(&h).await;
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    // Sign out everywhere, as /auth/logout does.
    h.ctx().sessions.clear();
    h.ctx().bus.publish(crate::events::Event::ForceLogout {
        message: "Signed out".into(),
    });
    assert!(
        closed_within(&mut ws, 10).await,
        "the server kept the signed-out connection open"
    );
}

/// S-09: a connection opened under a session id that was later rotated is
/// closed, as is everything when the trader signs out.
#[tokio::test]
async fn s09_connections_of_a_rotated_session_are_closed() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let s = serve(&h).await;
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    let rotated = h.ctx().sessions.rotate(&session_id(&cookie), h.ctx().now());
    h.ctx()
        .sessions
        .update(&rotated.id, |x| x.user = Some(USER.into()));
    assert!(closed_within(&mut ws, 10).await, "the old id's connection");
}

/// S-09: an account reset, a password change and the daily boundary each
/// close the live update connections of the sessions they end.
#[tokio::test]
async fn s09_reset_password_change_and_expiry_close_connections() {
    let h = H::new();
    h.setup();
    let s = serve(&h).await;

    // Password change (the service the page calls; going through another
    // in-process app here would swap the live update handle under test).
    let (cookie, _) = h.session(true);
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    AuthService::change_password(h.ctx(), USER, PASSWORD, "Changed@123", "Changed@123").unwrap();
    assert!(closed_within(&mut ws, 10).await, "password change");

    // The daily boundary.
    let (cookie, _) = h.session(true);
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    // The boundary ends sessions from before it (SES-02): a day later.
    let before = h.ctx().now();
    h.t.clock.set(before + chrono::Duration::hours(24));
    assert!(crate::session::expire_if_crossed(h.ctx(), before).await);
    assert!(closed_within(&mut ws, 10).await, "daily boundary");

    // Account reset from the desktop window.
    let (cookie, _) = h.session(true);
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    AuthService::reset_account_everywhere(h.ctx())
        .await
        .unwrap();
    assert!(closed_within(&mut ws, 10).await, "account reset");
}

/// S-09: a connection that opens while the trader signs out ends closed,
/// whichever happens first.
#[tokio::test]
async fn s09_a_connection_racing_a_sign_out_ends_closed() {
    let h = H::new();
    h.setup();
    let s = serve(&h).await;
    // The sign-out lands at a different moment each time: before the
    // connection is checked, while it is being set up, and after it is up.
    let mut connected = 0;
    for delay_ms in [0u64, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144] {
        let (cookie, _) = h.session(true);
        let ctx = h.ctx().clone();
        let sign_out = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            ctx.sessions.clear();
        });
        let ws = live_updates(&s, &cookie).await;
        sign_out.await.unwrap();
        if let Some(mut ws) = ws {
            connected += 1;
            // Pushes after the sign-out never reach it, and it is closed.
            h.ctx()
                .ui
                .emit_signed_in(None, "order_event", &json!({"probe": true}));
            assert!(
                closed_within(&mut ws, 10).await,
                "left open ({} ms)",
                delay_ms
            );
        }
    }
    assert!(connected > 0, "no connection was up before its sign-out");
}

/// S-09: ending one session closes only its connections; another signed-in
/// session keeps receiving pushes.
#[tokio::test]
async fn s09_other_sessions_stay_connected() {
    let h = H::new();
    h.setup();
    let s = serve(&h).await;
    let (a, _) = h.session(true);
    let (b, _) = h.session(true);
    let mut wa = live_updates(&s, &a).await.expect("a connected");
    let mut wb = live_updates(&s, &b).await.expect("b connected");
    h.ctx().sessions.remove(&session_id(&a));
    assert!(closed_within(&mut wa, 10).await, "the ended session");
    h.ctx()
        .ui
        .emit_signed_in(None, "order_event", &json!({"probe": true}));
    assert!(
        receives(&mut wb, "order_event", 10).await,
        "the other session lost its pushes"
    );
}

// --------------------------------------------- S-05 setup, S-13, S-16

/// S-05: a cross-site form post cannot create the account, and setup is
/// refused through a tunnel.
#[tokio::test]
async fn s05_setup_needs_the_page_token_and_this_computer() {
    let h = H::new();
    let fields = [("username", USER), ("email", EMAIL), ("password", PASSWORD)];
    // A page on another site: no session token, foreign origin.
    let (s, _) = h
        .json(cross_site(multipart("/setup", &fields), "empty"))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // No token at all.
    let (s, _) = h.json(multipart("/setup", &fields)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(AuthService::needs_setup(h.ctx()).unwrap());
    // Through a tunnel, even with the page's token.
    let (cookie, csrf) = h.session(false);
    let mut r = with_session(multipart("/setup", &fields), &cookie, Some(&csrf));
    r.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    let (s, _) = h.json(r).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(AuthService::needs_setup(h.ctx()).unwrap());
    // The app's own page on this computer.
    let (s, _) = h
        .json(with_session(
            multipart("/setup", &fields),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
}

/// S-14: no path under `/webhook/` skips the CSRF check (no route lives
/// there; a future one must not be exempt by accident).
#[tokio::test]
async fn s14_webhook_prefix_is_not_exempt_from_csrf() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(post_json("/webhook/anything", json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(
        v["message"],
        "Your session has expired. Refresh the page and try again."
    );
}

/// S-13: a request over a connection without a Host header is refused.
#[tokio::test]
async fn s13_a_connection_without_a_host_header_is_refused() {
    let h = H::new();
    let mut r = from_peer(get("/auth/check-setup"), "127.0.0.1");
    r.headers_mut().remove(header::HOST);
    let (s, _, _) = h.send(r).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = h
        .send(from_peer(get("/auth/check-setup"), "127.0.0.1"))
        .await;
    assert_eq!(s, StatusCode::OK);
    // HTTP/2 names the host in `:authority` (the URI), not in `Host`: it is
    // checked the same way.
    for (authority, expected) in [
        (format!("127.0.0.1:{}", app_port()), StatusCode::OK),
        ("evil.example".to_string(), StatusCode::BAD_REQUEST),
    ] {
        let mut r = from_peer(
            get(&format!("http://{}/auth/check-setup", authority)),
            "127.0.0.1",
        );
        r.headers_mut().remove(header::HOST);
        let (s, _, _) = h.send(r).await;
        assert_eq!(s, expected, "{}", authority);
    }
}

/// S-16: the session cookie is Secure when the page is served through the
/// HTTPS tunnel, and unchanged on plain loopback.
#[tokio::test]
async fn s16_session_cookie_is_secure_through_an_https_tunnel() {
    let h = H::new();
    {
        let conn = h.ctx().sqlite.conn().unwrap();
        crate::config::save(
            &conn,
            &crate::config::ServerConfigUpdate {
                host_server: Some("https://abc.ngrok.app".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    h.ctx().reload_config().unwrap();
    let mut r = get("/auth/csrf-token");
    r.headers_mut()
        .insert(header::HOST, "abc.ngrok.app".parse().unwrap());
    let (_, headers, _) = h.send(r).await;
    let set = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set.ends_with("; Secure"), "{}", set);
    let (_, headers, _) = h.send(get("/auth/csrf-token")).await;
    let set = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(!set.contains("Secure"), "{}", set);
}

// ------------------------- S-02 follow-up: the sign-in failure budget
//
// One budget per source (this computer, the tunnel, each network address)
// and account (the account, or every other name together), on top of the
// per-address request limit.

use crate::server::ratelimit::{SignInSource, BACKOFF_FIRST, BACKOFF_FREE_FAILURES, BACKOFF_MAX};

fn advance(h: &H, by: std::time::Duration) {
    let now = h.ctx().limiter.now();
    h.ctx().limiter.freeze(Some(now + by));
}

/// Past the per-address request limit's one-minute window.
const MINUTE: std::time::Duration = std::time::Duration::from_secs(61);

fn login_as(user: &str, password: &str) -> Request<Body> {
    multipart("/auth/login", &[("username", user), ("password", password)])
}

fn wrong_login() -> Request<Body> {
    login_as(USER, "Wrong@123")
}

fn right_login() -> Request<Body> {
    login_as(USER, PASSWORD)
}

/// A request from the app's own page in a browser: the page's `Origin`.
fn from_page(mut r: Request<Body>) -> Request<Body> {
    r.headers_mut().insert(
        header::ORIGIN,
        format!("http://127.0.0.1:{}", app_port()).parse().unwrap(),
    );
    r
}

/// The app's page opened through the tunnel.
fn tunnel_page(r: Request<Body>) -> Request<Body> {
    tunnelled(from_page(r))
}

/// The app's page opened from a device on the network.
fn lan_page(r: Request<Body>) -> Request<Body> {
    from_peer(from_page(r), LAN)
}

fn failures(h: &H, source: SignInSource) -> u32 {
    h.ctx().limiter.backoff.failures(source)
}

fn message(v: &Value) -> &str {
    v["message"].as_str().unwrap_or_default()
}

/// Bring the account's budget from one source (through `via`) to its first
/// wait: five free failures, then the sixth after the per-address minute.
async fn start_the_wait(h: &H, via: impl Fn(Request<Body>) -> Request<Body>) {
    for _ in 0..BACKOFF_FREE_FAILURES {
        let (s, v) = h.json(via(wrong_login())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", v);
    }
    advance(h, MINUTE);
    let (s, v) = h.json(via(wrong_login())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", v);
    let (s, v) = h.json(via(right_login())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert!(message(&v).contains("30 seconds"), "{}", v);
}

/// S-02: password guessing from this computer is throttled: the
/// per-address limit first, then a wait that doubles up to 5 minutes. The
/// right password works once the wait is over. A web page's posts are
/// refused and spend nothing.
#[tokio::test]
async fn s02_local_guessing_waits_longer_each_time_up_to_five_minutes() {
    let h = H::new();
    h.setup();
    for _ in 0..50 {
        let (s, _) = h.json(cross_site(wrong_login(), "empty")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    assert_eq!(failures(&h, SignInSource::Local), 0);

    // A hundred quick guesses: five are checked, the per-address limit
    // refuses the rest.
    let mut checked = 0;
    for _ in 0..100 {
        let (s, v) = h.json(wrong_login()).await;
        match s {
            StatusCode::UNAUTHORIZED => checked += 1,
            StatusCode::TOO_MANY_REQUESTS => {
                assert!(message(&v).contains("Too many login attempts"), "{}", v)
            }
            other => panic!("unexpected {} {}", other, v),
        }
    }
    assert_eq!(checked, BACKOFF_FREE_FAILURES);
    assert_eq!(failures(&h, SignInSource::Local), BACKOFF_FREE_FAILURES);

    // The next failure starts the wait; each one after it doubles it, up to
    // five minutes. Even the right password waits.
    advance(&h, MINUTE);
    let (s, _) = h.json(wrong_login()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let mut wait = BACKOFF_FIRST;
    for expected in [60u64, 120, 240, 300, 300] {
        let (s, v) = h.json(right_login()).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
        assert!(
            message(&v).contains(&format!("{} seconds", wait.as_secs())),
            "{}",
            v
        );
        advance(&h, wait);
        let (s, v) = h.json(wrong_login()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", v);
        wait = std::time::Duration::from_secs(expected);
    }
    assert_eq!(wait, BACKOFF_MAX);
    let (s, v) = h.json(right_login()).await;
    assert!(message(&v).contains("300 seconds"), "{}", v);
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    // After the cap's wait the right password signs in and clears the
    // budget.
    advance(&h, BACKOFF_MAX);
    let (s, v) = h.json(right_login()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(failures(&h, SignInSource::Local), 0);
}

/// S-02: failures through the tunnel or from a device on the network never
/// delay a sign-in at this computer, nor one from another device.
#[tokio::test]
async fn s02_remote_failures_never_delay_a_local_sign_in() {
    let h = H::new();
    h.setup();
    start_the_wait(&h, tunnel_page).await;
    start_the_wait(&h, lan_page).await;
    // The tunnel's first wait ran out meanwhile: one more failure starts
    // the next, so both remote sources are waiting now.
    let (s, _) = h.json(tunnel_page(wrong_login())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(h
        .ctx()
        .limiter
        .backoff
        .wait(SignInSource::Tunnel, h.ctx().limiter.now())
        .is_some());
    assert_eq!(failures(&h, SignInSource::Local), 0);

    let (s, v) = h.json(right_login()).await;
    assert_eq!(s, StatusCode::OK, "this computer: {}", v);
    let (s, v) = h
        .json(from_peer(from_page(right_login()), "192.168.1.51"))
        .await;
    assert_eq!(s, StatusCode::OK, "another device: {}", v);
    // The sources that failed still wait.
    let (s, _) = h.json(tunnel_page(right_login())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let (s, _) = h.json(lan_page(right_login())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// S-02: a thousand guesses through the tunnel, each with a different name
/// and password, all spend the one tunnel budget for names that are not
/// the account's: nothing typed makes a fresh budget.
#[tokio::test]
async fn s02_tunnel_guesses_with_new_names_and_passwords_share_one_budget() {
    let h = H::new();
    h.setup();
    let mut checked = 0;
    for i in 0..1_000 {
        let (s, v) = h
            .json(tunnel_page(login_as(
                &format!("nobody{}", i),
                &format!("Guess@{}", i),
            )))
            .await;
        match s {
            StatusCode::UNAUTHORIZED => checked += 1,
            StatusCode::TOO_MANY_REQUESTS => {}
            other => panic!("unexpected {} {}", other, v),
        }
    }
    assert_eq!(checked, BACKOFF_FREE_FAILURES);
    // Past the per-address minute, the budget itself refuses: one more
    // guess starts the wait, the next waits.
    advance(&h, MINUTE);
    let (s, _) = h.json(tunnel_page(login_as("nobody-a", "Guess@a"))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = h.json(tunnel_page(login_as("nobody-b", "Guess@b"))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert!(message(&v).contains("30 seconds"), "{}", v);
    assert_eq!(
        failures(&h, SignInSource::Tunnel),
        BACKOFF_FREE_FAILURES + 1
    );
    assert!(h.ctx().limiter.backoff.len() <= 1, "no budget per name");
    // The flood spent nothing of this computer's budget.
    let (s, v) = h.json(right_login()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// S-02: attempts sent in parallel cannot all be checked before the first
/// failure is recorded: each is claimed (and counted) before its password
/// is checked, under the lock that checks the wait.
#[tokio::test]
async fn s02_parallel_attempts_cannot_slip_through() {
    let h = H::new();
    h.setup();
    for _ in 0..BACKOFF_FREE_FAILURES {
        let (s, _) = h.json(wrong_login()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    advance(&h, MINUTE);
    // Twenty at once: the per-address limit lets five through; only the
    // first of those is checked (it starts the wait), the others wait.
    let wave = |n: usize| futures_util::future::join_all((0..n).map(|_| h.json(wrong_login())));
    let first = wave(20).await;
    let checked = first
        .iter()
        .filter(|(s, _)| *s == StatusCode::UNAUTHORIZED)
        .count();
    assert_eq!(checked, 1, "{:?}", first);
    // During the wait, fifty at once are all refused.
    advance(&h, MINUTE / 4);
    let during = wave(50).await;
    assert!(
        during
            .iter()
            .all(|(s, _)| *s == StatusCode::TOO_MANY_REQUESTS),
        "{:?}",
        during
    );
    assert_eq!(failures(&h, SignInSource::Local), BACKOFF_FREE_FAILURES + 1);
}

/// S-02: a sign-in that does not come from the app's own page is refused
/// before anything is counted or checked: a cross-site form (by
/// `Sec-Fetch-Site`, `Origin` or, without those, `Referer`), and a remote
/// caller that says nothing about its page. A program on this computer
/// that says nothing is accepted and spends the local budget.
#[tokio::test]
async fn s02_sign_ins_not_from_the_apps_page_are_refused_before_counting() {
    let h = H::new();
    h.setup();
    let with = |pairs: &[(&str, &str)]| {
        let mut r = wrong_login();
        for (k, v) in pairs {
            r.headers_mut().insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        r
    };
    for r in [
        with(&[("sec-fetch-site", "cross-site")]),
        with(&[("sec-fetch-site", "same-site")]),
        with(&[("origin", "https://evil.example")]),
        with(&[("origin", "null")]),
        with(&[("referer", "https://evil.example/page")]),
        tunnelled(wrong_login()),
        from_peer(wrong_login(), LAN),
    ] {
        let (s, v) = h.json(r).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{}", v);
    }
    for src in [
        SignInSource::Local,
        SignInSource::Tunnel,
        SignInSource::Network(ip(LAN)),
    ] {
        assert_eq!(failures(&h, src), 0, "{:?}", src);
    }
    // A same-site referer of the app's own page is fine; so is a local
    // program that sends nothing.
    let page = format!("http://127.0.0.1:{}/login", app_port());
    let (s, _) = h.json(with(&[("referer", &page)])).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h.json(wrong_login()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(failures(&h, SignInSource::Local), 2);
}

/// S-02: names that are not the account's spend the same budget as wrong
/// passwords: after them the right password waits too.
#[tokio::test]
async fn s02_unknown_user_names_are_delayed_too() {
    let h = H::new();
    h.setup();
    for i in 0..BACKOFF_FREE_FAILURES {
        let (s, _) = h.json(login_as(&format!("ghost{}", i), PASSWORD)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    advance(&h, MINUTE);
    let (s, _) = h.json(login_as("ghost", PASSWORD)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = h.json(login_as("ghost-2", PASSWORD)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert!(message(&v).contains("30 seconds"), "{}", v);
    let (s, v) = h.json(right_login()).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    advance(&h, BACKOFF_FIRST);
    let (s, v) = h.json(right_login()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// S-02: a name that is not the account's cannot be told from a wrong
/// password for the real name: over ten attempts the answers are
/// byte-identical, the delays follow the same sequence, and every unknown
/// name runs the Argon2 check against the stand-in hash.
#[tokio::test]
async fn s02_unknown_names_and_wrong_passwords_are_indistinguishable() {
    use crate::services::auth_service::STAND_IN_CHECKS;
    use std::sync::atomic::Ordering;
    async fn ten(users: [&str; 10]) -> Vec<(StatusCode, bool, Vec<u8>)> {
        let h = H::new();
        h.setup();
        let mut out = vec![];
        for user in users {
            // Spaced so the per-address minute limit is never what answers.
            advance(&h, std::time::Duration::from_secs(13));
            let (s, headers, body) = h.send(login_as(user, "Wrong@123")).await;
            out.push((s, headers.contains_key(header::SET_COOKIE), body));
        }
        out
    }
    let real = ten([USER; 10]).await;
    let before = STAND_IN_CHECKS.load(Ordering::Relaxed);
    let unknown = ten(["ghost"; 10]).await;
    let stand_in = STAND_IN_CHECKS.load(Ordering::Relaxed) - before;
    assert_eq!(real, unknown);
    // Mixed, in either order: one budget whatever the name, so the same
    // sequence again.
    let mut mixed = [USER; 10];
    mixed[5..].fill("ghost");
    assert_eq!(ten(mixed).await, real);
    let mut mixed = ["ghost"; 10];
    mixed[5..].fill(USER);
    assert_eq!(ten(mixed).await, real);
    // The delays did start: some answers were waits.
    assert!(real
        .iter()
        .any(|(s, _, _)| *s == StatusCode::TOO_MANY_REQUESTS));
    // Every unknown-name attempt that was checked ran the stand-in hash.
    let checked = unknown
        .iter()
        .filter(|(s, _, _)| *s == StatusCode::UNAUTHORIZED)
        .count() as u64;
    assert!(stand_in >= checked, "{} of {}", stand_in, checked);
}

/// S-02: authenticator codes spend the same budget as the password, each
/// a different code included; a right password alone neither counts nor
/// clears the code failures.
#[tokio::test]
async fn s02_totp_guessing_waits_on_the_same_budget() {
    let h = H::new();
    h.setup();
    let (row, secret) = AuthService::profile(h.ctx(), USER).unwrap().unwrap();
    crate::db::sqlite::user::set_two_factor(
        &h.ctx().sqlite.conn().unwrap(),
        row.id,
        crate::db::sqlite::user::TwoFactorFlags {
            enabled: true,
            login: true,
            password_reset: false,
            mcp: false,
        },
    )
    .unwrap();
    let known = |h: &H| failures(h, SignInSource::Local);
    let (s, headers, b) = h.send(right_login()).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["status"], "totp_required", "{}", v);
    assert_eq!(known(&h), 0, "a right password is not a failure");
    let cookie = cookie_from(&headers).unwrap();
    let (_, v) = h
        .json(with_session(get("/auth/csrf-token"), &cookie, None))
        .await;
    let csrf = v["csrf_token"].as_str().unwrap().to_string();
    let totp = |code: &str| {
        with_session(
            post_json("/auth/login/totp", json!({"totp_code": code})),
            &cookie,
            Some(&csrf),
        )
    };
    let right = || {
        crate::security::totp::generate(
            secret.as_ref().unwrap().expose(),
            h.ctx().now().timestamp() as u64,
        )
        .unwrap()
    };
    // A wrong code that is surely not the current one.
    let mut n = 0u32;
    let mut wrong = || loop {
        n += 1;
        let c = format!("{:06}", n);
        if c != right() {
            return c;
        }
    };
    // Cross-site attempts are refused and do not count.
    for _ in 0..20 {
        let (s, _) = h.json(cross_site(totp(&wrong()), "empty")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    assert_eq!(known(&h), 0);

    advance(&h, MINUTE);
    for _ in 0..BACKOFF_FREE_FAILURES {
        let (s, v) = h.json(totp(&wrong())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", v);
    }
    // The per-address limit applies to codes too.
    let (s, v) = h.json(totp(&wrong())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", v);
    assert!(message(&v).contains("Too many login attempts"), "{}", v);
    advance(&h, MINUTE);
    let (s, _) = h.json(totp(&wrong())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = h.json(totp(&right())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "the right code waits too");
    assert!(message(&v).contains("30 seconds"), "{}", v);
    // The next failure after the wait doubles it.
    advance(&h, BACKOFF_FIRST);
    let (s, _) = h.json(totp(&wrong())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = h.json(totp(&right())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(message(&v).contains("60 seconds"), "{}", v);
    // After the wait, the right password alone (from a fresh browser)
    // neither counts nor clears the code failures.
    advance(&h, BACKOFF_FIRST * 2);
    let before = known(&h);
    let (s, _, b) = h.send(right_login()).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v["status"], "totp_required", "{}", v);
    assert_eq!(known(&h), before);
    let (s, v) = h.json(totp(&right())).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(known(&h), 0);
}

// ------------------------------------- bans: one address, one check

fn ban(h: &H, ip: &str) {
    let c = h.ctx().logs.conn().unwrap();
    assert!(crate::db::sqlite::monitor::ban_ip(
        &c,
        ip,
        "test",
        Some(24),
        false,
        "test",
        h.ctx().now(),
        5
    )
    .unwrap());
    drop(c);
    h.ctx().monitor.reload_bans(h.ctx());
}

fn is_ban_refusal(s: StatusCode, b: &[u8]) -> bool {
    s == StatusCode::FORBIDDEN && b.starts_with(b"Access Denied")
}

/// Bans compare addresses, not spellings: a banned IPv4 address arriving
/// as IPv4-in-IPv6 is refused, and every spelling of an IPv6 address is
/// one address.
#[tokio::test]
async fn s03_a_ban_holds_in_every_spelling_of_the_address() {
    let h = H::new();
    h.setup();
    ban(&h, "192.168.1.60");
    let (s, _, b) = h
        .send(from_peer(get("/auth/check-setup"), "::ffff:192.168.1.60"))
        .await;
    assert!(is_ban_refusal(s, &b), "{} {:?}", s, b);
    ban(&h, "[2001:DB8:0:0::7]:443");
    for peer in ["2001:db8::7", "2001:0db8:0000:0000:0000:0000:0000:0007"] {
        let (s, _, b) = h.send(from_peer(get("/auth/check-setup"), peer)).await;
        assert!(is_ban_refusal(s, &b), "{}: {} {:?}", peer, s, b);
    }
    // Another address is not.
    let (s, _, _) = h
        .send(from_peer(get("/auth/check-setup"), "192.168.1.61"))
        .await;
    assert_eq!(s, StatusCode::OK);
}

/// A banned address on the network is refused on every surface, in its
/// plain and its IPv4-in-IPv6 form: the API, both webhooks, the live
/// update connection, `/mcp`, and the market data feed (a separate
/// listener, checked through the same ban list).
#[tokio::test]
async fn s03_a_banned_address_is_refused_on_every_surface() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    ban(&h, LAN);
    for peer in [LAN, "::ffff:192.168.1.50"] {
        for r in [
            post_json("/api/v1/ping", json!({"apikey": key})),
            post_json(
                &format!(
                    "/strategy/webhook/{}",
                    crate::strategy::store::generate_webhook_token()
                ),
                json!({}),
            ),
            post_json(
                "/chartink/webhook/22222222-2222-4222-8222-222222222222",
                json!({}),
            ),
            get("/socket.io/?EIO=4&transport=polling"),
            get("/mcp"),
        ] {
            let path = r.uri().to_string();
            let (s, _, b) = h.send(from_peer(r, peer)).await;
            assert!(is_ban_refusal(s, &b), "{} {}: {} {:?}", peer, path, s, b);
        }
        let feed = crate::feed::auth::AppAuth::new(h.ctx().clone());
        assert!(
            crate::feed::auth::FeedAuth::refused(&feed, ip(peer)),
            "feed {}",
            peer
        );
    }
    let feed = crate::feed::auth::AppAuth::new(h.ctx().clone());
    assert!(!crate::feed::auth::FeedAuth::refused(
        &feed,
        ip("192.168.1.51")
    ));
    assert!(!crate::feed::auth::FeedAuth::refused(
        &feed,
        ip("127.0.0.1")
    ));
}

/// A forged forwarding header can never match a ban or escape one: a
/// loopback request claiming a banned address is a tunnel request (which
/// is never banned), and a banned device on the network gains nothing by
/// sending one. The tunnel identity is never written to the ban list.
#[tokio::test]
async fn s03_forged_forwarding_headers_never_match_or_escape_a_ban() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    ban(&h, "203.0.113.9");
    ban(&h, LAN);
    let ping = || post_json("/api/v1/ping", json!({"apikey": key}));
    for name in ["x-forwarded-for", "cf-connecting-ip", "x-real-ip"] {
        let mut r = ping();
        r.headers_mut().insert(name, "203.0.113.9".parse().unwrap());
        let (s, _, b) = h.send(r).await;
        assert_eq!(s, StatusCode::OK, "{}: {:?}", name, b);
        let mut r = from_peer(ping(), LAN);
        r.headers_mut()
            .insert(name, "192.168.1.51".parse().unwrap());
        let (s, _, b) = h.send(r).await;
        assert!(is_ban_refusal(s, &b), "{}: {} {:?}", name, s, b);
    }
    let c = h.ctx().logs.conn().unwrap();
    assert!(!crate::db::sqlite::monitor::ban_ip(
        &c,
        &PROXIED_CALLER.to_string(),
        "manual",
        Some(24),
        false,
        "test",
        h.ctx().now(),
        5
    )
    .unwrap());
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM ip_bans WHERE ip_address = ?1",
            [PROXIED_CALLER.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}
// --------------------------------------- S-08 the live port, fail closed

fn set_bind_host(h: &H, host: &str) {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute("UPDATE settings SET bind_host = ?1 WHERE id = 1", [host])
        .unwrap();
    h.ctx().reload_config().unwrap();
}

/// S-08 follow-up: the port the app window trusts is the one the listener
/// is bound to, read from the bound socket; before the first bind, once
/// the listener stops and after a failed bind it is 0, and the window then
/// trusts no page on this computer. The listener only ever binds an
/// ephemeral port here (pinned to 0), never the app's own ports.
#[tokio::test]
async fn s08_the_live_port_is_the_bound_listener_and_fails_closed() {
    use crate::commands::main_window_may_load;
    let h = H::new();
    h.ctx().pin_listener_ports(0, 0);
    h.ctx().reload_config().unwrap();
    let page = |p: u16| url::Url::parse(&format!("http://127.0.0.1:{}/", p)).unwrap();
    let trusted = |p: u16| main_window_may_load(&page(p), h.ctx().live_port(), false);

    // Before the first bind.
    assert_eq!(h.ctx().live_port(), 0);

    // A successful bind: the port actually bound.
    let server = crate::server::start(h.ctx().clone()).await.unwrap();
    let bound = server.addr.port();
    assert_ne!(bound, 0);
    assert_eq!(h.ctx().live_port(), bound);
    assert!(trusted(bound));

    // The listener stops.
    server.stop().await;
    assert_eq!(h.ctx().live_port(), 0);
    assert!(!trusted(bound), "a stopped listener is still trusted");

    // A restart whose bind fails: an address this computer does not have
    // (TEST-NET), refused the same way on every system.
    set_bind_host(&h, "192.0.2.1");
    assert!(crate::server::start(h.ctx().clone()).await.is_err());
    assert_eq!(h.ctx().live_port(), 0);
    assert!(!trusted(bound), "a failed bind trusts the old port");
}

/// S-03, S-10: the HTTP server and the market data feed classify every
/// combination of socket peer, forwarding header and `Host` the same way:
/// both call the one classifier (`server::source::classify`), each with
/// its own port. A loopback peer is this computer only without any
/// forwarding header and naming this computer on the listener's port.
#[test]
fn s03_http_and_the_feed_classify_callers_identically() {
    use crate::server::source::Source;
    use axum::http::{HeaderName, HeaderValue};
    let cfg = crate::config::ServerConfig {
        http_port: 5000,
        ..crate::config::ServerConfig::default()
    };
    let (http, ws) = (5000u16, 8765u16);
    let peers = [
        "127.0.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "192.168.1.50",
        "::ffff:192.168.1.50",
        "fe80::1",
    ];
    // How the request names its host; `{}` is the listener's own port.
    let hosts: [&[&str]; 9] = [
        &["127.0.0.1:{}"],
        &["localhost:{}"],
        &["[::1]:{}"],
        &["LOCALHOST:{}"],
        &["evil.example:{}"],
        &["127.0.0.1:9999"],
        &["127.0.0.1"],
        &["127.0.0.1:{}", "127.0.0.1:{}"],
        &[],
    ];
    let extra: [&[(&str, &str)]; 9] = [
        &[],
        &[("x-forwarded-for", "203.0.113.7")],
        &[("X-Forwarded-Host", "evil.example")],
        &[("x-forwarded-for", "")],
        &[("forwarded", "for=203.0.113.7")],
        &[("x-real-ip", "127.0.0.1")],
        &[("cf-connecting-ip", "203.0.113.7")],
        &[("via", "1.1 proxy")],
        &[("tailscale-funnel-request", "?1")],
    ];
    let build = |host: &[&str], port: u16, extra: &[(&str, &str)]| {
        let mut h = axum::http::HeaderMap::new();
        for v in host {
            let v = v.replace("{}", &port.to_string());
            h.append(axum::http::header::HOST, HeaderValue::from_str(&v).unwrap());
        }
        for (n, v) in extra {
            h.append(
                HeaderName::from_bytes(n.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    };
    let mut seen = std::collections::HashSet::new();
    for peer in peers {
        let ip: std::net::IpAddr = peer.parse().unwrap();
        for host in hosts {
            for headers in extra {
                let via_http = crate::server::middleware::classify(
                    &cfg,
                    http,
                    Some(ip),
                    &build(host, http, headers),
                    None,
                );
                let via_feed =
                    crate::feed::server::handshake_source(ip, &build(host, ws, headers), ws);
                assert_eq!(via_http, via_feed, "{} {:?} {:?}", peer, host, headers);
                let canonical = crate::server::addr::canonical(ip);
                let local = headers.is_empty()
                    && host.len() == 1
                    && host[0].ends_with(":{}")
                    && !host[0].starts_with("evil");
                let expected = if !canonical.is_loopback() {
                    Source::Lan(canonical)
                } else if local {
                    Source::Local
                } else {
                    Source::Tunnel
                };
                assert_eq!(via_http, expected, "{} {:?} {:?}", peer, host, headers);
                seen.insert(std::mem::discriminant(&expected));
            }
        }
    }
    assert_eq!(seen.len(), 3, "the table covers all three kinds of caller");
}

/// S-02, end to end through real WebSocket connections: a thousand bad
/// keys over the market data feed through the tunnel spend the tunnel's
/// failure budget. Once it is spent, a bad key gets the web's error frame
/// and the connection is closed at once with 4401, so each further guess
/// costs a new connection. A valid key from another source (this
/// computer) still connects, and so does a valid key through the tunnel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s02_feed_tunnel_failures_never_refuse_a_valid_key() {
    use crate::server::ratelimit::Bucket;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    async fn next_frame<S>(ws: &mut S) -> Message
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        loop {
            match ws.next().await {
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(m)) => return m,
                other => panic!("the connection ended: {:?}", other.map(|r| r.is_ok())),
            }
        }
    }
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    h.ctx().pin_listener_ports(0, 0);
    h.ctx().reload_config().unwrap();
    let feed = crate::feed::FeedService::new(h.ctx().clone());
    assert!(matches!(
        feed.start().await,
        crate::state::ServerStatus::Running { .. }
    ));
    let url = format!("ws://{}", feed.local_addr().await.unwrap());
    let connect = || async {
        let mut r = url.as_str().into_client_request().unwrap();
        r.headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        tokio_tungstenite::connect_async(r)
            .await
            .expect("the feed accepts the connection")
            .0
    };
    let authenticate =
        |k: &str| Message::Text(json!({"action": "authenticate", "api_key": k}).to_string());
    let invalid =
        json!({"status": "error", "code": "AUTHENTICATION_ERROR", "message": "Invalid API key"});
    let t0 = h.ctx().limiter.now();
    let mut ws = connect().await;
    for i in 0..1000u64 {
        // Five hundred a second, under the resource guard.
        h.ctx()
            .limiter
            .freeze(Some(t0 + std::time::Duration::from_millis(2 * i)));
        ws.send(authenticate(&format!("wrong{}", i))).await.unwrap();
        match next_frame(&mut ws).await {
            Message::Text(t) => assert_eq!(serde_json::from_str::<Value>(&t).unwrap(), invalid),
            other => panic!("{:?}", other),
        }
        if i >= 10 {
            // Closed at once for the failed key, not by the auth timeout.
            let frame =
                tokio::time::timeout(std::time::Duration::from_secs(5), next_frame(&mut ws))
                    .await
                    .expect("closed at once once the budget is spent");
            match frame {
                Message::Close(Some(f)) => {
                    assert_eq!(u16::from(f.code), 4401);
                    assert_eq!(f.reason.as_ref(), "Invalid API key");
                }
                other => panic!("not closed after a spent budget: {:?}", other),
            }
            ws = connect().await;
        }
    }
    let now = h.ctx().limiter.now();
    assert!(h
        .ctx()
        .limiter
        .is_exhausted(Bucket::ApiKeyFail, PROXIED_CALLER, now));
    // Another source (this computer, no forwarding header) is unaffected.
    let (mut local, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    local.send(authenticate(&key)).await.unwrap();
    match next_frame(&mut local).await {
        Message::Text(t) => {
            let v: Value = serde_json::from_str(&t).unwrap();
            assert_eq!(v["status"], "success", "this computer: {}", v);
        }
        other => panic!("{:?}", other),
    }
    drop(local);
    // And a valid key through the spent tunnel itself.
    ws.send(authenticate(&key)).await.unwrap();
    match next_frame(&mut ws).await {
        Message::Text(t) => {
            let v: Value = serde_json::from_str(&t).unwrap();
            assert_eq!(v["type"], "auth", "{}", v);
            assert_eq!(v["status"], "success", "{}", v);
        }
        other => panic!("{:?}", other),
    }
    drop(ws);
    feed.stop().await;
}

/// S-02: an IPv6 device is one caller across its /64. A thousand addresses
/// of one /64 sending bad keys share one failure budget and take a couple
/// of limiter entries, not a thousand.
#[tokio::test]
async fn s02_an_ipv6_device_is_one_caller_across_its_64() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    h.setup();
    let now = h.ctx().limiter.now();
    h.ctx().limiter.freeze(Some(now));
    let before = h.ctx().limiter.len();
    for n in 0..1000u32 {
        let a = format!("2001:db8:1:2:{:x}:{:x}::1", n >> 8, n & 0xff);
        let _ = h
            .send(from_peer(
                post_json("/api/v1/ping", json!({"apikey": format!("bad{}", n)})),
                &a,
            ))
            .await;
    }
    let key: std::net::IpAddr = "2001:db8:1:2::".parse().unwrap();
    assert!(h.ctx().limiter.is_exhausted(Bucket::ApiKeyFail, key, now));
    assert!(
        h.ctx().limiter.len() <= before + 3,
        "{} entries",
        h.ctx().limiter.len()
    );
}

/// S-02: filling the limiter with failures from thousands of addresses
/// still limits new callers with bad keys (they share one bounded overflow
/// window) and never refuses a new device on the network that presents a
/// valid key: the overflow refuses only invalid credentials.
#[tokio::test]
async fn s02_a_full_limiter_never_refuses_a_new_device_with_a_valid_key() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let now = h.ctx().limiter.now();
    h.ctx().limiter.freeze(Some(now));
    // A small table, so filling it takes hundreds of callers, not
    // thousands (the behaviour is the same at any size).
    let capacity = 1024;
    h.ctx().limiter.set_capacity(capacity);
    for n in 0..(capacity as u32 + 800) {
        let a = std::net::Ipv4Addr::from(0x0a10_0000 + n).to_string();
        let (s, _, _) = h
            .send(from_peer(
                post_json("/api/v1/ping", json!({"apikey": "wrong"})),
                &a,
            ))
            .await;
        // Refused as invalid, or as over the overflow window once the
        // table is full.
        assert!(
            matches!(s, StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS),
            "{}",
            s
        );
    }
    assert!(h.ctx().limiter.len() <= capacity + 6);
    // New callers with bad keys are still limited: they share one bounded
    // overflow window, and once it is full they are refused as over the
    // limit (429), with the web's body.
    let mut limited = 0;
    for n in 0..300u32 {
        let a = std::net::Ipv4Addr::from(0x0a30_0000 + n).to_string();
        let (s, v) = h
            .json(from_peer(
                post_json("/api/v1/ping", json!({"apikey": "wrong"})),
                &a,
            ))
            .await;
        match s {
            StatusCode::FORBIDDEN => {}
            StatusCode::TOO_MANY_REQUESTS => {
                limited += 1;
                assert!(v.get("message").is_some(), "{}", v);
            }
            other => panic!("{} {}", other, v),
        }
    }
    assert!(limited >= 100, "only {} of 300 were limited", limited);
    // A new device presenting a valid key is not refused by the overflow.
    let (s, v) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            "10.200.0.1",
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "a new device with a valid key: {}", v);
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "this computer: {}", v);
}

/// S-03: automatic bans apply to devices on the network only. With
/// automatic bans on and low thresholds, many bad keys and missing pages
/// from this computer and through the tunnel ban neither; the same from a
/// device on the network bans it.
#[tokio::test]
async fn s03_automatic_bans_never_ban_this_computer_or_the_tunnel() {
    let h = H::new();
    h.setup();
    {
        let c = h.ctx().sqlite.conn().unwrap();
        let mut st = crate::db::sqlite::webui::security_settings(&c).unwrap();
        st.auto_ban_enabled = true;
        st.threshold_404 = 5;
        st.api_threshold = 5;
        crate::db::sqlite::webui::set_security_settings(&c, &st).unwrap();
    }
    h.ctx().monitor.invalidate_settings();
    let now = h.ctx().limiter.now();
    for i in 0..40u64 {
        h.ctx()
            .limiter
            .freeze(Some(now + std::time::Duration::from_millis(50 * i)));
        let bad = || post_json("/api/v1/ping", json!({"apikey": "wrong"}));
        let page = || get(&format!("/no-such-page-{}.php", i));
        let _ = h.send(bad()).await;
        let _ = h.send(page()).await;
        let _ = h.send(tunnelled(bad())).await;
        let _ = h.send(tunnelled(page())).await;
        let _ = h.send(from_peer(bad(), LAN)).await;
        let _ = h.send(from_peer(page(), LAN)).await;
    }
    h.ctx().monitor.drain_now(h.ctx());
    let t = h.ctx().now();
    for ip in ["127.0.0.1", "::1", &PROXIED_CALLER.to_string()] {
        assert!(!h.ctx().monitor.is_banned(ip, t), "{} was banned", ip);
    }
    let rows: i64 = h
        .ctx()
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ip_bans WHERE ip_address IN ('127.0.0.1', ?1)",
            [PROXIED_CALLER.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert!(
        h.ctx().monitor.is_banned(LAN, t),
        "the device was not banned"
    );
}

/// Availability guarantees (security review S-02, S-03), one table across
/// every surface: `/api/v1`, the strategy and Chartink webhooks, `/mcp`,
/// Socket.IO, the feed's `authenticate` and `/auth/login`. After hostile
/// traffic from strangers on the network (IPv4 devices, and an IPv6 device
/// rotating addresses in its /64), through the tunnel, from this machine's
/// own addresses, and a flood that fills the limiter:
///
/// * G1: this computer (a loopback peer) with a valid credential or
///   session is never refused;
/// * G2: a valid credential from any source (the tunnel, a new device on
///   the network, this machine's own LAN address, which is a device on the
///   network like any other) is never refused because of other callers;
/// * G3: bans fall only on the strangers on the network (an IPv4 address,
///   an IPv6 /64), capped and expiring, never on this computer, the tunnel
///   identity or this machine's own addresses, automatic or by hand;
/// * G4: invalid traffic stays bounded: once budgets are spent or the
///   overflow window is full, invalid callers are refused at once;
/// * G5: sign-in keeps its per-source budget (this computer's sign-in is
///   untouched by everyone else's failures).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn availability_guarantees_hold_on_every_surface() {
    use crate::server::ratelimit::Bucket;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    // ---------------------------------------------------------------- setup
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    {
        let c = h.ctx().sqlite.conn().unwrap();
        let mut st = crate::db::sqlite::webui::security_settings(&c).unwrap();
        st.auto_ban_enabled = true;
        st.threshold_404 = 5;
        st.api_threshold = 5;
        crate::db::sqlite::webui::set_security_settings(&c, &st).unwrap();
    }
    h.ctx().monitor.invalidate_settings();
    let token = remote_mcp(&h);
    let hook = crate::strategy::store::generate_webhook_token();
    let chartink = "22222222-2222-4222-8222-222222222222";
    {
        let c = h.ctx().sqlite.conn().unwrap();
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [crate::strategy::store::hash_webhook_token(&hook)],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id) VALUES ('scan', ?1)",
            [chartink],
        )
        .unwrap();
    }
    let (cookie, csrf) = h.session(true);
    // The feed on an ephemeral port; the HTTP port stays the one these
    // in-process requests name (nothing binds it).
    h.ctx().pin_listener_ports(app_port(), 0);
    h.ctx().reload_config().unwrap();
    let feed = crate::feed::FeedService::new(h.ctx().clone());
    assert!(matches!(
        feed.start().await,
        crate::state::ServerStatus::Running { .. }
    ));
    let feed_url = format!("ws://{}", feed.local_addr().await.unwrap());
    let own: Vec<String> = crate::server::addr::own_addresses()
        .into_iter()
        .filter(|a| !a.is_loopback())
        .map(|a| a.to_string())
        .collect();
    // Requests a moment apart, so no one's own resource guard is in play.
    let t0 = h.ctx().limiter.now();
    let tick = std::sync::atomic::AtomicU64::new(0);
    let step = || {
        let n = tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        h.ctx()
            .limiter
            .freeze(Some(t0 + std::time::Duration::from_millis(2 * n)));
    };
    let bad_key = || post_json("/api/v1/ping", json!({"apikey": "wrong"}));
    let page = |i: u32| get(&format!("/no-such-page-{}.php", i));

    // -------------------------------------------------------- hostile traffic
    let strangers_v4 = ["192.168.7.1", "192.168.7.2", "192.168.7.3"];
    for i in 0..10u32 {
        for a in strangers_v4 {
            step();
            let _ = h.send(from_peer(bad_key(), a)).await;
            let _ = h.send(from_peer(page(i), a)).await;
        }
        // An IPv6 device rotating addresses in its /64.
        for j in 0..3u32 {
            step();
            let a = format!("2001:db8:7:7:{:x}::{:x}", i, j + 1);
            let _ = h.send(from_peer(bad_key(), &a)).await;
            let _ = h.send(from_peer(page(i), &a)).await;
        }
        // Through the tunnel, on every surface.
        step();
        let _ = h.send(tunnelled(bad_key())).await;
        let _ = h.send(tunnelled(page(i))).await;
        let _ = h
            .send(tunnelled(mcp_ping(&format!("oamcp_guess{}", i))))
            .await;
        let _ = h
            .send(tunnelled(post_json(
                &format!("/strategy/webhook/oaws_{:0>43}", i),
                json!({}),
            )))
            .await;
        let _ = h
            .send(tunnelled(post_json(
                &format!("/chartink/webhook/{}", uuid::Uuid::new_v4()),
                json!({}),
            )))
            .await;
        // From this machine's own addresses, as a stranger would.
        for a in &own {
            step();
            let _ = h.send(from_peer(bad_key(), a)).await;
            let _ = h.send(from_peer(page(i), a)).await;
        }
        // Bad keys over the feed through the tunnel.
        step();
        let mut r = feed_url.as_str().into_client_request().unwrap();
        r.headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(r).await.unwrap();
        ws.send(Message::Text(
            json!({"action": "authenticate", "api_key": format!("wrong{}", i)}).to_string(),
        ))
        .await
        .unwrap();
        let _ = ws.next().await;
    }
    // A flood from many addresses that fills the limiter (a small table,
    // so it takes hundreds of callers rather than thousands).
    let capacity = 1024;
    h.ctx().limiter.set_capacity(capacity);
    for n in 0..(capacity as u32 + 200) {
        step();
        let a = std::net::Ipv4Addr::from(0x0a40_0000 + n).to_string();
        let (s, _, _) = h.send(from_peer(bad_key(), &a)).await;
        assert!(
            matches!(s, StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS),
            "{}",
            s
        );
    }
    h.ctx().monitor.drain_now(h.ctx());

    // ------------------------------------------------- G1 and G2, per surface
    let refused = |s: StatusCode, b: &[u8]| {
        s == StatusCode::TOO_MANY_REQUESTS
            || s == StatusCode::NOT_FOUND
            || (s == StatusCode::FORBIDDEN && b.starts_with(b"Access Denied"))
            || s == StatusCode::UNAUTHORIZED
    };
    // Each source: how its requests are sent, and whether it is this
    // computer (sign-in needs no page headers there).
    type Via = Box<dyn Fn(Request<Body>) -> Request<Body>>;
    let mut sources: Vec<(String, Via, bool)> = vec![
        ("this computer".into(), Box::new(|r: Request<Body>| r), true),
        ("the tunnel".into(), Box::new(tunnelled), false),
        (
            "a new device".into(),
            Box::new(|r: Request<Body>| from_peer(r, "10.250.0.1")),
            false,
        ),
    ];
    for a in &own {
        let a = a.clone();
        let label = format!("own address {}", a);
        sources.push((
            label,
            Box::new(move |r: Request<Body>| from_peer(r, &a)),
            false,
        ));
    }
    for (name, via, this_computer) in &sources {
        step();
        let surfaces: Vec<(&str, Request<Body>)> = vec![
            ("/api/v1", post_json("/api/v1/ping", json!({"apikey": key}))),
            (
                "strategy webhook",
                post_json(&format!("/strategy/webhook/{}", hook), json!({})),
            ),
            (
                "chartink webhook",
                post_json(&format!("/chartink/webhook/{}", chartink), json!({})),
            ),
            ("/mcp", mcp_ping(&token)),
            (
                "socket.io",
                with_session(get("/socket.io/?EIO=4&transport=polling"), &cookie, None),
            ),
        ];
        for (surface, r) in surfaces {
            let (s, _, b) = h.send(via(r)).await;
            assert!(
                !refused(s, &b),
                "{} on {} was refused: {} {}",
                name,
                surface,
                s,
                String::from_utf8_lossy(&b)
            );
            if surface == "/api/v1" || surface == "/mcp" {
                assert_eq!(s, StatusCode::OK, "{} on {}", name, surface);
            }
        }
        if *this_computer {
            // G1 and G5: sign-in from this computer, after everyone
            // else's failures.
            let (s, v) = h
                .json(via(multipart(
                    "/auth/login",
                    &[("username", USER), ("password", PASSWORD)],
                )))
                .await;
            assert_eq!(s, StatusCode::OK, "{} signing in: {}", name, v);
        }
    }
    // The feed: this computer and the tunnel authenticate with the key.
    for tunnel in [false, true] {
        let mut r = feed_url.as_str().into_client_request().unwrap();
        if tunnel {
            r.headers_mut()
                .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(r).await.unwrap();
        ws.send(Message::Text(
            json!({"action": "authenticate", "api_key": key}).to_string(),
        ))
        .await
        .unwrap();
        let frame = loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => break t,
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                other => panic!("feed (tunnel {}): {:?}", tunnel, other.map(|r| r.is_ok())),
            }
        };
        let v: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["status"], "success", "feed (tunnel {}): {}", tunnel, v);
    }

    // -------------------------------------------------------------- G3: bans
    let t = h.ctx().now();
    let proxied = PROXIED_CALLER.to_string();
    let mut never: Vec<String> = vec!["127.0.0.1".into(), "::1".into(), proxied.clone()];
    never.extend(own.iter().cloned());
    for ip in &never {
        assert!(!h.ctx().monitor.is_banned(ip, t), "{} was banned", ip);
        let rows: i64 = h
            .ctx()
            .logs
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM ip_bans WHERE ip_address = ?1",
                [ip],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0, "{} has a ban row", ip);
    }
    for ip in strangers_v4 {
        assert!(
            h.ctx().monitor.is_banned(ip, t),
            "stranger {} not banned",
            ip
        );
    }
    let rotated: std::net::IpAddr = "2001:db8:7:7:abcd::1".parse().unwrap();
    assert!(
        h.ctx().monitor.is_banned_peer(rotated, t),
        "the IPv6 device's /64 is not banned"
    );
    let (permanent, open_ended, system): (i64, i64, i64) = h
        .ctx()
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT SUM(is_permanent), SUM(expires_at IS NULL), COUNT(*)
             FROM ip_bans WHERE created_by = 'system'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((permanent, open_ended), (0, 0), "automatic bans expire");
    assert!(system <= crate::db::sqlite::monitor::AUTO_BAN_CAP);
    // By hand: this computer, the tunnel identity and own addresses are
    // refused with a reason the trader can read.
    let mut by_hand = vec!["127.0.0.1".to_string(), proxied.clone()];
    by_hand.extend(own.iter().cloned());
    for ip in by_hand {
        let (s, v) = h
            .json(with_session(
                post_json("/security/ban", json!({"ip_address": ip})),
                &cookie,
                Some(&csrf),
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "banning {}: {}", ip, v);
        assert!(
            v.to_string().contains("cannot be banned"),
            "banning {}: {}",
            ip,
            v
        );
    }

    // -------------------------------------------------- G4: invalid bounded
    let now = h.ctx().limiter.now();
    assert!(h
        .ctx()
        .limiter
        .is_exhausted(Bucket::ApiKeyFail, PROXIED_CALLER, now));
    step();
    let (s, _) = h.json(tunnelled(mcp_ping("oamcp_late"))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "a late bad token");
    let (s, _) = h
        .json(tunnelled(post_json(
            &format!("/strategy/webhook/oaws_{:0>43}", 999),
            json!({}),
        )))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "a late unknown webhook");
    let mut limited = 0;
    for n in 0..300u32 {
        let a = std::net::Ipv4Addr::from(0x0a60_0000 + n).to_string();
        let (s, _, _) = h.send(from_peer(bad_key(), &a)).await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
        }
    }
    assert!(
        limited >= 100,
        "new invalid callers after the flood: only {} of 300 limited",
        limited
    );
    feed.stop().await;
}

/// S-03: only a loopback peer is this computer. A request from one of this
/// machine's own LAN addresses is a device on the network (`Lan`), whatever
/// it names, so no list of interfaces can make a caller local.
#[test]
fn s03_this_computers_lan_address_is_a_network_device() {
    use crate::server::source::Source;
    let cfg = crate::config::ServerConfig {
        http_port: 5000,
        ..crate::config::ServerConfig::default()
    };
    let mut peers: Vec<std::net::IpAddr> = crate::server::addr::own_addresses()
        .into_iter()
        .filter(|a| !a.is_loopback())
        .collect();
    peers.push("192.168.1.5".parse().unwrap());
    for peer in peers {
        for host in [
            format!("{}:5000", peer),
            format!("[{}]:5000", peer),
            "127.0.0.1:5000".to_string(),
        ] {
            let mut h = axum::http::HeaderMap::new();
            h.insert(axum::http::header::HOST, host.parse().unwrap());
            let got = crate::server::middleware::classify(&cfg, 5000, Some(peer), &h, None);
            assert_eq!(
                got,
                Source::Lan(crate::server::addr::canonical(peer)),
                "{} {}",
                peer,
                host
            );
        }
    }
}

/// S-02, S-03: a device in our own IPv6 network (link-local here, which
/// every machine has) is counted by its own address and, with all the
/// devices of that /64, against an aggregate failure budget and request
/// ceiling. Rotating through a thousand addresses with bad keys hits the
/// aggregate cap (the resource guard's aggregate is filled directly); a
/// neighbour with a valid key is still served; a banned address does not
/// affect its neighbour. Ten thousand rotations are covered at the limiter
/// (`ten_thousand_rotated_addresses_in_our_network_hit_the_aggregate`).
#[tokio::test]
async fn s02_rotating_through_our_own_ipv6_network_hits_the_aggregate_cap() {
    use crate::server::ratelimit::Bucket;
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let now = h.ctx().limiter.now();
    h.ctx().limiter.freeze(Some(now));
    // The network's resource guard spent directly through the limiter (five
    // thousand requests' worth), so the HTTP part stays short.
    for n in 0..5_000u32 {
        let a: std::net::IpAddr = format!("fe80::7:{:x}", n + 1).parse().unwrap();
        let caller = crate::server::source::Source::Lan(a).ip();
        let _ = h.ctx().limiter.check(Bucket::Guard, caller, now);
    }
    let mut limited = 0;
    for n in 0..1_000u32 {
        let a = format!("fe80::{:x}:{:x}", (n >> 16) + 1, n & 0xffff);
        let (s, _, _) = h
            .send(from_peer(
                post_json("/api/v1/ping", json!({"apikey": format!("bad{}", n)})),
                &a,
            ))
            .await;
        match s {
            StatusCode::FORBIDDEN => {}
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => panic!("{}", other),
        }
    }
    let net = crate::server::addr::aggregate_key("fe80::1".parse().unwrap()).unwrap();
    assert!(
        h.ctx().limiter.hits(Bucket::ApiKeyFail, net, now)
            >= 10 * crate::server::ratelimit::AGGREGATE_FACTOR,
        "the network's aggregate failure budget is spent"
    );
    assert!(
        crate::server::middleware::failures_exhausted(
            h.ctx(),
            "fe80::9999:1".parse().unwrap(),
            Bucket::ApiKeyFail
        ),
        "a fresh address in the network is already over the aggregate budget"
    );
    assert!(
        limited >= 400,
        "the aggregate request window: only {} of 1000 limited",
        limited
    );
    // The network's request windows are spent by the invalid traffic (the
    // resource guard and the /api/v1 window); a neighbour with a valid key
    // is still served: shared windows refuse only invalid credentials.
    for bucket in [Bucket::Guard, Bucket::Api] {
        let (limit, _) = bucket.limit();
        assert!(
            h.ctx().limiter.hits(bucket, net, now)
                >= limit * crate::server::ratelimit::AGGREGATE_FACTOR,
            "{:?} aggregate not spent",
            bucket
        );
    }
    let (s, v) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            "fe80::beef",
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "the neighbour: {}", v);
    // A banned address does not affect its neighbour.
    {
        let c = h.ctx().logs.conn().unwrap();
        crate::db::sqlite::monitor::ban_ip(
            &c,
            "fe80::bad",
            "test",
            Some(1),
            false,
            "admin",
            h.ctx().now(),
            5,
        )
        .unwrap();
    }
    h.ctx().monitor.reload_bans(h.ctx());
    let (s, _, b) = h
        .send(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            "fe80::bad",
        ))
        .await;
    assert!(
        s == StatusCode::FORBIDDEN && b.starts_with(b"Access Denied"),
        "the banned address: {}",
        s
    );
    let (s, v) = h
        .json(from_peer(
            post_json("/api/v1/ping", json!({"apikey": key})),
            "fe80::beef",
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "its neighbour: {}", v);
    assert!(!h
        .ctx()
        .monitor
        .is_banned_peer("fe80::beef".parse().unwrap(), h.ctx().now()));
}

/// S-02, S-03: on every surface, a device rotating through a thousand
/// addresses of our own IPv6 network (link-local here, which every machine
/// has) with invalid credentials is charged to the network's aggregate,
/// in the same limit every surface uses, so it is capped in total: a fresh
/// address of that network is then already over the aggregate.
#[tokio::test]
async fn s02_every_surface_charges_our_networks_aggregate() {
    use crate::server::ratelimit::{Bucket, AGGREGATE_FACTOR};
    let rotated = |n: u32| format!("fe80::{:x}:{:x}", (n >> 16) + 1, n & 0xffff);
    let fresh: std::net::IpAddr = "fe80::abcd:1234".parse().unwrap();
    let net = crate::server::addr::aggregate_key(fresh).unwrap();
    let same_site = |mut r: Request<Body>| {
        r.headers_mut()
            .insert("sec-fetch-site", "same-origin".parse().unwrap());
        r
    };
    // Each surface, the bad request it is sent, and the limit it charges.
    type Bad = Box<dyn Fn(u32, &H) -> Request<Body>>;
    let rows: Vec<(&str, Bad, Bucket)> = vec![
        (
            "/api/v1",
            Box::new(|n: u32, _: &H| {
                post_json("/api/v1/ping", json!({"apikey": format!("bad{}", n)}))
            }),
            Bucket::ApiKeyFail,
        ),
        (
            "strategy webhook",
            Box::new(|n: u32, _: &H| {
                post_json(&format!("/strategy/webhook/oaws_{:0>43}", n), json!({}))
            }),
            Bucket::WebhookFail,
        ),
        (
            "chartink webhook",
            Box::new(|_: u32, _: &H| {
                post_json(
                    &format!("/chartink/webhook/{}", uuid::Uuid::new_v4()),
                    json!({}),
                )
            }),
            Bucket::WebhookFail,
        ),
        (
            "/mcp",
            Box::new(|n: u32, _: &H| mcp_ping(&format!("oamcp_guess{}", n))),
            Bucket::ApiKeyFail,
        ),
        (
            "socket.io",
            Box::new(|_: u32, _: &H| get("/socket.io/?EIO=4&transport=polling")),
            Bucket::Guard,
        ),
        (
            "/auth/login",
            Box::new(move |n: u32, _: &H| {
                same_site(multipart(
                    "/auth/login",
                    &[("username", USER), ("password", &format!("Wrong@{}", n))],
                ))
            }),
            Bucket::LoginMinute,
        ),
        (
            "/auth/login/totp",
            Box::new(move |_: u32, h: &H| {
                let (cookie, csrf) = h.session(false);
                same_site(with_session(
                    multipart("/auth/login/totp", &[("totp_code", "000000")]),
                    &cookie,
                    Some(&csrf),
                ))
            }),
            Bucket::LoginMinute,
        ),
    ];
    for (surface, bad, bucket) in rows {
        let h = H::new();
        h.setup();
        remote_mcp(&h);
        let now = h.ctx().limiter.now();
        h.ctx().limiter.freeze(Some(now));
        for n in 0..1000u32 {
            let _ = h.send(from_peer(bad(n, &h), &rotated(n))).await;
        }
        let (limit, _) = bucket.limit();
        let charged = h.ctx().limiter.hits(bucket, net, now);
        if bucket == Bucket::Guard {
            // The request ceiling: every request is charged to the network.
            assert!(charged >= 1000, "{}: {} charged", surface, charged);
        } else {
            assert!(
                charged >= limit * AGGREGATE_FACTOR,
                "{}: the aggregate holds {} of {}",
                surface,
                charged,
                limit * AGGREGATE_FACTOR
            );
            assert!(
                h.ctx().limiter.is_exhausted(bucket, fresh, now),
                "{}: a fresh address is not over the aggregate",
                surface
            );
        }
    }
    // The feed's authenticate, through the same budget.
    let h = H::new();
    h.setup();
    let auth = crate::feed::auth::AppAuth::new(h.ctx().clone());
    let now = h.ctx().limiter.now();
    h.ctx().limiter.freeze(Some(now));
    for n in 0..1000u32 {
        let caller = crate::server::source::Source::Lan(rotated(n).parse().unwrap()).ip();
        let _ = crate::feed::auth::FeedAuth::admit(&auth, caller);
        crate::feed::auth::FeedAuth::failed(&auth, caller);
    }
    assert!(
        crate::feed::auth::FeedAuth::spent(&auth, crate::server::source::Source::Lan(fresh).ip()),
        "feed: a fresh address is not over the aggregate"
    );
    assert!(h.ctx().limiter.hits(Bucket::Guard, net, now) >= 1000);
}
