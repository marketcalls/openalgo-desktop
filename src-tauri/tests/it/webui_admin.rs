//! Admin routes (web `blueprints/admin.py`): freeze quantities, holidays,
//! timings, the error log and diagnostics.

use crate::webui_support;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use webui_support::*;

const USER_ROUTES: &[(&str, &str)] = &[
    ("GET", "/admin/api/stats"),
    ("GET", "/admin/api/freeze"),
    ("POST", "/admin/api/freeze"),
    ("PUT", "/admin/api/freeze/1"),
    ("DELETE", "/admin/api/freeze/1"),
    ("POST", "/admin/api/freeze/upload"),
    ("GET", "/admin/api/holidays"),
    ("POST", "/admin/api/holidays"),
    ("DELETE", "/admin/api/holidays/1"),
    ("GET", "/admin/api/timings"),
    ("PUT", "/admin/api/timings/NSE"),
    ("POST", "/admin/api/timings/check"),
    ("GET", "/admin/api/errors"),
    ("POST", "/admin/api/errors/client"),
    ("GET", "/admin/api/errors/stats"),
    ("GET", "/admin/api/errors/groups"),
    ("GET", "/admin/api/system"),
    ("POST", "/admin/api/system/diagnostics"),
    ("GET", "/admin/api/system/report"),
];

#[tokio::test]
async fn admin_routes_need_the_user_and_writes_need_csrf() {
    let h = H::new();
    h.setup();
    let (anon, anon_csrf) = h.session(false);
    let (c, _) = h.session(true);
    for (m, p) in USER_ROUTES {
        let m = Method::from_bytes(m.as_bytes()).unwrap();
        let (s, v) = h
            .json(with(
                req(m.clone(), p, Some(json!({}))),
                &anon,
                Some(&anon_csrf),
            ))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{} {}", m, p);
        assert_eq!(v["message"], "Not authenticated");
        if m != Method::GET {
            // Signed in, but no CSRF token: refused before the handler.
            let (s, v) = h
                .json(with(req(m.clone(), p, Some(json!({}))), &c, None))
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", m, p);
            assert!(v["message"]
                .as_str()
                .unwrap()
                .contains("session has expired"));
        }
    }
}

