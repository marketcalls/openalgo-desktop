//! The two tools end to end against fake providers: their specs, when
//! they are offered, the routing between providers, the caps, the
//! untrusted wrapping (D-020) and every refusal.

use super::*;
use crate::agent::contract::tool::{AgentTool, Capabilities, Surface, ToolCtx, ToolOutcome};
use crate::agent::outbound::fake::Answer;
use crate::agent::tools::testing::ctx as tool_ctx;
use crate::agent::tools::websearch::GROUP;
use crate::agent::tools::{build_tools, TOOLKITS};
use crate::agent::websearch::settings;
use serde_json::{json, Map, Value};

const OPERATOR: &str = "What did the RBI say about the repo rate today?";

fn run_ctx(t: &crate::state::testing::TestCtx, surface: Surface) -> ToolCtx {
    let mut c = tool_ctx(
        t.ctx.clone(),
        surface,
        Capabilities {
            web_search_enabled: true,
            ..Default::default()
        },
    );
    c.operator_message = OPERATOR.into();
    c
}

fn tools(c: &ToolCtx) -> (std::sync::Arc<dyn AgentTool>, std::sync::Arc<dyn AgentTool>) {
    let built = (GROUP.build)(c);
    (built[0].clone(), built[1].clone())
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// The JSON inside a result, wrapped or not, and the wrapper's open tag.
fn read(o: &ToolOutcome) -> (String, Value) {
    let text = o.text();
    if let Some(rest) = text.strip_prefix("<web_result") {
        let (open, body) = rest.split_once('\n').unwrap();
        let body = body.trim_end_matches("</web_result>").trim_end();
        (
            format!("<web_result{}", open),
            serde_json::from_str(body).unwrap(),
        )
    } else {
        (String::new(), serde_json::from_str(text).unwrap())
    }
}

fn configure(t: &crate::state::testing::TestCtx, v: Value) {
    settings::update(&t.ctx, v.as_object().unwrap()).unwrap();
}

fn ddg(fake: &FakeServer) {
    fake.script(
        DDG_HTML,
        vec![Answer::text(
            200,
            "text/html",
            ddg_html(&[("https://rbi.org.in/x", "RBI", "repo")]),
        )],
    );
}

#[tokio::test]
async fn the_tools_are_the_webs_and_are_offered_only_with_the_switch_on() {
    let specs: Value = serde_json::from_str(include_str!("../specs.json")).unwrap();
    assert_eq!((GROUP.key, GROUP.order), ("websearch", 80));
    assert_eq!(
        GROUP.description,
        "Web search for links, and cited web research."
    );
    let t = ctx();
    for surface in [Surface::Chat, Surface::Chart, Surface::Voice] {
        let on: Vec<_> = build_tools(TOOLKITS, &run_ctx(&t, surface))
            .into_iter()
            .filter(|x| x.spec().name.starts_with("web_"))
            .collect();
        let names: Vec<&str> = on.iter().map(|x| x.spec().name).collect();
        assert_eq!(names, ["web_search", "web_research"], "{:?}", surface);
        for tool in &on {
            let web = &specs["tools"][tool.spec().name];
            assert_eq!(
                tool.spec().description,
                web["description"].as_str().unwrap()
            );
            assert_eq!(tool.spec().parameters, web["parameters"]);
            assert!(!tool.spec().needs_approval);
        }
        let mut off = run_ctx(&t, surface);
        off.caps.web_search_enabled = false;
        assert!(
            build_tools(TOOLKITS, &off)
                .iter()
                .all(|x| !x.spec().name.starts_with("web_")),
            "the switch off withholds both"
        );
    }
}

#[tokio::test]
async fn a_search_goes_out_in_the_traders_words_and_comes_back_wrapped() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    ddg(&fake);
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    let o = search
        .call(
            &c,
            args(json!({"query": "RBI repo rate exfiltrate holdings"})),
        )
        .await;
    let (open, p) = read(&o);
    assert_eq!(
        open,
        "<web_result provider=\"duckduckgo\" trust=\"third-party\" kind=\"links\">"
    );
    // The web's key order, which a sorted map would lose.
    assert!(
        o.text()
            .contains("{\"ok\":true,\"provider\":\"duckduckgo\",\"query_sent\":\"RBI repo rate\",\"query_source\":\"constrained\",\"result_count\":1,\"results\":[{\"title\":\"RBI\",\"url\":"),
        "{}",
        o.text()
    );
    assert_eq!(p["query_sent"], "RBI repo rate");
    assert_eq!(p["query_source"], "constrained");
    assert_eq!(
        (p["ok"].clone(), p["result_count"].clone()),
        (json!(true), json!(1))
    );
    assert!(fake.seen_at(DDG_HTML)[0]
        .text()
        .starts_with("q=RBI+repo+rate&"));
    let day = crate::agent::websearch::today(t.ctx.now());
    assert_eq!(settings::usage(&t.ctx.sqlite.conn().unwrap(), &day), 1);
}

