//! The web's `test/test_agent_openui_tool.py`, ported (Q-625..Q-643).

use super::*;
use crate::agent::contract::tool::{Capabilities, Surface};
use crate::agent::tools::testing::ctx;
use crate::agent::tools::{select, spec_contract, TOOLKITS};
use serde_json::json;

/// The web's GOOD markup.
const GOOD: &str = "root = Card([title])\ntitle = TextContent(\"Funds\", \"large-heavy\")";

fn rendered(markup: &str) -> (ToolOutcome, Vec<Frame>) {
    let sink = VizSink::new();
    let outcome = render(&sink, Some(&json!(markup)));
    (outcome, sink.drain())
}

fn app() -> Arc<crate::state::AppState> {
    crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now()).ctx
}

/// Q-626, Q-627, Q-628: registered under `openui`, offered where there is a
/// screen (chat and voice) and withheld from the chart panel, with no
/// capability needed.
#[tokio::test]
async fn it_is_registered_for_chat_and_voice_only() {
    assert!(TOOLKITS.iter().any(|g| g.key == "openui"));
    assert_eq!(GROUP.surfaces, Surfaces::CHAT_AND_VOICE);
    assert!(GROUP.requires.is_empty());
    let app = app();
    let off = Capabilities::default();
    for (surface, offered) in [
        (Surface::Chat, true),
        (Surface::Voice, true),
        (Surface::Chart, false),
    ] {
        let keys: Vec<&str> = select(TOOLKITS, &ctx(app.clone(), surface, off))
            .iter()
            .map(|g| g.key)
            .collect();
        assert_eq!(keys.contains(&"openui"), offered, "{:?}", surface);
    }
}

/// Q-629 and the spec contract: one tool, `render_ui`, exactly as the web
/// describes it.
#[tokio::test]
async fn the_built_toolkit_is_render_ui_as_the_web_describes_it() {
    let chat = ctx(app(), Surface::Chat, Capabilities::default());
    let tools = (GROUP.build)(&chat);
    let names: Vec<&str> = tools.iter().map(|t| t.spec().name).collect();
    assert_eq!(names, ["render_ui"]);
    spec_contract::assert_toolkit(&GROUP, &chat);
}

/// Q-638: valid markup reaches the client as one `ui` frame.
#[test]
fn valid_markup_reaches_the_client_as_a_ui_frame() {
    let (outcome, frames) = rendered(GOOD);
    assert!(outcome.is_ok(), "{:?}", outcome);
    assert!(outcome.text().contains("Rendered the card"));
    assert!(outcome.text().starts_with("<tool_result"));
    assert_eq!(frames, vec![Frame::Ui { delta: GOOD.into() }]);
    assert_eq!(frames[0].wire_type(), "ui");
}

/// Q-639: the markup is not read back to the model.
#[test]
fn the_markup_is_not_read_back_to_the_model() {
    let long = "x".repeat(4000);
    let markup = format!("root = Card([t])\nt = TextContent(\"{}\")", long);
    let (outcome, frames) = rendered(&markup);
    assert!(!outcome.text().contains(&long));
    assert!(outcome.text().len() < 400);
    let Frame::Ui { delta } = &frames[0] else {
        panic!("{:?}", frames)
    };
    assert!(delta.len() > 4000);
}

/// Q-640: a code fence around the program is unwrapped.
#[test]
fn a_code_fence_around_the_program_is_unwrapped() {
    let (_, frames) = rendered(&format!("```openui\n{}\n```", GOOD));
    assert_eq!(frames, vec![Frame::Ui { delta: GOOD.into() }]);
}

