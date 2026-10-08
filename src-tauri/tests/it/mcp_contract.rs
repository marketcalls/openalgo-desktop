//! The tool registry is pinned to the web's MCP server: every tool's name,
//! description, input schema, annotations and scope must equal the capture
//! in `tests/fixtures/mcp/tools_list.json` (taken from the web's
//! `mcp/mcpserver.py` and `utils/mcp_tool_registry.py`). Any drift fails.

use crate::mcp_support::M;
use openalgo_desktop_lib::mcp::{self, catalog::TOOLS, store::TokenScope};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/mcp/tools_list.json")).unwrap()
}

#[test]
fn every_web_tool_is_registered_with_the_same_descriptor() {
    let f = fixture();
    let web = f["tools"].as_array().unwrap();
    assert_eq!(web.len(), 49);
    let web_names: BTreeSet<&str> = web.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let ours: BTreeSet<&str> = TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(web_names, ours, "tool set drifted");
    for w in web {
        let name = w["name"].as_str().unwrap();
        let d = mcp::tool(name).unwrap().descriptor();
        assert_eq!(
            d["description"], w["description"],
            "{}: description drifted",
            name
        );
        assert_eq!(
            d["inputSchema"], w["inputSchema"],
            "{}: input schema drifted",
            name
        );
        assert_eq!(
            d["annotations"], w["annotations"],
            "{}: annotations drifted",
            name
        );
        assert_eq!(
            json!(mcp::tool(name).unwrap().scope.as_str()),
            f["scopes"][name],
            "{}: scope drifted",
            name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_over_http_is_the_fixture_filtered_by_scope() {
    let m = M::new().await;
    let rw = m.token(TokenScope::ReadWrite);
    let ro = m.token(TokenScope::Read);
    let f = fixture();
    let listed = m.rpc(&rw, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 49);
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "web lists tools sorted by name");
    for t in tools {
        let w = f["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["name"] == t["name"])
            .unwrap();
        assert_eq!(t, w);
    }
    let read = m.rpc(&ro, "tools/list", json!({})).await;
    let read_names: BTreeSet<String> = read["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let want: BTreeSet<String> = f["scopes"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, s)| *s != "write:orders")
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(read_names, want);
    m.h.shutdown().await;
}
