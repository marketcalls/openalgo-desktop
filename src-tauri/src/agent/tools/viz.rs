//! Charts the platform builds, never charts the model types (the web's
//! `services/agent/tools/viz.py`, toolkit `viz`).
//!
//! Four read-only tools. Each calls one service, turns what came back into
//! a renderer payload and leaves it on the run's sink ([`VizSink`]); the
//! model gets one or two lines. Two rules shape every line:
//!
//! - **The tool fetches the data, so the model never types a price.** That
//!   is the difference from `render_ui`, where the model composes the
//!   numbers and the provenance rule lives in the prompt.
//! - **The series does not go through the model's context.** Charting five
//!   hundred candles costs the conversation a sentence.
//!
//! | Domain | `kind` | Engine |
//! | --- | --- | --- |
//! | Candles, OHLC, price with indicators | `candles` | `openalgo-charts`, as `/trading` |
//! | Option analytics | `plotly` | `lib/Plot2D`, `lib/Plot3D`, as `/tools` |
//!
//! Each reads the service its `/tools` page reads: history (the in-process
//! `/api/v1/history`), the option chain (`/api/v1/optionchain`), gamma
//! exposure and the volatility surface (the GEX and Vol Surface pages'
//! services, called in process).

mod figures;
pub mod indicators;
#[cfg(test)]
mod tests;

use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::answer::{label, tool_answer, Label};
use crate::agent::tools::market::candles::{
    candle_columns, chart_bars, roll_up_candles, summarise_candles,
};
use crate::agent::tools::market::normalise::{
    history_range, normalise_pair, normalise_source, plan_interval, today,
};
use crate::agent::tools::market::{data_of, master, BrokerIntervals, DEFAULT_HISTORY_BARS};
use crate::agent::tools::numbers::{as_number, format_number, py_round};
use crate::agent::tools::options::{
    index_underlying, normalise_exchange, normalise_expiry_argument, normalise_int,
    normalise_symbol, UNDERLYING_EXCHANGES,
};
use crate::agent::tools::service::{busy_message, unwrap, ServiceReply, SERVICE_TIMEOUT};
use crate::agent::tools::specs;
use crate::agent::tools::values::py_str;
use crate::services::core::Reply;
use crate::services::{chain_tools_service, history_tools_service};
use async_trait::async_trait;
use figures::{
    markers, numbers, peak, plotly_spec, CALL_COLOUR, NEGATIVE_COLOUR, POSITIVE_COLOUR, PUT_COLOUR,
};
use indicators::{chart_type, expiries, normalise_indicators, overlay_label};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::sync::Arc;

/// Most bars one chart carries (newest kept, with a notice).
pub const MAX_CHART_BARS: usize = 1500;

/// Strikes each side of ATM on an open interest chart, and the most.
pub const DEFAULT_OI_STRIKE_COUNT: i64 = 15;
pub const MAX_OI_STRIKE_COUNT: i64 = 100;

/// Strikes each side of ATM on a volatility surface, and the most.
pub const DEFAULT_SURFACE_STRIKE_COUNT: i64 = 8;
pub const MAX_SURFACE_STRIKE_COUNT: i64 = 25;

/// Every surface states its timestamps in IST.
pub const TIMEZONE: &str = "Asia/Kolkata";

pub const GROUP: ToolGroup = ToolGroup {
    key: "viz",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 45,
    description: "Draw a price chart or an option analytics chart in the conversation. The tool fetches the data, so the model never supplies a number.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    PriceChart,
    OpenInterest,
    GammaExposure,
    VolatilitySurface,
}

