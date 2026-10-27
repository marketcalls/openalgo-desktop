//! A fake model provider for tests: a local HTTP server speaking the
//! OpenAI chat-completions stream (and the Responses stream), so a test
//! drives genai and the whole run end to end with no network.
//!
//! Each request takes the next scripted [`Reply`] and is recorded, so a
//! test can read what the run sent (the system prompt, the history, the
//! tools, the tool results).

use crate::agent::contract::model::{
    ModelCapabilities, ModelError, ModelResolver, NoCredentials, Prices, Pricing, ResolvedModel,
};
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One scripted answer.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Prose in these chunks, then usage.
    Text(Vec<&'static str>),
    /// Tool calls: `(call id, tool name, arguments as JSON text)`, each
    /// streamed as an announcement then its arguments in two halves.
    Calls(Vec<(&'static str, &'static str, String)>),
    /// A provider error: status and body.
    Fail(u16, &'static str),
    /// A raw SSE body (the Responses stream).
    Raw(String),
    /// Hold the stream open, sending nothing, until the test ends.
    Hang,
}

pub struct FakeProvider {
    pub port: u16,
    script: Arc<Mutex<VecDeque<Reply>>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeProvider {
    pub async fn start(script: Vec<Reply>) -> FakeProvider {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (s, r) = (script.clone(), requests.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let (s, r) = (s.clone(), r.clone());
                tokio::spawn(async move { serve(sock, s, r).await });
            }
        });
        FakeProvider {
            port,
            script,
            requests,
            task,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/v1/", self.port)
    }

    pub fn push(&self, reply: Reply) {
        self.script.lock().push_back(reply);
    }

    /// The JSON bodies the run sent, in order.
    pub fn sent(&self) -> Vec<Value> {
        self.requests.lock().clone()
    }
}

async fn serve(
    mut sock: tokio::net::TcpStream,
    script: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Value>>>,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    let body_start = loop {
        let Ok(n) = sock.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..body_start]).to_lowercase();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < body_start + length {
        let Ok(n) = sock.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value = serde_json::from_slice(&buf[body_start..]).unwrap_or(Value::Null);
    requests.lock().push(body);
    let reply = script
        .lock()
        .pop_front()
        .unwrap_or(Reply::Text(vec!["(no script)"]));
    let (status, payload) = match reply {
        Reply::Fail(status, body) => (status, body.to_string()),
        Reply::Hang => {
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            return;
        }
        Reply::Raw(sse) => (200, sse),
        other => (200, chat_completions(&other)),
    };
    let kind = if status == 200 {
        "text/event-stream"
    } else {
        "application/json"
    };
    let head = format!(
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        status,
        kind,
        payload.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(payload.as_bytes()).await;
    let _ = sock.shutdown().await;
}

fn chunk(delta: Value, finish: Option<&str>) -> String {
    let v = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "gpt-fake",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
    });
    format!("data: {}\n\n", v)
}

fn chat_completions(reply: &Reply) -> String {
    let mut out = String::new();
    let finish = match reply {
        Reply::Text(parts) => {
            for p in parts {
                out.push_str(&chunk(json!({"role": "assistant", "content": p}), None));
            }
            "stop"
        }
        Reply::Calls(calls) => {
            for (i, (id, name, args)) in calls.iter().enumerate() {
                out.push_str(&chunk(
                    json!({"tool_calls": [{"index": i, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]}),
                    None,
                ));
                let mid = args.len() / 2;
                for half in [&args[..mid], &args[mid..]] {
                    out.push_str(&chunk(
                        json!({"tool_calls": [{"index": i, "function": {"arguments": half}}]}),
                        None,
                    ));
                }
            }
            "tool_calls"
        }
        _ => "stop",
    };
    out.push_str(&chunk(json!({}), Some(finish)));
    let usage = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "gpt-fake",
        "choices": [],
        "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110,
                  "prompt_tokens_details": {"cached_tokens": 40}}
    });
    out.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", usage));
    out
}

/// A model on the fake provider, priced per token.
pub fn fake_model(endpoint: &str, adapter: genai::adapter::AdapterKind) -> ResolvedModel {
    ResolvedModel {
        id: 7,
        provider_kind: "openai".into(),
        model_name: "gpt-fake".into(),
        display_name: "Fake GPT".into(),
        catalog_id: "openai/gpt-fake".into(),
        adapter,
        endpoint: endpoint.into(),
        credentials: Arc::new(NoCredentials),
        capabilities: ModelCapabilities::default(),
        context_window: Some(128_000),
        pricing: Pricing::Metered(Some(Prices {
            input: Some(0.000_001),
            output: Some(0.000_002),
            cache_read: Some(0.000_000_5),
        })),
    }
}

/// A resolver that always answers the fake model.
pub struct FakeResolver(pub ResolvedModel);

#[async_trait]
impl ModelResolver for FakeResolver {
    fn is_configured(&self, _app: &AppState) -> bool {
        true
    }

    async fn resolve(
        &self,
        _app: &Arc<AppState>,
        requested: Option<i64>,
    ) -> Result<ResolvedModel, ModelError> {
        match requested {
            Some(id) if id != self.0.id => Err(ModelError::NotFound(format!(
                "Model {} is not registered. Choose one of the configured models.",
                id
            ))),
            _ => Ok(self.0.clone()),
        }
    }
}
