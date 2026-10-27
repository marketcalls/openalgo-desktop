//! The instrument card (the web's `services/agent/tools/instrument.py`,
//! toolkit `instrument`): one tool, `show_instrument`, that draws a full card
//! for one symbol on the run's viz side channel (kind `instrument`) and
//! answers the model in one or two lines.
//!
//! The card carries the quote and the day's move, an intraday chart, the 52
//! week range, the top of the order book and the trader's own position in
//! that exact instrument. Every optional section is gathered on its own: one
//! that fails is named under `unavailable` with a short reason and left out,
//! never drawn as a placeholder. There are no fundamentals, by design: the
//! platform has no source for them.

#[cfg(test)]
mod tests;

use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::account::{current_mode, looks_flat, CURRENCY};
use crate::agent::tools::answer::{label, tool_answer};
use crate::agent::tools::market::candles::{
    candle_columns, chart_bars, iso_ist, summarise_candles,
};
use crate::agent::tools::market::normalise::{
    daily_interval, normalise_interval, normalise_pair, today,
};
use crate::agent::tools::market::{data_of, master, BrokerIntervals, IST};
use crate::agent::tools::numbers::{as_number, format_number, format_price, price, py_round};
use crate::agent::tools::options::normalise_int;
use crate::agent::tools::specs;
use crate::agent::tools::symbols::{DERIVATIVE_EXCHANGES, INDEX_EXCHANGES};
use crate::agent::tools::values::truthy_text;
use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The renderer this tool emits.
pub const INSTRUMENT_VIZ: &str = "instrument";
/// The intraday candle size when the model names none.
pub const DEFAULT_INTRADAY_INTERVAL: &str = "5m";
/// Sessions of intraday history by default.
pub const DEFAULT_SESSIONS: i64 = 1;
/// Most sessions one card spans.
pub const MAX_SESSIONS: i64 = 5;
/// Most bars on the intraday strip.
pub const MAX_INSTRUMENT_BARS: usize = 500;
/// Calendar days of daily history for the 52 week range.
pub const WEEK_52_LOOKBACK_DAYS: i64 = 372;
/// Levels a side of the order book.
pub const MAX_DEPTH_LEVELS: usize = 5;
/// Characters of a failure kept as the reason a section is missing.
pub const MAX_REASON_CHARS: usize = 300;

/// The quote fields the platform returns, in order.
const QUOTE_FIELDS: [&str; 9] = [
    "ltp",
    "open",
    "high",
    "low",
    "prev_close",
    "volume",
    "bid",
    "ask",
    "oi",
];

/// `part` as a percentage of `whole`, two places; `None` for a missing
/// part or a zero or missing whole.
pub fn percent_change(part: Option<f64>, whole: Option<f64>) -> Option<f64> {
    let (part, whole) = (part?, whole?);
    (whole != 0.0).then(|| py_round(part / whole * 100.0, 2))
}

/// The nine quote fields as numbers; a field that is exactly zero is an
/// absence and dropped, except the last traded price.
pub fn normalise_quote(raw: &Value) -> Map<String, Value> {
    let mut quote = Map::new();
    for field in QUOTE_FIELDS {
        let Some(value) = raw.get(field).and_then(as_number) else {
            continue;
        };
        if value == 0.0 && field != "ltp" {
            continue;
        }
        quote.insert(field.into(), json!(value));
    }
    quote
}

