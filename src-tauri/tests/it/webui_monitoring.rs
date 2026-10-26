//! Logs and monitoring: API order log, traffic logger, API latency,
//! security dashboard, health monitor; and that every store is bounded.

use crate::webui_support;

use axum::http::{header, Method, StatusCode};
use openalgo_desktop_lib::db::sqlite::monitor as store;
use serde_json::json;
use webui_support::*;

const USER_ROUTES: &[(&str, &str)] = &[
    ("GET", "/logs/export"),
    ("GET", "/traffic/api/logs"),
    ("GET", "/traffic/api/stats"),
    ("GET", "/traffic/export"),
    ("GET", "/latency/api/logs"),
    ("GET", "/latency/api/stats"),
    ("GET", "/latency/api/broker/zerodha/stats"),
    ("GET", "/latency/export"),
    ("POST", "/security/ban"),
    ("POST", "/security/unban"),
    ("POST", "/security/ban-host"),
    ("POST", "/security/clear-404"),
    ("GET", "/security/api/data"),
    ("GET", "/security/stats"),
    ("POST", "/security/settings"),
    ("GET", "/security/api/login-activity"),
    ("POST", "/security/api/login-activity/clear"),
    ("GET", "/security/api/active-sessions"),
    ("GET", "/health/status"),
    ("GET", "/health/check"),
    ("GET", "/health/api/current"),
    ("GET", "/health/api/history"),
    ("GET", "/health/api/stats"),
    ("GET", "/health/api/alerts"),
    ("POST", "/health/api/alerts/1/acknowledge"),
    ("POST", "/health/api/alerts/1/resolve"),
    ("GET", "/health/api/held-orders"),
    ("POST", "/health/api/held-orders/not-placed"),
    ("GET", "/health/export"),
];

