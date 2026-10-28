//! The chart toolkit (the web's `services/agent/tools/chart.py`): read the
//! chart the trader has open on `/trading`, analyse it, and draw on it.
//!
//! What makes the panel different from the chat page is the context: the
//! panel reports its symbol, exchange, interval, viewport, indicators and
//! the trader's own drawings with every message ([`ChartView`]), so no tool
//! here takes a symbol, an exchange, an interval, a date or a price.
//!
//! Provenance is stricter here than anywhere else. A level drawn on a chart
//! is read as a fact about the market, so every number that reaches the
//! canvas is computed by `chart_geometry` from candles fetched through the
//! history service. The one string a tool passes to the canvas is `note`:
//! its digits are removed and only the trader's own words survive
//! (`draw::caption_of`). Every other string argument is a closed
//! vocabulary.
//!
//! Everything drawn goes into one of the Agent's groups and is id'd
//! `ai:{group}:{index}`, so a clear can only remove the Agent's markup,
//! never the trader's. A draw replaces its group. None of these tools
//! changes the account, so none needs approval and none writes an audit
//! row.
//!
//! - this file: the group, the state one run's chart tools share,
//!   dispatch, and the answers every tool gives;
//! - `window`: the chart's own candles, fetched once per run and lookback,
//!   clipped to the viewport;
//! - `read`: `read_chart` and `analyse_chart` (with its momentum block);
//! - `draw`: `find_patterns`, the three draw tools, `clear_drawings` and
//!   the caption control;
//! - `indicators`: list, add and remove the chart's own indicators.

mod draw;
mod indicators;
mod read;
mod window;

#[cfg(test)]
mod tests;

use crate::agent::chart_contract::{groups_after, ChartView, Command};
use crate::agent::contract::frames::Frame;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::tools::answer::{tool_answer, Label};
use crate::agent::tools::market::candles::ist_label;
use crate::agent::tools::market::BrokerIntervals;
use crate::agent::tools::specs;
use crate::agent::tools::values::{py_repr, py_str};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const GROUP: ToolGroup = ToolGroup {
    key: "chart",
    surfaces: Surfaces::CHART_ONLY,
    requires: &[],
    order: 5,
    description: "Read the chart the operator has open, analyse its trend, structure, momentum and patterns, and draw levels, trendlines and zones on it. Every price drawn comes from real candles; the model supplies none.",
    build,
};

/// Said when the panel sent no usable context: the model is told plainly
/// rather than left to guess an instrument.
const NO_CHART: &str = "No chart context came with this message, so there is nothing to read or draw on. Tell the operator to open a chart on the trading terminal, and do not guess a symbol, an exchange or an interval.";

/// Said when a drawing was computed but the surface has nowhere to send it.
const NO_SINK: &str = "This surface cannot apply chart commands, so nothing was drawn. Report the levels in prose and do not tell the operator that anything is on the chart.";

#[derive(Debug, Clone, Copy)]
enum Kind {
    Read,
    Analyse,
    Patterns,
    Levels,
    Trendline,
    Zone,
    Clear,
    List,
    Add,
    Remove,
}

/// The tools in the web's order.
const TOOLS: [(&str, Kind); 10] = [
    ("read_chart", Kind::Read),
    ("analyse_chart", Kind::Analyse),
    ("find_patterns", Kind::Patterns),
    ("draw_levels", Kind::Levels),
    ("draw_trendline", Kind::Trendline),
    ("draw_zone", Kind::Zone),
    ("clear_drawings", Kind::Clear),
    ("list_chart_indicators", Kind::List),
    ("add_chart_indicator", Kind::Add),
    ("remove_chart_indicator", Kind::Remove),
];

/// What one run's chart tools share, built with the run and dropped with
/// it (the web binds these on the toolkit at construction).
struct ChartRun {
    /// The chart as the panel reported it, with the Agent's groups brought
    /// up to date by every command this run sends.
    view: parking_lot::Mutex<ChartView>,
    intervals: BrokerIntervals,
    /// Candle windows already fetched this run, keyed by the clamped bar
    /// count: the instrument and interval are fixed by the context, so an
    /// analysis and two draws are one broker call.
    windows: tokio::sync::Mutex<HashMap<u32, Arc<window::Fetched>>>,
}

