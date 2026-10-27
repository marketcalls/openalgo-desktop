//! Each provider against a local fake: what is sent, how the answer is
//! shaped, and how a failure reads.

use super::*;
use crate::agent::outbound::fake::{closed_port, Answer};
use crate::agent::websearch::duckduckgo::{self, parse_html, parse_lite, target_of};
use crate::agent::websearch::providers::{
    self, bounded, parse_perplexity, status_message, tavily_topic,
};
use crate::security::Secret;
use serde_json::{json, Value};

fn key() -> Secret {
    Secret::new("tvly-test-0123456789")
}

#[tokio::test]
async fn tavily_is_asked_as_the_web_asks_it_and_its_rows_are_shaped() {
    let fake = FakeServer::start().await;
    let long = "x".repeat(500);
    fake.script(
        TAVILY_PATH,
        vec![Answer::json(
            200,
            json!({"results": [
                {"title": "RBI policy", "url": "https://rbi.org.in/a", "content": long},
                {"title": "no url"},
                "not a row",
                {"title": "Second", "url": "https://example.com/b", "content": "  short  "},
                {"title": "Beyond the count", "url": "https://example.com/c", "content": "c"},
            ]}),
        )],
    );
    let net = net_on(&fake, 8);
    let o = providers::tavily(&net, "RBI repo rate", 4, &key()).await;
    assert!(o.ok && o.key_used);
    let sent = &fake.seen_at(TAVILY_PATH)[0];
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer tvly-test-0123456789")
    );
    assert_eq!(
        sent.json(),
        json!({"query": "RBI repo rate", "max_results": 4, "search_depth": "advanced", "topic": "finance"})
    );
    assert_eq!(
        o.results.len(),
        2,
        "rows without a URL, and past the count, are left out"
    );
    assert_eq!(
        o.results[0]["snippet"].as_str().unwrap().chars().count(),
        400
    );
    assert!(o.results[0]["snippet"].as_str().unwrap().ends_with("..."));
    assert_eq!(
        o.results[1],
        json!({"title": "Second", "url": "https://example.com/b", "snippet": "short"})
    );
}

#[test]
fn a_market_question_gets_the_finance_topic() {
    assert_eq!(tavily_topic("Nifty, today?"), "finance");
    assert_eq!(tavily_topic("who wrote Hamlet"), "general");
}

#[test]
fn a_status_reads_as_the_next_step() {
    assert_eq!(
        status_message("tavily", 401),
        "Tavily rejected the stored key (HTTP 401). Check the tavily key in the agent settings; it is wrong, expired or revoked."
    );
    assert_eq!(
        status_message("perplexity", 429),
        "Perplexity rate limited this request (HTTP 429). Wait before searching again."
    );
    assert_eq!(
        status_message("tavily", 503),
        "Tavily is having an outage (HTTP 503)."
    );
    assert_eq!(
        status_message("tavily", 400),
        "Tavily rejected the request (HTTP 400)."
    );
}

#[tokio::test]
async fn a_tavily_failure_is_a_sentence_never_a_body() {
    let fake = FakeServer::start().await;
    let net = net_on(&fake, 8);
    fake.script(
        TAVILY_PATH,
        vec![Answer::json(403, json!({"detail": "tvly-leaked-key"}))],
    );
    let o = providers::tavily(&net, "q", 3, &key()).await;
    assert!(!o.ok && !o.key_used);
    assert!(!o.error.contains("leaked"));
    fake.script(TAVILY_PATH, vec![Answer::text(200, "text/html", "<html>")]);
    assert_eq!(
        providers::tavily(&net, "q", 3, &key()).await.error,
        "Tavily returned a body that is not JSON."
    );
    let mut gone = net.clone();
    gone.tavily = format!("http://127.0.0.1:{}/x", closed_port().await);
    assert_eq!(
        providers::tavily(&gone, "q", 3, &key()).await.error,
        "Tavily could not be reached."
    );
}

