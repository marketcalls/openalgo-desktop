//! What a run carries for the web tools: the "where to look for company
//! figures" section (web 46af9591f's `_web_section`), only on a run offered
//! the web tools.

use super::*;

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
