//! Shell and settings routes: WebSocket config and key, host config,
//! analyze mode, desktop server settings (and its alias), leverage,
//! playground.

use crate::webui_support;

use axum::http::{Method, StatusCode};
use serde_json::json;
use webui_support::*;

#[tokio::test]
async fn shell_routes_need_the_signed_in_user() {
    let h = H::new();
    h.setup();
    let (anon, csrf) = h.session(false);
    for p in [
        "/api/websocket/config",
        "/api/websocket/apikey",
        "/api/config/host",
        "/settings/analyze-mode",
        "/settings/api/server",
        "/api/desktop/settings",
        "/leverage/api/current",
        "/playground/api-key",
        "/playground/endpoints",
    ] {
        let (s, v) = h.json(with(get(p), &anon, None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", p);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Not authenticated"})
        );
    }
    for p in ["/settings/api/server", "/leverage/api/update"] {
        let (s, _) = h
            .json(with(
                req(Method::POST, p, Some(json!({}))),
                &anon,
                Some(&csrf),
            ))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", p);
    }
}

#[tokio::test]
async fn websocket_config_and_key() {
    let (h, c, _) = signed_in();
    let (s, v) = h.json(with(get("/api/websocket/config"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    let port = h.ctx.server_config().ws_port;
    assert_eq!(v["status"], "success");
    assert_eq!(v["websocket_url"], format!("ws://127.0.0.1:{}", port));
    assert_eq!(v["is_secure"], false);
    assert_eq!(v["original_url"], v["websocket_url"]);
    let (s, v) = h.json(with(get("/api/websocket/apikey"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    assert_eq!(v["api_key"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn host_config_and_analyze_mode() {
    let (h, c, _) = signed_in();
    let (s, v) = h.json(with(get("/api/config/host"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["host_server"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));
    assert_eq!(v["is_localhost"], true);
    let (s, v) = h.json(with(get("/settings/analyze-mode"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["analyze_mode"].is_boolean());
}

#[tokio::test]
async fn server_settings_read_save_and_refuse() {
    let (h, c, t) = signed_in();
    let (s, v) = h.json(with(get("/settings/api/server"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    let d = &v["data"];
    for k in [
        "http_host",
        "http_port",
        "ws_host",
        "ws_port",
        "lan_enabled",
    ] {
        assert!(d.get(k).is_some(), "{}", k);
    }
    assert_eq!(d["lan_enabled"], false);
    let (_, alias) = h.json(with(get("/api/desktop/settings"), &c, None)).await;
    assert_eq!(alias, v);

    // Free ports chosen by the OS (never 5000/8765).
    let free = || {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let (hp, wp) = (free(), free());
    let body = json!({"http_host": "127.0.0.1", "http_port": hp, "ws_host": "127.0.0.1", "ws_port": wp, "lan_enabled": false});
    // CSRF is required.
    let (s, _) = h
        .json(with(
            req(Method::POST, "/settings/api/server", Some(body.clone())),
            &c,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = h
        .json(with(
            req(Method::POST, "/settings/api/server", Some(body)),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    assert_eq!(v["restart_required"], true);
    let stored: (i64, i64) = h
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT http_port, ws_port FROM settings WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (hp as i64, wp as i64));

    for (bad, why) in [
        (json!({"http_port": 80, "ws_port": wp}), "1024"),
        (json!({"http_port": hp, "ws_port": hp}), "different"),
        (json!({"http_port": "x", "ws_port": wp}), "1024"),
        (
            json!({"http_host": "evil.example", "http_port": hp, "ws_port": wp}),
            "IP address",
        ),
        (
            json!({"http_host": "0.0.0.0", "ws_host": "0.0.0.0", "http_port": hp, "ws_port": wp, "lan_enabled": false}),
            "other devices",
        ),
    ] {
        let (s, v) = h
            .json(with(
                req(Method::POST, "/settings/api/server", Some(bad)),
                &c,
                Some(&t),
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(v["message"].as_str().unwrap().contains(why), "{}", v);
    }

    // A port another program holds is refused with the reason.
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/settings/api/server",
                Some(json!({"http_port": taken, "ws_port": free()})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let msg = v["message"].as_str().unwrap();
    assert!(msg.contains("already used by another program"), "{}", msg);
    assert!(!msg.contains("Errno") && !msg.contains("os error"));
}

#[tokio::test]
async fn lan_toggle_binds_all_interfaces() {
    let (h, c, t) = signed_in();
    let free = || {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let (s, v) = h
        .json(with(
            req(Method::POST, "/api/desktop/settings", Some(json!({"http_host": "127.0.0.1", "ws_host": "127.0.0.1", "http_port": free(), "ws_port": free(), "lan_enabled": true}))),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["lan_enabled"], true);
    assert_eq!(v["data"]["http_host"], "0.0.0.0");
}

#[tokio::test]
async fn leverage_round_trip_and_validation() {
    let (h, c, t) = signed_in();
    let (s, v) = h.json(with(get("/leverage/api/current"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"status": "success", "leverage": 0.0}));
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/leverage/api/update",
                Some(json!({"leverage": 10})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Leverage set to 10x"})
    );
    let (_, v) = h.json(with(get("/leverage/api/current"), &c, None)).await;
    assert_eq!(v["leverage"], 10.0);
    for (b, m) in [
        (json!({}), "Missing leverage field"),
        (json!({"leverage": -1}), "Leverage cannot be negative"),
        (json!({"leverage": 2.5}), "Leverage must be a whole number"),
        (json!({"leverage": "abc"}), "Invalid leverage value"),
    ] {
        let (s, v) = h
            .json(with(
                req(Method::POST, "/leverage/api/update", Some(b)),
                &c,
                Some(&t),
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(v["message"], m);
    }
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/leverage/api/update",
                Some(json!({"leverage": 0})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Leverage set to Default");
}

#[tokio::test]
async fn playground_key_and_endpoints() {
    let (h, c, _) = signed_in();
    let (s, v) = h.json(with(get("/playground/api-key"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["api_key"].as_str().unwrap().len(), 64);
    let (s, v) = h.json(with(get("/playground/endpoints"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    for k in ["account", "orders", "data", "utilities", "websocket"] {
        assert!(v[k].is_array(), "{}", k);
    }
    let orders = v["orders"].as_array().unwrap();
    let place = orders
        .iter()
        .find(|e| e["path"] == "/api/v1/placeorder")
        .unwrap();
    assert_eq!(place["method"], "POST");
    assert_eq!(place["body"]["apikey"], "");
    let text = v.to_string();
    assert!(
        !text.contains("8765"),
        "feed address follows this app's port"
    );
    assert!(!text.contains("/api/v1/python"));
}
