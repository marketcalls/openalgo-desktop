//! A local HTTP server for tests of the Agent's outbound requests: each
//! path answers from its own script (the last answer repeats), and every
//! request is recorded with its method, path, headers, body and arrival
//! time, so a test reads exactly what was sent. Nothing leaves the machine.

use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One scripted answer.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// Wait this long before answering.
    pub delay: Duration,
}

impl Answer {
    pub fn json(status: u16, body: Value) -> Answer {
        Answer {
            status,
            content_type: "application/json",
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }

    pub fn text(status: u16, content_type: &'static str, body: impl Into<String>) -> Answer {
        Answer {
            status,
            content_type,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn after(mut self, delay: Duration) -> Answer {
        self.delay = delay;
        self
    }
}

/// One request as it arrived.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    /// Names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub at: Instant,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == &name.to_ascii_lowercase())
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

type Scripts = Arc<Mutex<HashMap<String, VecDeque<Answer>>>>;

pub struct FakeServer {
    pub port: u16,
    scripts: Scripts,
    seen: Arc<Mutex<Vec<Seen>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeServer {
    pub async fn start() -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let scripts: Scripts = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (s, r) = (scripts.clone(), seen.clone());
        let task = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let (s, r) = (s.clone(), r.clone());
                tokio::spawn(async move { serve(sock, s, r).await });
            }
        });
        FakeServer {
            port,
            scripts,
            seen,
            task,
        }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// What `path` answers, in order; the last one repeats.
    pub fn script(&self, path: &str, answers: Vec<Answer>) {
        self.scripts.lock().insert(path.to_string(), answers.into());
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().clone()
    }

    pub fn seen_at(&self, path: &str) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.path == path).collect()
    }
}

/// A local address nothing listens on.
pub async fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

async fn read_request(
    sock: &mut TcpStream,
) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    let start = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..start]).to_string();
    let mut lines = head.lines();
    let first: Vec<&str> = lines.next()?.split_whitespace().collect();
    let (method, path) = (first.first()?.to_string(), first.get(1)?.to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < start + length {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some((method, path, headers, buf[start..].to_vec()))
}

async fn serve(mut sock: TcpStream, scripts: Scripts, seen: Arc<Mutex<Vec<Seen>>>) {
    let Some((method, path, headers, body)) = read_request(&mut sock).await else {
        return;
    };
    let route = path.split('?').next().unwrap_or_default().to_string();
    seen.lock().push(Seen {
        method,
        path: route.clone(),
        headers,
        body,
        at: Instant::now(),
    });
    let answer = {
        let mut s = scripts.lock();
        match s.get_mut(&route) {
            Some(q) if q.len() > 1 => q.pop_front(),
            Some(q) => q.front().cloned(),
            None => None,
        }
    }
    .unwrap_or_else(|| Answer::text(404, "text/plain", "no script"));
    if !answer.delay.is_zero() {
        tokio::time::sleep(answer.delay).await;
    }
    let head = format!(
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        answer.status,
        answer.content_type,
        answer.body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(answer.body.as_bytes()).await;
    let _ = sock.shutdown().await;
}
