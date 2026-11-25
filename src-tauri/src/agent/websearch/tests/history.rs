//! A follow-up keeps its company (web 5e19ce407, `test_agent_webfetch.py`
//! "Earlier messages in the same conversation"): the search query and the
//! page check take words from the trader's earlier messages too, never
//! from a reply, and a run with no message of its own takes none.

use super::fetch::{kit_with_history, links, page, SCREENER};
use super::tool::read;
use super::*;
use serde_json::json;

const FOLLOW_UP: &str = "and get the marketcap of it";
const EARLIER: &str = "can you get the live quotes of reliance";

/// The search query keeps the company named earlier (desktop: in the
/// trader's form, whole words only).
#[tokio::test]
async fn a_follow_up_search_keeps_its_company() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SEARCH, vec![links(&[SCREENER])]);
    let k = kit_with_history(&t, FOLLOW_UP, EARLIER, &["firecrawl"], 10);
    let (_, body) = read(&k.search(json!({"query": "Reliance market cap"})).await);
    assert_eq!(body["query_sent"], "reliance");
    assert_eq!(body["query_source"], "constrained");
}

/// Web `test_the_screener_page_of_a_company_named_earlier_is_opened_directly`.
#[tokio::test]
async fn the_screener_page_of_a_company_named_earlier_is_opened_directly() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SCRAPE, vec![page("ok")]);
    let k = kit_with_history(&t, FOLLOW_UP, EARLIER, &["firecrawl"], 10);
    let (_, body) = read(&k.fetch(json!({ "url": SCREENER })).await);
    assert_eq!(
        (body["ok"].clone(), body["url_source"].clone()),
        (json!(true), json!("preferred"))
    );
}

/// Web `test_a_symbol_named_by_nobody_is_still_refused_with_history`.
#[tokio::test]
async fn a_symbol_named_by_nobody_is_still_refused_with_history() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    let k = kit_with_history(&t, FOLLOW_UP, EARLIER, &["firecrawl"], 10);
    let (_, body) = read(
        &k.fetch(json!({"url": "https://www.screener.in/company/INFY/consolidated/"}))
            .await,
    );
    assert_eq!(body["error"], "url_not_allowed");
    assert!(fake.seen().is_empty());
}

/// Web `test_a_url_typed_in_an_earlier_message_is_accepted_and_sent_as_typed`.
#[tokio::test]
async fn a_url_typed_in_an_earlier_message_is_accepted_and_sent_as_typed() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(FIRECRAWL_SCRAPE, vec![page("ok")]);
    let k = kit_with_history(
        &t,
        "read it again",
        "look at https://Scanx.Trade/Company/Reliance-Industries-Ltd",
        &["firecrawl"],
        10,
    );
    k.fetch(json!({"url": "https://scanx.trade/company/reliance-industries-ltd"}))
        .await;
    assert_eq!(
        fake.seen_at(FIRECRAWL_SCRAPE)[0].json()["url"],
        "https://Scanx.Trade/Company/Reliance-Industries-Ltd"
    );
}

/// Web `test_a_run_with_no_message_of_its_own_fetches_nothing_the_history_named`.
#[tokio::test]
async fn a_run_with_no_message_of_its_own_fetches_nothing_the_history_named() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    let k = kit_with_history(&t, "", EARLIER, &["firecrawl"], 10);
    let (_, body) = read(&k.fetch(json!({ "url": SCREENER })).await);
    assert_eq!(body["error"], "url_not_allowed");
    assert!(fake.seen().is_empty());
}
