//! The ChatGPT plan and web search routes against the web's
//! `blueprints/agent.py`: paths and methods, statuses, envelopes and the
//! shapes the carried-over panels read. Providers and OpenAI's sign-in are
//! local fakes; nothing leaves the machine.

use super::*;
use crate::agent::chatgpt::flow::{Endpoints, Flow};
use crate::agent::chatgpt::ChatGptPlan;
use crate::agent::outbound::fake::{Answer, FakeServer};
use crate::security::Unlocked;

fn harness() -> (H, String, String) {
    let h = H::new();
    h.ctx().security.install(&Unlocked::random().unwrap());
    let (cookie, csrf) = h.session(true);
    (h, cookie, csrf)
}

fn call(method: Method, path: &str, body: Option<&str>, cookie: &str, csrf: &str) -> Request<Body> {
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    with_session(r, cookie, Some(csrf))
}

#[tokio::test]
async fn every_route_is_the_webs_and_needs_the_trader() {
    let table = routes::table();
    let agent: Vec<String> = table
        .iter()
        .filter(|r| r.path.starts_with("/agent/api"))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let ours = [
        ("GET", "/agent/api/websearch"),
        ("PUT", "/agent/api/websearch"),
        ("PUT", "/agent/api/websearch/providers/{provider}/key"),
        ("DELETE", "/agent/api/websearch/providers/{provider}/key"),
        ("POST", "/agent/api/websearch/providers/{provider}/test"),
        ("GET", "/agent/api/chatgpt/status"),
        ("POST", "/agent/api/chatgpt/login"),
        ("POST", "/agent/api/chatgpt/cancel"),
        ("DELETE", "/agent/api/chatgpt/session"),
    ];
    for (m, p) in ours {
        let route = format!("{} {}", m, p);
        assert!(agent.iter().any(|r| r == &route), "{} is not routed", route);
    }
    let h = H::new();
    let (cookie, _) = h.session(true);
    for (m, p) in ours {
        let path = p.replace("{provider}", "tavily");
        let method: Method = m.parse().unwrap();
        let anonymous = Request::builder()
            .method(method.clone())
            .uri(&path)
            .header(header::ACCEPT, "application/json")
            .body(Body::empty())
            .unwrap();
        let (s, _) = h.json(anonymous).await;
        if method == Method::GET {
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{} {}", m, path);
        } else {
            assert!(!s.is_success(), "{} {} answers a stranger", m, path);
        }
        if method != Method::GET {
            let no_token = with_session(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .body(Body::empty())
                    .unwrap(),
                &cookie,
                None,
            );
            let (s, _) = h.json(no_token).await;
            assert!(!s.is_success(), "{} {} needs the CSRF token", m, path);
        }
    }
}

#[tokio::test]
async fn the_web_search_configuration_reads_and_writes_as_the_web() {
    let (h, c, t) = harness();
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/websearch", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    assert_eq!(
        v["defaults"],
        json!({"provider": "duckduckgo", "perplexity_model": "openai/gpt-5.6-luna", "max_calls_per_turn": 5, "daily_cap": 200})
    );
    assert_eq!(v["data"]["providers"].as_array().unwrap().len(), 3);
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/websearch",
            Some("{}"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Nothing to update"))
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/websearch",
            Some(r#"{"provider": "bing"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("provider must be one of: duckduckgo, tavily, perplexity")
        )
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/websearch",
            Some(r#"{"provider": "Tavily", "daily_cap": 9}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (
            v["data"]["provider"].clone(),
            v["data"]["daily_cap"].clone()
        ),
        (json!("tavily"), json!(9))
    );
    assert_eq!(v["data"]["usage"]["remaining"], json!(9));
}

#[tokio::test]
async fn a_key_goes_in_write_only_and_comes_back_as_a_fingerprint() {
    let (h, c, t) = harness();
    let path = "/agent/api/websearch/providers/tavily/key";
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/websearch/providers/bing/key",
            Some("not json"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("provider must be one of: duckduckgo, tavily, perplexity")
        ),
        "the provider is resolved first"
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/websearch/providers/duckduckgo/key",
            Some(r#"{"api_key": "x"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("DuckDuckGo needs no API key")
        )
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            path,
            Some(r#"{"api_key": "  "}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("An api_key is required for Tavily")
        )
    );
    let (s, v) = h.json(call(Method::PUT, path, None, &c, &t)).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("A JSON body is required"))
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            path,
            Some(r#"{"api_key": "tvly-aaaaaaaabbbbbbbb"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Tavily key stored"))
    );
    assert_eq!(v["data"]["providers"][1]["has_api_key"], json!(true));
    assert!(!v.to_string().contains("tvly-aaaa"));
    for _ in 0..2 {
        let (s, v) = h.json(call(Method::DELETE, path, None, &c, &t)).await;
        assert_eq!(
            (s, v["message"].clone()),
            (StatusCode::OK, json!("Tavily key cleared"))
        );
    }
}