#[tokio::test]
async fn monitoring_routes_need_the_user_and_writes_need_csrf() {
    let h = H::new();
    h.setup();
    let (anon, anon_csrf) = h.session(false);
    let (c, _) = h.session(true);
    for (m, p) in USER_ROUTES {
        let m = Method::from_bytes(m.as_bytes()).unwrap();
        let (s, _) = h
            .json(with(
                req(m.clone(), p, Some(json!({}))),
                &anon,
                Some(&anon_csrf),
            ))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{} {}", m, p);
        if m != Method::GET {
            let (s, _) = h
                .json(with(req(m.clone(), p, Some(json!({}))), &c, None))
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {} without CSRF", m, p);
        }
    }
    // Page paths: a browser navigation gets the app, JSON needs the user.
    for p in ["/logs", "/health"] {
        let r = axum::http::Request::builder()
            .uri(p)
            .body(axum::body::Body::empty())
            .unwrap();
        let (s, hd, _) = h.send(r).await;
        assert_eq!(s, StatusCode::OK, "{}", p);
        assert!(hd[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let (s, _) = h.json(get(p)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", p);
    }
}

#[tokio::test]
async fn order_log_pages_filters_and_exports_without_the_key() {
    let (h, c, _) = signed_in();
    for i in 0..25 {
        h.ctx
            .logs
            .insert_order_log(
                "placeorder",
                &json!({"apikey": "SECRETKEY", "symbol": format!("SBIN{i}"), "strategy": "S1", "exchange": "NSE"}),
                &json!({"status": "success", "orderid": format!("{i}")}),
            )
            .unwrap();
    }
    // Rows are stamped with wall-clock time; ask for that IST date.
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .format("%Y-%m-%d")
        .to_string();
    let q = format!("/logs?page=2&start_date={today}&end_date={today}");
    let mut r = with(get(&q), &c, None);
    r.headers_mut()
        .insert("x-requested-with", "XMLHttpRequest".parse().unwrap());
    let (s, v) = h.json(r).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total_pages"], 2);
    assert_eq!(v["current_page"], 2);
    let logs = v["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 5);
    let e = &logs[0];
    for k in [
        "id",
        "api_type",
        "request_data",
        "response_data",
        "strategy",
        "created_at",
    ] {
        assert!(e.get(k).is_some(), "{}", k);
    }
    assert_eq!(e["strategy"], "S1");
    assert!(e["request_data"].get("apikey").is_none());
    let (_, v) = h
        .json(with(
            get(&format!(
                "/logs?start_date={today}&end_date={today}&search=SBIN24"
            )),
            &c,
            None,
        ))
        .await;
    assert_eq!(v["logs"].as_array().unwrap().len(), 1);
    let (s, v) = h.json(with(get("/logs?start_date=bad"), &c, None)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"].is_string());

    let (s, hd, b) = h
        .send(with(
            get(&format!("/logs/export?start_date={today}&end_date={today}")),
            &c,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hd[header::CONTENT_TYPE], "text/csv");
    assert!(hd[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("openalgo_logs_"));
    let csv = String::from_utf8(b).unwrap();
    assert!(csv.starts_with("ID,Timestamp,API Type,Strategy,Exchange,Symbol"));
    assert_eq!(csv.lines().count(), 26);
    assert!(!csv.contains("SECRETKEY"));
}

#[tokio::test]
async fn traffic_logger_records_requests_without_queries_or_bodies() {
    let (h, c, _) = signed_in();
    h.send(with(get("/admin/api/stats?token=SECRET"), &c, None))
        .await;
    h.send(get("/api/v1/nosuchthing")).await;
    let (s, v) = h
        .json(with(get("/traffic/api/logs?limit=50"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    let rows = v.as_array().unwrap();
    let stats_row = rows
        .iter()
        .find(|r| r["path"] == "/admin/api/stats")
        .expect("logged");
    for k in [
        "timestamp",
        "client_ip",
        "method",
        "path",
        "status_code",
        "duration_ms",
        "host",
        "error",
    ] {
        assert!(stats_row.get(k).is_some(), "{}", k);
    }
    assert_eq!(stats_row["status_code"], 200);
    assert_eq!(stats_row["client_ip"], "127.0.0.1");
    assert!(!v.to_string().contains("SECRET"));
    assert!(rows
        .iter()
        .all(|r| !r["path"].as_str().unwrap().starts_with("/traffic")));

    let (s, v) = h.json(with(get("/traffic/api/stats"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["overall"]["total_requests"].as_i64().unwrap() >= 2);
    assert_eq!(v["api"]["error_requests"], 1);
    assert!(v["endpoints"]["placeorder"].is_object());
    let (s, hd, b) = h.send(with(get("/traffic/export"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(hd[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("traffic_logs.csv"));
    assert!(String::from_utf8(b)
        .unwrap()
        .starts_with("Timestamp,Client IP,Method,Path"));
}

/// The traffic log's Error column says why a request was refused: the
/// reason an `/api/v1` or page reply gave, or a validation reply's field
/// errors. It was always empty (docs-48); the web fills it only for an
/// exception its handler raised.
#[tokio::test]
async fn the_traffic_log_says_why_a_request_was_refused() {
    let (h, c, t) = signed_in();
    let timings = |body| req(Method::POST, "/api/v1/market/timings", Some(body));
    h.send(timings(json!({"apikey": "wrong", "date": "2026-10-01"})))
        .await;
    h.send(timings(json!({"date": "2026-10-01"}))).await;
    let unban = req(
        Method::POST,
        "/security/unban",
        Some(json!({"ip_address": "192.168.1.77"})),
    );
    h.send(with(unban, &c, Some(&t))).await;

    let (s, v) = h
        .json(with(get("/traffic/api/logs?limit=50"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    let rows = v.as_array().unwrap();
    let error_of = |path: &str, status: u16| {
        rows.iter()
            .find(|r| r["path"] == path && r["status_code"] == status)
            .map(|r| r["error"].clone())
            .unwrap_or_else(|| panic!("no {} {} in {}", status, path, v))
    };
    assert_eq!(
        error_of("/api/v1/market/timings", 403),
        "Invalid openalgo apikey"
    );
    assert_eq!(
        error_of("/api/v1/market/timings", 400),
        r#"{"apikey":["Missing data for required field."]}"#
    );
    assert_eq!(error_of("/security/unban", 404), "IP not found in ban list");
    for r in rows
        .iter()
        .filter(|r| r["status_code"].as_u64() < Some(400))
    {
        assert_eq!(r["error"], serde_json::Value::Null, "{}", r);
    }
    let (_, _, csv) = h.send(with(get("/traffic/export"), &c, None)).await;
    assert!(String::from_utf8(csv)
        .unwrap()
        .contains("IP not found in ban list"));
}

#[tokio::test]
async fn api_calls_are_timed_for_the_latency_dashboard() {
    let (h, c, _) = signed_in();
    let key = h.ctx.monitor.dropped();
    assert_eq!(key, 0);
    let api_key = openalgo_desktop_lib::services::apikey_service::ApiKeyService::current(&h.ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    for _ in 0..3 {
        h.send(req(
            Method::POST,
            "/api/v1/market/holidays",
            Some(json!({"apikey": api_key, "year": 2026})),
        ))
        .await;
    }
    h.send(req(
        Method::POST,
        "/api/v1/market/timings",
        Some(json!({"apikey": "wrong", "date": "2026-10-01"})),
    ))
    .await;
    let (s, v) = h.json(with(get("/latency/api/logs"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 4);
    let failed = rows.iter().find(|r| r["status"] == "FAILED").unwrap();
    assert_eq!(failed["order_type"], "MARKET_TIMINGS");
    assert_eq!(failed["error"], "Invalid openalgo apikey");
    for k in [
        "timestamp",
        "id",
        "order_id",
        "broker",
        "symbol",
        "order_type",
        "rtt_ms",
        "validation_latency_ms",
        "response_latency_ms",
        "overhead_ms",
        "total_latency_ms",
        "status",
        "error",
    ] {
        assert!(rows[0].get(k).is_some(), "{}", k);
    }
    assert!(!v.to_string().contains(&api_key));
    let (s, v) = h.json(with(get("/latency/api/stats"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total_orders"], 4);
    assert_eq!(v["failed_orders"], 1);
    assert_eq!(v["success_rate"], 75.0);
    for k in [
        "avg_rtt",
        "p50_total",
        "p90_total",
        "p95_total",
        "p99_total",
        "sla_100ms",
        "sla_150ms",
        "sla_200ms",
        "broker_stats",
        "broker_histograms",
    ] {
        assert!(v.get(k).is_some(), "{}", k);
    }
    let (s, v) = h
        .json(with(get("/latency/api/broker/nobroker/stats"), &c, None))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v, json!({"error": "Broker not found"}));
    let (s, _, b) = h.send(with(get("/latency/export"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(b)
        .unwrap()
        .starts_with("Date & Time (IST),Broker,Order ID"));
    // The invalid key was tracked for the security dashboard.
    let (_, v) = h.json(with(get("/security/api/data"), &c, None)).await;
    assert_eq!(v["api_abuse_ips"][0]["ip_address"], "127.0.0.1");
}

#[tokio::test]
async fn bans_block_remote_addresses_until_unbanned() {
    let (h, c, t) = signed_in();
    let remote = [192, 168, 1, 77];
    let (s, _, _) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_ne!(s, StatusCode::FORBIDDEN);
    for (b, m) in [
        (json!({}), "IP address is required"),
        (json!({"ip_address": "nope"}), "Invalid IP address format"),
        (
            json!({"ip_address": "127.0.0.1"}),
            "This is your own computer, so it cannot be banned.",
        ),
    ] {
        let (s, v) = h
            .json(with(
                req(Method::POST, "/security/ban", Some(b)),
                &c,
                Some(&t),
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(v, json!({"error": m}));
    }
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/ban",
                Some(json!({"ip_address": "192.168.1.77", "reason": "test", "duration_hours": 2})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"success": true, "message": "IP 192.168.1.77 has been banned"})
    );
    let (s, _, b) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(b, b"Access Denied: Your IP has been banned");

    let (_, v) = h.json(with(get("/security/api/data"), &c, None)).await;
    let ban = &v["banned_ips"][0];
    assert_eq!(ban["ip_address"], "192.168.1.77");
    assert_eq!(ban["is_permanent"], false);
    assert_eq!(ban["created_by"], "manual");
    assert!(ban["banned_at"].as_str().unwrap().contains("-2026 "));
    assert_eq!(v["security_settings"]["404_threshold"], 100);
    let (_, v) = h.json(with(get("/security/stats"), &c, None)).await;
    assert_eq!(
        v,
        json!({"total_bans": 1, "permanent_bans": 0, "temporary_bans": 1, "suspicious_ips": 0, "near_threshold": 0})
    );

    // The ban expires with time.
    h.clock.advance(chrono::Duration::hours(3));
    h.ctx.monitor.reload_bans(&h.ctx);
    let (s, _, _) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_ne!(s, StatusCode::FORBIDDEN);

    h.json(with(
        req(
            Method::POST,
            "/security/ban",
            Some(json!({"ip_address": "192.168.1.77", "permanent": true})),
        ),
        &c,
        Some(&t),
    ))
    .await;
    let (s, _, _) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/unban",
                Some(json!({"ip_address": "192.168.1.77"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["success"], true);
    let (s, _, _) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_ne!(s, StatusCode::FORBIDDEN);
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/unban",
                Some(json!({"ip_address": "192.168.1.77"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v, json!({"error": "IP not found in ban list"}));
}

#[tokio::test]
async fn not_found_tracking_auto_ban_and_host_ban() {
    let (h, c, t) = signed_in();
    let (s, v) = h
        .json(with(req(Method::POST, "/security/settings", Some(json!({"auto_ban_enabled": true, "threshold_404": 3, "ban_duration_404": 0, "threshold_api": 5, "ban_duration_api": 1, "repeat_offender_limit": 2}))), &c, Some(&t)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["settings"]["404_threshold"], 3);
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/settings",
                Some(json!({"threshold_404": 0})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v,
        json!({"error": "404 threshold must be between 1 and 1000"})
    );

    let remote = [10, 9, 8, 7];
    for i in 0..3 {
        let (s, _, _) = h
            .send_from(
                req(Method::POST, &format!("/api/v1/probe{i}"), Some(json!({}))),
                remote,
            )
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    // Traffic seen from that address under a foreign Host name.
    store::insert_traffic(
        &h.ctx.logs.conn().unwrap(),
        &store::TrafficRow {
            timestamp: store::ts(h.ctx.now()),
            client_ip: "10.9.8.7".into(),
            method: "GET".into(),
            path: "/".into(),
            status_code: 400,
            duration_ms: 1.0,
            host: Some("attacker.example:5500".into()),
            error: None,
        },
    )
    .unwrap();
    h.ctx.monitor.drain_now(&h.ctx);
    let (s, _, _) = h.send_from(get("/auth/csrf-token"), remote).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "auto-banned after 3 missing pages"
    );
    let (_, v) = h.json(with(get("/security/api/data"), &c, None)).await;
    let tr = &v["suspicious_ips"][0];
    assert_eq!(tr["ip_address"], "10.9.8.7");
    assert_eq!(tr["error_count"], 3);
    // Automatic bans always expire: a duration of 0 means the default of
    // 24 hours, never permanent (security review availability guarantees).
    assert_eq!(v["banned_ips"][0]["is_permanent"], false);
    assert!(v["banned_ips"][0]["expires_at"].is_string());

    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/clear-404",
                Some(json!({"ip_address": "10.9.8.7"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["success"], true);
    let (s, _) = h
        .json(with(
            req(
                Method::POST,
                "/security/clear-404",
                Some(json!({"ip_address": "10.9.8.7"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/ban-host",
                Some(json!({"host": "bad%host"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v, json!({"error": "Invalid hostname format"}));
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/ban-host",
                Some(json!({"host": "nobody.example"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(v["suggestion"].is_string());
    h.json(with(
        req(
            Method::POST,
            "/security/unban",
            Some(json!({"ip_address": "10.9.8.7"})),
        ),
        &c,
        Some(&t),
    ))
    .await;
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/security/ban-host",
                Some(json!({"host": "attacker.example"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["message"],
        "Banned 1 IPs associated with host: attacker.example"
    );
}

#[tokio::test]
async fn login_activity_and_active_sessions() {
    let h = H::new();
    h.setup();
    let form = |u: &str, p: &str| {
        axum::http::Request::builder()
            .method(Method::POST)
            .uri("/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(format!("username={u}&password={p}")))
            .unwrap()
    };
    let (s, _, _) = h.send(form(USER, "wrong")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = h.send(form(USER, "Secret%40123")).await;
    assert_eq!(s, StatusCode::OK);
    let (c, t) = h.session(true);
    let (s, v) = h
        .json(with(get("/security/api/login-activity"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    let a = v["attempts"].as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0]["status"], "success");
    assert_eq!(a[1]["status"], "failed");
    assert_eq!(a[1]["failure_reason"], "invalid_credentials");
    for k in [
        "username",
        "ip_address",
        "device_info",
        "status",
        "login_type",
        "broker",
        "failure_reason",
        "timestamp",
    ] {
        assert!(a[0].get(k).is_some(), "{}", k);
    }
    assert!(!v.to_string().contains("Secret"));
    let (_, v) = h
        .json(with(
            get("/security/api/login-activity?status=failed"),
            &c,
            None,
        ))
        .await;
    assert_eq!(v["attempts"].as_array().unwrap().len(), 1);
    let (s, v) = h
        .json(with(
            req(Method::POST, "/security/api/login-activity/clear", None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Login history cleared"})
    );

    let (s, v) = h
        .json(with(get("/security/api/active-sessions"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    let sessions = v["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty());
    let cookie_id = c.trim_start_matches("session=");
    assert!(
        !v.to_string().contains(cookie_id),
        "the cookie value never leaves the server"
    );
    assert!(sessions
        .iter()
        .any(|s| s["session_id"] == v["current_session_id"]));
}

#[tokio::test]
async fn health_sampler_endpoints_and_alerts() {
    // The sampler reads this process's descriptors, threads and RSS.
    crate::isolated!(health_sampler_endpoints_and_alerts);
    let (h, c, t) = signed_in();
    let (s, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    for k in [
        "timestamp",
        "fd",
        "memory",
        "database",
        "websocket",
        "threads",
        "processes",
        "overall_status",
    ] {
        assert!(v.get(k).is_some(), "{}", k);
    }
    assert!(v["memory"]["rss_mb"].as_f64().unwrap() > 0.0);
    assert!(v["database"]["total"].as_i64().unwrap() >= 1);
    // Every database's size, the sandbox's included (it grows with the
    // trader's analyzer history).
    for db in ["openalgo", "logs", "sandbox", "historify"] {
        assert!(
            v["database"]["sizes_mb"][db].is_number(),
            "{}: {}",
            db,
            v["database"]
        );
    }
    assert!(v["timestamp"].as_str().unwrap().ends_with("+05:30"));
    openalgo_desktop_lib::services::health_service::sample_once(&h.ctx).unwrap();
    let (_, v) = h
        .json(with(get("/health/api/history?hours=1"), &c, None))
        .await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    let (_, v) = h
        .json(with(get("/health/api/stats?hours=9999"), &c, None))
        .await;
    assert_eq!(v["total_samples"], 2);
    assert_eq!(v["time_period_hours"], 168);
    for k in ["fd", "memory", "database", "websocket", "threads", "status"] {
        assert!(v[k].is_object(), "{}", k);
    }
    let (s, v) = h.json(with(get("/health"), &c, None)).await;
    if s != StatusCode::OK {
        let (_, cur) = h.json(with(get("/health/api/current"), &c, None)).await;
        panic!("health is {} with sample {}", s, cur);
    }
    assert_eq!(v["serviceId"], "openalgo");
    let (s, v) = h.json(with(get("/health/check"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["checks"]["database:connectivity"][0]["status"], "pass");

    // An alert raised and handled.
    {
        let conn = h.ctx.logs.conn().unwrap();
        store::raise_alert(
            &conn,
            &store::AlertRow {
                alert_type: "memory_rss_mb_warn".into(),
                severity: "warn".into(),
                metric_name: "memory_rss_mb".into(),
                metric_value: 600.0,
                threshold_value: 500.0,
                message: "Memory use is high".into(),
                ..Default::default()
            },
            h.ctx.now(),
        )
        .unwrap();
    }
    let (_, v) = h.json(with(get("/health/api/alerts"), &c, None)).await;
    let id = v[0]["id"].as_i64().unwrap();
    assert_eq!(v[0]["acknowledged"], false);
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                &format!("/health/api/alerts/{id}/acknowledge"),
                None,
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Alert acknowledged"})
    );
    let (s, _) = h
        .json(with(
            req(
                Method::POST,
                &format!("/health/api/alerts/{id}/resolve"),
                None,
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = h.json(with(get("/health/api/alerts"), &c, None)).await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    let (s, v) = h
        .json(with(
            req(Method::POST, "/health/api/alerts/9999/resolve", None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["message"], "Alert not found");
    let (s, _, b) = h.send(with(get("/health/export?hours=1"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(b)
        .unwrap()
        .starts_with("Date & Time (IST),FD Count"));
}

/// NEW-REL-05: health shows what the broker feed delivered since it
/// connected and what each reader skipped, and what the apps on the market
/// data feed were not sent; a skip makes the block a warning. PROD-REL-05:
/// each reader's skips since OpenAlgo started are there too, and a
/// reconnect starts the counts since the connection began again but not
/// those since start, which it cannot hide.
#[tokio::test]
async fn health_shows_market_data_its_readers_skipped() {
    use crate::broker_session_e2e::until;
    use openalgo_desktop_lib::brokers::mock::MockFeed;
    use openalgo_desktop_lib::websocket::Reader;
    let (h, c, _) = signed_in();
    let (url, srv) = crate::lifecycle::feed_server().await;
    let feed = &h.ctx.websocket;
    feed.connect(Box::new(MockFeed::new(url))).await.unwrap();
    until("the first connection", || feed.is_connected()).await;
    let (s, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let md = &v["market_data"];
    assert_eq!(md["status"], "pass", "{}", md);
    assert_eq!(md["broker_feeds"][0]["name"], "market_data", "{}", md);
    assert_eq!(md["broker_feeds"][0]["connected"], true, "{}", md);
    assert_eq!(
        md["broker_feeds"][0]["skipped_total_since_connect"], 0,
        "{}",
        md
    );
    for k in ["coalesced", "dropped", "skipped"] {
        assert_eq!(md["feed_clients"][k], 0, "{}: {}", k, md);
    }
    feed.stats().note_skipped(Reader::Scalping, 7);
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    let md = &v["market_data"];
    assert_eq!(md["status"], "warn", "{}", md);
    let f = &md["broker_feeds"][0];
    assert_eq!(f["skipped_since_connect"]["scalping"], 7, "{}", f);
    assert_eq!(f["skipped_since_connect"]["strategy"], 0, "{}", f);
    assert_eq!(f["skipped_total_since_connect"], 7, "{}", f);
    assert_eq!(f["skipped_since_start"]["scalping"], 7, "{}", f);
    assert_eq!(f["skipped_since_start"]["sandbox"], 0, "{}", f);
    assert_eq!(f["skipped_total_since_start"], 7, "{}", f);

    // The feed reconnects; then the strategy reader skips 2.
    let connects = || {
        feed.stats()
            .connects
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    feed.reconnect();
    until("the second connection", || {
        connects() == 2 && feed.is_connected() && feed.stats().since_connect().skipped_total() == 0
    })
    .await;
    feed.stats().note_skipped(Reader::Strategy, 2);
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    let md = &v["market_data"];
    assert_eq!(md["status"], "warn", "{}", md);
    let f = &md["broker_feeds"][0];
    assert_eq!(f["skipped_since_connect"]["scalping"], 0, "{}", f);
    assert_eq!(f["skipped_since_connect"]["strategy"], 2, "{}", f);
    assert_eq!(f["skipped_total_since_connect"], 2, "{}", f);
    assert_eq!(f["skipped_since_start"]["scalping"], 7, "{}", f);
    assert_eq!(f["skipped_since_start"]["strategy"], 2, "{}", f);
    assert_eq!(f["skipped_total_since_start"], 9, "{}", f);
    feed.disconnect().await.unwrap();
    srv.abort();
}

/// PROD-REL-05: what the bridge skipped relaying the broker's deeper-book
/// socket to apps on the feed is counted against that socket, and health
/// lists it once anything was skipped, under its own name.
#[tokio::test]
async fn health_shows_market_depth_its_readers_skipped() {
    let (h, c, _) = signed_in();
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["market_data"]["broker_feeds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&v), ["market_data"], "{}", v);
    h.ctx
        .runtime
        .depth_ws
        .stats()
        .note_skipped(openalgo_desktop_lib::websocket::Reader::FeedClients, 4);
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(names(&v), ["market_data", "market_depth"], "{}", v);
    let md = &v["market_data"];
    assert_eq!(md["status"], "warn", "{}", md);
    let depth = &md["broker_feeds"][1];
    assert_eq!(depth["connected"], false, "{}", depth);
    assert_eq!(depth["skipped_since_start"]["feed_clients"], 4, "{}", depth);
    assert_eq!(depth["skipped_total_since_start"], 4, "{}", depth);
}

/// NEW-REL-05: health shows each database's size and how much it grew over
/// the monitoring window (the oldest sample kept against the newest), the
/// order and analyzer logs being kept forever as on the web. PROD-REL-05:
/// and over the last day, with no daily rate stretched from fewer hours.
#[tokio::test]
async fn health_shows_each_databases_growth_over_the_monitoring_window() {
    let (h, c, _) = signed_in();
    // Two days ago, when the stores were smaller, and 12 hours ago.
    let sample = |hours_ago: i64, mb: f64| store::HealthRow {
        timestamp: store::ts(h.ctx.now() - chrono::Duration::hours(hours_ago)),
        db_sizes: Some(json!({"openalgo": mb, "logs": mb, "sandbox": mb, "historify": mb})),
        overall_status: Some("pass".into()),
        ..Default::default()
    };
    {
        let conn = h.ctx.logs.conn().unwrap();
        store::insert_health(&conn, &sample(48, 0.01)).unwrap();
        store::insert_health(&conn, &sample(12, 0.005)).unwrap();
    }
    openalgo_desktop_lib::services::health_service::sample_once(&h.ctx).unwrap();
    let (s, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let g = &v["database"]["growth"];
    assert_eq!(g["window_hours"], 48.0, "{}", g);
    assert!(g["since"].as_str().unwrap().ends_with("+05:30"), "{}", g);
    for db in ["openalgo", "logs", "sandbox", "historify"] {
        let st = &g["stores"][db];
        let size = st["size_mb"]
            .as_f64()
            .unwrap_or_else(|| panic!("{}: {}", db, g));
        let growth = st["growth_mb"].as_f64().unwrap();
        assert!((growth - (size - 0.01)).abs() < 0.011, "{}: {}", db, st);
        let per_day = st["per_day_mb"].as_f64().unwrap();
        assert!((per_day - growth / 2.0).abs() < 0.011, "{}: {}", db, st);
    }
    assert!(g["total"]["growth_mb"].as_f64().unwrap() > 0.0, "{}", g);
    let day = &g["last_day"];
    assert_eq!(day["window_hours"], 12.0, "{}", day);
    for db in ["openalgo", "logs", "sandbox", "historify"] {
        let st = &day["stores"][db];
        let size = st["size_mb"].as_f64().unwrap();
        let growth = st["growth_mb"].as_f64().unwrap();
        assert!((growth - (size - 0.005)).abs() < 0.011, "{}: {}", db, st);
        assert!(st["per_day_mb"].is_null(), "{}: {}", db, st);
    }
    assert!(day["total"]["per_day_mb"].is_null(), "{}", day);
}

/// DIA-01: no sample, a stale one or an unreadable one used to answer
/// "pass". Each is "unknown" (HTTP 200) with the sample's age and a reason;
/// a fresh sample keeps its own status.
#[tokio::test]
async fn health_status_is_unknown_without_a_fresh_sample() {
    let (h, c, _) = signed_in();
    let routes = ["/health", "/health/status", "/health/check"];

    // No sample yet.
    for p in routes {
        let (s, v) = h.json(with(get(p), &c, None)).await;
        assert_eq!(s, StatusCode::OK, "{} {}", p, v);
        assert_eq!(v["status"], "unknown", "{} {}", p, v);
        assert!(v["sample_age_s"].is_null(), "{} {}", p, v);
        assert!(
            v["reason"].as_str().unwrap().contains("No health sample"),
            "{} {}",
            p,
            v
        );
    }

    // A fresh sample: its own status, and its age.
    let taken = openalgo_desktop_lib::services::health_service::sample_once(&h.ctx).unwrap();
    let own = taken.overall_status.clone().unwrap();
    assert_ne!(own, "unknown");
    h.clock.advance(chrono::Duration::seconds(30));
    for p in routes {
        let (_, v) = h.json(with(get(p), &c, None)).await;
        assert_eq!(v["status"], own.as_str(), "{} {}", p, v);
        assert_eq!(v["sample_age_s"], 30, "{} {}", p, v);
        assert!(v.get("reason").is_none(), "{} {}", p, v);
    }
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(v["overall_status"], own.as_str(), "{}", v);
    assert_eq!(v["sample_age_s"], 30, "{}", v);

    // The sampler stops: three minutes later the sample no longer counts.
    h.clock.advance(chrono::Duration::seconds(150));
    for p in routes {
        let (s, v) = h.json(with(get(p), &c, None)).await;
        assert_eq!(s, StatusCode::OK, "{} {}", p, v);
        assert_eq!(v["status"], "unknown", "{} {}", p, v);
        assert_eq!(v["sample_age_s"], 180, "{} {}", p, v);
        assert!(
            v["reason"].as_str().unwrap().contains("3 minutes old"),
            "{} {}",
            p,
            v
        );
    }
    let (s, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["overall_status"], "unknown", "{}", v);
    assert_eq!(v["sample_age_s"], 180, "{}", v);
    // The readings are still the sample's own, for the trader to judge.
    assert_eq!(v["memory"]["rss_mb"].as_f64(), taken.memory_rss_mb, "{}", v);

    // Samples that cannot be read.
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute("DROP TABLE health_metrics", [])
        .unwrap();
    for p in routes {
        let (s, v) = h.json(with(get(p), &c, None)).await;
        assert_eq!(s, StatusCode::OK, "{} {}", p, v);
        assert_eq!(v["status"], "unknown", "{} {}", p, v);
        assert!(
            v["reason"].as_str().unwrap().contains("could not be read"),
            "{} {}",
            p,
            v
        );
    }
    // The databases themselves still answer.
    let (_, v) = h.json(with(get("/health/check"), &c, None)).await;
    assert_eq!(v["checks"]["database:connectivity"][0]["status"], "pass");
    assert_eq!(v["checks"]["database:connectivity"][1]["status"], "pass");
}

#[tokio::test]
async fn health_samples_and_traffic_are_bounded_by_retention() {
    let (h, c, _) = signed_in();
    // A week and a half of samples, one per minute.
    for _ in 0..3 {
        openalgo_desktop_lib::services::health_service::sample_once(&h.ctx).unwrap();
    }
    {
        let conn = h.ctx.logs.conn().unwrap();
        let old = store::ts(h.ctx.now() - chrono::Duration::days(10));
        conn.execute(
            "UPDATE health_metrics SET timestamp = ?1 WHERE id = (SELECT MIN(id) FROM health_metrics)",
            [&old],
        )
        .unwrap();
        for i in 0..100_050 {
            conn.execute(
                "INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code, duration_ms)
                 VALUES (?1, '1.2.3.4', 'GET', '/x', 200, 1.0)",
                [if i < 50 { old.clone() } else { store::ts(h.ctx.now()) }],
            )
            .unwrap();
        }
    }
    openalgo_desktop_lib::services::monitor::housekeep(&h.ctx);
    let conn = h.ctx.logs.conn().unwrap();
    assert_eq!(
        store::count(&conn, "health_metrics").unwrap(),
        2,
        "the 10-day-old sample is gone"
    );
    let traffic = store::count(&conn, "traffic_logs").unwrap();
    assert!(traffic <= 100_000, "{}", traffic);
    drop(conn);
    // Driving the logger many times keeps the queue and the ban cache flat.
    for _ in 0..200 {
        h.send(with(get("/admin/api/stats"), &c, None)).await;
    }
    assert_eq!(h.ctx.monitor.dropped(), 0);
    assert_eq!(h.ctx.monitor.ban_count(), 0);
    h.ctx.monitor.drain_now(&h.ctx);
    let after = store::count(&h.ctx.logs.conn().unwrap(), "traffic_logs").unwrap();
    openalgo_desktop_lib::services::monitor::housekeep(&h.ctx);
    let capped = store::count(&h.ctx.logs.conn().unwrap(), "traffic_logs").unwrap();
    assert!(after > 100_000 && capped <= 100_000, "{} {}", after, capped);
}