impl ChartRun {
    fn view(&self) -> ChartView {
        self.view.lock().clone()
    }

    /// Send chart commands to the panel, every one through here, and bring
    /// the run's own view of the Agent's groups up to date, so a
    /// `read_chart` later in the same turn sees a clear or a draw. False
    /// when the surface has no sink.
    fn emit(&self, ctx: &ToolCtx, commands: Vec<Command>) -> bool {
        let wire = commands.iter().map(Command::to_value).collect();
        if !ctx
            .viz
            .emit_frame("chart", Frame::ChartCommand { commands: wire })
        {
            return false;
        }
        let mut view = self.view.lock();
        view.agent_groups = groups_after(&view.agent_groups, &commands);
        true
    }
}

struct ChartTool {
    spec: ToolSpec,
    kind: Kind,
    run: Arc<ChartRun>,
}

fn build(ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let run = Arc::new(ChartRun {
        view: parking_lot::Mutex::new(ChartView::from_context(ctx.chart_context.as_ref())),
        intervals: BrokerIntervals::default(),
        windows: tokio::sync::Mutex::new(HashMap::new()),
    });
    TOOLS
        .into_iter()
        .filter_map(|(name, kind)| {
            let spec = specs::web_or_log("chart", name)?;
            Some(Arc::new(ChartTool {
                spec,
                kind,
                run: run.clone(),
            }) as Arc<dyn AgentTool>)
        })
        .collect()
}

#[async_trait]
impl AgentTool for ChartTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let run = &self.run;
        let done = match self.kind {
            Kind::Read => Ok(read::read_chart(run)),
            Kind::Analyse => read::analyse_chart(run, ctx, &args).await,
            Kind::Patterns => draw::find_patterns(run, ctx, &args).await,
            Kind::Levels => draw::draw_levels(run, ctx, &args).await,
            Kind::Trendline => draw::draw_trendline(run, ctx, &args).await,
            Kind::Zone => draw::draw_zone(run, ctx, &args).await,
            Kind::Clear => draw::clear_drawings(run, ctx, &args),
            Kind::List => Ok(indicators::list_chart_indicators(&args)),
            Kind::Add => indicators::add_chart_indicator(run, ctx, &args),
            Kind::Remove => indicators::remove_chart_indicator(run, ctx, &args),
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// A point as the model reads it: the IST time a person reads, and the
/// price.
#[derive(serde::Serialize)]
struct At {
    at: String,
    price: f64,
}

/// UTC epoch seconds as the IST wall clock.
fn ist(seconds: f64) -> String {
    ist_label(&Value::from(seconds))
}

/// A one-line answer labelled as data, with the chart's symbol when the
/// web labels it.
fn answer(tool: &str, text: &str, symbol: Option<&str>) -> ToolOutcome {
    let labels: Vec<Label<'_>> = symbol
        .map(|s| vec![("symbol", Some(s.to_string()))])
        .unwrap_or_default();
    tool_answer(tool, text, &[], &labels)
}

/// What every tool that needs a chart answers without one.
fn no_chart(tool: &str) -> ToolOutcome {
    answer(tool, NO_CHART, None)
}

/// One closed-vocabulary argument (`_choice`): lower-cased, an alias
/// applied, refused when outside `allowed`. An empty value takes
/// `default`, as the web's `value or default` does.
fn choice(
    field: &str,
    value: Option<&Value>,
    default: &str,
    allowed: &[&'static str],
    aliases: &[(&str, &'static str)],
) -> Result<&'static str, ToolOutcome> {
    let given = value
        .filter(|v| crate::agent::tools::values::truthy(v))
        .cloned()
        .unwrap_or_else(|| Value::from(default));
    let cleaned = py_str(&given).trim().to_lowercase();
    let cleaned = aliases
        .iter()
        .find(|(alias, _)| *alias == cleaned)
        .map_or(cleaned.as_str(), |(_, canonical)| *canonical);
    allowed
        .iter()
        .find(|a| **a == cleaned)
        .copied()
        .ok_or_else(|| {
            ToolOutcome::invalid_argument(
                field,
                &format!(
                    "{} is not one of the values this tool accepts",
                    py_repr(&given)
                ),
                Some(&format!("Use one of: {}.", allowed.join(", "))),
            )
        })
}
