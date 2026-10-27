//! The rendering tool: cards, tables and general charts the model composes
//! (the web's `services/agent/tools/openui.py`).
//!
//! This is the one renderer whose numbers the model types itself. The chart
//! tools fetch their own data, which is what makes a price chart
//! trustworthy; here the model writes the markup, so the provenance rule
//! lives in the prompt rather than in the plumbing, and this tier is for
//! general data, never for prices, candles, open interest or Greeks.
//!
//! The markup is OpenUI Lang. The model is taught it by the OPENUI LANG
//! section, generated from `src/lib/agent/openuiLibrary.ts`, the library the
//! browser renders with ([`crate::agent::prompts`]).
//!
//! What this file refuses, each a correction the model can act on:
//!
//! - **No `root` binding.** Markup without `root = Card(...)` parses and
//!   renders nothing at all, silently.
//! - **A URL.** `MarkDownRenderer` renders markdown, where a link or an image
//!   is a fetch to an address the model chose, and this tool's input is
//!   exposed to the injection that would aim one.
//! - **More than [`MAX_MARKUP_CHARS`].** The markup crosses the wire and is
//!   stored on the message row.
//!
//! The markup does not go back to the model: it goes on the run's sink and
//! reaches the browser as a `ui` frame, and the model gets one line.

use super::specs;
use crate::agent::contract::frames::Frame;
use crate::agent::contract::json::strip_code_fence;
use crate::agent::contract::tool::Surfaces;
use crate::agent::contract::tool::{AgentTool, ToolCtx, ToolGroup, ToolOutcome, ToolSpec};
use crate::agent::contract::viz::VizSink;
use crate::agent::prompts::untrusted::wrap_tool_result;
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};

/// Longest markup one call may render, in characters.
pub const MAX_MARKUP_CHARS: usize = 20_000;

/// The registered tool name.
const TOOL: &str = "render_ui";

/// A fetchable address anywhere in the markup: an explicit `http(s)://`, or
/// a protocol-relative `//host.tld`. The second half needs a dot, so
/// `50//50` and `a // b` are prose, not addresses.
static URL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)https?://|//[^\s/]*\.[a-z]").ok());

/// The entry point every program must bind, at the start of a line.
static ROOT: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?m)^\s*root\s*=").ok());

/// Said when the surface has no sink.
const NO_SINK: &str = "This surface cannot render UI, so nothing was drawn. Answer in markdown instead, and do not tell the operator a card was rendered.";

/// Said when the card went out.
const RENDERED: &str = "Rendered the card in the conversation. The operator can see it, so describe what it shows rather than repeating its contents, and do not restate the same numbers as a markdown table underneath it.";

/// The `openui` toolkit: chat and voice, never the chart panel (which has no
/// OPENUI LANG section to teach it either).
pub const GROUP: ToolGroup = ToolGroup {
    key: "openui",
    surfaces: Surfaces::CHAT_AND_VOICE,
    requires: &[],
    order: 46,
    description: "Render a card of general data: bar, line, area and pie charts, tables, metric callouts. The model composes the markup, so never prices.",
    build,
};

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    specs::web_or_log(GROUP.key, TOOL)
        .map(|spec| Arc::new(RenderUi(spec)) as Arc<dyn AgentTool>)
        .into_iter()
        .collect()
}

/// `render_ui(markup)`.
struct RenderUi(ToolSpec);

#[async_trait]
impl AgentTool for RenderUi {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        render(&ctx.viz, args.get("markup"))
    }
}

fn refuse(reason: &str, fix: &str) -> ToolOutcome {
    ToolOutcome::invalid_argument("markup", reason, Some(fix))
}

/// The markup to send, or the refusal (the web's checks, in its order).
fn checked(markup: Option<&Value>) -> Result<String, ToolOutcome> {
    let raw = match markup {
        Some(Value::String(s)) if !s.trim().is_empty() => s,
        _ => {
            return Err(refuse(
                "it is empty",
                "Send the whole OpenUI Lang program, starting with root = Card([...]).",
            ))
        }
    };
    let text = strip_code_fence(raw);
    if text.is_empty() {
        return Err(refuse(
            "it holds nothing but a code fence",
            "Send the program itself, unfenced.",
        ));
    }
    let length = text.chars().count();
    if length > MAX_MARKUP_CHARS {
        return Err(refuse(
            &format!(
                "it is {} characters, more than the {} one rendering may carry",
                length, MAX_MARKUP_CHARS
            ),
            "Render fewer rows or fewer series, or answer in markdown instead.",
        ));
    }
    // A pattern that failed to compile refuses rather than letting markup
    // through unchecked.
    if !ROOT.as_ref().is_some_and(|re| re.is_match(&text)) {
        return Err(refuse(
            "it binds no 'root', so it would render nothing at all",
            "Every program starts with a line binding root, such as root = Card([summary, chart]).",
        ));
    }
    if URL.as_ref().is_none_or(|re| re.is_match(&text)) {
        return Err(refuse(
            "it carries a URL, and a rendered card never fetches one",
            "Remove the link or image and state the fact in text instead.",
        ));
    }
    Ok(text)
}

/// Check the markup and leave it on the sink as one `ui` frame.
fn render(sink: &VizSink, markup: Option<&Value>) -> ToolOutcome {
    let text = match checked(markup) {
        Ok(t) => t,
        Err(refused) => return refused,
    };
    let chars = text.len();
    if !sink.emit_frame(TOOL, Frame::Ui { delta: text }) {
        return ToolOutcome::Ok(wrap_tool_result(TOOL, NO_SINK, &[]));
    }
    tracing::debug!("Agent rendered {} bytes of OpenUI markup", chars);
    ToolOutcome::Ok(wrap_tool_result(TOOL, RENDERED, &[]))
}

#[cfg(test)]
mod tests;