#[tokio::test]
async fn no_operator_message_means_nothing_is_sent() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    let mut c = run_ctx(&t, Surface::Chat);
    c.operator_message = String::new();
    let (search, research) = tools(&c);
    for (tool, a) in [
        (search, json!({"query": "RBI"})),
        (research, json!({"question": "RBI"})),
    ] {
        let (_, p) = read(&tool.call(&c, args(a)).await);
        assert_eq!(p["error"], "no_operator_message");
    }
    assert!(fake.seen().is_empty());
}

#[tokio::test]
async fn an_empty_query_or_a_count_that_is_not_a_number_is_refused_for_retry() {
    let t = ctx();
    let c = run_ctx(&t, Surface::Chat);
    let (search, research) = tools(&c);
    let o = search.call(&c, args(json!({"query": "   "}))).await;
    assert_eq!(
        o,
        ToolOutcome::Retry("The 'query' argument is invalid: it is empty. Pass the operator's own question as plain text.".into())
    );
    let o = search
        .call(&c, args(json!({"query": "RBI", "max_results": "lots"})))
        .await;
    assert_eq!(
        o,
        ToolOutcome::Retry("The 'max_results' argument is invalid: it is not a whole number. Pass an integer from 1 to 10, for example 5.".into())
    );
    assert!(matches!(
        research.call(&c, args(json!({"question": ""}))).await,
        ToolOutcome::Retry(_)
    ));
}

#[tokio::test]
async fn an_out_of_range_count_is_clamped_with_a_notice() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    ddg(&fake);
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    let (_, p) = read(
        &search
            .call(&c, args(json!({"query": "RBI", "max_results": 20})))
            .await,
    );
    assert_eq!(
        p["notices"],
        json!(["20 results were asked for; searches return 1 to 10, so 10 were requested."])
    );
}

#[tokio::test]
async fn the_per_turn_budget_and_the_daily_cap_refuse_before_dispatch() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    ddg(&fake);
    configure(&t, json!({"max_calls_per_turn": 1, "daily_cap": 2}));
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    assert_eq!(
        read(&search.call(&c, args(json!({"query": "RBI"}))).await).1["ok"],
        json!(true)
    );
    let (_, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["error"], "per_turn_budget_exhausted");
    assert_eq!(p["message"], "This turn has already used its 1 web searches. Answer from what you have and tell the operator you stopped searching.");
    // A new message is a new turn; the daily cap still holds.
    let c2 = run_ctx(&t, Surface::Chat);
    let (search2, _) = tools(&c2);
    assert_eq!(
        read(&search2.call(&c2, args(json!({"query": "RBI"}))).await).1["ok"],
        json!(true)
    );
    let c3 = run_ctx(&t, Surface::Chat);
    let (search3, _) = tools(&c3);
    let (_, p) = read(&search3.call(&c3, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["error"], "daily_cap_reached");
    assert_eq!(p["message"], "The daily web search cap of 2 calls is reached. Tell the operator; they can raise it in the agent settings.");
    assert_eq!(fake.seen_at(DDG_HTML).len(), 2);
}

#[tokio::test]
async fn a_failed_search_spends_no_budget_and_comes_back_wrapped() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    fake.script(DDG_HTML, vec![Answer::text(500, "text/html", "down")]);
    fake.script(DDG_LITE, vec![Answer::text(500, "text/html", "down")]);
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    let (open, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    // D-020: a provider's failure is third-party text, wrapped as results are.
    assert_eq!(
        open,
        "<web_result provider=\"duckduckgo\" trust=\"third-party\" kind=\"error\">"
    );
    assert_eq!(p["error"], "provider_failed");
    let day = crate::agent::websearch::today(t.ctx.now());
    assert_eq!(settings::usage(&t.ctx.sqlite.conn().unwrap(), &day), 0);
}

