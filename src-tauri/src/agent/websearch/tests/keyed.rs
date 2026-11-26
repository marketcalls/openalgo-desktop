//! Firecrawl and TinyFish against a local fake (the web's
//! `test_agent_webfetch.py`, "What each provider returns"): what is sent,
//! how each answer is read, and how a failure reads.

use super::*;
use crate::agent::outbound::fake::Answer;
use crate::agent::websearch::keyed::{
    firecrawl_fetch, firecrawl_search, tinyfish_fetch, tinyfish_search,
};
use crate::agent::websearch::providers::MAX_ANSWER_BYTES;
use crate::security::Secret;
use serde_json::json;

const SCREENER: &str = "https://www.screener.in/company/RELIANCE/consolidated/";

fn fc() -> Secret {
    Secret::new("fc-test-0123456789")
}

fn tf() -> Secret {
    Secret::new("tf-test-0123456789")
}

/// Web `test_firecrawl_search_reads_the_v2_shape`.
#[tokio::test]
async fn firecrawl_search_reads_the_v2_shape() {
    let fake = FakeServer::start().await;
    fake.script(
        FIRECRAWL_SEARCH,
        vec![Answer::json(
            200,
            json!({"success": true, "data": {"web": [
                {"url": SCREENER, "title": "Reliance", "description": "Financials"}]}}),
        )],
    );
    let o = firecrawl_search(&net_on(&fake, 8), "reliance screener", 3, &fc()).await;
    assert!(o.ok && o.key_used);
    assert_eq!(o.results[0].url, SCREENER);
    assert_eq!(o.results[0].snippet, "Financials");
    let sent = &fake.seen_at(FIRECRAWL_SEARCH)[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer fc-test-0123456789")
    );
    assert_eq!(
        sent.json(),
        json!({"query": "reliance screener", "limit": 3, "sources": ["web"]})
    );
}

/// An older Firecrawl answer is the list itself.
#[tokio::test]
async fn firecrawl_search_reads_the_older_shape_too() {
    let fake = FakeServer::start().await;
    fake.script(
        FIRECRAWL_SEARCH,
        vec![Answer::json(
            200,
            json!({"data": [{"url": SCREENER, "title": "R", "snippet": "old"}, {"title": "no url"}]}),
        )],
    );
    let o = firecrawl_search(&net_on(&fake, 8), "q", 5, &fc()).await;
    assert_eq!(o.results.len(), 1);
    assert_eq!(o.results[0].snippet, "old");
}