#[tokio::test]
async fn a_full_gate_answers_busy_and_asks_no_provider() {
    let fake = FakeServer::start().await;
    let net = net_on(&fake, 0);
    let o = providers::tavily(&net, "q", 3, &key()).await;
    assert!(o.busy && !o.ok);
    assert_eq!(
        o.error,
        "Web search could not run because the web server was too busy to take it. Try the search again in a moment."
    );
    let r = providers::perplexity(&net, "q", "m", &key()).await;
    assert!(r.busy && r.error.starts_with("Web research could not run"));
    assert!(duckduckgo::search(&net, "q", 3).await.busy);
    assert!(fake.seen().is_empty());
}

fn perplexity_answer() -> Value {
    let results: Vec<Value> = (0..25)
        .map(|i| json!({"id": format!("s{}", i), "url": format!("https://src.example/{}", i % 22), "title": format!("Source {}", i)}))
        .collect();
    json!({
        "output": [
            {"type": "search_results", "results": results},
            {"type": "reasoning", "summary": "ignored"},
            {"type": "message", "content": [
                {"type": "output_text", "text": " The rupee fell. "},
                {"type": "refusal", "text": "ignored"},
                {"type": "output_text", "text": "Oil rose."},
            ]},
        ],
        "usage": {"cost": {"total_cost": 0.0123}},
    })
}

#[tokio::test]
async fn perplexity_is_asked_for_a_cited_answer_and_it_is_read_back() {
    let fake = FakeServer::start().await;
    fake.script(
        PERPLEXITY_PATH,
        vec![Answer::json(200, perplexity_answer())],
    );
    let o = providers::perplexity(
        &net_on(&fake, 8),
        "why did the rupee fall",
        "openai/gpt-5.6-luna",
        &key(),
    )
    .await;
    assert!(o.ok && o.key_used);
    assert_eq!(o.answer, "The rupee fell.\n\nOil rose.");
    assert_eq!(o.citations.len(), 20, "at most 20");
    assert_eq!(
        o.citations[0],
        json!({"id": "s0", "url": "https://src.example/0", "title": "Source 0"})
    );
    assert_eq!(o.cost_usd, Some(0.0123));
    let sent = fake.seen_at(PERPLEXITY_PATH)[0].json();
    assert_eq!(
        sent,
        json!({"model": "openai/gpt-5.6-luna", "input": "why did the rupee fall",
               "tools": [{"type": "web_search", "search_context_size": "medium", "max_results": 10}],
               "instructions": providers::PERPLEXITY_INSTRUCTIONS, "max_output_tokens": 1500})
    );
}

#[test]
fn an_empty_perplexity_answer_and_duplicate_sources_are_handled() {
    let (answer, citations) = parse_perplexity(&json!({"output": [
        {"type": "search_results", "results": [{"url": "https://a"}, {"url": "https://a"}, {"title": "no url"}]},
    ]}));
    assert_eq!((answer.as_str(), citations.len()), ("", 1));
    assert_eq!(
        parse_perplexity(&json!("nonsense")),
        (String::new(), vec![])
    );
    assert_eq!(bounded(Some(&json!("  abcdefgh ")), 5), "ab...");
    assert_eq!(bounded(Some(&json!(42)), 5), "42");
}

#[tokio::test]
async fn nothing_from_perplexity_is_a_failure_that_still_reports_its_cost() {
    let fake = FakeServer::start().await;
    fake.script(
        PERPLEXITY_PATH,
        vec![Answer::json(
            200,
            json!({"output": [], "usage": {"cost": {"total_cost": "0.5"}}}),
        )],
    );
    let o = providers::perplexity(&net_on(&fake, 8), "q", "m", &key()).await;
    assert!(!o.ok);
    assert_eq!(o.error, "Perplexity returned no answer and no citations.");
    assert_eq!(o.cost_usd, Some(0.5));
}