#[tokio::test]
async fn a_provider_test_runs_one_real_query_and_never_stores_a_typed_key() {
    let (h, c, t) = harness();
    let fake = FakeServer::start().await;
    let _on = crate::agent::websearch::tests::OnFake::new(&fake, 8);
    let path = "/agent/api/websearch/providers/tavily/test";
    let (s, v) = h.json(call(Method::POST, path, None, &c, &t)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (v["ok"].clone(), v["message"].clone()),
        (
            json!(false),
            json!("No Tavily key is stored, so there is nothing to test.")
        )
    );
    let (s, v) = h.json(call(Method::POST, path, Some("[1]"), &c, &t)).await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("The request body must be a JSON object")
        )
    );
    let (s, v) = h
        .json(call(Method::POST, path, Some(r#"{"api_key": 5}"#), &c, &t))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("api_key must be a string"))
    );
    fake.script("/tavily/search", vec![Answer::json(200, json!({"results": [{"title": "Hamlet", "url": "https://e.example/h", "content": "Shakespeare"}]}))]);
    let (s, v) = h
        .json(call(
            Method::POST,
            path,
            Some(r#"{"api_key": "tvly-ccccccccdddddddd"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (
            v["ok"].clone(),
            v["message"].clone(),
            v["result_count"].clone()
        ),
        (
            json!(true),
            json!("Tavily answered with 1 result."),
            json!(1)
        )
    );
    assert!(v["latency_ms"].is_u64());
    assert_eq!(
        v["data"]["providers"][1]["has_api_key"],
        json!(false),
        "the typed key was not stored"
    );
    assert_eq!(
        fake.seen_at("/tavily/search")[0].json()["query"],
        "who wrote Hamlet"
    );
    assert_eq!(
        v["data"]["usage"]["count"],
        json!(0),
        "a test is not counted against the cap"
    );
    fake.script(
        "/tavily/search",
        vec![Answer::json(200, json!({"results": []}))],
    );
    let (_, v) = h
        .json(call(
            Method::POST,
            path,
            Some(r#"{"api_key": "tvly-ccccccccdddddddd"}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(v["message"], "Tavily answered but returned no results. It is most likely rate limiting this server; try again in a minute.");
}

fn plan_on(h: &H, fake: &FakeServer) {
    h.ctx()
        .agent
        .set_plan(std::sync::Arc::new(ChatGptPlan::new(Flow {
            endpoints: Endpoints::at(&fake.url("")),
            lifetime: std::time::Duration::from_secs(60),
            interval: std::time::Duration::from_millis(20),
        })));
}

#[tokio::test]
async fn the_chatgpt_sign_in_answers_as_the_web() {
    let (h, c, t) = harness();
    let fake = FakeServer::start().await;
    plan_on(&h, &fake);
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/chatgpt/status", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::OK);
    let keys: Vec<&String> = v["data"].as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 9, "{:?}", keys);
    assert_eq!(
        (
            v["data"]["token_dir"].clone(),
            v["data"]["authorised"].clone()
        ),
        (Value::Null, json!(false))
    );
    // A refused code is a 502 carrying the reason and the snapshot.
    fake.script(
        "/api/accounts/deviceauth/usercode",
        vec![Answer::json(500, json!({}))],
    );
    let (s, v) = h
        .json(call(Method::POST, "/agent/api/chatgpt/login", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(v["message"], v["data"]["message"]);
    assert_eq!(v["data"]["state"], "failed");
    // A body that is not an object is refused.
    let (s, _) = h
        .json(call(
            Method::POST,
            "/agent/api/chatgpt/login",
            Some("[true]"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    fake.script(
        "/api/accounts/deviceauth/usercode",
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d", "user_code": "ABCD-1234"}),
        )],
    );
    fake.script(
        "/api/accounts/deviceauth/token",
        vec![Answer::json(403, json!({}))],
    );
    let (s, v) = h
        .json(call(
            Method::POST,
            "/agent/api/chatgpt/login",
            Some("{}"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (v["data"]["user_code"].clone(), v["reused"].clone()),
        (json!("ABCD-1234"), json!(false))
    );
    let (_, v) = h
        .json(call(
            Method::POST,
            "/agent/api/chatgpt/login",
            Some(r#"{"force": false}"#),
            &c,
            &t,
        ))
        .await;
    assert_eq!(v["reused"], json!(true));
    let (_, v) = h
        .json(call(
            Method::POST,
            "/agent/api/chatgpt/cancel",
            Some("{}"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (v["stopped"].clone(), v["data"]["state"].clone()),
        (json!(true), json!("cancelled"))
    );
    let (_, v) = h
        .json(call(
            Method::POST,
            "/agent/api/chatgpt/cancel",
            Some("{}"),
            &c,
            &t,
        ))
        .await;
    assert_eq!(v["stopped"], json!(false), "cancelling nothing succeeds");
    let (s, v) = h
        .json(call(
            Method::DELETE,
            "/agent/api/chatgpt/session",
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"status": "success", "removed": false, "message": "ChatGPT subscription signed out"})
    );
}
