//! `web_search` for company figures on the trader's preferred sources (the
//! web's `test_agent_webfetch.py`, "Preferred sources for fundamentals"):
//! best first, the first source with a page ends the search, a source
//! whose page could not be read is dropped for the run, and the open web
//! only when every source has failed or has nothing.
//!
//! The web's tests mark a source failed in the toolkit
//! (`_failed_sources`); here a source fails the way it does in a run, by a
//! page of it that could not be read.

use super::fetch::{kit, links, page, usage, OPERATOR, SCREENER};
use super::tool::{configure, read};
use super::*;
use crate::agent::contract::tool::ToolOutcome;
use crate::agent::outbound::fake::Answer;
use serde_json::{json, Value};

const SCANX: &str = "https://scanx.trade/company/reliance-industries-ltd";
const TICKERTAPE: &str = "https://www.tickertape.in/stocks/reliance-industries-RELI";

fn sources(t: &crate::state::testing::TestCtx, list: &[&str]) {
    configure(t, json!({ "preferred_sources": list }));
}

/// The `site:` term of each search that went to Firecrawl, in order; an
/// open-web search reads as `open`.
fn sites(fake: &FakeServer) -> Vec<String> {
    fake.seen_at(FIRECRAWL_SEARCH)
        .iter()
        .map(|s| {
            let q = s.json()["query"].as_str().unwrap_or_default().to_string();
            q.rsplit_once("site:")
                .map(|(_, d)| d.to_string())
                .unwrap_or_else(|| "open".into())
        })
        .collect()
}

fn fundamentals() -> Value {
    json!({"query": "reliance fundamentals", "topic": "fundamentals"})
}

/// Web `test_a_fundamentals_search_stops_at_the_first_source_that_has_the_company`:
/// an ordinary question is one paid call, not one per source.
#[tokio::test]
async fn a_fundamentals_search_stops_at_the_first_source_that_has_the_company() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[SCREENER])]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    let (open, body) = read(&k.search(fundamentals()).await);
    assert_eq!(sites(&fake), ["screener.in"]);
    assert_eq!(
        open,
        "<web_result provider=\"firecrawl\" trust=\"third-party\" kind=\"links\">"
    );
    assert_eq!(body["results"][0]["preferred_source"], "screener.in");
    assert_eq!(body["results"][0]["url"], SCREENER);
    assert_eq!(body["topic"], "fundamentals");
    assert_eq!(body["preferred_sources_searched"], json!(["screener.in"]));
    assert_eq!(
        body["preferred_sources_still_to_try"],
        json!(["scanx.trade", "tickertape.in"])
    );
    assert_eq!(usage(&t), 1);
    let last = body["notices"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last,
        "These are pages from screener.in, the best preferred source that has the company. Open the best one with web_fetch. If it cannot be read, call web_search again with topic=\"fundamentals\": screener.in is then dropped and scanx.trade or tickertape.in is searched next."
    );
}

/// Web `test_the_next_source_is_searched_only_when_the_one_above_has_nothing`.
#[tokio::test]
async fn the_next_source_is_searched_only_when_the_one_above_has_nothing() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[]), links(&[SCANX])]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    let (_, body) = read(&k.search(fundamentals()).await);
    assert_eq!(sites(&fake), ["screener.in", "scanx.trade"]);
    assert_eq!(body["results"][0]["preferred_source"], "scanx.trade");
    assert_eq!(usage(&t), 2);
}

/// Web `test_the_site_term_is_added_by_the_code_not_taken_from_the_model`.
#[tokio::test]
async fn the_site_term_is_added_by_the_code_not_taken_from_the_model() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[SCREENER])]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    k.search(
        json!({"query": "reliance fundamentals site:attacker.example", "topic": "fundamentals"}),
    )
    .await;
    let sent: Vec<String> = fake
        .seen_at(FIRECRAWL_SEARCH)
        .iter()
        .map(|s| s.json()["query"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(sent, ["reliance fundamentals site:screener.in"]);
}

/// Web `test_a_result_off_the_preferred_domain_is_dropped_even_if_the_provider_sent_it`.
#[tokio::test]
async fn a_result_off_the_preferred_domain_is_dropped_even_if_the_provider_sent_it() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    sources(&t, &["screener.in"]);
    fake.script(
        FIRECRAWL_SEARCH,
        vec![links(&["https://evil.example/screener.in/x", SCREENER])],
    );
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    let (_, body) = read(&k.search(fundamentals()).await);
    let urls: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["url"].as_str().unwrap())
        .collect();
    assert_eq!(urls, [SCREENER]);
}