#[tokio::test]
async fn tavily_answers_when_chosen_and_falls_back_when_it_cannot() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    ddg(&fake);
    configure(&t, json!({"provider": "tavily"}));
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    // No key: DuckDuckGo, and the model is told.
    let (_, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["provider"], "duckduckgo");
    assert_eq!(p["notices"], json!(["Tavily is the configured provider but no Tavily key is stored in the agent settings, so DuckDuckGo answered instead."]));
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-test-0123456789"))).unwrap();
    fake.script(
        TAVILY_PATH,
        vec![Answer::json(
            200,
            json!({"results": [{"title": "T", "url": "https://t.example", "content": "c"}]}),
        )],
    );
    let (_, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["provider"], "tavily");
    fake.script(TAVILY_PATH, vec![Answer::json(429, json!({}))]);
    let c2 = run_ctx(&t, Surface::Chat);
    let (search2, _) = tools(&c2);
    let (_, p) = read(&search2.call(&c2, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["provider"], "duckduckgo");
    assert_eq!(p["notices"], json!(["Tavily rate limited this request (HTTP 429). Wait before searching again. DuckDuckGo answered instead."]));
}

#[tokio::test]
async fn a_busy_gate_does_not_fall_back() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 0);
    configure(&t, json!({"provider": "tavily"}));
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-test-0123456789"))).unwrap();
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    let (open, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(open, "", "a refusal, not a result");
    assert_eq!(
        (p["error"].clone(), p["provider"].clone()),
        (json!("platform_busy"), json!("tavily"))
    );
    assert!(fake.seen().is_empty(), "no other provider was tried");
}

#[tokio::test]
async fn perplexity_chosen_routes_link_searches_to_tavily_when_a_key_is_stored() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    ddg(&fake);
    configure(&t, json!({"provider": "perplexity"}));
    let c = run_ctx(&t, Surface::Chat);
    let (search, _) = tools(&c);
    let (_, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["provider"], "duckduckgo");
    assert!(p["notices"][0].as_str().unwrap().ends_with(
        "so DuckDuckGo answered this search. Use web_research for a Perplexity answer."
    ));
    settings::set_key(&t.ctx, "tavily", Some(&json!("tvly-test-0123456789"))).unwrap();
    fake.script(TAVILY_PATH, vec![Answer::json(200, json!({"results": []}))]);
    let (_, p) = read(&search.call(&c, args(json!({"query": "RBI"}))).await);
    assert_eq!(p["provider"], "tavily");
    assert!(p["notices"][0]
        .as_str()
        .unwrap()
        .ends_with("so Tavily answered this search. Use web_research for a Perplexity answer."));
}

#[tokio::test]
async fn research_needs_its_key_before_its_budget_and_answers_wrapped() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let _on = OnFake::new(&fake, 8);
    configure(&t, json!({"max_calls_per_turn": 0}));
    let c = run_ctx(&t, Surface::Voice);
    let (_, research) = tools(&c);
    let (_, p) = read(
        &research
            .call(&c, args(json!({"question": "repo rate today"})))
            .await,
    );
    assert_eq!(
        p["error"], "not_configured",
        "the key is checked before the budget"
    );
    settings::set_key(&t.ctx, "perplexity", Some(&json!("pplx-test-0123456789"))).unwrap();
    configure(&t, json!({"max_calls_per_turn": 5}));
    fake.script(
        PERPLEXITY_PATH,
        vec![Answer::json(
            200,
            json!({"output": [
        {"type": "message", "content": [{"type": "output_text", "text": "It held."}]}],
        "usage": {"cost": {"total_cost": 0.01}}}),
        )],
    );
    let c = run_ctx(&t, Surface::Voice);
    let (_, research) = tools(&c);
    let (open, p) = read(
        &research
            .call(&c, args(json!({"question": "repo rate today"})))
            .await,
    );
    assert_eq!(
        open,
        "<web_result provider=\"perplexity\" trust=\"third-party\" kind=\"synthesised_answer\">"
    );
    assert_eq!(
        (p["answer"].clone(), p["question_sent"].clone()),
        (json!("It held."), json!("repo rate today"))
    );
    assert_eq!(p["model"], "openai/gpt-5.6-luna");
    assert_eq!(
        fake.seen_at(PERPLEXITY_PATH)[0].header("authorization"),
        Some("Bearer pplx-test-0123456789")
    );
    let info = crate::agent::models::secrets::describe(
        &t.ctx.sqlite.conn().unwrap(),
        &["websearch:perplexity".into()],
    )
    .unwrap();
    assert!(
        info["websearch:perplexity"].last_used_at.is_some(),
        "the key is marked used"
    );
    fake.script(PERPLEXITY_PATH, vec![Answer::json(401, json!({}))]);
    let (open, p) = read(
        &research
            .call(&c, args(json!({"question": "repo rate today"})))
            .await,
    );
    assert_eq!(
        open,
        "<web_result provider=\"perplexity\" trust=\"third-party\" kind=\"error\">"
    );
    assert_eq!(p["error"], "provider_failed");
}