/// Q-641: refused markup is a correction naming `markup`, and nothing
/// reaches the sink. The URL cases are the exfiltration guard.
#[test]
fn markup_it_refuses() {
    let over = format!(
        "root = Card([t])\nt = TextContent(\"{}\")",
        "x".repeat(30_000)
    );
    let cases: [(&str, &str); 7] = [
        ("", "it is empty"),
        ("   ", "it is empty"),
        ("```\n```", "it holds nothing but a code fence"),
        ("title = TextContent(\"Funds\")", "it binds no 'root'"),
        (
            "root = Card([m])\nm = MarkDownRenderer(\"[x](https://evil.test/?q=1)\")",
            "it carries a URL",
        ),
        (
            "root = Card([m])\nm = MarkDownRenderer(\"![x](//evil.test/?q=1)\")",
            "it carries a URL",
        ),
        (&over, "more than the 20000 one rendering may carry"),
    ];
    for (markup, reason) in cases {
        let (outcome, frames) = rendered(markup);
        let ToolOutcome::Retry(text) = &outcome else {
            panic!("{:?} was not refused: {:?}", markup.get(..40), outcome)
        };
        assert!(
            text.starts_with("The 'markup' argument is invalid: "),
            "{}",
            text
        );
        assert!(text.contains(reason), "{}", text);
        assert!(frames.is_empty());
    }
    let sink = VizSink::new();
    assert!(!render(&sink, None).is_ok());
    assert!(!render(&sink, Some(&json!(42))).is_ok());
    assert!(sink.drain().is_empty());
}

/// Q-642: prose with a double slash is not an address.
#[test]
fn ordinary_prose_with_a_double_slash_is_not_mistaken_for_a_url() {
    for text in ["Win rate 50//50 this week", "margin // collateral split"] {
        let markup = format!("root = Card([t])\nt = TextContent(\"{}\")", text);
        let (outcome, frames) = rendered(&markup);
        assert!(outcome.is_ok(), "{}: {:?}", text, outcome);
        assert_eq!(frames.len(), 1);
    }
}

/// Q-643: a run with no sink says so rather than claiming it drew.
#[test]
fn a_run_with_no_sink_says_so_rather_than_claiming_it_drew() {
    let outcome = render(&VizSink::none(), Some(&json!(GOOD)));
    assert!(outcome.is_ok());
    assert!(outcome
        .text()
        .contains("This surface cannot render UI, so nothing was drawn. Answer in markdown"));
    assert!(!outcome.text().contains("Rendered the card"));
}

/// The tool reads the run's own sink.
#[tokio::test]
async fn a_call_leaves_the_frame_on_the_runs_sink() {
    let run = ctx(app(), Surface::Voice, Capabilities::default());
    let tools = (GROUP.build)(&run);
    let mut args = Map::new();
    args.insert("markup".into(), json!(GOOD));
    let outcome = tools[0].call(&run, args).await;
    assert!(outcome.is_ok());
    assert_eq!(run.viz.drain(), vec![Frame::Ui { delta: GOOD.into() }]);
}

/// Q-110: a spoken turn draws on screen like a typed one.
#[tokio::test]
async fn the_voice_surface_can_draw() {
    let voice = ctx(app(), Surface::Voice, Capabilities::default());
    let keys: Vec<&str> = select(TOOLKITS, &voice).iter().map(|g| g.key).collect();
    for key in ["openui", "viz", "option_viz"] {
        assert!(keys.contains(&key), "{} not on voice: {:?}", key, keys);
    }
}

/// Q-630: every rendering tool the chat prompt names is a tool the chat
/// surface is given (trading off).
#[tokio::test]
async fn every_tool_the_prompt_names_is_a_tool_the_chat_surface_has() {
    use crate::agent::prompts::{build_system_prompt, SystemPrompt};
    let prompt = build_system_prompt(&SystemPrompt {
        surface: Some(Surface::Chat),
        ..Default::default()
    });
    let chat = ctx(app(), Surface::Chat, Capabilities::default());
    let offered: Vec<&str> = crate::agent::tools::build_tools(TOOLKITS, &chat)
        .iter()
        .map(|t| t.spec().name)
        .collect();
    for name in [
        "render_ui",
        "plot_price_chart",
        "plot_open_interest",
        "plot_gamma_exposure",
        "plot_volatility_surface",
    ] {
        assert!(
            prompt.contains(name),
            "the chat prompt does not name {}",
            name
        );
        assert!(offered.contains(&name), "{} is not offered on chat", name);
    }
}