/// Web `test_when_no_preferred_source_has_a_page_the_open_web_is_searched_and_says_so`.
#[tokio::test]
async fn when_no_preferred_source_has_a_page_the_open_web_is_searched_and_says_so() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    sources(&t, &["screener.in", "scanx.trade"]);
    fake.script(
        FIRECRAWL_SEARCH,
        vec![
            links(&[]),
            links(&[]),
            links(&["https://example.org/reliance"]),
        ],
    );
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    let (_, body) = read(&k.search(fundamentals()).await);
    assert_eq!(sites(&fake), ["screener.in", "scanx.trade", "open"]);
    assert_eq!(body["results"][0]["url"], "https://example.org/reliance");
    assert!(body["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("open web was searched")));
}

/// Web `test_a_general_search_never_touches_the_preferred_sources`.
#[tokio::test]
async fn a_general_search_never_touches_the_preferred_sources() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[])]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    k.search(json!({"query": "reliance fundamentals"})).await;
    assert_eq!(sites(&fake), ["open"]);
}

/// Web `test_an_unknown_topic_is_a_correction_for_the_model`.
#[tokio::test]
async fn an_unknown_topic_is_a_correction_for_the_model() {
    let t = ctx();
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    let o = k
        .search(json!({"query": "reliance", "topic": "gossip"}))
        .await;
    assert_eq!(
        o,
        ToolOutcome::Retry("The 'topic' argument is invalid: 'gossip' is not a topic. Pass 'general', or 'fundamentals' for a company's figures.".into())
    );
}

/// Web `test_when_scanx_cannot_be_read_screener_carries_on_alone`.
#[tokio::test]
async fn when_scanx_cannot_be_read_screener_carries_on_alone() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    sources(&t, &["screener.in", "scanx.trade"]);
    fake.script(
        FIRECRAWL_SCRAPE,
        vec![
            Answer::json(
                200,
                json!({"data": {"markdown": "x", "metadata": {"statusCode": 403}}}),
            ),
            page("screener figures"),
        ],
    );
    let k = kit(
        &t,
        &format!("compare {} with {}", SCANX, SCREENER),
        &["firecrawl"],
        10,
    );
    let (_, failed) = read(&k.fetch(json!({ "url": SCANX })).await);
    let (_, worked) = read(&k.fetch(json!({ "url": SCREENER })).await);
    assert_eq!(failed["preferred_source_failed"], "scanx.trade");
    assert!(failed["next_step"]
        .as_str()
        .unwrap()
        .contains("move to screener.in"));
    assert_eq!(
        (worked["ok"].clone(), worked["text"].clone()),
        (json!(true), json!("screener figures"))
    );
}

/// Web `test_a_source_that_failed_is_not_searched_again_this_turn`: here
/// the source that failed is the first, so the search starts at the next.
#[tokio::test]
async fn a_source_that_failed_is_not_searched_again_this_turn() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    sources(&t, &["screener.in", "scanx.trade"]);
    fake.script(FIRECRAWL_SCRAPE, vec![Answer::json(500, json!({}))]);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[SCANX])]);
    let k = kit(
        &t,
        &format!("reliance fundamentals from {} please", SCREENER),
        &["firecrawl"],
        10,
    );
    k.fetch(json!({ "url": SCREENER })).await;
    let (_, body) = read(&k.search(fundamentals()).await);
    assert_eq!(sites(&fake), ["scanx.trade"]);
    assert_eq!(body["preferred_sources_searched"], json!(["scanx.trade"]));
}

/// Web `test_three_sources_failing_one_after_another_end_on_the_open_web`
/// and `test_when_every_preferred_source_has_failed_the_next_step_is_the_open_web`.
#[tokio::test]
async fn three_sources_failing_one_after_another_end_on_the_open_web() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SCRAPE, vec![Answer::json(500, json!({}))]);
    fake.script(
        FIRECRAWL_SEARCH,
        vec![links(&["https://example.org/reliance"])],
    );
    let k = kit(
        &t,
        &format!("compare {} {} {}", SCREENER, SCANX, TICKERTAPE),
        &["firecrawl"],
        20,
    );
    let mut steps = Vec::new();
    for url in [SCREENER, SCANX, TICKERTAPE] {
        let (_, failed) = read(&k.fetch(json!({ "url": url })).await);
        steps.push(failed["next_step"].as_str().unwrap().to_string());
    }
    assert!(steps[0]
        .starts_with("screener.in could not be read, so move to scanx.trade or tickertape.in."));
    assert!(steps[1].starts_with("scanx.trade could not be read, so move to tickertape.in."));
    assert!(
        steps[2].contains("topic=\"general\""),
        "the last source failing points at the open web"
    );
    let (_, body) = read(&k.search(fundamentals()).await);
    assert_eq!(body["results"][0]["url"], "https://example.org/reliance");
    assert_eq!(sites(&fake), ["open"], "no preferred source is left to ask");
}

/// Web `test_an_empty_preference_means_the_open_web_straight_away`.
#[tokio::test]
async fn an_empty_preference_means_the_open_web_straight_away() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    sources(&t, &[]);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[])]);
    let k = kit(&t, OPERATOR, &["firecrawl"], 10);
    k.search(fundamentals()).await;
    assert_eq!(sites(&fake), ["open"]);
}
