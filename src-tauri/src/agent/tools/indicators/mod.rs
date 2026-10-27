//! Indicator tools: the 127 `openalgo.ta` indicators behind five tools
//! (the web's `services/agent/tools/indicators.py`, toolkit `indicators`).
//!
//! The model picks an indicator by name, found with `list_indicators` and
//! pinned down with `describe_indicator`; one dispatcher
//! ([`crate::agent::indicators`]) computes it over candles this run fetched,
//! never over a chart's drawing and never from a number the model supplied.
//!
//! No tool takes a date: each takes a lookback in bars and pads the fetch by
//! the indicator's measured warm-up, so "the last ten values of beta" fetches
//! the 253 bars beta needs. A run keeps the frames it fetched (at most 24),
//! so five indicators on one instrument are one history call.
//!
//! `scan_symbols` never evaluates text: its condition is read by a fixed
//! parser that accepts a comparison against a number or a cross between two
//! named outputs, and nothing else.
//!
//! Every tool reads; none needs approval, and all are offered on every
//! surface with trading off.
//!
//! - [`args`]: the tools' own argument handling and caps.
//! - [`payload`]: the instrument block and the per-indicator tables.

mod args;
mod payload;
#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::indicators::{
    compute, compute_batch, describe_indicator, get_spec, list_indicators, plan_scan,
    required_bars, scan_symbol, unknown_indicator, Comparison, Json,
};
use crate::agent::tools::answer::label;
use crate::agent::tools::market::candles::{candle_columns, candle_frame, CandleFrame};
use crate::agent::tools::market::normalise::{
    lookback_range, normalise_interval, normalise_pair, normalise_source, today,
};
use crate::agent::tools::market::{data_of, master, BrokerIntervals};
use crate::agent::tools::specs;
use crate::agent::tools::values::{py_str, text, truthy, truthy_text};
use async_trait::async_trait;
use parking_lot::Mutex;
use payload::{answer, fit, shared, table, trimmed, unique, Shared};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The interval when the model names none (`DEFAULT_INTERVAL`).
pub const DEFAULT_INTERVAL: &str = "D";

/// Most candle frames one run keeps (`_MAX_CACHED_FRAMES`).
const MAX_CACHED_FRAMES: usize = 24;

/// Characters of one instrument's failure a scan keeps.
const SCAN_FAILURE_CHARS: usize = 200;

pub const GROUP: ToolGroup = ToolGroup {
    key: "indicators",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 25,
    description: "Compute any of the 127 openalgo.ta indicators over real candles, several in \
                  one call, or screen a list of instruments for one condition.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    List,
    Describe,
    Compute,
    Batch,
    Scan,
}

/// The candles one run fetched: instrument, interval and source, how many
/// bars were asked for, and the frame.
type FrameKey = (String, String, String, String);

/// What the five tools of one run share.
#[derive(Default)]
struct RunState {
    intervals: BrokerIntervals,
    frames: Mutex<Vec<(FrameKey, usize, Arc<CandleFrame>)>>,
}