struct VizTool {
    spec: ToolSpec,
    kind: Kind,
    intervals: Arc<BrokerIntervals>,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let intervals = Arc::new(BrokerIntervals::default());
    [
        ("plot_price_chart", Kind::PriceChart),
        ("plot_open_interest", Kind::OpenInterest),
        ("plot_gamma_exposure", Kind::GammaExposure),
        ("plot_volatility_surface", Kind::VolatilitySurface),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log(GROUP.key, name)?;
        Some(Arc::new(VizTool {
            spec,
            kind,
            intervals: intervals.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for VizTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::PriceChart => plot_price_chart(ctx, &args, &self.intervals).await,
            Kind::OpenInterest => plot_open_interest(ctx, &args).await,
            Kind::GammaExposure => plot_gamma_exposure(ctx, &args).await,
            Kind::VolatilitySurface => plot_volatility_surface(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// A `/tools` page's service, called in process and read as the web's
/// `service_call` reads its service: the payload, or the correction naming
/// the call; one that runs past the service timeout is "busy".
pub(crate) async fn tools_service(
    label_name: &str,
    call: impl Future<Output = Reply>,
) -> Result<Value, ToolOutcome> {
    match tokio::time::timeout(SERVICE_TIMEOUT, call).await {
        Ok(reply) => unwrap(
            label_name,
            ServiceReply::Answered {
                status: reply.status,
                body: reply.body,
                outcome_unknown: false,
            },
        ),
        Err(_) => Err(ToolOutcome::Retry(busy_message(label_name))),
    }
}

/// Leave a chart on the run's sink; false when the run has none.
fn deliver(
    ctx: &ToolCtx,
    tool: &str,
    kind: &str,
    spec: Map<String, Value>,
    title: &str,
    source: &str,
) -> bool {
    let safe = match json_safe(&Value::Object(spec)) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    ctx.viz.emit(tool, kind, safe, title, source)
}

/// The rows of a history response that are objects.
pub(crate) fn history_records(response: Value) -> Vec<Map<String, Value>> {
    match data_of(response) {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An argument as the model sent it, or the web signature's default when
/// it left the argument out (an explicit null is the model's, and reaches
/// the normaliser as the web's `None` does).
pub(crate) fn arg_or(args: &Map<String, Value>, key: &str, default: Value) -> Value {
    args.get(key).cloned().unwrap_or(default)
}

/// The value at `key` of an object, or null.
fn field(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

/// A price chart's arguments, read and checked (the web's order).
struct PriceChart {
    symbol: String,
    exchange: String,
    notices: Vec<String>,
    source: String,
    /// The interval fetched from the broker.
    fetched: String,
    /// The interval charted: the roll-up target when daily candles are
    /// rolled up, which is also what the default range is sized in.
    interval: String,
    rollup: Option<String>,
    start: String,
    end: String,
    shape: &'static str,
    overlays: Vec<Value>,
}

impl PriceChart {
    async fn read(
        ctx: &ToolCtx,
        args: &Map<String, Value>,
        intervals: &BrokerIntervals,
    ) -> Result<PriceChart, ToolOutcome> {
        let listed = master(&ctx.app);
        let (symbol, exchange, mut notices) =
            normalise_pair(&listed, args.get("symbol"), args.get("exchange"))?;
        let source = normalise_source(args.get("source"))?;
        let accepted = intervals.accepted(ctx).await;
        let asked = arg_or(args, "interval", json!("D"));
        let (fetched, interval_notice, rollup) =
            plan_interval(Some(&asked), &source, accepted.as_deref())?;
        notices.extend(interval_notice);
        let interval = rollup.clone().unwrap_or_else(|| fetched.clone());
        let (start, end) = history_range(
            &interval,
            args.get("start_date"),
            args.get("end_date"),
            DEFAULT_HISTORY_BARS,
            today(&ctx.app),
        )?;
        Ok(PriceChart {
            shape: chart_type(args.get("chart_type"))?,
            overlays: normalise_indicators(args.get("indicators"))?,
            symbol,
            exchange,
            notices,
            source,
            fetched,
            interval,
            rollup,
            start,
            end,
        })
    }

    /// The history request.
    fn request(&self) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("symbol".into(), json!(self.symbol));
        payload.insert("exchange".into(), json!(self.exchange));
        payload.insert("interval".into(), json!(self.fetched));
        payload.insert("start_date".into(), json!(self.start));
        payload.insert("end_date".into(), json!(self.end));
        payload.insert("source".into(), json!(self.source));
        payload
    }

    /// The bars to draw (rolled up when asked, newest
    /// [`MAX_CHART_BARS`] kept with a notice) and the summary of the whole
    /// range.
    fn bars(&mut self, response: Value) -> (Vec<Map<String, Value>>, Map<String, Value>) {
        let mut records = history_records(response);
        let columns = candle_columns(records.first().unwrap_or(&Map::new()));
        if let Some(target) = &self.rollup {
            records = roll_up_candles(&records, &columns, target);
        }
        let summary = summarise_candles(&records, &columns);
        let mut bars = chart_bars(&records, &columns);
        let total = bars.len();
        if total > MAX_CHART_BARS {
            let omitted = total - MAX_CHART_BARS;
            bars.drain(..omitted);
            self.notices.push(format!(
                "{} of {} bars were older than the {} the chart carries, so the chart starts later than the range asked for. The summary covers the whole range.",
                omitted, total, MAX_CHART_BARS
            ));
        }
        (bars, summary)
    }

    /// The `candles` spec.
    fn spec(
        &self,
        bars: Vec<Map<String, Value>>,
        summary: &Map<String, Value>,
    ) -> Map<String, Value> {
        let mut spec = Map::new();
        spec.insert("symbol".into(), json!(self.symbol));
        spec.insert("exchange".into(), json!(self.exchange));
        spec.insert("interval".into(), json!(self.interval));
        spec.insert("chart_type".into(), json!(self.shape));
        spec.insert("start_date".into(), json!(self.start));
        spec.insert("end_date".into(), json!(self.end));
        spec.insert("source".into(), json!(self.source));
        spec.insert("timezone".into(), json!(TIMEZONE));
        spec.insert("bar_count".into(), json!(bars.len()));
        spec.insert("bars".into(), json!(bars));
        spec.insert("indicators".into(), json!(self.overlays));
        spec.insert("summary".into(), json_safe(&Value::Object(summary.clone())));
        if !self.notices.is_empty() {
            spec.insert("notices".into(), json!(self.notices));
        }
        spec
    }

    /// The line the model reads once the chart is on screen.
    fn message(&self, bar_count: usize, summary: &Map<String, Value>) -> String {
        let overlay_text = if self.overlays.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = self.overlays.iter().map(overlay_label).collect();
            format!(" with {}", names.join(", "))
        };
        let stat = |k: &str| format_number(summary.get(k).unwrap_or(&Value::Null));
        format!(
            "Drew a {} chart of {} on {} at {}{}, {} bars from {} to {}. Open {}, close {}, high {}, low {}, change {} percent. The operator can see it, so describe what it shows rather than listing bars.",
            self.shape, self.symbol, self.exchange, self.interval, overlay_text, bar_count,
            self.start, self.end, stat("first_open"), stat("last_close"), stat("highest_high"),
            stat("lowest_low"), stat("change_percent")
        )
    }
}

/// `plot_price_chart`: candles from `history_service`, drawn by the
/// terminal's engine.
async fn plot_price_chart(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    intervals: &BrokerIntervals,
) -> Result<ToolOutcome, ToolOutcome> {
    const TOOL: &str = "plot_price_chart";
    let mut chart = PriceChart::read(ctx, args, intervals).await?;
    let response = ctx
        .service_read("history_service.get_history", "history", chart.request())
        .await?;
    let (bars, summary) = chart.bars(response);
    let pair: [Label<'_>; 2] = [
        label("symbol", &chart.symbol),
        label("exchange", &chart.exchange),
    ];
    if bars.is_empty() {
        let text = format!(
            "No candles came back for {} on {} at {} between {} and {}, so nothing was drawn. Check the dates against the trading calendar and confirm the interval is one this broker supports.",
            chart.symbol, chart.exchange, chart.interval, chart.start, chart.end
        );
        return Ok(tool_answer(TOOL, &text, &[], &pair));
    }
    let bar_count = bars.len();
    let title = format!("{} {} {}", chart.symbol, chart.exchange, chart.interval);
    let spec = chart.spec(bars, &summary);
    if !deliver(ctx, TOOL, "candles", spec, &title, "history_service") {
        return Ok(tool_answer(TOOL, &no_sink_message("chart"), &[], &pair));
    }
    Ok(tool_answer(
        TOOL,
        &chart.message(bar_count, &summary),
        &chart.notices,
        &pair,
    ))
}

/// The underlying, its exchange (an index moved onto its index exchange,
/// with a notice) and the expiry, as every option chart reads them.
fn option_target(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<(String, String, Vec<String>), ToolOutcome> {
    let underlying = normalise_symbol(args.get("underlying"), "underlying")?;
    let named = normalise_exchange(args.get("exchange"), &UNDERLYING_EXCHANGES)?;
    let listed = master(&ctx.app);
    let (exchange, notice) = index_underlying(&listed, &underlying, &named);
    Ok((underlying, exchange, notice.into_iter().collect()))
}

/// The rows of a chain that are objects.
fn chain_rows(payload: &Value) -> Vec<Value> {
    payload
        .get("chain")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter(|r| r.is_object()).cloned().collect())
        .unwrap_or_default()
}

/// An expiry label, or nothing when it is empty (the web's `expiry or None`).
fn expiry_label(expiry: &str) -> Label<'static> {
    ("expiry", (!expiry.is_empty()).then(|| expiry.to_string()))
}

/// `plot_open_interest`: call and put OI by strike from the option chain.
async fn plot_open_interest(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    const TOOL: &str = "plot_open_interest";
    let (underlying, exchange, notices) = option_target(ctx, args)?;
    let expiry =
        normalise_expiry_argument(args.get("expiry_date"), &underlying, true, "expiry_date")?;
    let count = normalise_int(
        Some(&arg_or(
            args,
            "strike_count",
            json!(DEFAULT_OI_STRIKE_COUNT),
        )),
        "strike_count",
        1,
        MAX_OI_STRIKE_COUNT,
        Some(DEFAULT_OI_STRIKE_COUNT),
    )?;
    let mut request = Map::new();
    request.insert("underlying".into(), json!(underlying));
    request.insert("exchange".into(), json!(exchange));
    request.insert("expiry_date".into(), json!(expiry));
    request.insert("strike_count".into(), json!(count));
    request.insert("with_greeks".into(), json!(false));
    let payload = ctx
        .service_read(
            "option_chain_service.get_option_chain",
            "optionchain",
            request,
        )
        .await?;

    let rows = chain_rows(&payload);
    if rows.is_empty() {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "The chain for {} {} came back with no strikes, so nothing was drawn. Confirm the expiry is listed.",
                underlying, expiry
            ),
            &[],
            &[label("underlying", &underlying), expiry_label(&expiry)],
        ));
    }
    let strikes: Vec<Value> = rows.iter().map(|r| field(r, "strike")).collect();
    let oi = |side: &str| -> Vec<Option<f64>> {
        rows.iter()
            .map(|r| r.get(side).and_then(|s| s.get("oi")).and_then(as_number))
            .collect()
    };
    let (call_oi, put_oi) = (oi("ce"), oi("pe"));
    let atm = field(&payload, "atm_strike");
    let spot = field(&payload, "underlying_ltp");
    let (shapes, annotations) = markers(&atm, &spot);
    let mut layout = Map::new();
    layout.insert("barmode".into(), json!("group"));
    layout.insert("xaxis".into(), json!({"title": {"text": "Strike"}}));
    layout.insert("yaxis".into(), json!({"title": {"text": "Open interest"}}));
    layout.insert("shapes".into(), json!(shapes));
    layout.insert("annotations".into(), json!(annotations));
    layout.insert("legend".into(), json!({"orientation": "h"}));
    let spec = plotly_spec(
        vec![
            json!({"type": "bar", "name": "Call OI", "x": strikes, "y": numbers(&call_oi), "marker": {"color": CALL_COLOUR}}),
            json!({"type": "bar", "name": "Put OI", "x": strikes, "y": numbers(&put_oi), "marker": {"color": PUT_COLOUR}}),
        ],
        layout,
        "2d",
    );
    let title = format!("{} {} open interest by strike", underlying, expiry);
    if !deliver(ctx, TOOL, "plotly", spec, &title, "option_chain_service") {
        return Ok(tool_answer(
            TOOL,
            &no_sink_message("chart"),
            &[],
            &[label("underlying", &underlying)],
        ));
    }

    let total = |v: &[Option<f64>]| -> f64 { v.iter().flatten().filter(|x| **x != 0.0).sum() };
    let (call_total, put_total) = (total(&call_oi), total(&put_oi));
    let ratio = if call_total != 0.0 {
        json!(py_round(put_total / call_total, 2))
    } else {
        Value::Null
    };
    let message = format!(
        "Drew open interest by strike for {} {}: {} strikes, ATM {}, spot {}. Call OI peaks at {} and put OI at {}; total call OI {}, total put OI {}, PCR {}.",
        underlying, expiry, rows.len(), format_number(&atm), format_number(&spot),
        format_number(&peak(&strikes, &call_oi)), format_number(&peak(&strikes, &put_oi)),
        format_number(&json!(call_total)), format_number(&json!(put_total)), format_number(&ratio)
    );
    Ok(tool_answer(
        TOOL,
        &message,
        &notices,
        &[label("underlying", &underlying), expiry_label(&expiry)],
    ))
}

/// `plot_gamma_exposure`: net GEX by strike from the GEX page's service.
async fn plot_gamma_exposure(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    const TOOL: &str = "plot_gamma_exposure";
    let (underlying, exchange, notices) = option_target(ctx, args)?;
    let expiry =
        normalise_expiry_argument(args.get("expiry_date"), &underlying, true, "expiry_date")?;
    let payload = tools_service(
        "gex_service.get_gex_data",
        chain_tools_service::gex(&ctx.app, &underlying, &exchange, &expiry),
    )
    .await?;

    let rows = chain_rows(&payload);
    if rows.is_empty() {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "No gamma exposure came back for {} {}, so nothing was drawn. Confirm the expiry is listed and that the chain is quoting.",
                underlying, expiry
            ),
            &[],
            &[label("underlying", &underlying), expiry_label(&expiry)],
        ));
    }
    let strikes: Vec<Value> = rows.iter().map(|r| field(r, "strike")).collect();
    let net: Vec<Option<f64>> = rows
        .iter()
        .map(|r| r.get("net_gex").and_then(as_number))
        .collect();
    let colours: Vec<&str> = net
        .iter()
        .map(|v| match v {
            Some(x) if *x < 0.0 => NEGATIVE_COLOUR,
            _ => POSITIVE_COLOUR,
        })
        .collect();
    let atm = field(&payload, "atm_strike");
    let spot = field(&payload, "spot_price");
    let (shapes, annotations) = markers(&atm, &spot);
    let mut layout = Map::new();
    layout.insert("xaxis".into(), json!({"title": {"text": "Strike"}}));
    layout.insert(
        "yaxis".into(),
        json!({"title": {"text": "Net gamma exposure"}, "zeroline": true}),
    );
    layout.insert("shapes".into(), json!(shapes));
    layout.insert("annotations".into(), json!(annotations));
    layout.insert("showlegend".into(), json!(false));
    let spec = plotly_spec(
        vec![
            json!({"type": "bar", "name": "Net GEX", "x": strikes, "y": numbers(&net), "marker": {"color": colours}}),
        ],
        layout,
        "2d",
    );
    let title = format!("{} {} net gamma exposure by strike", underlying, expiry);
    if !deliver(ctx, TOOL, "plotly", spec, &title, "gex_service") {
        return Ok(tool_answer(
            TOOL,
            &no_sink_message("chart"),
            &[],
            &[label("underlying", &underlying)],
        ));
    }