/// The day's move (`change`, `change_percent`) when both are meaningful; a
/// last traded price of zero means nothing has printed, said in a notice
/// (naming `symbol` when given) rather than shown as a fall.
pub fn quote_move(
    quote: &Map<String, Value>,
    notices: &mut Vec<String>,
    symbol: &str,
) -> Map<String, Value> {
    let mut out = Map::new();
    let ltp = quote.get("ltp").and_then(as_number);
    let previous = quote
        .get("prev_close")
        .and_then(as_number)
        .filter(|p| *p != 0.0);
    let (Some(ltp), Some(previous)) = (ltp, previous) else {
        return out;
    };
    if ltp == 0.0 {
        let subject = if symbol.is_empty() {
            "The last traded price".to_string()
        } else {
            format!("{}'s last traded price", symbol)
        };
        notices.push(format!(
            "{} is zero, so no trade has printed yet and the day's change is not stated.",
            subject
        ));
        return out;
    }
    let change = py_round(ltp - previous, 4);
    out.insert("change".into(), json!(change));
    out.insert(
        "change_percent".into(),
        json!(percent_change(Some(change), Some(previous))),
    );
    out
}

/// One side of the order book: padding levels (no price and no quantity)
/// dropped, at most five, best first.
pub fn depth_levels(raw: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(entries)) = raw else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|e| e.is_object())
        .filter_map(|e| {
            let price = e.get("price").and_then(as_number);
            let quantity = e.get("quantity").and_then(as_number);
            let empty = |v: Option<f64>| v.is_none_or(|x| x == 0.0);
            (!(empty(price) && empty(quantity)))
                .then(|| json!({"price": price, "quantity": quantity}))
        })
        .take(MAX_DEPTH_LEVELS)
        .collect()
}

/// The IST session date of a bar's UTC epoch `time`.
fn session_date(time: &Value) -> String {
    as_number(time)
        .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
        .map(|t| {
            t.with_timezone(&IST)
                .date_naive()
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

fn sessions_of(bars: &[Map<String, Value>]) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::new();
    for bar in bars {
        let day = session_date(bar.get("time").unwrap_or(&Value::Null));
        if !day.is_empty() && ordered.last() != Some(&day) {
            ordered.push(day);
        }
    }
    ordered
}

/// The calendar date of an ISO timestamp a summary rendered.
fn day_part(value: Option<&Value>) -> Option<String> {
    let head = value?.as_str()?.get(..10)?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()?;
    Some(head.to_string())
}

/// Why a section is missing, for a person: the first sentence, capped.
fn reason(failure: &ToolOutcome) -> String {
    let t = failure.text().replace('\n', " ").trim().to_string();
    let first = match t.split_once(". ") {
        Some((head, _)) => format!("{}.", head),
        None => t,
    };
    first.chars().take(MAX_REASON_CHARS).collect()
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "instrument",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 44,
    description: "Draw a full instrument card: quote, the day's move, an intraday chart, the 52 week range, the order book and the operator's own position in it.",
    build,
};

struct InstrumentTool {
    spec: ToolSpec,
    intervals: Arc<BrokerIntervals>,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    specs::web_or_log("instrument", "show_instrument")
        .map(|spec| {
            Arc::new(InstrumentTool {
                spec,
                intervals: Arc::new(BrokerIntervals::default()),
            }) as Arc<dyn AgentTool>
        })
        .into_iter()
        .collect()
}

#[async_trait]
impl AgentTool for InstrumentTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        show_instrument(ctx, &args, &self.intervals)
            .await
            .unwrap_or_else(|refusal| refusal)
    }
}

/// What a section gather yields: the section, or `None` with why.
type Section = Result<(Option<Value>, Option<String>), ToolOutcome>;

/// One card's request: the instrument and how much intraday history.
struct Card<'a> {
    symbol: &'a str,
    exchange: &'a str,
    interval: &'a str,
    sessions: i64,
    today: NaiveDate,
}