/// Web `test_tinyfish_search_is_a_get_with_the_key_header`.
#[tokio::test]
async fn tinyfish_search_is_a_get_with_the_key_header() {
    let fake = FakeServer::start().await;
    fake.script(
        TINYFISH_SEARCH,
        vec![Answer::json(
            200,
            json!({"results": [
                {"url": SCREENER, "title": "Reliance", "snippet": "Financials"},
                {"url": "https://example.com/b", "title": "B", "snippet": "b"}]}),
        )],
    );
    let o = tinyfish_search(&net_on(&fake, 8), "reliance screener", 1, &tf()).await;
    assert!(o.ok);
    assert_eq!(
        o.results.len(),
        1,
        "TinyFish takes no count, so it is trimmed here"
    );
    assert_eq!(o.results[0].snippet, "Financials");
    let sent = &fake.seen_at(TINYFISH_SEARCH)[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(sent.header("x-api-key"), Some("tf-test-0123456789"));
    assert_eq!(sent.header("authorization"), None);
    assert_eq!(sent.query, "query=reliance+screener");
}

/// Web `test_firecrawl_fetch_returns_markdown_and_a_title`.
#[tokio::test]
async fn firecrawl_fetch_returns_markdown_and_a_title() {
    let fake = FakeServer::start().await;
    fake.script(
        FIRECRAWL_SCRAPE,
        vec![Answer::json(
            200,
            json!({"success": true, "data": {
                "markdown": "# Reliance\n| Market Cap | 19 Lakh Cr |",
                "metadata": {"title": ["Reliance Industries", "x"], "statusCode": 200,
                             "url": "https://www.screener.in/company/RELIANCE/"}}}),
        )],
    );
    let o = firecrawl_fetch(&net_on(&fake, 8), SCREENER, &fc()).await;
    assert!(o.ok && o.text.contains("Market Cap"));
    assert_eq!(o.title, "Reliance Industries");
    assert_eq!(o.final_url, "https://www.screener.in/company/RELIANCE/");
    assert_eq!(
        fake.seen_at(FIRECRAWL_SCRAPE)[0].json(),
        json!({"url": SCREENER, "formats": ["markdown"], "onlyMainContent": true, "timeout": 45000})
    );
}

/// Web `test_firecrawl_reports_a_page_that_answered_with_an_error`.
#[tokio::test]
async fn firecrawl_reports_a_page_that_answered_with_an_error() {
    let fake = FakeServer::start().await;
    fake.script(
        FIRECRAWL_SCRAPE,
        vec![Answer::json(
            200,
            json!({"success": true, "data": {"markdown": "Not found", "metadata": {"statusCode": 404}}}),
        )],
    );
    let o = firecrawl_fetch(&net_on(&fake, 8), SCREENER, &fc()).await;
    assert!(!o.ok && o.error.contains("404"));
    assert!(o.key_used, "the provider answered, so its key was used");
}

/// Web `test_tinyfish_fetch_reads_a_result`: a fetched page hands back no
/// links.
#[tokio::test]
async fn tinyfish_fetch_reads_a_result() {
    let fake = FakeServer::start().await;
    fake.script(
        TINYFISH_FETCH,
        vec![Answer::json(
            200,
            json!({"results": [{"url": SCREENER, "title": "Reliance", "text": "# Reliance\nPE 24"}],
                   "errors": []}),
        )],
    );
    let o = tinyfish_fetch(&net_on(&fake, 8), SCREENER, &tf()).await;
    assert!(o.ok && o.text.ends_with("PE 24"));
    let sent = fake.seen_at(TINYFISH_FETCH)[0].json();
    assert_eq!(sent["urls"], json!([SCREENER]));
    assert_eq!(sent["links"], json!(false));
    assert_eq!(
        (sent["image_links"].clone(), sent["iframe_links"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(
        fake.seen_at(TINYFISH_FETCH)[0].header("x-api-key"),
        Some("tf-test-0123456789")
    );
}

/// Web `test_tinyfish_reports_a_per_page_failure_that_arrives_inside_a_200`.
#[tokio::test]
async fn tinyfish_reports_a_per_page_failure_that_arrives_inside_a_200() {
    let fake = FakeServer::start().await;
    fake.script(
        TINYFISH_FETCH,
        vec![Answer::json(
            200,
            json!({"results": [], "errors": [{"url": SCREENER, "error": "bot_blocked", "status": 403}]}),
        )],
    );
    let o = tinyfish_fetch(&net_on(&fake, 8), SCREENER, &tf()).await;
    assert!(!o.ok && o.error.contains("bot_blocked"));
}

/// Web `test_a_rejected_key_names_the_provider_properly`: TinyFish, not
/// Tinyfish.
#[tokio::test]
async fn a_rejected_key_names_the_provider_properly() {
    let fake = FakeServer::start().await;
    fake.script(TINYFISH_SEARCH, vec![Answer::json(401, json!({}))]);
    let o = tinyfish_search(&net_on(&fake, 8), "anything", 3, &tf()).await;
    assert!(!o.ok && !o.key_used);
    assert!(o.error.starts_with("TinyFish rejected the stored key"));
}

/// Desktop: an answer larger than the app reads is refused, never held.
#[tokio::test]
async fn an_answer_too_large_to_read_is_refused() {
    let fake = FakeServer::start().await;
    let huge = "x".repeat(MAX_ANSWER_BYTES + 1);
    fake.script(
        FIRECRAWL_SCRAPE,
        vec![Answer::text(
            200,
            "application/json",
            format!("\"{}\"", huge),
        )],
    );
    let o = firecrawl_fetch(&net_on(&fake, 8), SCREENER, &fc()).await;
    assert!(!o.ok);
    assert_eq!(o.error, "Firecrawl returned an answer too large to read.");
}

/// A full gate answers busy for a page read too, and asks nobody.
#[tokio::test]
async fn a_full_gate_reads_no_page() {
    let fake = FakeServer::start().await;
    let o = tinyfish_fetch(&net_on(&fake, 0), SCREENER, &tf()).await;
    assert!(o.busy && !o.ok);
    assert!(o.error.starts_with("Web fetch could not run"));
    assert!(fake.seen().is_empty());
}
