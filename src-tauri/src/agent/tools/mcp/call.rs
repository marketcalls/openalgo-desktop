//! `call_mcp_tool`: the arguments read and checked, built through the taint
//! boundary, the tool called (or a later page read from the kept answer),
//! and the page returned.

use super::{
    failure, paging, python_dumps_len, tool_failed, wrapped, Answer, McpTool, MAX_ARGUMENT_CHARS,
    TRUST,
};
use crate::agent::contract::json::strip_code_fence;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::mcp::client::{tool_text, McpClient};
use crate::agent::mcp::registry::McpServerSpec;
use crate::agent::mcp::taint::{construct, Vocabulary};
use crate::agent::tools::service::py_repr;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// The `tool` argument: one plausible name.
fn checked_tool(raw: Option<&Value>) -> Result<String, ToolOutcome> {
    let name = super::as_text(raw).trim().to_string();
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_whitespace) {
        return Err(ToolOutcome::invalid_argument(
            "tool",
            "it must be one tool's exact name.",
            Some("Call list_mcp_tools to see the names."),
        ));
    }
    Ok(name)
}

/// The `arguments`: an object, a JSON string of one, or nothing.
fn arguments(raw: Option<&Value>) -> Result<Map<String, Value>, ToolOutcome> {
    let value = match raw {
        None | Some(Value::Null) => return Ok(Map::new()),
        Some(Value::String(s)) => {
            let text = strip_code_fence(s);
            let text = text.trim();
            if text.is_empty() {
                return Ok(Map::new());
            }
            serde_json::from_str::<Value>(text).map_err(|_| {
                ToolOutcome::invalid_argument(
                    "arguments",
                    "it is not valid JSON.",
                    Some("Pass a JSON object, for example {\"symbol\": \"SBIN\"}."),
                )
            })?
        }
        Some(other) => other.clone(),
    };
    let Value::Object(args) = value else {
        return Err(ToolOutcome::invalid_argument(
            "arguments",
            "it must be a JSON object of argument names to values.",
            Some("For example {\"symbol\": \"SBIN\", \"months\": 3}, or omit it."),
        ));
    };
    if python_dumps_len(&Value::Object(args.clone())) > MAX_ARGUMENT_CHARS {
        return Err(ToolOutcome::invalid_argument(
            "arguments",
            &format!("they exceed {} characters.", MAX_ARGUMENT_CHARS),
            Some("Pass only the arguments the tool lists."),
        ));
    }
    Ok(args)
}

/// The `page`: a whole number from 1 (absent is 1).
fn page(raw: Option<&Value>) -> Result<usize, ToolOutcome> {
    let number = match raw {
        None => 1,
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(_) => 0,
    };
    if number < 1 {
        return Err(ToolOutcome::invalid_argument(
            "page",
            "it must be a whole number from 1.",
            Some("Pass 1 first."),
        ));
    }
    Ok(number as usize)
}

/// The kept answer's key: the server, the tool and the arguments sent.
fn answer_key(server: &str, tool: &str, args: &Map<String, Value>) -> String {
    let canonical = serde_json::to_string(args).unwrap_or_default();
    let digest = hex::encode(Sha256::digest(canonical.as_bytes()));
    format!("{}:{}:{}", server, tool, &digest[..24])
}

/// The arguments read and checked, in the web's order.
fn read(
    t: &McpTool,
    args: &Map<String, Value>,
) -> Result<(Arc<McpServerSpec>, String, Map<String, Value>, usize), ToolOutcome> {
    Ok((
        t.server(args.get("server"))?,
        checked_tool(args.get("tool"))?,
        arguments(args.get("arguments"))?,
        page(args.get("page"))?,
    ))
}

/// The taint boundary: what is sent is built from the trader's words and
/// the tool's own schema, never from what the model invented.
fn bounded(
    ctx: &ToolCtx,
    spec: &McpServerSpec,
    name: &str,
    schema: Option<&Value>,
    given: &Map<String, Value>,
) -> Result<Map<String, Value>, ToolOutcome> {
    let vocabulary = Vocabulary::new(&ctx.operator_message, schema);
    construct(given, &vocabulary).map_err(|refused| {
        tracing::info!("call_mcp_tool {}.{} refused at the taint boundary: {}", spec.key, name, refused.at);
        ToolOutcome::invalid_argument(
            "arguments",
            &format!(
                "{} holds words the trader did not write in this message ({}), and an external server is sent only the trader's own words or the values its tool lists.",
                refused.at,
                py_repr(&Value::String(refused.word))
            ),
            Some("Use the trader's words for it, or ask the trader."),
        )
    })
}

