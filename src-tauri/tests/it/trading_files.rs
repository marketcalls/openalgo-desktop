//! /custom-indicators and /openscript file routes: access, CSRF on writes,
//! traversal refused, size limits, content types (web
//! `blueprints/custom_indicators.py`, `blueprints/openscript.py`).

use crate::webui_support::{get, req, signed_in, with, H};
use axum::http::{header, Method, StatusCode};
use openalgo_desktop_lib::trading::scripts::{source_hash, MAX_PROGRAM_BYTES, MAX_SOURCE_BYTES};
use serde_json::{json, Value};

fn program_for(source: &str) -> String {
    json!({"format": 1, "source": {"hash": source_hash(source)}}).to_string()
}

#[tokio::test]
async fn indicators_need_the_user_and_serve_es_modules() {
    let (h, cookie, _) = signed_in();
    let (s, _, _) = h.send(get("/custom-indicators/index.json")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, v) = h
        .json(with(get("/custom-indicators/index.json"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!([]));

    let dir = h.dir.path().join("indicators");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ribbon.js"), "export default function () {}").unwrap();
    std::fs::write(dir.join("notes.txt"), "not a module").unwrap();
    std::fs::write(h.dir.path().join("secret.js"), "outside").unwrap();

    let (s, headers, b) = h
        .send(with(get("/custom-indicators/index.json"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["file"], "ribbon.js");
    assert!(v[0]["mtime"].as_i64().unwrap() > 0);

    let (s, headers, b) = h
        .send(with(
            get("/custom-indicators/ribbon.js?v=12"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    assert!(headers[header::CACHE_CONTROL]
        .to_str()
        .unwrap()
        .contains("immutable"));
    assert_eq!(b, b"export default function () {}");
    let (_, headers, _) = h
        .send(with(get("/custom-indicators/ribbon.js"), &cookie, None))
        .await;
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");

    for bad in [
        "/custom-indicators/notes.txt",
        "/custom-indicators/..%2Fsecret.js",
        "/custom-indicators/.hidden.js",
    ] {
        let (s, v) = h.json(with(get(bad), &cookie, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", bad);
        assert_eq!(v, json!({"error": "Invalid indicator filename"}));
    }
    // A path with a separator never reaches the handler as one name.
    let (s, _, _) = h
        .send(with(get("/custom-indicators/../secret.js"), &cookie, None))
        .await;
    assert_ne!(s, StatusCode::OK);
    let (s, _) = h
        .json(with(get("/custom-indicators/missing.js"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openscript_files_round_trip_with_their_program() {
    let (h, cookie, csrf) = signed_in();
    let (s, v) = h
        .json(with(get("/openscript/index.json"), &cookie, None))
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!([])));

    let body = json!({"source": "version 1\r\nstudy(\"x\")", "program": program_for("version 1\nstudy(\"x\")")});
    // A write without the session's token is refused before the route.
    let (s, _) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/trend.oscript",
                Some(body.clone()),
            ),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(!h.dir.path().join("openscript/trend.oscript").exists());

    let (s, v) = h
        .json(with(
            req(Method::POST, "/openscript/trend.oscript", Some(body)),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    assert_eq!(v["file"], "trend.oscript");
    assert_eq!(v["program"], true);

    let (_, v) = h
        .json(with(get("/openscript/index.json"), &cookie, None))
        .await;
    assert_eq!(v[0]["file"], "trend.oscript");
    assert_eq!(v[0]["program"], true);
    assert_eq!(
        v.as_array().unwrap().len(),
        1,
        "programs and backups are not listed"
    );

    let (s, headers, b) = h
        .send(with(get("/openscript/trend.oscript"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    assert_eq!(b, b"version 1\r\nstudy(\"x\")");
    let (s, headers, _) = h
        .send(with(
            get("/openscript/program/trend.oscript"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("application/json"));

    // A stale program is refused and nothing changes.
    let stale = json!({"source": "version 1\nstudy(\"y\")", "program": program_for("old")});
    let (s, v) = h
        .json(with(
            req(Method::POST, "/openscript/trend.oscript", Some(stale)),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"].as_str().unwrap().contains("different text"));
    let (_, v) = h
        .json(with(get("/openscript/index.json"), &cookie, None))
        .await;
    assert_eq!(v[0]["program"], true);

    // Saving without a program removes the one that was there.
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/trend.oscript",
                Some(json!({"source": "draft", "program": null})),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!((s, v["program"].clone()), (StatusCode::OK, json!(false)));
    let (s, _) = h
        .json(with(
            get("/openscript/program/trend.oscript"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, v) = h
        .json(with(
            req(Method::DELETE, "/openscript/trend.oscript", None),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "file": "trend.oscript"})
        )
    );
    let (_, v) = h
        .json(with(get("/openscript/index.json"), &cookie, None))
        .await;
    assert_eq!(v, json!([]));
}

#[tokio::test]
async fn openscript_names_and_sizes_are_held_to_the_web_rules() {
    let (h, cookie, csrf) = signed_in();
    for bad in [
        "/openscript/x.js",
        "/openscript/..%2Fx.oscript",
        "/openscript/.x.oscript",
    ] {
        let (s, v) = h.json(with(get(bad), &cookie, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", bad);
        assert!(v["message"]
            .as_str()
            .unwrap()
            .starts_with("Invalid script name"));
    }
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/t.oscript",
                Some(json!({"code": "x"})),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Send a JSON body with a 'source' string");
    let big = "a".repeat(MAX_SOURCE_BYTES + 1);
    let (s, v) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/t.oscript",
                Some(json!({"source": big})),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("the limit is 262144"));
    let program = "p".repeat(MAX_PROGRAM_BYTES + 1);
    let (s, _) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/t.oscript",
                Some(json!({"source": "x", "program": program})),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // A full-size pair still fits the route's own body limit.
    let source = "a".repeat(MAX_SOURCE_BYTES);
    let (s, _) = h
        .json(with(
            req(
                Method::POST,
                "/openscript/t.oscript",
                Some(json!({"source": source, "program": 5})),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // The runner's own routes are not swallowed by the file route.
    let (s, v) = h
        .json(with(get("/openscript/runner/status"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    // Anonymous: every file route needs the user.
    let (s, _, _) = h.send(get("/openscript/index.json")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn instrument_facts_are_validated_and_stated_without_a_contract() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let (s, v) = h
        .json(with(
            get("/openscript/instrument?exchange=NSE"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["message"],
        "Pick a symbol from the search to read its details."
    );
    let (s, _) = h
        .json(with(
            get("/openscript/instrument?symbol=SBIN&exchange=n%20se"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = h
        .json(with(
            get("/openscript/instrument?symbol=NIFTY&exchange=nse_index"),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    assert_eq!(v["contractFound"], false);
    assert_eq!(v["instrument"]["exchange"], "NSE_INDEX");
    assert_eq!(v["instrument"]["instrumentType"], "index");
    assert_eq!(v["instrument"]["timezone"], "Asia/Kolkata");
    assert_eq!(v["instrument"]["session"]["start"], "09:15");
    assert_eq!(v["today"]["open"], true);
}
