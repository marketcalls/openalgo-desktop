//! The guarded client against a local server on an ephemeral port: it
//! connects to the pinned address without looking the name up, follows no
//! redirect, stops at 5 MB and keeps its timeout.

use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A server answering by path; every request's head is recorded.
async fn server() -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut head = Vec::new();
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                log.lock().push(text.clone());
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let response: Vec<u8> = match path.as_str() {
                    "/redirect" => b"HTTP/1.1 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    "/big" => {
                        let body = vec![b'x'; MAX_RESPONSE_BYTES + 1024];
                        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
                        r.extend_from_slice(&body);
                        r
                    }
                    "/slow" => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()
                    }
                    _ => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
                };
                let _ = sock.write_all(&response).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen, task)
}

fn call(url: String, port: u16, timeout: Duration) -> HttpCall {
    HttpCall {
        method: "GET".into(),
        url,
        headers: vec![("X-Test".into(), "1".into())],
        body: None,
        timeout,
        connect_to: SocketAddr::from(([127, 0, 0, 1], port)),
    }
}

#[tokio::test]
async fn a_name_goes_to_its_pinned_address_without_a_lookup() {
    let (port, seen, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    // `.invalid` never resolves (RFC 2606): only the pin can answer it.
    let url = format!("http://flow-check.invalid:{}/ok", port);
    let reply = http
        .send(call(url, port, Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"hello");
    let head = seen.lock()[0].to_lowercase();
    assert!(
        head.contains(&format!("host: flow-check.invalid:{}", port)),
        "{}",
        head
    );
    assert!(head.contains("x-test: 1"));
    // The pin is gone once the request is done.
    assert!(http.pins.addresses("flow-check.invalid").is_empty());
    task.abort();
}

#[tokio::test]
async fn an_address_nobody_vetted_is_not_used() {
    let http = GuardedHttp::new().unwrap();
    // The URL names one address, the vetted one is another.
    let mut c = call("http://127.0.0.2:9/".into(), 9, Duration::from_secs(1));
    c.connect_to = SocketAddr::from(([93, 184, 216, 34], 80));
    let err = http.send(c).await.unwrap_err();
    assert!(err.contains("not checked"), "{}", err);
}

#[tokio::test]
async fn a_redirect_is_answered_not_followed() {
    let (port, seen, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    let url = format!("http://flow-check.invalid:{}/redirect", port);
    let reply = http
        .send(call(url, port, Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(reply.status, 302);
    assert_eq!(seen.lock().len(), 1);
    task.abort();
}

#[tokio::test]
async fn a_response_over_five_megabytes_is_refused() {
    let (port, _, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    let url = format!("http://flow-check.invalid:{}/big", port);
    let err = http
        .send(call(url, port, Duration::from_secs(30)))
        .await
        .unwrap_err();
    assert_eq!(err, TOO_LARGE);
    task.abort();
}

#[tokio::test]
async fn a_slow_answer_times_out() {
    let (port, _, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    let url = format!("http://flow-check.invalid:{}/slow", port);
    let err = http
        .send(call(url, port, Duration::from_millis(200)))
        .await
        .unwrap_err();
    assert_eq!(err, "timed out");
    task.abort();
}

#[tokio::test]
async fn literal_addresses_resolve_to_themselves() {
    let http = GuardedHttp::new().unwrap();
    assert_eq!(
        http.resolve("127.0.0.1").await,
        Ok(vec!["127.0.0.1".parse().unwrap()])
    );
    assert_eq!(http.resolve("::1").await, Ok(vec!["::1".parse().unwrap()]));
}

const PROXY_CHILD: &str = "FLOW_GUARDED_HTTP_PROXY_CHILD";

/// N-httpRequest-17, decisions.md 12: the system's proxy settings are not
/// honoured. A child process runs `proxy_child` with every proxy variable
/// pointing at a listener here; the request must reach its server
/// directly, and the listener must see no connection.
#[test]
fn system_proxy_settings_are_ignored() {
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let url = format!("http://{}", proxy.local_addr().unwrap());
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "flow::nodes::http::client::tests::proxy_child",
        "--exact",
        "--include-ignored",
        "--nocapture",
    ])
    .env(PROXY_CHILD, "1")
    .env("NO_PROXY", "")
    .env("no_proxy", "");
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env(var, &url);
    }
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}{}",
        text,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("1 passed"), "{}", text);
    assert!(
        matches!(proxy.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the request went through the system proxy"
    );
}

/// The child half of `system_proxy_settings_are_ignored`.
#[tokio::test]
#[ignore = "run by system_proxy_settings_are_ignored in a process with proxy variables set"]
async fn proxy_child() {
    if std::env::var(PROXY_CHILD).is_err() {
        return;
    }
    let (port, seen, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    let url = format!("http://flow-check.invalid:{}/ok", port);
    let reply = http
        .send(call(url, port, Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(seen.lock().len(), 1);
    task.abort();
}

/// decisions.md 32: an empty body (a node's JSON `null`) goes out as httpx
/// sends it: no bytes, no type, and its length stated once.
#[tokio::test]
async fn an_empty_body_states_its_length() {
    let (port, seen, task) = server().await;
    let http = GuardedHttp::new().unwrap();
    let url = format!("http://flow-check.invalid:{}/ok", port);
    let mut c = call(url, port, Duration::from_secs(5));
    c.method = "POST".into();
    c.body = Some(Vec::new());
    let reply = http.send(c).await.unwrap();
    assert_eq!(reply.status, 200);
    let head = seen.lock()[0].to_lowercase();
    assert_eq!(head.matches("content-length: 0\r\n").count(), 1, "{}", head);
    assert!(!head.contains("content-type"), "{}", head);
    task.abort();
}