struct IndicatorTool {
    spec: ToolSpec,
    kind: Kind,
    run: Arc<RunState>,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let run = Arc::new(RunState::default());
    [
        ("list_indicators", Kind::List),
        ("describe_indicator", Kind::Describe),
        ("compute_indicator", Kind::Compute),
        ("compute_indicators_batch", Kind::Batch),
        ("scan_symbols", Kind::Scan),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("indicators", name)?;
        Some(Arc::new(IndicatorTool {
            spec,
            kind,
            run: run.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for IndicatorTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::List => list(&args),
            Kind::Describe => describe(&args),
            Kind::Compute => compute_one(ctx, &self.run, &args).await,
            Kind::Batch => compute_many(ctx, &self.run, &args).await,
            Kind::Scan => scan(ctx, &self.run, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// `list_indicators(query, category)`.
fn list(args: &Map<String, Value>) -> Result<ToolOutcome, ToolOutcome> {
    let raw = |key: &str| args.get(key).map(py_str).unwrap_or_default();
    let payload = list_indicators(&raw("query"), &raw("category"))?;
    Ok(answer("list_indicators", &payload, &[]))
}

/// `describe_indicator(name)`.
fn describe(args: &Map<String, Value>) -> Result<ToolOutcome, ToolOutcome> {
    let raw = args.get("name").map(py_str).unwrap_or_default();
    let payload = describe_indicator(&raw)?;
    let cleaned = raw.trim().to_lowercase();
    Ok(answer(
        "describe_indicator",
        &payload,
        &[label("name", &cleaned)],
    ))
}

/// The interval argument, `D` when the model left it out.
fn interval_arg(args: &Map<String, Value>) -> Value {
    args.get("interval")
        .cloned()
        .unwrap_or_else(|| Value::from(DEFAULT_INTERVAL))
}

/// `_frame`: this run's copy when it holds enough bars, otherwise one
/// history call over the lookback window; the frame and how many rows that
/// call returned (0 for a reuse).
async fn frame(
    ctx: &ToolCtx,
    run: &RunState,
    key: FrameKey,
    bars: usize,
) -> Result<(Arc<CandleFrame>, usize), ToolOutcome> {
    if let Some(found) = cached(run, &key, bars) {
        return Ok((found, 0));
    }
    let (symbol, exchange, interval, source) = &key;
    let wanted = u32::try_from(bars).unwrap_or(u32::MAX);
    let (start, end) = lookback_range(interval, wanted, today(&ctx.app));
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    request.insert("interval".into(), json!(interval));
    request.insert("start_date".into(), json!(start));
    request.insert("end_date".into(), json!(end));
    request.insert("source".into(), json!(source));
    let response = ctx
        .service_read("history_service.get_history", "history", request)
        .await?;
    let records: Vec<Map<String, Value>> = match data_of(response) {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let columns = records.first().map(candle_columns).unwrap_or_default();
    let mut built = candle_frame(&records, &columns);
    if built.is_empty() {
        return Err(ToolOutcome::Retry(format!(
            "No usable candles came back for {} on {} at {} between {} and {}. Check the symbol \
             and exchange, confirm the interval is one this broker serves, and remember an index \
             is quoted on an index exchange.",
            symbol, exchange, interval, start, end
        )));
    }
    keep_last(&mut built, bars);
    let built = Arc::new(built);
    remember(run, key, bars, built.clone());
    Ok((built, records.len()))
}

/// The last `bars` rows, so a broker that over-serves the range does not
/// make every indicator run over years of bars.
fn keep_last(frame: &mut CandleFrame, bars: usize) {
    let cut = frame.len().saturating_sub(bars);
    if cut > 0 {
        frame.time.drain(..cut);
        for (_, values) in frame.columns.iter_mut() {
            values.drain(..cut);
        }
    }
}

fn cached(run: &RunState, key: &FrameKey, bars: usize) -> Option<Arc<CandleFrame>> {
    run.frames
        .lock()
        .iter()
        .find(|(k, held, _)| k == key && *held >= bars)
        .map(|(_, _, f)| f.clone())
}

/// Keep a frame for the rest of the run, the oldest dropped at the cap.
fn remember(run: &RunState, key: FrameKey, bars: usize, frame: Arc<CandleFrame>) {
    let mut frames = run.frames.lock();
    if frames.len() >= MAX_CACHED_FRAMES {
        frames.remove(0);
    }
    match frames.iter_mut().find(|(k, _, _)| *k == key) {
        Some(slot) => *slot = (key, bars, frame),
        None => frames.push((key, bars, frame)),
    }
}

/// A computation's refusal as the model reads it (the web raises
/// `RetryAgentRun(str(exc))`).
fn retry(error: impl std::fmt::Display) -> ToolOutcome {
    ToolOutcome::Retry(error.to_string())
}

/// The instrument, source and interval every compute tool starts from.
struct Window {
    symbol: String,
    exchange: String,
    source: String,
    interval: String,
    notices: Vec<String>,
}

async fn window(
    ctx: &ToolCtx,
    run: &RunState,
    args: &Map<String, Value>,
) -> Result<Window, ToolOutcome> {
    let listed = master(&ctx.app);
    let (symbol, exchange, mut notices) =
        normalise_pair(&listed, args.get("symbol"), args.get("exchange"))?;
    let source = normalise_source(args.get("source"))?;
    let accepted = run.intervals.accepted(ctx).await;
    let (interval, notice) =
        normalise_interval(Some(&interval_arg(args)), &source, accepted.as_deref())?;
    notices.extend(notice);
    Ok(Window {
        symbol,
        exchange,
        source,
        interval,
        notices,
    })
}

impl Window {
    fn key(&self, symbol: &str, exchange: &str) -> FrameKey {
        (
            symbol.to_string(),
            exchange.to_string(),
            self.interval.clone(),
            self.source.clone(),
        )
    }

    fn head<'a>(&'a self, fetched: usize) -> Shared<'a> {
        Shared {
            symbol: &self.symbol,
            exchange: &self.exchange,
            interval: &self.interval,
            source: &self.source,
            bars_fetched: fetched,
            notices: &self.notices,
        }
    }
}

/// `compute_indicator`: one indicator, its comparison instrument for beta
/// and correlation, the table cut to fit.
async fn compute_one(
    ctx: &ToolCtx,
    run: &RunState,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let raw = args.get("indicator").map(py_str).unwrap_or_default();
    let name = raw.trim().to_lowercase();
    let Some(spec) = get_spec(&name) else {
        return Err(unknown_indicator("indicator", &raw));
    };
    let mut w = window(ctx, run, args).await?;
    let tail = args::tail(args.get("last_n"), 10)?;
    let arguments = args::params(args.get("params"), "params")?;
    let bars = match args::lookback(args.get("lookback_bars"))? {
        0 => required_bars(spec, Some(&arguments), tail as usize),
        n => n,
    };
    let (data, mut fetched) = frame(ctx, run, w.key(&w.symbol, &w.exchange), bars).await?;
    let mut second = None;
    if spec.needs_second_series {
        let (other, other_fetched) = comparison(ctx, run, &mut w, args, &name, bars).await?;
        fetched += other_fetched;
        second = Some(other);
    }
    let result = compute(&name, &data, Some(&arguments), second.as_ref(), tail).map_err(retry)?;
    let head = w.head(fetched);
    let payload = fit(|limit| table(&result, &data, limit, &head), tail as usize);
    Ok(answer(
        "compute_indicator",
        &payload,
        &[
            label("symbol", &w.symbol),
            label("exchange", &w.exchange),
            label("indicator", &name),
        ],
    ))
}

/// The comparison instrument's closes for beta and correlation.
async fn comparison(
    ctx: &ToolCtx,
    run: &RunState,
    w: &mut Window,
    args: &Map<String, Value>,
    name: &str,
    bars: usize,
) -> Result<(Comparison, usize), ToolOutcome> {
    if truthy_text(args.get("compare_symbol")).is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "compare_symbol",
            &format!("it is empty, and {} compares two instruments", name),
            Some(
                "Pass the benchmark as well, for example compare_symbol='NIFTY', \
                 compare_exchange='NSE_INDEX'.",
            ),
        ));
    }
    let listed = master(&ctx.app);
    let own_exchange = Value::from(w.exchange.clone());
    let exchange = match args.get("compare_exchange") {
        Some(v) if truthy(v) => v,
        _ => &own_exchange,
    };
    let (other_symbol, other_exchange, notices) =
        normalise_pair(&listed, args.get("compare_symbol"), Some(exchange))?;
    w.notices.extend(notices);
    let (other, fetched) = frame(ctx, run, w.key(&other_symbol, &other_exchange), bars).await?;
    let Some(closes) = other.column("close").filter(|_| !other.is_empty()) else {
        return Err(ToolOutcome::invalid_argument(
            "compare_symbol",
            &format!(
                "no candles came back for {} on {}",
                other_symbol, other_exchange
            ),
            Some("Check the symbol and exchange, and that it traded over this range."),
        ));
    };
    Ok((
        Comparison {
            timestamps: other.time.clone(),
            closes: closes.to_vec(),
        },
        fetched,
    ))
}

/// `compute_indicators_batch`: up to eight indicators over one fetch, each
/// failure reported by name rather than failing the call.
async fn compute_many(
    ctx: &ToolCtx,
    run: &RunState,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let requests = args::batch_requests(args.get("indicators"))?;
    let w = window(ctx, run, args).await?;
    let tail = args::tail(args.get("last_n"), 10)?;
    let bars = match args::lookback(args.get("lookback_bars"))? {
        0 => requests
            .iter()
            .filter_map(|(name, params)| {
                get_spec(name).map(|spec| required_bars(spec, Some(params), tail as usize))
            })
            .max()
            .unwrap_or(0),
        n => n,
    };
    let (data, fetched) = frame(ctx, run, w.key(&w.symbol, &w.exchange), bars).await?;
    let outcome = compute_batch(&data, &requests, tail);
    if let Some(all_failed) = outcome.all_failed() {
        return Err(ToolOutcome::Retry(all_failed));
    }
    let head = w.head(fetched);
    let failed: Vec<Json> = outcome
        .failures
        .iter()
        .map(|(name, error)| {
            Json::obj([
                ("indicator", Json::str(name.clone())),
                ("error", Json::str(error.clone())),
            ])
        })
        .collect();
    let payload = fit(
        |limit| {
            let mut p = shared(&data, limit, &head);
            let results = outcome.results.iter().map(|r| trimmed(r, limit)).collect();
            p.set("results", Json::Arr(results));
            if !failed.is_empty() {
                p.set("failed", Json::Arr(failed.clone()));
            }
            p
        },
        tail as usize,
    );
    Ok(answer(
        "compute_indicators_batch",
        &payload,
        &[
            label("symbol", &w.symbol),
            label("exchange", &w.exchange),
            label("count", &outcome.results.len()),
        ],
    ))
}

/// One instrument's reason it could not be scanned, cut to 200 characters.
fn scan_failure(symbol: &str, reason: &str) -> Json {
    Json::obj([
        ("symbol", Json::str(symbol.trim().to_uppercase())),
        (
            "error",
            Json::str(reason.chars().take(SCAN_FAILURE_CHARS).collect::<String>()),
        ),
    ])
}

/// What a scan found: every instrument read, the ones that matched, and
/// the ones that could not be read.
#[derive(Default)]
struct ScanFindings {
    checked: Vec<Json>,
    matched: Vec<Json>,
    failures: Vec<Json>,
    notices: Vec<String>,
}

/// One instrument of a scan: its pair, its candles, the indicator and the
/// condition. A refusal costs only this instrument's row.
async fn scan_one(
    ctx: &ToolCtx,
    run: &RunState,
    job: &ScanJob<'_>,
    symbol: &str,
    found: &mut ScanFindings,
) {
    let listed = master(&ctx.app);
    let raw = Value::from(symbol.to_string());
    let pair = normalise_pair(&listed, Some(&raw), job.exchange);
    let (pair_symbol, pair_exchange, pair_notices) = match pair {
        Ok(p) => p,
        Err(refusal) => return found.failures.push(scan_failure(symbol, refusal.text())),
    };
    let key = (
        pair_symbol.clone(),
        pair_exchange.clone(),
        job.interval.to_string(),
        job.source.to_string(),
    );
    let evaluated = match frame(ctx, run, key, job.bars).await {
        Ok((data, _)) => scan_symbol(job.plan, &data, Some(job.arguments)).map_err(retry),
        Err(refusal) => Err(refusal),
    };
    let (hit, detail) = match evaluated {
        Ok(done) => done,
        Err(refusal) => return found.failures.push(scan_failure(symbol, refusal.text())),
    };
    found.notices.extend(pair_notices);
    let mut entry = Json::obj([
        ("symbol", Json::str(pair_symbol)),
        ("exchange", Json::str(pair_exchange)),
    ]);
    if let Json::Obj(fields) = detail {
        for (key, value) in fields {
            entry.set(&key, value);
        }
    }
    if hit {
        found.matched.push(entry.clone());
    }
    found.checked.push(entry);
}

/// The parts of a scan every instrument shares.
struct ScanJob<'a> {
    plan: &'a crate::agent::indicators::ScanPlan,
    exchange: Option<&'a Value>,
    interval: &'a str,
    source: &'a str,
    arguments: &'a Map<String, Value>,
    bars: usize,
}

/// `scan_symbols`: one indicator and one condition over up to fifteen
/// instruments; an instrument that cannot be read costs only its own row.
async fn scan(
    ctx: &ToolCtx,
    run: &RunState,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let raw = args.get("indicator").map(py_str).unwrap_or_default();
    let condition = args.get("condition").map(py_str).unwrap_or_default();
    let plan = plan_scan(&raw, &condition)?;
    let wanted = args::scan_symbols(args.get("symbols"))?;
    let source = normalise_source(args.get("source"))?;
    let accepted = run.intervals.accepted(ctx).await;
    let (interval, interval_notice) =
        normalise_interval(Some(&interval_arg(args)), &source, accepted.as_deref())?;
    let arguments = args::params(args.get("params"), "params")?;
    let job = ScanJob {
        plan: &plan,
        exchange: args.get("exchange"),
        interval: &interval,
        source: &source,
        arguments: &arguments,
        bars: required_bars(plan.spec, Some(&arguments), 5),
    };
    let mut found = ScanFindings {
        notices: interval_notice.into_iter().collect(),
        ..ScanFindings::default()
    };
    for symbol in &wanted {
        scan_one(ctx, run, &job, symbol, &mut found).await;
    }
    if found.checked.is_empty() {
        let first = match found.failures.first().and_then(|f| f.get("error")) {
            Some(Json::Str(reason)) => reason.clone(),
            _ => "no symbols were usable.".into(),
        };
        return Err(ToolOutcome::Retry(format!(
            "No instrument could be scanned. First failure: {}",
            first
        )));
    }
    let name = plan.spec.name;
    let count = found.matched.len();
    Ok(answer(
        "scan_symbols",
        &scan_payload(
            name, &condition, args, &interval, &source, &arguments, found,
        ),
        &[label("indicator", &name), label("matched", &count)],
    ))
}

/// The scan's answer, in the web's key order.
fn scan_payload(
    name: &str,
    condition: &str,
    args: &Map<String, Value>,
    interval: &str,
    source: &str,
    arguments: &Map<String, Value>,
    found: ScanFindings,
) -> Json {
    let mut payload = Json::obj([
        ("indicator", Json::str(name)),
        ("condition", Json::str(condition)),
        (
            "exchange",
            Json::str(text(args.get("exchange")).to_uppercase()),
        ),
        ("interval", Json::str(interval)),
        ("source", Json::str(source)),
        (
            "params_used",
            Json::from_value(&Value::Object(arguments.clone())),
        ),
        ("scanned", Json::Int(found.checked.len() as i64)),
        ("matched_count", Json::Int(found.matched.len() as i64)),
        ("matches", Json::Arr(found.matched)),
        ("all_results", Json::Arr(found.checked)),
    ]);
    if !found.failures.is_empty() {
        payload.set("failed", Json::Arr(found.failures));
    }
    if !found.notices.is_empty() {
        payload.set("notices", unique(&found.notices));
    }
    payload
}