    let negated: Vec<Option<f64>> = net.iter().map(|v| v.map(|x| -x)).collect();
    let message = format!(
        "Drew net gamma exposure for {} {}: {} strikes, ATM {}, spot {}. Total net GEX {}, PCR {}. Longest gamma at strike {}, shortest at {}.",
        underlying, expiry, rows.len(), format_number(&atm), format_number(&spot),
        format_number(&field(&payload, "total_net_gex")), format_number(&field(&payload, "pcr_oi")),
        format_number(&peak(&strikes, &net)), format_number(&peak(&strikes, &negated))
    );
    Ok(tool_answer(
        TOOL,
        &message,
        &notices,
        &[label("underlying", &underlying), expiry_label(&expiry)],
    ))
}

/// `plot_volatility_surface`: IV across strikes and expiries, from the Vol
/// Surface page's service, drawn in 3D.
async fn plot_volatility_surface(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    const TOOL: &str = "plot_volatility_surface";
    let (underlying, exchange, notices) = option_target(ctx, args)?;
    let dates = expiries(args.get("expiry_dates"))?;
    let count = normalise_int(
        Some(&arg_or(
            args,
            "strike_count",
            json!(DEFAULT_SURFACE_STRIKE_COUNT),
        )),
        "strike_count",
        1,
        MAX_SURFACE_STRIKE_COUNT,
        Some(DEFAULT_SURFACE_STRIKE_COUNT),
    )?;
    let response = tools_service(
        "vol_surface_service.get_vol_surface_data",
        history_tools_service::vol_surface(
            &ctx.app,
            &underlying,
            &exchange,
            &dates,
            count as usize,
        ),
    )
    .await?;
    let payload = match response.get("data") {
        Some(data @ Value::Object(_)) => data.clone(),
        _ if response.is_object() => response,
        _ => json!({}),
    };

    let strikes = payload.get("strikes").and_then(Value::as_array);
    let surface = payload
        .get("surface")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty());
    let (Some(strikes), Some(surface)) = (strikes, surface) else {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "No volatility surface came back for {} across {}, so nothing was drawn. Confirm the expiries are listed and that the chain is quoting.",
                underlying,
                dates.join(", ")
            ),
            &[],
            &[label("underlying", &underlying)],
        ));
    };
    let rows: Vec<Value> = payload
        .get("expiries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let days: Vec<Value> = if rows.is_empty() {
        (0..surface.len()).map(|i| json!(i)).collect()
    } else {
        rows.iter()
            .map(|r| json!(r.get("dte").and_then(as_number)))
            .collect()
    };
    let labels: Vec<String> = if rows.is_empty() {
        dates.clone()
    } else {
        rows.iter()
            .map(|r| match r {
                Value::Object(m) => py_str(m.get("date").unwrap_or(&Value::Null)),
                _ => String::new(),
            })
            .collect()
    };
    let mut layout = Map::new();
    layout.insert(
        "scene".into(),
        json!({
            "xaxis": {"title": {"text": "Strike"}},
            "yaxis": {"title": {"text": "Days to expiry"}},
            "zaxis": {"title": {"text": "Implied volatility %"}},
        }),
    );
    layout.insert("showlegend".into(), json!(false));
    let mut spec = plotly_spec(
        vec![json!({
            "type": "surface",
            "name": "Implied volatility",
            "x": strikes,
            "y": days,
            "z": surface,
            "connectgaps": false,
            "colorbar": {"title": {"text": "IV %"}},
        })],
        layout,
        "3d",
    );
    spec.insert("expiry_labels".into(), json!(labels));
    let title = format!("{} implied volatility surface", underlying);
    if !deliver(ctx, TOOL, "plotly", spec, &title, "vol_surface_service") {
        return Ok(tool_answer(
            TOOL,
            &no_sink_message("chart"),
            &[],
            &[label("underlying", &underlying)],
        ));
    }
    let message = format!(
        "Drew an implied volatility surface for {}: {} strikes across {} expiries ({}), ATM {}, spot {}.",
        underlying,
        strikes.len(),
        surface.len(),
        labels.join(", "),
        format_number(&field(&payload, "atm_strike")),
        format_number(&field(&payload, "underlying_ltp"))
    );
    Ok(tool_answer(
        TOOL,
        &message,
        &notices,
        &[label("underlying", &underlying)],
    ))
}
