//! HTTP Request, ported from the web's `test/test_flow_qa_regressions.py`
//! (483-529), with the URL split, the log redaction and the refused address
//! classes checked against Python 3.12 itself (`logic_vectors.json`), and
//! the web defects this lane fixes pinned against the web's answer.

use super::*;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::{NodeEnv, OutboundHttp};
use crate::flow::nodes::testkit::{answer, ctx_at, logs, messages, run};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

/// A name table and one scripted answer; records every request sent.
struct Stub {
    names: HashMap<String, Vec<IpAddr>>,
    reply: Result<HttpReply, String>,
    sent: Mutex<Vec<HttpCall>>,
}

#[async_trait]
impl OutboundHttp for Stub {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        self.names
            .get(host)
            .cloned()
            .ok_or_else(|| "no address is known for this name".into())
    }

    async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
        self.sent.lock().push(call);
        self.reply.clone()
    }
}

fn reply(status: u16, body: &str) -> Result<HttpReply, String> {
    Ok(HttpReply {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
    })
}

fn stub(answer: Result<HttpReply, String>) -> Arc<Stub> {
    let mut names = HashMap::new();
    names.insert(
        "api.example.com".to_string(),
        vec!["93.184.216.34".parse().unwrap()],
    );
    names.insert(
        "dual.example.com".to_string(),
        vec![
            "2606:2800:220:1::".parse().unwrap(),
            "93.184.216.34".parse().unwrap(),
        ],
    );
    names.insert("localhost".to_string(), vec!["127.0.0.1".parse().unwrap()]);
    names.insert(
        "router.example.com".to_string(),
        vec!["192.168.1.1".parse().unwrap()],
    );
    names.insert(
        "mixed.example.com".to_string(),
        vec![
            "93.184.216.34".parse().unwrap(),
            "10.0.0.7".parse().unwrap(),
        ],
    );
    names.insert(
        "tailnet.example.com".to_string(),
        vec!["100.64.1.1".parse().unwrap()],
    );
    Arc::new(Stub {
        names,
        reply: answer,
        sent: Mutex::default(),
    })
}

fn ctx_with(http: Arc<Stub>) -> NodeCtx {
    let env = NodeEnv {
        http,
        ..Default::default()
    };
    ctx_at(Arc::new(FakeFlowClient::new()), "2026-10-29T10:15:00", env)
}

async fn send(d: Value) -> (NodeOutcome, NodeCtx, Arc<Stub>) {
    let http = stub(reply(200, "{\"ok\": true}"));
    let c = ctx_with(http.clone());
    let out = run(&HttpRequest, &c, d).await;
    (out, c, http)
}

