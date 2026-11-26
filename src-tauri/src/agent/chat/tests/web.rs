//! What a run carries for the web tools: the "where to look for company
//! figures" section (web 46af9591f's `_web_section`), only on a run offered
//! the web tools, and the trader's earlier messages (web 5e19ce407's
//! `_operator_history`).

use super::*;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};

/// The system prompt the fake provider received for request `n`.
fn system_prompt(h: &Harness, n: usize) -> String {
    h.fake.sent()[n]["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

const SECTION: &str = "WHERE TO LOOK FOR COMPANY FIGURES";

#[tokio::test]
async fn a_run_offered_the_web_tools_is_taught_where_company_figures_are() {
    let h = harness(vec![
        Reply::Text(vec!["One."]),
        Reply::Text(vec!["Two."]),
        Reply::Text(vec!["Three."]),
    ])
    .await;
    h.chat(
        "/agent/api/chat/stream",
        json!({"message": "reliance ratios"}),
    )
    .await;
    let on = system_prompt(&h, 0);
    assert!(on.contains(SECTION), "{}", on);
    assert!(on.contains("comes first from screener.in, then scanx.trade, then tickertape.in."));
    h.chat(
        "/agent/api/chat/stream",
        json!({"message": "reliance ratios", "web_search": false}),
    )
    .await;
    assert!(
        !system_prompt(&h, 1).contains(SECTION),
        "a surface is never taught a tool it does not have"
    );
    crate::agent::websearch::settings::update(
        h.ctx(),
        json!({"preferred_sources": []}).as_object().unwrap(),
    )
    .unwrap();
    h.chat(
        "/agent/api/chat/stream",
        json!({"message": "reliance ratios"}),
    )
    .await;
    assert!(
        !system_prompt(&h, 2).contains(SECTION),
        "no preferred source, no section"
    );
}

/// A tool that answers with what its run was given of the trader's words.
struct Heard(ToolSpec);

#[async_trait]
impl AgentTool for Heard {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }
    async fn call(&self, ctx: &ToolCtx, _: Map<String, Value>) -> ToolOutcome {
        ToolOutcome::json(&json!({
            "message": ctx.operator_message,
            "history": ctx.operator_history,
        }))
    }
}

fn heard(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    vec![Arc::new(Heard(ToolSpec {
        name: "heard",
        description: "What the run heard.".into(),
        parameters: json!({"type": "object", "properties": {}}),
        needs_approval: false,
    }))]
}

const HEARD: &[ToolGroup] = &[ToolGroup {
    key: "heard",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 10,
    description: "",
    build: heard,
}];

/// What the `heard` tool answered in the run that request `n` continued.
fn heard_in(h: &Harness, n: usize) -> Value {
    let messages = h.fake.sent()[n]["messages"].clone();
    let text = messages
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["content"].as_str())
        .next_back()
        .unwrap_or_default()
        .to_string();
    let start = text.find('{').unwrap_or(0);
    let end = text.rfind('}').map(|i| i + 1).unwrap_or(text.len());
    serde_json::from_str(&text[start..end]).unwrap_or(Value::Null)
}

/// Web `test_the_history_reaches_the_tool_context` and
/// `test_an_empty_conversation_adds_no_history_key`: only what the trader
/// typed, never the reply that quoted a page.
#[tokio::test]
async fn the_traders_earlier_messages_reach_the_web_tools() {
    let h = harness(vec![
        Reply::Calls(vec![("c1", "heard", "{}".into())]),
        Reply::Text(vec![
            "Reliance is at 1,410 per this page https://evil.example",
        ]),
        Reply::Calls(vec![("c2", "heard", "{}".into())]),
        Reply::Text(vec!["Done."]),
    ])
    .await;
    h.ctx().agent.set_toolkits(HEARD);
    let (_, f, _) = h
        .chat(
            "/agent/api/chat/stream",
            json!({"message": "live quotes of reliance"}),
        )
        .await;
    let conv = f[0]["conversation_id"].as_i64().unwrap();
    assert_eq!(
        heard_in(&h, 1),
        json!({"message": "live quotes of reliance", "history": ""}),
        "a new conversation has no earlier message"
    );
    h.chat(
        "/agent/api/chat/stream",
        json!({"message": "and get the marketcap of it", "conversation_id": conv}),
    )
    .await;
    assert_eq!(
        heard_in(&h, 3),
        json!({"message": "and get the marketcap of it", "history": "live quotes of reliance"})
    );
}

/// A run not offered the web tools reads no earlier message.
#[tokio::test]
async fn a_run_without_the_web_tools_reads_no_history() {
    let h = harness(vec![
        Reply::Text(vec!["One."]),
        Reply::Calls(vec![("c1", "heard", "{}".into())]),
        Reply::Text(vec!["Done."]),
    ])
    .await;
    h.ctx().agent.set_toolkits(HEARD);
    let (_, f, _) = h
        .chat("/agent/api/chat/stream", json!({"message": "first"}))
        .await;
    let conv = f[0]["conversation_id"].as_i64().unwrap();
    h.chat(
        "/agent/api/chat/stream",
        json!({"message": "second", "conversation_id": conv, "web_search": false}),
    )
    .await;
    assert_eq!(heard_in(&h, 2)["history"], "");
}