async fn show_instrument(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    intervals: &BrokerIntervals,
) -> Result<ToolOutcome, ToolOutcome> {
    let listed = master(&ctx.app);
    let (symbol, exchange, mut notices) =
        normalise_pair(&listed, args.get("symbol"), args.get("exchange"))?;
    let sessions = match args.get("days") {
        None => DEFAULT_SESSIONS,
        given => normalise_int(given, "days", 1, MAX_SESSIONS, Some(DEFAULT_SESSIONS))?,
    };
    let accepted = intervals.accepted(ctx).await;
    let default_interval = json!(DEFAULT_INTRADAY_INTERVAL);
    let raw_interval = args.get("interval").or(Some(&default_interval));
    let (interval, interval_notice) = normalise_interval(raw_interval, "api", accepted.as_deref())?;
    notices.extend(interval_notice);
    let labels = [label("symbol", &symbol), label("exchange", &exchange)];
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    // The quote is the one hard requirement: no quote, no card.
    let response = ctx
        .service_read("quotes_service.get_quotes", "quotes", request)
        .await?;
    let quote = normalise_quote(&data_of(response));
    let Some(ltp) = quote.get("ltp").and_then(as_number) else {
        let message = format!(
            "The quote for {} on {} came back with no last traded price, so no card was drawn. Say the price could not be read rather than reporting a number, and check the symbol is one this broker quotes.",
            symbol, exchange
        );
        return Ok(tool_answer("show_instrument", &message, &[], &labels));
    };
    let mut spec = card_head(ctx, &symbol, &exchange, quote, &mut notices);
    let card = Card {
        symbol: &symbol,
        exchange: &exchange,
        interval: &interval,
        sessions,
        today: today(&ctx.app),
    };
    let (unavailable, sources) = gather(ctx, &card, ltp, accepted.as_deref(), &mut spec).await;
    if !notices.is_empty() {
        spec.insert("notices".into(), json!(notices));
    }
    let safe = match json_safe(&Value::Object(spec.clone())) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let title = format!("{} {}", symbol, exchange);
    if !ctx.viz.emit(
        "show_instrument",
        INSTRUMENT_VIZ,
        safe,
        &title,
        &sources.join(", "),
    ) {
        let message = no_sink_message("instrument card");
        return Ok(tool_answer("show_instrument", &message, &[], &labels));
    }
    let message = confirmation(&spec, &unavailable);
    Ok(tool_answer("show_instrument", &message, &[], &labels))
}

/// The card's head: the instrument, the account it was read from, the
/// quote and the day's move.
fn card_head(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
    quote: Map<String, Value>,
    notices: &mut Vec<String>,
) -> Map<String, Value> {
    let mut spec = Map::new();
    spec.insert("symbol".into(), json!(symbol));
    spec.insert("exchange".into(), json!(exchange));
    spec.insert("currency".into(), json!(CURRENCY));
    spec.insert("mode".into(), json!(current_mode(ctx)));
    spec.insert("as_of".into(), json!(iso_ist(whole_seconds(ctx.app.now()))));
    spec.insert("timezone".into(), json!("Asia/Kolkata"));
    spec.insert(
        "is_derivative".into(),
        json!(DERIVATIVE_EXCHANGES.contains(&exchange)),
    );
    spec.insert(
        "is_index".into(),
        json!(INDEX_EXCHANGES.contains(&exchange)),
    );
    let movement = quote_move(&quote, notices, "");
    spec.insert("quote".into(), Value::Object(quote));
    spec.extend(movement);
    spec
}

/// Every optional section, each gathered on its own: one that fails costs
/// itself and is named with a reason under `unavailable`; a service is
/// claimed as a source only once it answered. Returns the missing section
/// names and the sources.
async fn gather(
    ctx: &ToolCtx,
    card: &Card<'_>,
    ltp: f64,
    accepted: Option<&[String]>,
    spec: &mut Map<String, Value>,
) -> (Vec<String>, Vec<&'static str>) {
    let mut unavailable = Map::new();
    let mut sources = vec!["quotes_service"];
    let gathered = [
        (
            "instrument",
            "symbol_service",
            master_section(ctx, card).await,
        ),
        (
            "intraday",
            "history_service",
            intraday_section(ctx, card).await,
        ),
        (
            "week_52",
            "history_service",
            week_52_section(ctx, card, ltp, accepted).await,
        ),
        ("depth", "depth_service", depth_section(ctx, card).await),
        (
            "position",
            "positionbook_service",
            position_section(ctx, card).await,
        ),
    ];
    for (name, service, result) in gathered {
        match result {
            Ok((Some(section), _)) => {
                spec.insert(name.into(), section);
                if !sources.contains(&service) {
                    sources.push(service);
                }
            }
            Ok((None, why)) => {
                let why =
                    why.unwrap_or_else(|| "the platform returned nothing for this section".into());
                unavailable.insert(name.into(), json!(why));
            }
            Err(failure) => {
                tracing::warn!("Instrument card: the {} section failed for the card", name);
                unavailable.insert(name.into(), json!(reason(&failure)));
            }
        }
    }
    let names: Vec<String> = unavailable.keys().cloned().collect();
    if !unavailable.is_empty() {
        spec.insert("unavailable".into(), Value::Object(unavailable));
    }
    (names, sources)
}

