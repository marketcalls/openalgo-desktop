//! A failed link search falls through the other keyed providers before
//! DuckDuckGo (web 5e19ce407, `test_agent_webfetch.py`): each request that
//! went out and failed uses the turn's allowance, the daily cap counts only
//! answers, and a busy gate falls through to nothing.

use super::fetch::{kit, usage, OPERATOR, SCREENER};
use super::tool::{configure, ddg, read};
use super::*;
use crate::agent::outbound::fake::Answer;
use serde_json::json;

fn query() -> serde_json::Value {
    json!({"query": "reliance screener"})
}

/// Web `test_a_failed_search_uses_the_turn_but_not_the_daily_cap`:
/// Firecrawl and DuckDuckGo both went out and failed.
#[tokio::test]
async fn a_failed_search_uses_the_turn_but_not_the_daily_cap() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![Answer::json(503, json!({}))]);
    fake.script(DDG_HTML, vec![Answer::text(500, "text/html", "down")]);
    fake.script(DDG_LITE, vec![Answer::text(500, "text/html", "down")]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 2);
    let (_, body) = read(&k.search(query()).await);
    assert_eq!(body["error"], "provider_failed");
    assert_eq!(usage(&t), 0);
    let (_, next) = read(&k.search(query()).await);
    assert_eq!(next["error"], "per_turn_budget_exhausted");
}

/// Web `test_a_failed_search_that_a_fallback_answered_counts_both_requests_but_one_success`.
#[tokio::test]
async fn a_failed_search_that_a_fallback_answered_counts_both_requests_but_one_success() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![Answer::json(503, json!({}))]);
    ddg(&fake);
    let k = kit(&t, OPERATOR, &["firecrawl"], 2);
    let (_, body) = read(&k.search(query()).await);
    assert_eq!(body["provider"], "duckduckgo");
    assert_eq!(usage(&t), 1);
    let (_, next) = read(&k.search(query()).await);
    assert_eq!(next["error"], "per_turn_budget_exhausted");
}

/// Web `test_a_rate_limited_firecrawl_is_answered_by_tinyfish_not_duckduckgo`:
/// the real run, where DuckDuckGo took 22 seconds while a working TinyFish
/// key sat unused.
#[tokio::test]
async fn a_rate_limited_firecrawl_is_answered_by_tinyfish_not_duckduckgo() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![Answer::json(429, json!({}))]);
    fake.script(
        TINYFISH_SEARCH,
        vec![Answer::json(
            200,
            json!({"results": [{"url": SCREENER, "title": "t", "snippet": ""}]}),
        )],
    );
    let k = kit(&t, OPERATOR, &["firecrawl", "tinyfish"], 10);
    let (_, body) = read(&k.search(query()).await);
    assert_eq!(body["provider"], "tinyfish");
    assert!(fake.seen_at(DDG_HTML).is_empty());
    assert_eq!(
        body["notices"],
        json!(["Firecrawl rate limited this request (HTTP 429). Wait before trying again. TinyFish was tried instead."])
    );
    assert_eq!(usage(&t), 1);
}

/// Web `test_duckduckgo_answers_only_after_every_keyed_provider_has_failed`:
/// the configured one first, then the others in their order.
#[tokio::test]
async fn duckduckgo_answers_only_after_every_keyed_provider_has_failed() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    for path in [TAVILY_PATH, FIRECRAWL_SEARCH, TINYFISH_SEARCH] {
        fake.script(path, vec![Answer::json(503, json!({}))]);
    }
    ddg(&fake);
    let k = kit(&t, OPERATOR, &["firecrawl", "tinyfish", "tavily"], 10);
    let (_, body) = read(&k.search(query()).await);
    let order: Vec<String> = fake.seen().iter().map(|s| s.path.clone()).collect();
    assert_eq!(
        order,
        [FIRECRAWL_SEARCH, TAVILY_PATH, TINYFISH_SEARCH, DDG_HTML],
        "the configured one first"
    );
    assert_eq!(body["provider"], "duckduckgo");
    assert_eq!(
        body["notices"],
        json!([
            "Firecrawl is having an outage (HTTP 503). Tavily was tried instead.",
            "Tavily is having an outage (HTTP 503). TinyFish was tried instead.",
            "TinyFish is having an outage (HTTP 503). DuckDuckGo answered instead."
        ])
    );
}

/// Web `test_a_configured_provider_with_no_key_hands_over_to_one_that_has_a_key`.
#[tokio::test]
async fn a_configured_provider_with_no_key_hands_over_to_one_that_has_a_key() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(
        TINYFISH_SEARCH,
        vec![Answer::json(200, json!({"results": []}))],
    );
    let k = kit(&t, OPERATOR, &["tinyfish"], 10);
    let (_, body) = read(&k.search(query()).await);
    assert_eq!(body["provider"], "tinyfish");
    assert_eq!(
        body["notices"],
        json!(["Firecrawl is the configured provider but no Firecrawl key is stored in the agent settings, so TinyFish answered instead."])
    );
}

/// Web `test_a_busy_hub_still_does_not_fall_through_to_another_provider`.
#[tokio::test]
async fn a_busy_gate_still_does_not_fall_through_to_another_provider() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 0);
    let k = kit(&t, OPERATOR, &["firecrawl", "tinyfish"], 10);
    let (_, body) = read(&k.search(query()).await);
    assert_eq!(body["error"], "platform_busy");
    assert!(fake.seen().is_empty());
    assert_eq!(usage(&t), 0);
}

/// A fundamentals search falls through the same way for each source.
#[tokio::test]
async fn a_fundamentals_search_falls_through_the_keyed_providers_too() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    configure(&t, json!({"preferred_sources": ["screener.in"]}));
    fake.script(FIRECRAWL_SEARCH, vec![Answer::json(429, json!({}))]);
    fake.script(
        TINYFISH_SEARCH,
        vec![Answer::json(
            200,
            json!({"results": [{"url": SCREENER, "title": "t", "snippet": ""}]}),
        )],
    );
    let k = kit(&t, OPERATOR, &["firecrawl", "tinyfish"], 10);
    let (_, body) = read(
        &k.search(json!({"query": "reliance fundamentals", "topic": "fundamentals"}))
            .await,
    );
    assert_eq!(body["provider"], "tinyfish");
    assert_eq!(body["results"][0]["preferred_source"], "screener.in");
}
