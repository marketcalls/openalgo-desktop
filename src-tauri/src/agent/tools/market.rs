//! Market data tools: quotes, depth, history and the broker's intervals
//! (the web's `services/agent/tools/market.py`, toolkit `market`).
//!
//! Every tool reads; none needs approval or writes an audit row, and all
//! are offered on every surface with trading off. Results leave through
//! [`json_result`] as a `<tool_result tool=... symbol=... exchange=...>`
//! block; corrections the tool made to the arguments travel in `notices`.
//!
//! The normalisers ([`normalise`]) and the history readings ([`candles`])
//! are shared with the instrument, live, chart, indicator and viz tools.

pub mod candles;
pub mod normalise;
#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::tools::answer::{json_result, label, Label};
use crate::agent::tools::specs;
use crate::state::AppState;
use async_trait::async_trait;
use candles::{candle, candle_columns, fit_to_budget, roll_up_candles, summarise_candles};
use normalise::{
    history_range, normalise_pair, normalise_source, plan_interval, quote_pairs, today, Listed,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The platform states every timestamp in IST.
pub const IST: chrono_tz::Tz = chrono_tz::Asia::Kolkata;

/// Every OpenAlgo exchange code, in the web's `VALID_EXCHANGES` order.
pub const VALID_EXCHANGES: [&str; 14] = [
    "NSE",
    "NFO",
    "CDS",
    "BSE",
    "BFO",
    "BCD",
    "MCX",
    "NCDEX",
    "NCO",
    "NSE_INDEX",
    "BSE_INDEX",
    "MCX_INDEX",
    "GLOBAL_INDEX",
    "CRYPTO",
];

/// The quote-only exchanges an index is listed on, in the order they are
/// tried.
pub const INDEX_EXCHANGES: [&str; 4] = ["NSE_INDEX", "BSE_INDEX", "MCX_INDEX", "GLOBAL_INDEX"];

/// Most symbols one `get_quotes` call may carry.
pub const MAX_MULTIQUOTE_SYMBOLS: usize = 50;

/// Most candles `get_history` returns before the character budget.
pub const MAX_HISTORY_ROWS: usize = 200;

/// The characters a self-sized payload may take, under the result cap so
/// it is never truncated.
pub const JSON_BUDGET: usize = MAX_RESULT_CHARS - 256;

/// Candles a history call covers when no start date is named.
pub const DEFAULT_HISTORY_BARS: u32 = 100;

/// Ceiling on the calendar span of one history request (the web's
/// `indicator_service.MAX_HISTORY_CALENDAR_DAYS`).
pub const MAX_HISTORY_CALENDAR_DAYS: i64 = 4000;

/// Month abbreviations in calendar order.
pub const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

/// The instrument master's answer to "is this pair listed" (the web's
/// `is_listed`), read from one snapshot for the whole call.
pub fn master(app: &AppState) -> impl Fn(&str, &str) -> bool + Send + Sync {
    let snap = app.symbols.snapshot();
    move |symbol: &str, exchange: &str| snap.by_symbol(exchange, symbol).is_some()
}

/// The connected broker's intervals, fetched at most once per run (the
/// web's `BrokerIntervals`): held by the run's toolkit, so a broker change
/// is picked up next turn. A failed lookup is remembered and reported, and
/// the interval check is then skipped rather than refusing a read.
#[derive(Debug, Default)]
pub struct BrokerIntervals {
    state: Mutex<Option<Loaded>>,
}

#[derive(Debug, Clone, Default)]
struct Loaded {
    payload: Option<Value>,
    accepted: Option<Vec<String>>,
    error: Option<String>,
}

impl BrokerIntervals {
    /// The grouped list for `get_intervals`, fetched once; the first
    /// failure is returned, later calls report nothing new.
    pub async fn payload(&self, ctx: &ToolCtx) -> Result<Option<Value>, ToolOutcome> {
        let mut state = self.state.lock().await;
        if let Some(loaded) = state.as_ref() {
            return Ok(loaded.payload.clone());
        }
        let mut loaded = Loaded::default();
        let reply = ctx
            .service_read("intervals_service.get_intervals", "intervals", Map::new())
            .await;
        let response = match reply {
            Ok(r) => r,
            Err(failure) => {
                loaded.error = Some(failure.text().to_string());
                *state = Some(loaded);
                return Err(failure);
            }
        };
        if let Some(grouped) = response.get("data").and_then(Value::as_object) {
            let accepted: Vec<String> = grouped
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .map(crate::agent::tools::values::py_str)
                .collect();
            loaded.payload = Some(json!({"intervals": grouped, "accepted": accepted}));
            loaded.accepted = Some(accepted);
        }
        let payload = loaded.payload.clone();
        *state = Some(loaded);
        Ok(payload)
    }

    /// The flat accepted list, or `None` when unknown (the lookup failed:
    /// logged, and the check skipped).
    pub async fn accepted(&self, ctx: &ToolCtx) -> Option<Vec<String>> {
        if let Some(loaded) = self.state.lock().await.as_ref() {
            return loaded.accepted.clone();
        }
        if let Err(e) = self.payload(ctx).await {
            tracing::warn!(
                "Agent market tool could not read the broker's intervals; skipping the interval check for this run: {}",
                e.text()
            );
        }
        self.state
            .lock()
            .await
            .as_ref()
            .and_then(|l| l.accepted.clone())
    }

    async fn error(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .as_ref()
            .and_then(|l| l.error.clone())
    }
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "market",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 10,
    description: "Quotes, depth, history and intervals.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    Quote,
    Quotes,
    Depth,
    History,
    Intervals,
}

struct MarketTool {
    spec: ToolSpec,
    kind: Kind,
    intervals: Arc<BrokerIntervals>,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let intervals = Arc::new(BrokerIntervals::default());
    [
        ("get_quote", Kind::Quote),
        ("get_quotes", Kind::Quotes),
        ("get_depth", Kind::Depth),
        ("get_history", Kind::History),
        ("get_intervals", Kind::Intervals),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("market", name)?;
        Some(Arc::new(MarketTool {
            spec,
            kind,
            intervals: intervals.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for MarketTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let listed = master(&ctx.app);
        let done = match self.kind {
            Kind::Quote => quote_or_depth(ctx, &listed, &args, false).await,
            Kind::Depth => quote_or_depth(ctx, &listed, &args, true).await,
            Kind::Quotes => get_quotes(ctx, &listed, &args).await,
            Kind::History => get_history(ctx, &listed, &args, &self.intervals).await,
            Kind::Intervals => get_intervals(ctx, &self.intervals).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// Notices about corrections, attached when there are any.
pub fn note(payload: &mut Map<String, Value>, notices: &[String]) {
    if !notices.is_empty() {
        payload.insert("notices".into(), json!(notices));
    }
}

fn pair_payload(symbol: &str, exchange: &str) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("symbol".into(), json!(symbol));
    p.insert("exchange".into(), json!(exchange));
    p
}

/// `get_quote` and `get_depth`: one pair, its service's `data`.
async fn quote_or_depth(
    ctx: &ToolCtx,
    listed: Listed<'_>,
    args: &Map<String, Value>,
    depth: bool,
) -> Result<ToolOutcome, ToolOutcome> {
    let (symbol, exchange, notices) =
        normalise_pair(listed, args.get("symbol"), args.get("exchange"))?;
    let (label_name, endpoint, tool, field) = match depth {
        false => ("quotes_service.get_quotes", "quotes", "get_quote", "quote"),
        true => ("depth_service.get_depth", "depth", "get_depth", "depth"),
    };
    let response = ctx
        .service_read(label_name, endpoint, pair_payload(&symbol, &exchange))
        .await?;
    let mut payload = pair_payload(&symbol, &exchange);
    payload.insert(field.into(), data_of(response));
    note(&mut payload, &notices);
    Ok(json_result(
        tool,
        &Value::Object(payload),
        &[label("symbol", &symbol), label("exchange", &exchange)],
    ))
}

/// A service response's `data`, or the response itself when it is not an
/// object (the web's `response.get("data") if Mapping else response`).
pub fn data_of(response: Value) -> Value {
    match response {
        Value::Object(mut m) => m.remove("data").unwrap_or(Value::Null),
        other => other,
    }
}

async fn get_quotes(
    ctx: &ToolCtx,
    listed: Listed<'_>,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let (pairs, notices) = quote_pairs(listed, args.get("symbols"))?;
    let symbols: Vec<Value> = pairs
        .iter()
        .map(|(s, e)| Value::Object(pair_payload(s, e)))
        .collect();
    let mut request = Map::new();
    request.insert("symbols".into(), Value::Array(symbols));
    let response = ctx
        .service_read("quotes_service.get_multiquotes", "multiquotes", request)
        .await?;
    let results = match response.get("results") {
        Some(Value::Array(r)) => r.clone(),
        None | Some(Value::Null) => Vec::new(),
        Some(other) => vec![other.clone()],
    };
    let total = results.len();
    let payload = fit_to_budget(
        |limit| {
            let mut p = Map::new();
            p.insert("requested".into(), json!(pairs.len()));
            p.insert("results_returned".into(), json!(limit.min(total)));
            p.insert(
                "results".into(),
                Value::Array(results[..limit.min(total)].to_vec()),
            );
            if total > limit {
                p.insert("results_omitted".into(), json!(total - limit));
                p.insert(
                    "note".into(),
                    json!(format!(
                        "{} of {} results were dropped to fit the reply. Ask for fewer symbols per call to see them all.",
                        total - limit,
                        total
                    )),
                );
            }
            note(&mut p, &notices);
            Value::Object(p)
        },
        total,
    );
    Ok(json_result(
        "get_quotes",
        &payload,
        &[label("count", pairs.len())],
    ))
}

async fn get_history(
    ctx: &ToolCtx,
    listed: Listed<'_>,
    args: &Map<String, Value>,
    intervals: &BrokerIntervals,
) -> Result<ToolOutcome, ToolOutcome> {
    let (symbol, exchange, mut notices) =
        normalise_pair(listed, args.get("symbol"), args.get("exchange"))?;
    let source = normalise_source(args.get("source"))?;
    let accepted = intervals.accepted(ctx).await;
    let default_interval = Value::from("D");
    let raw_interval = match args.get("interval") {
        None => Some(&default_interval),
        given => given,
    };
    let (fetched, interval_notice, rollup) =
        plan_interval(raw_interval, &source, accepted.as_deref())?;
    notices.extend(interval_notice);
    let interval = rollup.clone().unwrap_or_else(|| fetched.clone());
    let (start, end) = history_range(
        &interval,
        args.get("start_date"),
        args.get("end_date"),
        DEFAULT_HISTORY_BARS,
        today(&ctx.app),
    )?;
    let mut request = pair_payload(&symbol, &exchange);
    request.insert("interval".into(), json!(fetched));
    request.insert("start_date".into(), json!(start));
    request.insert("end_date".into(), json!(end));
    request.insert("source".into(), json!(source));
    let response = ctx
        .service_read("history_service.get_history", "history", request)
        .await?;
    let rows = match data_of(response) {
        Value::Array(rows) => rows,
        _ => Vec::new(),
    };
    let mut records: Vec<Map<String, Value>> = rows
        .into_iter()
        .filter_map(|r| match r {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect();
    let columns = records.first().map(candle_columns).unwrap_or_default();
    if let Some(target) = rollup.as_deref() {
        records = roll_up_candles(&records, &columns, target);
    }
    let total = records.len();
    let summary = summarise_candles(&records, &columns);
    let head = HistoryHead {
        symbol: &symbol,
        exchange: &exchange,
        interval: &interval,
        start: &start,
        end: &end,
        source: &source,
    };
    let payload = fit_to_budget(
        |limit| history_payload(&head, &records, &columns, &summary, &notices, limit),
        total.min(MAX_HISTORY_ROWS),
    );
    let labels: [Label<'_>; 3] = [
        label("symbol", &symbol),
        label("exchange", &exchange),
        label("interval", &interval),
    ];
    Ok(json_result("get_history", &payload, &labels))
}

struct HistoryHead<'a> {
    symbol: &'a str,
    exchange: &'a str,
    interval: &'a str,
    start: &'a str,
    end: &'a str,
    source: &'a str,
}

fn history_payload(
    head: &HistoryHead<'_>,
    records: &[Map<String, Value>],
    columns: &candles::Columns,
    summary: &Map<String, Value>,
    notices: &[String],
    limit: usize,
) -> Value {
    let total = records.len();
    let tail = &records[total - limit.min(total)..];
    let omitted = total - tail.len();
    let mut p = Map::new();
    p.insert("symbol".into(), json!(head.symbol));
    p.insert("exchange".into(), json!(head.exchange));
    p.insert("interval".into(), json!(head.interval));
    p.insert("start_date".into(), json!(head.start));
    p.insert("end_date".into(), json!(head.end));
    p.insert("source".into(), json!(head.source));
    p.insert("timezone".into(), json!("Asia/Kolkata"));
    p.insert("rows_total".into(), json!(total));
    p.insert("rows_returned".into(), json!(tail.len()));
    p.insert("rows_omitted".into(), json!(omitted));
    p.insert("summary".into(), Value::Object(summary.clone()));
    p.insert(
        "candles".into(),
        Value::Array(tail.iter().map(|r| candle(r, columns)).collect()),
    );
    if omitted > 0 {
        p.insert(
            "note".into(),
            json!(format!(
                "{} older candles of {} were omitted; 'candles' holds the most recent {}, oldest first. The summary covers all of them. Narrow the dates or use a larger interval to see more.",
                omitted,
                total,
                tail.len()
            )),
        );
    } else if total == 0 {
        p.insert(
            "note".into(),
            json!("The range returned no candles. Check the dates against the trading calendar and confirm the interval is one this broker supports."),
        );
    }
    note(&mut p, notices);
    Value::Object(p)
}

async fn get_intervals(
    ctx: &ToolCtx,
    intervals: &BrokerIntervals,
) -> Result<ToolOutcome, ToolOutcome> {
    let payload = match intervals.payload(ctx).await? {
        Some(p) => p,
        None => {
            let mut p = Map::new();
            p.insert("intervals".into(), json!({}));
            p.insert("accepted".into(), json!([]));
            p.insert(
                "note".into(),
                json!("The broker did not return a usable interval list. Try a common interval such as 'D' or '5m' with get_history and report the error if it is rejected."),
            );
            if let Some(error) = intervals.error().await {
                p.insert("error".into(), json!(error));
            }
            Value::Object(p)
        }
    };
    Ok(json_result("get_intervals", &payload, &[]))
}