/// An instant to whole seconds (Python's `isoformat(timespec="seconds")`).
fn whole_seconds(t: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(t.timestamp(), 0).unwrap_or(t)
}

fn pair_request(card: &Card<'_>) -> Map<String, Value> {
    let mut request = Map::new();
    request.insert("symbol".into(), json!(card.symbol));
    request.insert("exchange".into(), json!(card.exchange));
    request
}

/// The contract's own details from the master: name, type, expiry, and the
/// strike, lot size and tick that apply to it (a zero means "does not
/// apply" and is dropped; the freeze quantity is not a price fact).
async fn master_section(ctx: &ToolCtx, card: &Card<'_>) -> Section {
    let payload = ctx
        .service_read(
            "symbol_service.get_symbol_info",
            "symbol",
            pair_request(card),
        )
        .await?;
    let info = match data_of(payload) {
        Value::Object(m) if !m.is_empty() => m,
        _ => {
            return Ok((
                None,
                Some("the instrument master holds no row for this symbol".into()),
            ))
        }
    };
    let mut section = Map::new();
    for (key, field) in [
        ("name", "name"),
        ("instrumenttype", "instrument_type"),
        ("expiry", "expiry"),
    ] {
        if let Some(Value::String(s)) = info.get(key) {
            if !s.trim().is_empty() {
                section.insert(field.into(), json!(s.trim()));
            }
        }
    }
    for (key, field) in [
        ("strike", "strike"),
        ("lotsize", "lot_size"),
        ("tick_size", "tick_size"),
    ] {
        let Some(value) = info.get(key).and_then(as_number).filter(|v| *v != 0.0) else {
            continue;
        };
        let shown = if field == "lot_size" && value.fract() == 0.0 {
            json!(value as i64)
        } else {
            json!(value)
        };
        section.insert(field.into(), shown);
    }
    if section.is_empty() {
        return Ok((
            None,
            Some("the instrument master row carried no usable detail".into()),
        ));
    }
    Ok((Some(Value::Object(section)), None))
}