#[tokio::test]
async fn freeze_quantity_crud_and_upload() {
    let (h, c, t) = signed_in();
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/admin/api/freeze",
                Some(json!({"symbol": "nifty", "freeze_qty": 1800})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["data"],
        json!({"id": v["data"]["id"], "exchange": "NFO", "symbol": "NIFTY", "freeze_qty": 1800})
    );
    let id = v["data"]["id"].as_i64().unwrap();
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/admin/api/freeze",
                Some(json!({"symbol": "NIFTY", "freeze_qty": 1})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "NIFTY already exists for NFO");
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/admin/api/freeze",
                Some(json!({"symbol": "X"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Symbol and freeze_qty are required");

    let (s, v) = h
        .json(with(
            req(
                Method::PUT,
                &format!("/admin/api/freeze/{id}"),
                Some(json!({"freeze_qty": 1200})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["freeze_qty"], 1200);
    let (s, _) = h
        .json(with(
            req(
                Method::PUT,
                "/admin/api/freeze/999",
                Some(json!({"freeze_qty": 5})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = h
        .json(with(
            req(
                Method::PUT,
                &format!("/admin/api/freeze/{id}"),
                Some(json!({})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "No freeze_qty provided");

    let (s, v) = h.json(with(get("/admin/api/freeze"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"].as_array().unwrap().len(), 1);

    // CSV upload replaces the exchange's entries.
    let boundary = "XB";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"exchange\"\r\n\r\nNFO\r\n--{b}\r\nContent-Disposition: form-data; name=\"csv_file\"; filename=\"qtyfreeze.csv\"\r\nContent-Type: text/csv\r\n\r\nSYMBOL    ,VOL_FRZ_QTY\r\nNIFTY,1800\r\nBANKNIFTY,600\r\n\r\n--{b}--\r\n",
        b = boundary
    );
    let up = Request::builder()
        .method(Method::POST)
        .uri("/admin/api/freeze/upload")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap();
    let (s, v) = h.json(with(up, &c, Some(&t))).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["count"], 2);
    assert_eq!(
        v["message"],
        "Successfully loaded 2 freeze quantities for NFO"
    );

    let bad = Request::builder()
        .method(Method::POST)
        .uri("/admin/api/freeze/upload")
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XB")
        .body(Body::from("--XB\r\nContent-Disposition: form-data; name=\"csv_file\"; filename=\"a.txt\"\r\n\r\nx\r\n--XB--\r\n"))
        .unwrap();
    let (s, v) = h.json(with(bad, &c, Some(&t))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Please upload a CSV file");

    let (s, v) = h.json(with(get("/admin/api/stats"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "freeze_count": 2, "holiday_count": 31})
    );

    let id = h.json(with(get("/admin/api/freeze"), &c, None)).await.1["data"][0]["id"]
        .as_i64()
        .unwrap();
    let (s, v) = h
        .json(with(
            req(Method::DELETE, &format!("/admin/api/freeze/{id}"), None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .starts_with("Deleted freeze qty for "));
}

#[tokio::test]
async fn holidays_list_add_delete() {
    let (h, c, t) = signed_in();
    let (s, v) = h
        .json(with(get("/admin/api/holidays?year=2026"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["current_year"], 2026);
    assert_eq!(v["data"].as_array().unwrap().len(), 17);
    let first = &v["data"][0];
    for k in [
        "id",
        "date",
        "day_name",
        "description",
        "holiday_type",
        "closed_exchanges",
    ] {
        assert!(first.get(k).is_some(), "{}", k);
    }
    assert_eq!(first["day_name"], "Thursday");
    assert_eq!(v["years"], json!([2025, 2026, 2027]));
    assert_eq!(v["exchanges"].as_array().unwrap().len(), 9);

    let add = json!({"date": "2026-12-31", "description": "Year end", "holiday_type": "TRADING_HOLIDAY",
                     "closed_exchanges": ["NSE", "BSE"]});
    let (s, v) = h
        .json(with(
            req(Method::POST, "/admin/api/holidays", Some(add.clone())),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "Added holiday: Year end on 2026-12-31");
    let id = v["data"]["id"].as_i64().unwrap();
    let (s, _) = h
        .json(with(
            req(Method::POST, "/admin/api/holidays", Some(add)),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    for (b, m) in [
        (
            json!({"date": "2026-12-30"}),
            "Date and description are required",
        ),
        (
            json!({"date": "30/12/2026", "description": "x"}),
            "Invalid date format. Use YYYY-MM-DD",
        ),
        (
            json!({"date": "2026-12-30", "description": "x", "holiday_type": "SPECIAL_SESSION"}),
            "Special session requires at least one exchange with timings",
        ),
    ] {
        let (s, v) = h
            .json(with(
                req(Method::POST, "/admin/api/holidays", Some(b)),
                &c,
                Some(&t),
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(v["message"], m);
    }
    // The new holiday changes that day's timings.
    let (_, v) = h
        .json(with(
            req(
                Method::POST,
                "/admin/api/timings/check",
                Some(json!({"date": "2026-12-31"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    let ex: Vec<&str> = v["timings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["exchange"].as_str().unwrap())
        .collect();
    assert!(
        ex.is_empty(),
        "trading holiday with no open exchange: {:?}",
        ex
    );

    let (s, v) = h
        .json(with(
            req(Method::DELETE, &format!("/admin/api/holidays/{id}"), None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Deleted holiday: Year end");
    let (s, v) = h
        .json(with(
            req(Method::DELETE, &format!("/admin/api/holidays/{id}"), None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["message"], "Holiday not found");
}

#[tokio::test]
async fn timings_list_edit_check() {
    let (h, c, t) = signed_in();
    let (s, v) = h.json(with(get("/admin/api/timings"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["today"], "2026-10-05");
    assert_eq!(v["data"].as_array().unwrap().len(), 9);
    let nfo = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["exchange"] == "NFO")
        .unwrap()
        .clone();
    assert_eq!(nfo["end_time"], "15:40");
    assert_eq!(nfo["end_offset"], 56_400_000);
    assert_eq!(v["market_status"][0]["exchange"], "NSE");
    assert!(v["market_status"][0]["start_time"].is_i64());
    assert_eq!(
        v["today_timings"][0],
        json!({"exchange": "NSE", "start_time": "09:15", "end_time": "15:30"})
    );

    let (s, v) = h
        .json(with(
            req(
                Method::PUT,
                "/admin/api/timings/NSE",
                Some(json!({"start_time": "09:00", "end_time": "15:30"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Updated timing for NSE: 09:00 - 15:30");
    for (b, m) in [
        (
            json!({"start_time": "09:00"}),
            "Start time and end time are required",
        ),
        (
            json!({"start_time": "9am", "end_time": "15:30"}),
            "Invalid time format. Use HH:MM",
        ),
    ] {
        let (s, v) = h
            .json(with(
                req(Method::PUT, "/admin/api/timings/NSE", Some(b)),
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
                "/admin/api/timings/check",
                Some(json!({"date": "2026-10-01"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["date"], "2026-10-01");
    assert_eq!(
        v["timings"][0],
        json!({"exchange": "NSE", "start_time": "09:00", "end_time": "15:30"})
    );
    let (s, v) = h
        .json(with(
            req(Method::POST, "/admin/api/timings/check", Some(json!({}))),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Date is required");
}

#[tokio::test]
async fn client_errors_are_sanitized_listed_counted_grouped_and_bounded() {
    let (h, c, t) = signed_in();
    let report = json!({
        "message": "Cannot read properties of undefined\u{1b}[31m",
        "stack": "at x (app.js:1:2)",
        "url": "http://127.0.0.1:5500/broker?code=SECRET&page=2#frag",
        "user_agent": "Test",
        "level": "WARN",
    });
    let (s, v) = h
        .json(with(
            req(Method::POST, "/admin/api/errors/client", Some(report)),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"status": "success"}));
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/admin/api/errors/client",
                Some(json!({"message": ""})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Missing message");

    let (s, v) = h
        .json(with(get("/admin/api/errors?level=WARNING"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["count"], 1);
    let msg = v["data"][0]["message"].as_str().unwrap();
    assert!(msg.starts_with("[CLIENT] Cannot read properties"));
    assert!(!msg.contains("SECRET") && !msg.contains('\u{1b}') && !msg.contains("frag"));
    assert!(msg.contains("code=%5Bredacted%5D"));
    for k in ["count", "scanned", "total_in_window"] {
        assert!(v[k].is_number());
    }
    let (s, v) = h
        .json(with(get("/admin/api/errors?level=NOPE"), &c, None))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Invalid level");

    let (_, v) = h.json(with(get("/admin/api/errors/stats"), &c, None)).await;
    assert_eq!(v["status"], "success");
    assert!(v["total"].as_i64().unwrap() >= 1);
    assert!(v["by_level"]["WARNING"].as_i64().unwrap() >= 1);
    assert!(v["last_1h"].as_i64().unwrap() >= 1);

    for _ in 0..3 {
        h.json(with(
            req(
                Method::POST,
                "/admin/api/errors/client",
                Some(json!({"message": "Same failure 42"})),
            ),
            &c,
            Some(&t),
        ))
        .await;
    }
    let (_, v) = h
        .json(with(get("/admin/api/errors/groups?limit=5"), &c, None))
        .await;
    let g = &v["groups"][0];
    assert_eq!(g["count"], 3);
    assert_eq!(g["fingerprint"].as_str().unwrap().len(), 12);
    assert!(v["total_groups"].as_i64().unwrap() >= 2);

    // Bounded: the table never holds more than its cap after cleanup.
    {
        let conn = h.ctx.logs.conn().unwrap();
        for i in 0..5_100 {
            conn.execute(
                "INSERT INTO error_logs (ts, level, message) VALUES ('2026-10-05 04:00:00', 'ERROR', ?1)",
                [format!("e{i}")],
            )
            .unwrap();
        }
    }
    openalgo_desktop_lib::services::monitor::housekeep(&h.ctx);
    let n: i64 = h
        .ctx
        .logs
        .conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM error_logs", [], |r| r.get(0))
        .unwrap();
    assert!(n <= 5_000, "{}", n);
}

#[tokio::test]
async fn system_info_diagnostics_and_report_have_no_secrets() {
    let (h, c, t) = signed_in();
    let key = openalgo_desktop_lib::services::apikey_service::ApiKeyService::current(&h.ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let (s, v) = h.json(with(get("/admin/api/system"), &c, None)).await;
    assert_eq!(s, StatusCode::OK);
    let d = &v["data"];
    for k in [
        "mode",
        "host",
        "runtime",
        "hardware",
        "build",
        "config",
        "brokers",
        "databases",
        "time",
    ] {
        assert!(d.get(k).is_some(), "{}", k);
    }
    assert_eq!(d["build"]["openalgo_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(d["host"]["machine"], std::env::consts::ARCH);
    assert!(d["databases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["name"] == "openalgo" && x["exists"] == true));
    assert!(d["runtime"]["listeners"].is_array());
    assert!(d["config"]["secrets_present"].is_object());
    assert!(!v.to_string().contains(&key));

    let (s, v) = h
        .json(with(
            req(Method::POST, "/admin/api/system/diagnostics", None),
            &c,
            Some(&t),
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    let checks = v["checks"].as_array().unwrap();
    assert!(checks
        .iter()
        .all(|c| c["name"].is_string() && c["ok"].is_boolean() && c["detail"].is_string()));
    assert_eq!(checks[0]["ok"], true);

    let (s, hd, b) = h
        .send(with(get("/admin/api/system/report?format=txt"), &c, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(hd[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    let cd = hd[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(
        cd.starts_with("attachment; filename=\"openalgo-system-report-") && cd.ends_with(".txt\"")
    );
    let body = String::from_utf8(b).unwrap();
    assert!(body.contains("OpenAlgo Desktop System Report"));
    assert!(!body.contains(&key));
    let (_, hd, b) = h
        .send(with(get("/admin/api/system/report"), &c, None))
        .await;
    assert!(hd[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/markdown"));
    assert!(String::from_utf8(b).unwrap().starts_with("# OpenAlgo"));
    let _: Value = Value::Null;
}