/// X-166: loopback, metadata, LAN and odd schemes are refused; so are a
/// name resolving to a private address and one of several addresses.
#[tokio::test]
async fn non_public_destinations_are_refused() {
    for url in [
        "http://127.0.0.1:5000/api/v1/funds",
        "http://localhost/admin",
        "http://169.254.169.254/latest/meta-data/",
        "http://192.168.1.1/",
        "http://10.0.0.5/",
        "file:///etc/passwd",
        "ftp://example.com/x",
        "http://router.example.com/",
        "http://mixed.example.com/",
        "http://[::1]/",
        "http://[::ffff:127.0.0.1]/",
    ] {
        let (out, c, http) = send(json!({"url": url, "method": "GET"})).await;
        assert!(out.is_error(), "{}", url);
        assert!(out.failure_message().contains("refusing"), "{}", url);
        assert!(http.sent.lock().is_empty(), "{}", url);
        assert!(
            messages(&c)[0].starts_with("HTTP request aborted: refusing"),
            "{}",
            url
        );
    }
    let (out, _, _) = send(json!({"url": "http://router.example.com/"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: host 'router.example.com' resolves to 192.168.1.1, which is not a public address"
    );
    let (out, _, _) = send(json!({"url": "ftp://example.com/x"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: scheme 'ftp' is not allowed; use http or https"
    );
    let (out, _, _) = send(json!({"url": "http:///path"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: URL has no host"
    );
    let (out, _, _) = send(json!({"url": "http://nowhere.example.com/"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: host 'nowhere.example.com' could not be resolved (no address is known for this name)"
    );
}

/// N-httpRequest-07: carrier-grade NAT (the Tailscale range) is refused.
/// Python counts 100.64.0.0/10 as public (fails on the web: it sends).
#[tokio::test]
async fn carrier_grade_nat_is_refused() {
    let (out, _, http) = send(json!({"url": "http://tailnet.example.com/"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: host 'tailnet.example.com' resolves to 100.64.1.1, which is not a public address"
    );
    assert!(http.sent.lock().is_empty());
    assert!(guard::is_refused("::ffff:100.64.0.1".parse().unwrap()));
}

/// N-httpRequest-08: the request connects to the address the check
/// vetted (an IPv4 one when there is one), never a fresh lookup; a URL the
/// two parsers read differently is refused.
#[tokio::test]
async fn the_request_goes_to_the_vetted_address() {
    let (out, _, http) = send(json!({"url": "https://dual.example.com:8443/x"})).await;
    assert!(!out.is_error());
    let sent = http.sent.lock().clone();
    assert_eq!(
        sent[0].connect_to,
        "93.184.216.34:8443".parse::<SocketAddr>().unwrap()
    );
    let (out, _, http) = send(json!({"url": "http://api.example.com\\@router.example.com/"})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: URL could not be parsed"
    );
    assert!(http.sent.lock().is_empty());
}

/// N-httpRequest-19: a bad port fails the node only. The web raised out
/// of the check and ended the whole run (fails on the web: an abort).
#[tokio::test]
async fn a_bad_port_fails_the_node_not_the_run() {
    for url in [
        "http://api.example.com:99999/",
        "http://api.example.com:ab/",
    ] {
        let (out, c, _) = send(json!({"url": url})).await;
        assert_eq!(out.aborted(), None);
        assert_eq!(
            answer(&out),
            json!({"status": "error", "message": "refusing to send this request: URL could not be parsed"})
        );
        assert_eq!(
            messages(&c),
            vec!["HTTP request aborted: refusing to send this request: URL could not be parsed"]
        );
    }
}

/// X-167 and N-httpRequest-03: headers are a JSON object, parsed before
/// interpolation.
#[tokio::test]
async fn headers_must_be_a_json_object() {
    let (out, c, _) =
        send(json!({"url": "http://api.example.com/", "headers": "{not json}"})).await;
    assert!(out.failure_message().contains("not valid JSON"));
    assert_eq!(
        out.failure_message(),
        "headers is not valid JSON: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
    );
    assert_eq!(logs(&c)[0].0, "error");
    for (headers, kind) in [
        (json!("[1]"), "list"),
        (json!(5), "int"),
        (json!("\"x\""), "str"),
        (json!("null"), "NoneType"),
    ] {
        let (out, _, _) = send(json!({"url": "http://api.example.com/", "headers": headers})).await;
        assert_eq!(
            out.failure_message(),
            format!("headers must be a JSON object, got {}", kind)
        );
    }
}

/// N-httpRequest-04: each field is interpolated once, and a substituted
/// quote cannot add a header.
#[tokio::test]
async fn fields_are_interpolated_once() {
    let http = stub(reply(200, "{}"));
    let c = ctx_with(http.clone());
    c.vars.set("note", json!("x\", \"X-Injected\": \"yes"));
    c.vars.set("webhook", json!({"path": "{{secret}}"}));
    c.vars.set("secret", json!("s3cret"));
    run(
        &HttpRequest,
        &c,
        json!({"url": "http://api.example.com/r/{{webhook.path}}", "headers": "{\"X-Note\": \"{{note}}\", \"X-Num\": 1.0}"}),
    )
    .await;
    let sent = http.sent.lock().clone();
    assert!(
        sent[0].url.ends_with("/r/%7B%7Bsecret%7D%7D") || sent[0].url.ends_with("/r/{{secret}}")
    );
    assert!(sent[0].headers.contains(&(
        "X-Note".to_string(),
        "x\", \"X-Injected\": \"yes".to_string()
    )));
    assert!(sent[0]
        .headers
        .contains(&("X-Num".to_string(), "1.0".to_string())));
    assert!(!sent[0].headers.iter().any(|(k, _)| k == "X-Injected"));
    assert_eq!(
        messages(&c)[0],
        "HTTP GET http://api.example.com/r/{{secret}}"
    );
}

/// N-httpRequest-10, N-httpRequest-11: GET and DELETE send no body;
/// POST, PUT and PATCH send JSON (re-written compactly) or the text as it
/// is; an object body is sent as JSON where the web raised.
#[tokio::test]
async fn bodies_are_sent_as_httpx_sends_them() {
    let cases = [
        (json!({"method": "GET", "body": "{\"a\": 1}"}), None, false),
        (json!({"method": "DELETE"}), None, false),
        (
            json!({"method": "POST", "body": "{\"symbol\": \"SBIN\", \"qty\": 10, \"px\": 1e16}"}),
            Some("{\"px\":1e+16,\"qty\":10,\"symbol\":\"SBIN\"}"),
            true,
        ),
        (
            json!({"method": "patch", "body": "[1, 2]"}),
            Some("[1,2]"),
            true,
        ),
        (json!({"method": "PUT"}), Some("{}"), true),
        (
            json!({"method": "POST", "body": "symbol=SBIN"}),
            Some("symbol=SBIN"),
            false,
        ),
        (
            json!({"method": "POST", "body": {"symbol": "SBIN"}}),
            Some("{\"symbol\":\"SBIN\"}"),
            true,
        ),
        (
            json!({"method": "POST", "body": "{\"name\": \"caf\\u00e9\"}"}),
            Some("{\"name\":\"caf\u{e9}\"}"),
            true,
        ),
    ];
    for (mut d, body, is_json) in cases {
        d["url"] = json!("http://api.example.com/v1");
        let (out, _, http) = send(d.clone()).await;
        assert_eq!(answer(&out)["status"], "success", "{}", d);
        let sent = http.sent.lock().clone();
        assert_eq!(
            sent[0]
                .body
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned()),
            body.map(str::to_string),
            "{}",
            d
        );
        let typed = sent[0]
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "application/json");
        assert_eq!(typed, is_json, "{}", d);
    }
    let (out, c, http) = send(json!({"url": "http://api.example.com/v1", "method": "HEAD"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "Unsupported method: HEAD"})
    );
    assert_eq!(messages(&c), vec!["HTTP HEAD http://api.example.com/v1"]);
    assert!(http.sent.lock().is_empty());
}

/// decisions.md 32: a body that is JSON `null` sends no body, as the web's
/// `json=None` does: no bytes and no JSON type (the client states the
/// empty length). A node-data null is still the web's empty body, `{}`.
#[tokio::test]
async fn a_null_body_is_no_body() {
    for d in [
        json!({"method": "POST", "body": "null"}),
        json!({"method": "PUT", "body": " null\n"}),
        json!({"method": "patch", "body": "null"}),
    ] {
        let mut d = d;
        d["url"] = json!("http://api.example.com/v1");
        let (out, _, http) = send(d.clone()).await;
        assert_eq!(answer(&out)["status"], "success", "{}", d);
        let sent = http.sent.lock().clone();
        assert_eq!(sent[0].body.as_deref(), Some(&b""[..]), "{}", d);
        assert!(
            !sent[0]
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type")),
            "{}",
            d
        );
    }
    let (_, _, http) =
        send(json!({"url": "http://api.example.com/v1", "method": "POST", "body": null})).await;
    assert_eq!(
        http.sent.lock().clone()[0].body.as_deref(),
        Some(&b"{}"[..])
    );
}

/// N-httpRequest-12, N-httpRequest-13: the answer is stored, success or
/// not, and a non-2xx says its status. The web's had no message, so the run
/// read "node failed" (fails on the web).
#[tokio::test]
async fn answers_are_stored_and_failures_say_their_status() {
    let http = stub(reply(404, "{\"error\": \"no\"}"));
    let c = ctx_with(http);
    let out = run(
        &HttpRequest,
        &c,
        json!({"url": "http://api.example.com/x", "outputVariable": "resp"}),
    )
    .await;
    let want = json!({"status": "error", "statusCode": 404, "data": {"error": "no"}, "message": "HTTP 404"});
    assert_eq!(answer(&out), want);
    assert_eq!(out.failure_message(), "HTTP 404");
    assert_eq!(c.vars.get("resp"), Some(want));
    let http = stub(Ok(HttpReply {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain; charset=latin-1".into())],
        body: b"caf\xe9".to_vec(),
    }));
    let c = ctx_with(http);
    let out = run(&HttpRequest, &c, json!({"url": "http://api.example.com/x"})).await;
    assert_eq!(answer(&out)["data"], "caf\u{e9}");
    let http = stub(reply(302, ""));
    let c = ctx_with(http);
    let out = run(&HttpRequest, &c, json!({"url": "http://api.example.com/x"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "statusCode": 302, "data": "", "message": "HTTP 302"})
    );
}

/// N-httpRequest-14: a transport failure has a reason and no line of the
/// node's own.
#[tokio::test]
async fn a_transport_failure_says_why() {
    for (err, want) in [("timed out", "timed out"), ("", "the request failed")] {
        let http = stub(Err(err.to_string()));
        let c = ctx_with(http);
        let out = run(
            &HttpRequest,
            &c,
            json!({"url": "http://api.example.com/x", "outputVariable": "r"}),
        )
        .await;
        assert_eq!(answer(&out), json!({"status": "error", "message": want}));
        assert_eq!(messages(&c), vec!["HTTP GET http://api.example.com/x"]);
        assert!(!c.vars.contains("r"));
    }
}

/// X-169, N-httpRequest-02: the timeout is milliseconds, defaulted and
/// capped.
#[tokio::test]
async fn timeouts_are_milliseconds_and_capped() {
    assert_eq!(HTTP_TIMEOUT_DEFAULT_MS, 30000);
    assert_eq!(HTTP_TIMEOUT_MAX_MS, 60000);
    for (t, secs) in [
        (json!(100), 0.1),
        (json!(0), 30.0),
        (json!(-5), 30.0),
        (json!(999999), 60.0),
        (json!("2500"), 2.5),
        (json!(null), 30.0),
    ] {
        let (_, _, http) = send(json!({"url": "http://api.example.com/", "timeout": t})).await;
        assert_eq!(http.sent.lock()[0].timeout.as_secs_f64(), secs, "{}", t);
    }
}

/// X-168, N-httpRequest-15: the logged URL carries no query or
/// credentials; credentials become a Basic header, as httpx sends them.
#[tokio::test]
async fn logged_urls_are_redacted_and_credentials_sent_as_a_header() {
    let r = url::redact("https://api.example.com/send?token=SECRET");
    assert!(!r.contains("SECRET"));
    assert!(r.starts_with("https://api.example.com/send"));
    let (_, c, http) =
        send(json!({"url": "http://user:p%40ss@api.example.com:8080/v1?key=k"})).await;
    assert_eq!(
        messages(&c)[0],
        "HTTP GET http://api.example.com:8080/v1?<redacted>"
    );
    let sent = http.sent.lock().clone();
    assert_eq!(sent[0].url, "http://api.example.com:8080/v1?key=k");
    assert!(sent[0].headers.contains(&(
        "authorization".to_string(),
        "Basic dXNlcjpwQHNz".to_string()
    )));
    let (out, _, _) = send(json!({})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "No URL specified"})
    );
    let (out, _, _) = send(json!({"url": 12345})).await;
    assert_eq!(
        out.failure_message(),
        "refusing to send this request: scheme 'none' is not allowed; use http or https"
    );
}

fn vectors() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/flow/logic_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// N-httpRequest-15 and the check's reading of a URL: Python's
/// `urlsplit`, `hostname`, `port` and the web's `_redact_url`.
#[test]
fn urls_split_as_python_splits_them() {
    for case in vectors()["urls"].as_array().unwrap() {
        let u = case["url"].as_str().unwrap();
        let split = url::urlsplit(u);
        if let Some(err) = case.get("split_error") {
            assert_eq!(split.err().as_deref(), err.as_str(), "{}", u);
            assert_eq!(url::redact(u), "<unparseable url>");
            continue;
        }
        let parts = split.unwrap();
        assert_eq!(json!(parts.scheme), case["scheme"], "{}", u);
        assert_eq!(json!(parts.hostname()), case["hostname"], "{}", u);
        match case.get("port_error") {
            Some(e) => assert_eq!(parts.port().err().as_deref(), e.as_str(), "{}", u),
            None => assert_eq!(json!(parts.port().unwrap()), case["port"], "{}", u),
        }
        if let Some(r) = case.get("redacted").and_then(Value::as_str) {
            assert_eq!(url::redact(u), r, "{}", u);
        }
    }
}

/// N-httpRequest-06: Python 3.12's refused classes, address by address,
/// plus carrier-grade NAT.
#[test]
fn refused_addresses_match_python() {
    let cgnat = |ip: IpAddr| {
        let v4 = match ip {
            IpAddr::V4(a) => Some(a),
            IpAddr::V6(a) => a.to_ipv4_mapped(),
        };
        v4.is_some_and(|a| a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64)
    };
    let all = vectors()["addresses"].as_array().unwrap().clone();
    assert!(all.len() > 150);
    for case in all {
        let ip: IpAddr = case["address"].as_str().unwrap().parse().unwrap();
        let want = case["web_refuses"].as_bool().unwrap() || cgnat(ip);
        assert_eq!(guard::is_refused(ip), want, "{}", ip);
    }
}

/// N-logic-06, decisions.md 24: a timeout the web's int() cannot read (a
/// list, a dict, infinity) ends the whole run before anything else, even
/// before the empty-URL check; text that is not a number is the default.
#[tokio::test]
async fn an_unreadable_timeout_ends_the_run() {
    for (t, raised) in [
        (
            json!([1]),
            "int() argument must be a string, a bytes-like object or a real number, not 'list'",
        ),
        (
            json!({"ms": 5}),
            "int() argument must be a string, a bytes-like object or a real number, not 'dict'",
        ),
        (json!("inf"), "cannot convert float infinity to integer"),
    ] {
        let (out, c, http) =
            send(json!({"url": "http://api.example.com/", "timeout": t.clone()})).await;
        assert_eq!(out.aborted(), Some(raised), "{}", t);
        assert!(http.sent.lock().is_empty());
        assert!(messages(&c).is_empty());
        let (out, _, _) = send(json!({"timeout": t.clone()})).await;
        assert_eq!(out.aborted(), Some(raised), "{}", t);
    }
    let (_, _, http) = send(json!({"url": "http://api.example.com/", "timeout": "soon"})).await;
    assert_eq!(http.sent.lock().clone()[0].timeout.as_secs_f64(), 30.0);
}

/// N-httpRequest-03 (decisions.md 36): header text of NaN or Infinity is a
/// Python float; a float or a bool is named as Python names it; an empty
/// URL is answered before the headers are read, with no log line.
#[tokio::test]
async fn header_types_are_named_as_python_names_them() {
    for (headers, kind) in [
        (json!("NaN"), "float"),
        (json!(" -Infinity "), "float"),
        (json!("Infinity"), "float"),
        (json!("1.5"), "float"),
        (json!(1.5), "float"),
        (json!("true"), "bool"),
        (json!(false), "bool"),
        (json!("\"NaN\""), "str"),
    ] {
        let d = json!({"url": "http://api.example.com/", "headers": headers});
        let (out, c, http) = send(d.clone()).await;
        assert_eq!(
            out.failure_message(),
            format!("headers must be a JSON object, got {}", kind),
            "{}",
            d
        );
        assert_eq!(logs(&c)[0].0, "error");
        assert!(http.sent.lock().is_empty());
    }
    let (out, c, http) = send(json!({"url": "", "headers": "{bad"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "No URL specified"})
    );
    assert!(logs(&c).is_empty());
    assert!(http.sent.lock().is_empty());
}

/// N-httpRequest-01: the method is interpolated and upper-cased.
#[tokio::test]
async fn the_method_is_interpolated_and_upper_cased() {
    let http = stub(reply(200, "{}"));
    let c = ctx_with(http.clone());
    c.vars.set("verb", json!("post"));
    let d = json!({"url": "http://api.example.com/v1", "method": "{{verb}}", "body": "{\"a\": 1}"});
    run(&HttpRequest, &c, d).await;
    let sent = http.sent.lock().clone();
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].body.as_deref(), Some(&b"{\"a\":1}"[..]));
    assert_eq!(messages(&c)[0], "HTTP POST http://api.example.com/v1");
}

/// N-httpRequest-17: analyzer mode changes nothing: a run with analyzer
/// mode on sends the request on Flow's client.
#[tokio::test]
async fn analyzer_mode_still_sends_the_request() {
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-08-04T06:30:00Z".parse().unwrap(),
    );
    t.ctx.flow.triggers().stop().await;
    t.ctx
        .security
        .install(&crate::security::Unlocked::random().unwrap());
    t.ctx.sqlite.set_analyze_mode(true).unwrap();
    let http = stub(reply(200, "{\"ok\": true}"));
    let installed: Arc<dyn OutboundHttp> = http.clone();
    t.ctx.flow.install_env(move |env| env.http = installed);
    let nodes = json!([
        {"id": "s", "type": "start", "position": {"x": 0, "y": 0},
         "data": {"scheduleType": "daily", "time": "09:15"}},
        {"id": "h", "type": "httpRequest", "position": {"x": 0, "y": 100},
         "data": {"url": "http://api.example.com/v1", "method": "GET"}}
    ]);
    let edges = json!([{"id": "e", "source": "s", "target": "h"}]);
    let conn = t.ctx.sqlite.conn().unwrap();
    let new = crate::flow::store::workflows::NewWorkflow {
        name: "Analyzer",
        description: None,
        nodes: &nodes,
        edges: &edges,
    };
    let w =
        crate::flow::store::workflows::create(&conn, &t.ctx.security, &new, t.ctx.now()).unwrap();
    let req = crate::flow::executor::RunRequest {
        api_key: Some(crate::security::Secret::new("k")),
        ..Default::default()
    };
    let result = crate::flow::executor::execute(&t.ctx, w.id, req).await;
    assert_eq!(http.sent.lock().len(), 1, "{}", result);
    assert_eq!(result["status"], "success", "{}", result);
}