#[test]
fn duckduckgo_links_are_unwrapped_and_advertisements_left_out() {
    assert_eq!(
        target_of(
            "//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rbi.org.in%2Fpolicy%3Fa%3D1&amp;rut=abc"
        )
        .as_deref(),
        Some("https://www.rbi.org.in/policy?a=1")
    );
    assert_eq!(
        target_of("https://example.com/x").as_deref(),
        Some("https://example.com/x")
    );
    assert_eq!(target_of("https://duckduckgo.com/y.js?ad_domain=x"), None);
    assert_eq!(target_of("/relative"), None);
}

#[test]
fn the_html_page_is_read_as_title_url_and_snippet() {
    let page = ddg_html(&[
        (
            "//duckduckgo.com/l/?uddg=https%3A%2F%2Frbi.org.in%2F&amp;rut=1",
            "RBI <b>repo</b> rate",
            "The &quot;repo&quot; rate &amp; more",
        ),
        ("https://duckduckgo.com/y.js?ad=1", "An advert", "buy"),
        ("https://example.com/two", "Two", "Second"),
    ]);
    let rows = parse_html(&page);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        json!({"title": "RBI repo rate", "url": "https://rbi.org.in/", "snippet": "The \"repo\" rate & more"})
    );
    assert_eq!(rows[1]["url"], "https://example.com/two");
}

#[test]
fn the_lite_page_is_read_too() {
    let page = "<table><tr><td>1.&nbsp;</td><td><a rel=\"nofollow\" href=\"https://example.com/a\" class='result-link'>First &amp; best</a></td></tr>\
                <tr><td></td><td class='result-snippet'>Snippet <b>one</b></td></tr>\
                <tr><td>2.&nbsp;</td><td><a rel=\"nofollow\" href=\"https://example.com/b\" class='result-link'>Second</a></td></tr></table>";
    let rows = parse_lite(page);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        json!({"title": "First & best", "url": "https://example.com/a", "snippet": "Snippet one"})
    );
    assert_eq!(rows[1]["snippet"], "");
}

#[tokio::test]
async fn duckduckgo_is_posted_as_its_form_and_falls_back_to_the_lite_page() {
    let fake = FakeServer::start().await;
    fake.script(DDG_HTML, vec![Answer::text(202, "text/html", "anomaly")]);
    fake.script(
        DDG_LITE,
        vec![Answer::text(
            200,
            "text/html",
            "<a href=\"https://example.com/a\" class=\"result-link\">A</a>",
        )],
    );
    let o = duckduckgo::search(&net_on(&fake, 8), "RBI repo rate", 5).await;
    assert!(o.ok);
    assert_eq!(o.results.len(), 1);
    let html = &fake.seen_at(DDG_HTML)[0];
    assert_eq!(
        html.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert!(
        html.text().starts_with("q=RBI+repo+rate"),
        "{}",
        html.text()
    );
    assert!(html
        .header("user-agent")
        .unwrap()
        .starts_with("Mozilla/5.0"));
    fake.script(DDG_LITE, vec![Answer::text(500, "text/html", "down")]);
    let o = duckduckgo::search(&net_on(&fake, 8), "q", 5).await;
    assert!(!o.ok);
    assert_eq!(o.error, duckduckgo::FAILED);
}

#[tokio::test]
async fn duckduckgo_keeps_the_count_asked_for() {
    let fake = FakeServer::start().await;
    let rows: Vec<(String, String, String)> = (0..8)
        .map(|i| {
            (
                format!("https://e.example/{}", i),
                format!("T{}", i),
                format!("S{}", i),
            )
        })
        .collect();
    let refs: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    fake.script(
        DDG_HTML,
        vec![Answer::text(200, "text/html", ddg_html(&refs))],
    );
    let o = duckduckgo::search(&net_on(&fake, 8), "q", 3).await;
    assert_eq!(o.results.len(), 3);
    assert!(fake.seen_at(DDG_LITE).is_empty(), "the html page answered");
}