/// The answer's text: a later page from the kept answer when it is still
/// there, else a fresh call (kept for its later pages).
async fn fetch(
    ctx: &ToolCtx,
    client: &McpClient,
    spec: &McpServerSpec,
    name: &str,
    sent: &Map<String, Value>,
    page_number: usize,
) -> Result<(Arc<String>, u64), ToolOutcome> {
    let key = answer_key(&spec.key, name, sent);
    let hub = ctx.app.agent.mcp();
    if page_number > 1 {
        if let Some(text) = hub.answer(&key) {
            return Ok((text, 0));
        }
    }
    match client.call_tool(name, sent).await {
        Ok(result) => {
            let text = match (&result.structured, result.text.is_empty()) {
                (Some(s), true) => s.to_string(),
                _ => result.text,
            };
            let text = Arc::new(text);
            hub.keep_answer(key, text.clone());
            Ok((text, result.elapsed_ms))
        }
        Err(e) => Err(match tool_text(&e) {
            Some(server_text) => tool_failed(spec, name, &e, server_text),
            None => failure("call_mcp_tool", spec, &e, Some(name)),
        }),
    }
}

/// One page of the answer, labelled third-party.
fn respond(
    spec: &McpServerSpec,
    name: &str,
    text: &str,
    page_number: usize,
    elapsed_ms: u64,
) -> ToolOutcome {
    let (pages, truncated) = paging::pages(text);
    let Some(current) = pages.get(page_number - 1).cloned() else {
        return ToolOutcome::invalid_argument(
            "page",
            &format!("this answer has {} page(s).", pages.len()),
            Some(&format!("Pass a page from 1 to {}.", pages.len())),
        );
    };
    tracing::info!(
        "call_mcp_tool {}.{} page {}/{} chars={} elapsed_ms={}",
        spec.key,
        name,
        page_number,
        pages.len(),
        text.chars().count(),
        elapsed_ms
    );
    let payload = Answer {
        ok: true,
        server: &spec.key,
        tool: name,
        page: page_number,
        pages: pages.len(),
        rows: current.rows,
        content: current.content,
        note: (pages.len() > 1).then(|| {
            format!(
                "Long answer: this is page {} of {}. Call again with the same arguments and page set to read more, only if the question needs it.",
                page_number,
                pages.len()
            )
        }),
        truncated: truncated.then_some(true),
    };
    wrapped(
        "call_mcp_tool",
        &payload,
        &[
            ("server", Some(&spec.key)),
            ("mcp_tool", Some(name)),
            ("trust", Some(TRUST)),
        ],
    )
}

pub(super) async fn call(t: &McpTool, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
    let (spec, name, given, page_number) = match read(t, args) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let client = match t.client(ctx, &spec, Some(&name)) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let listed = match client.list_tools(false).await {
        Ok(l) => l,
        Err(e) => return failure("call_mcp_tool", &spec, &e, Some(&name)),
    };
    let Some(found) = listed
        .iter()
        .find(|x| x.get("name").and_then(Value::as_str) == Some(name.as_str()))
    else {
        let mut names: Vec<&str> = listed
            .iter()
            .filter_map(|x| x.get("name").and_then(Value::as_str))
            .collect();
        names.sort_unstable();
        return ToolOutcome::invalid_argument(
            "tool",
            &format!(
                "{} offers no tool named {}.",
                spec.key,
                py_repr(&Value::String(name.clone()))
            ),
            Some(&format!(
                "Pick one of: {}. Call list_mcp_tools for their arguments.",
                names.join(", ")
            )),
        );
    };
    let sent = match bounded(ctx, &spec, &name, found.get("inputSchema"), &given) {
        Ok(sent) => sent,
        Err(r) => return r,
    };
    match fetch(ctx, &client, &spec, &name, &sent, page_number).await {
        Ok((text, elapsed_ms)) => respond(&spec, &name, &text, page_number, elapsed_ms),
        Err(r) => r,
    }
}