async fn history_rows(
    ctx: &ToolCtx,
    card: &Card<'_>,
    interval: &str,
    start: NaiveDate,
) -> Result<Vec<Map<String, Value>>, ToolOutcome> {
    let mut request = pair_request(card);
    request.insert("interval".into(), json!(interval));
    request.insert(
        "start_date".into(),
        json!(start.format("%Y-%m-%d").to_string()),
    );
    request.insert(
        "end_date".into(),
        json!(card.today.format("%Y-%m-%d").to_string()),
    );
    request.insert("source".into(), json!("api"));
    let response = ctx
        .service_read("history_service.get_history", "history", request)
        .await?;
    Ok(match data_of(response) {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// The intraday price and volume series: a window of calendar days, the
/// most recent sessions kept, capped at the most recent bars.
async fn intraday_section(ctx: &ToolCtx, card: &Card<'_>) -> Section {
    let start = card.today - Duration::days(7.max(card.sessions * 4));
    let records = history_rows(ctx, card, card.interval, start).await?;
    let columns = records.first().map(candle_columns).unwrap_or_default();
    let bars = chart_bars(&records, &columns);
    if bars.is_empty() {
        return Ok((
            None,
            Some(format!(
                "the broker returned no {} candles between {} and {}",
                card.interval,
                start.format("%Y-%m-%d"),
                card.today.format("%Y-%m-%d")
            )),
        ));
    }
    let ordered = sessions_of(&bars);
    let keep = ordered.len().saturating_sub(card.sessions as usize);
    let kept = &ordered[keep..];
    let mut selected: Vec<Map<String, Value>> = if kept.is_empty() {
        bars
    } else {
        bars.into_iter()
            .filter(|b| kept.contains(&session_date(b.get("time").unwrap_or(&Value::Null))))
            .collect()
    };
    let omitted = selected.len().saturating_sub(MAX_INSTRUMENT_BARS);
    if omitted > 0 {
        selected.drain(..omitted);
    }
    let first = selected
        .first()
        .and_then(|b| b.get("time"))
        .cloned()
        .unwrap_or(Value::Null);
    let last = selected
        .last()
        .and_then(|b| b.get("time"))
        .cloned()
        .unwrap_or(Value::Null);
    let has_volume = selected.iter().any(|b| {
        b.get("volume")
            .is_some_and(crate::agent::tools::values::truthy)
    });
    let mut section = Map::new();
    section.insert("interval".into(), json!(card.interval));
    section.insert("sessions".into(), json!(sessions_of(&selected)));
    section.insert("bar_count".into(), json!(selected.len()));
    section.insert(
        "bars".into(),
        Value::Array(selected.into_iter().map(Value::Object).collect()),
    );
    section.insert("first_time".into(), first);
    section.insert("last_time".into(), last);
    section.insert("has_volume".into(), json!(has_volume));
    if omitted > 0 {
        section.insert("bars_omitted".into(), json!(omitted));
    }
    Ok((Some(Value::Object(section)), None))
}

/// The 52 week high and low from daily candles, where the price sits in
/// that range, and whether the instrument was listed for most of the year.
async fn week_52_section(
    ctx: &ToolCtx,
    card: &Card<'_>,
    ltp: f64,
    accepted: Option<&[String]>,
) -> Section {
    let start = card.today - Duration::days(WEEK_52_LOOKBACK_DAYS);
    let interval = daily_interval(accepted);
    let records = history_rows(ctx, card, &interval, start).await?;
    let columns = records.first().map(candle_columns).unwrap_or_default();
    let summary = summarise_candles(&records, &columns);
    let high = summary.get("highest_high").and_then(as_number);
    let low = summary.get("lowest_low").and_then(as_number);
    let (Some(high), Some(low)) = (high, low) else {
        return Ok((
            None,
            Some(format!(
                "the broker returned no daily candles between {} and {}",
                start.format("%Y-%m-%d"),
                card.today.format("%Y-%m-%d")
            )),
        ));
    };
    let first_date = day_part(summary.get("first_timestamp"));
    let full_year = first_date
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .is_some_and(|d| d <= card.today - Duration::days(330));
    let mut section = Map::new();
    section.insert("high".into(), json!(high));
    section.insert("low".into(), json!(low));
    section.insert(
        "start_date".into(),
        json!(start.format("%Y-%m-%d").to_string()),
    );
    section.insert(
        "end_date".into(),
        json!(card.today.format("%Y-%m-%d").to_string()),
    );
    section.insert("bar_count".into(), json!(records.len()));
    section.insert("full_year".into(), json!(full_year));
    let optional = [
        ("first_date", first_date.map(Value::from)),
        (
            "last_date",
            day_part(summary.get("last_timestamp")).map(Value::from),
        ),
        (
            "high_date",
            day_part(summary.get("highest_high_timestamp")).map(Value::from),
        ),
        (
            "low_date",
            day_part(summary.get("lowest_low_timestamp")).map(Value::from),
        ),
        (
            "position_percent",
            percent_change(Some(ltp - low), Some(high - low)).map(Value::from),
        ),
        (
            "from_high_percent",
            percent_change(Some(ltp - high), Some(high)).map(Value::from),
        ),
        (
            "from_low_percent",
            percent_change(Some(ltp - low), Some(low)).map(Value::from),
        ),
    ];
    for (field, value) in optional {
        if let Some(v) = value {
            section.insert(field.into(), v);
        }
    }
    Ok((Some(Value::Object(section)), None))
}

/// Both sides of the resting book and the spread against the mid.
async fn depth_section(ctx: &ToolCtx, card: &Card<'_>) -> Section {
    let response = ctx
        .service_read("depth_service.get_depth", "depth", pair_request(card))
        .await?;
    let data = data_of(response);
    if !data.is_object() {
        return Ok((
            None,
            Some("the broker returned no order book for this instrument".into()),
        ));
    }
    let bids = depth_levels(data.get("bids"));
    let asks = depth_levels(data.get("asks"));
    if bids.is_empty() && asks.is_empty() {
        return Ok((
            None,
            Some("there are no resting orders on either side".into()),
        ));
    }
    let best = |side: &[Value]| {
        side.first()
            .and_then(|l| l.get("price"))
            .and_then(as_number)
    };
    let (best_bid, best_ask) = (best(&bids), best(&asks));
    let mut section = Map::new();
    section.insert("bids".into(), Value::Array(bids));
    section.insert("asks".into(), Value::Array(asks));
    for (field, key) in [
        ("total_buy_quantity", "totalbuyqty"),
        ("total_sell_quantity", "totalsellqty"),
    ] {
        if let Some(v) = data.get(key).and_then(as_number) {
            section.insert(field.into(), json!(v));
        }
    }
    if let (Some(bid), Some(ask)) = (
        best_bid.filter(|b| *b != 0.0),
        best_ask.filter(|a| *a != 0.0),
    ) {
        if ask > bid {
            let spread = py_round(ask - bid, 4);
            section.insert("spread".into(), json!(spread));
            if let Some(p) = percent_change(Some(spread), Some((ask + bid) / 2.0)) {
                section.insert("spread_percent".into(), json!(p));
            }
        }
    }
    Ok((Some(Value::Object(section)), None))
}

/// What the trader holds in this exact instrument, across products: the
/// whole position book matched on symbol and exchange, zero quantities
/// dropped, an aggregate only when every leg supplied it. Holding nothing
/// (`{"held": false}`) and not knowing (no section) are different answers.
async fn position_section(ctx: &ToolCtx, card: &Card<'_>) -> Section {
    let payload = ctx
        .service_read(
            "positionbook_service.get_positionbook",
            "positionbook",
            Map::new(),
        )
        .await?;
    let Value::Array(rows) = data_of(payload) else {
        return Ok((
            None,
            Some("the broker returned the position book in an unrecognised shape".into()),
        ));
    };
    let mut legs: Vec<Map<String, Value>> = Vec::new();
    for row in rows.iter().filter(|r| r.is_object()) {
        if truthy_text(row.get("symbol")).to_uppercase() != card.symbol
            || truthy_text(row.get("exchange")).to_uppercase() != card.exchange
            || looks_flat(row.get("quantity").unwrap_or(&Value::Null))
        {
            continue;
        }
        let mut leg = Map::new();
        if let Some(Value::String(p)) = row.get("product") {
            if !p.trim().is_empty() {
                leg.insert("product".into(), json!(p.trim().to_uppercase()));
            }
        }
        for field in ["quantity", "average_price", "ltp", "pnl"] {
            if let Some(v) = row.get(field).and_then(as_number) {
                leg.insert(field.into(), json!(v));
            }
        }
        legs.push(leg);
    }
    if legs.is_empty() {
        return Ok((Some(json!({"held": false})), None));
    }
    Ok((Some(Value::Object(position_summary(legs))), None))
}

fn position_summary(legs: Vec<Map<String, Value>>) -> Map<String, Value> {
    let sum = |field: &str| -> Option<f64> {
        legs.iter()
            .map(|l| l.get(field).and_then(as_number))
            .sum::<Option<f64>>()
    };
    let quantity = sum("quantity");
    let pnl = sum("pnl").map(|p| py_round(p, 2));
    let single_average = match legs.as_slice() {
        [only] => only.get("average_price").and_then(as_number),
        _ => None,
    };
    let single = legs.len() == 1;
    let mut section = Map::new();
    section.insert("held".into(), json!(true));
    section.insert(
        "legs".into(),
        Value::Array(legs.into_iter().map(Value::Object).collect()),
    );
    if let Some(q) = quantity {
        section.insert("quantity".into(), json!(q));
        if q != 0.0 {
            section.insert("side".into(), json!(if q > 0.0 { "long" } else { "short" }));
        }
    }
    if let Some(p) = pnl {
        section.insert("pnl".into(), json!(p));
    }
    if single {
        if let Some(average) = single_average {
            section.insert("average_price".into(), json!(average));
            if let (Some(p), Some(q)) = (pnl, quantity.filter(|q| *q != 0.0)) {
                if average != 0.0 {
                    if let Some(percent) = percent_change(Some(p), Some(q.abs() * average)) {
                        section.insert("pnl_percent".into(), json!(percent));
                    }
                }
            }
        }
    }
    section
}

/// The one or two lines the model receives: the price and the move, the
/// trader's position, what is missing, and that the card is on screen.
fn confirmation(spec: &Map<String, Value>, unavailable: &[String]) -> String {
    let empty = Value::Null;
    let quote = spec.get("quote").unwrap_or(&empty);
    let symbol = spec
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let exchange = spec
        .get("exchange")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let change = spec.get("change").and_then(as_number);
    let percent = spec.get("change_percent").and_then(as_number);
    let field = |k: &str| quote.get(k).cloned().unwrap_or(Value::Null);
    let movement = match (change, percent) {
        (Some(c), Some(p)) => {
            let direction = if c > 0.0 {
                "up"
            } else if c < 0.0 {
                "down"
            } else {
                "unchanged at"
            };
            format!(
                "{} {} ({:.2} percent) from the previous close of {}",
                direction,
                price(c.abs()),
                p.abs(),
                format_price(&field("prev_close"))
            )
        }
        _ => "with no stated change against the previous close".to_string(),
    };
    let mut parts = vec![format!(
        "{} on {} is {}, {}. Day {} to {}.",
        symbol,
        exchange,
        format_price(&field("ltp")),
        movement,
        format_price(&field("low")),
        format_price(&field("high"))
    )];
    if let Some(position) = spec
        .get("position")
        .filter(|p| p.get("held") == Some(&json!(true)))
    {
        let side = position
            .get("side")
            .and_then(Value::as_str)
            .unwrap_or("holding");
        let size = position
            .get("quantity")
            .and_then(as_number)
            .map_or("an unstated quantity".to_string(), |q| {
                format_number(&json!(q.abs()))
            });
        let at = position
            .get("average_price")
            .map(|a| format!(" at {}", format_price(a)))
            .unwrap_or_default();
        let profit = position
            .get("pnl")
            .map(|p| format!(", unrealised P and L {}", format_price(p)))
            .unwrap_or_default();
        parts.push(format!(
            "The operator is {} {}{}{}.",
            side, size, at, profit
        ));
    }
    if spec.get("mode") == Some(&json!("analyze")) {
        parts.push("Analyzer mode is on, so the position is a sandbox one; say so.".into());
    }
    if !unavailable.is_empty() {
        let mut names = unavailable.to_vec();
        names.sort_unstable();
        parts.push(format!("Not on the card: {}.", names.join(", ")));
    }
    parts.push("The card is on the operator's screen with the intraday chart, the 52 week range and the order book, so describe what it shows rather than listing its numbers. It carries no fundamentals because this platform has none; do not add any.".into());
    parts.join(" ")
}
