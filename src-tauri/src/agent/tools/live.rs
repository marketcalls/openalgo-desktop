//! Live cards (the web's `services/agent/tools/live.py`, toolkit `live`):
//! `stream_quotes`, a card of instruments the screen subscribes to, and
//! `stream_combo` ([`combo`]), one derived value (an ATM straddle, a spread,
//! a basis) recomputed on every tick of its legs.
//!
//! The server never streams. It resolves every instrument against the
//! master, seeds a snapshot, bounds the list and carries the trading
//! session; the trader's screen subscribes through the desktop's own market
//! data socket. Every instrument on a card is a live subscription for as
//! long as the message is on screen, so the caps here are resource bounds:
//! 12 instruments (4 in Depth mode), 8 legs.

pub mod combo;
#[cfg(test)]
mod tests;

use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::account::{current_mode, CURRENCY};
use crate::agent::tools::answer::{label, tool_answer};
use crate::agent::tools::instrument::{depth_levels, normalise_quote, quote_move};
use crate::agent::tools::market::candles::iso_ist;
use crate::agent::tools::market::normalise::{symbol_pairs, Pair};
use crate::agent::tools::market::{data_of, master, IST};
use crate::agent::tools::numbers::{as_number, format_price, py_round};
use crate::agent::tools::specs;
use crate::agent::tools::symbols::{DERIVATIVE_EXCHANGES, INDEX_EXCHANGES};
use crate::agent::tools::values::{quote, truthy_text, Pattern};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// The renderer a quotes card selects.
pub const LIVE_QUOTES_VIZ: &str = "live_quotes";
/// The renderer a combination card selects.
pub const LIVE_COMBO_VIZ: &str = "live_combo";
/// The subscription modes, spelled as the screen's manager spells them.
pub const MODES: [&str; 3] = ["LTP", "Quote", "Depth"];
/// The mode a card opens in by default.
pub const DEFAULT_MODE: &str = "Quote";
/// Most instruments on one quotes card.
pub const MAX_LIVE_SYMBOLS: usize = 12;
/// Most instruments on one Depth card.
pub const MAX_LIVE_DEPTH_SYMBOLS: usize = 4;
/// The mode a combination card subscribes in.
pub const COMBO_MODE: &str = "Quote";

const TIMEZONE: &str = "Asia/Kolkata";

static LIST_SEPARATORS: Pattern = Pattern::new(r"[,\n]");

/// The subscription mode: LTP, Quote (the default) or Depth, also as the
/// protocol's 1, 2, 3.
pub fn normalise_mode(value: Option<&Value>) -> Result<&'static str, ToolOutcome> {
    let text = match value {
        None | Some(Value::Null) | Some(Value::Bool(_)) => String::new(),
        Some(v) => crate::agent::tools::values::py_str(v).trim().to_lowercase(),
    };
    match text.as_str() {
        "" => return Ok(DEFAULT_MODE),
        "1" => return Ok("LTP"),
        "2" => return Ok("Quote"),
        "3" => return Ok("Depth"),
        _ => {}
    }
    MODES
        .into_iter()
        .find(|m| m.to_lowercase() == text)
        .ok_or_else(|| {
            ToolOutcome::invalid_argument(
                "mode",
                &format!("{} is not a subscription mode", quote(&text)),
                Some(&format!(
                    "Pass one of {} (or 1, 2, 3). LTP is the last price only, Quote adds the day's open, high, low, volume and the touch, and Depth adds the order book.",
                    MODES.join(", ")
                )),
            )
        })
}

/// The exchange whose session an instrument follows: an index follows its
/// cash exchange (NSE_INDEX trades when NSE does).
pub fn calendar_exchange(exchange: &str) -> String {
    exchange
        .strip_suffix("_INDEX")
        .unwrap_or(exchange)
        .to_string()
}

/// The spacing of a listed strike ladder: the most common positive gap,
/// ties to the smaller; `None` with fewer than two strikes.
pub fn strike_spacing(strikes: &[f64]) -> Option<f64> {
    let mut gaps: Vec<(f64, usize)> = Vec::new();
    for pair in strikes.windows(2) {
        let gap = py_round(pair[1] - pair[0], 4);
        if gap > 0.0 {
            match gaps.iter_mut().find(|(g, _)| *g == gap) {
                Some((_, n)) => *n += 1,
                None => gaps.push((gap, 1)),
            }
        }
    }
    gaps.into_iter()
        .min_by(|a, b| b.1.cmp(&a.1).then(a.0.total_cmp(&b.0)))
        .map(|(g, _)| g)
}

/// A bare `NSE:INFY` (or a comma or newline separated run of them) as the
/// list it stands for; anything else passed through for the symbol list
/// reader to judge.
pub fn symbols_argument(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(s)) => {
            let t = s.trim();
            if !t.is_empty() && !t.starts_with('[') && !t.starts_with('{') {
                return Some(Value::Array(
                    LIST_SEPARATORS
                        .split(t)
                        .into_iter()
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(Value::from)
                        .collect(),
                ));
            }
            Some(Value::String(s.clone()))
        }
        other => other.cloned(),
    }
}

/// The distinct instruments a card subscribes, in first-seen order.
pub fn subscription_list<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Value> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (symbol, exchange) in pairs {
        let (s, e) = (symbol.trim().to_uppercase(), exchange.trim().to_uppercase());
        if s.is_empty() || e.is_empty() || out.iter().any(|(a, b)| *a == s && *b == e) {
            continue;
        }
        out.push((s, e));
    }
    out.into_iter()
        .map(|(s, e)| json!({"symbol": s, "exchange": e}))
        .collect()
}

/// Python's `format(x, 'g')`: six significant digits, exponent form below
/// 1e-4 and from 1e6, trailing zeros dropped.
pub fn py_g(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    if !x.is_finite() {
        return crate::agent::tools::values::float_repr(x);
    }
    let sci = format!("{:.5e}", x);
    let exp: i32 = sci
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);
    let trim = |t: String| -> String {
        if t.contains('.') {
            t.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            t
        }
    };
    if !(-4..6).contains(&exp) {
        let (mantissa, _) = sci.split_once('e').unwrap_or((sci.as_str(), ""));
        let sign = if exp < 0 { "-" } else { "+" };
        return format!("{}e{}{:02}", trim(mantissa.to_string()), sign, exp.abs());
    }
    trim(format!("{:.*}", (5 - exp) as usize, x))
}

/// The batched snapshot's rows that carried data, by pair.
fn quote_rows(response: &Value) -> HashMap<(String, String), Value> {
    let mut rows = HashMap::new();
    for row in response
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(data) = row.get("data").filter(|d| d.is_object()) else {
            continue;
        };
        let symbol = truthy_text(row.get("symbol")).to_uppercase();
        let exchange = truthy_text(row.get("exchange")).to_uppercase();
        if !symbol.is_empty() && !exchange.is_empty() {
            rows.insert((symbol, exchange), data.clone());
        }
    }
    rows
}

/// One batched quote for every instrument on a card; a failure costs the
/// seed, never the card.
async fn snapshot(
    ctx: &ToolCtx,
    pairs: &[Value],
    notices: &mut Vec<String>,
) -> HashMap<(String, String), Value> {
    let mut request = Map::new();
    request.insert("symbols".into(), Value::Array(pairs.to_vec()));
    match ctx
        .service_read("quotes_service.get_multiquotes", "multiquotes", request)
        .await
    {
        Ok(response) => quote_rows(&response),
        Err(failure) => {
            tracing::warn!(
                "Agent live card could not seed a snapshot: {}",
                failure.text()
            );
            notices.push("The opening snapshot could not be read (RetryAgentRun), so the card starts empty and fills in on the first tick.".into());
            HashMap::new()
        }
    }
}

/// One instrument's order book, or why there is none.
async fn ladder(ctx: &ToolCtx, symbol: &str, exchange: &str) -> (Option<Value>, Option<String>) {
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    let data = match ctx
        .service_read("depth_service.get_depth", "depth", request)
        .await
    {
        Ok(response) => data_of(response),
        Err(_) => {
            return (
                None,
                Some("the order book could not be read (RetryAgentRun)".into()),
            )
        }
    };
    if !data.is_object() {
        return (
            None,
            Some("the broker returned no order book for this instrument".into()),
        );
    }
    let bids = depth_levels(data.get("bids"));
    let asks = depth_levels(data.get("asks"));
    if bids.is_empty() && asks.is_empty() {
        return (
            None,
            Some("there are no resting orders on either side".into()),
        );
    }
    let mut out = Map::new();
    out.insert("bids".into(), Value::Array(bids));
    out.insert("asks".into(), Value::Array(asks));
    for (field, key) in [
        ("total_buy_quantity", "totalbuyqty"),
        ("total_sell_quantity", "totalsellqty"),
    ] {
        if let Some(v) = data.get(key).and_then(as_number) {
            out.insert(field.into(), json!(v));
        }
    }
    (Some(Value::Object(out)), None)
}

/// Today's trading session for every exchange on a card: the window and the
/// verdict at resolution (the screen recomputes from `opens_at` and
/// `closes_at`, epoch milliseconds). A calendar that cannot be read costs
/// the block its certainty (`known: false`), never the card.
pub fn market_block<'a>(
    ctx: &ToolCtx,
    exchanges: impl IntoIterator<Item = &'a str>,
) -> Map<String, Value> {
    let now = ctx.app.now();
    let today = now.with_timezone(&IST).date_naive();
    let mut wanted: Vec<String> = Vec::new();
    for e in exchanges {
        let venue = calendar_exchange(&e.trim().to_uppercase());
        if !venue.is_empty() && !wanted.contains(&venue) {
            wanted.push(venue);
        }
    }
    let mut block = Map::new();
    block.insert("date".into(), json!(today.format("%Y-%m-%d").to_string()));
    let whole = chrono::DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now);
    block.insert("as_of".into(), json!(iso_ist(whole)));
    block.insert("timezone".into(), json!(TIMEZONE));
    let read =
        crate::services::market_calendar_service::timings_for(&ctx.app, today).and_then(|t| {
            let conn = ctx.app.sqlite.conn()?;
            let holiday = crate::db::sqlite::market_calendar::is_market_holiday(&conn, today)?;
            Ok((t, holiday))
        });
    let (timings, holiday) = match read {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Agent live card could not read the market calendar: {}", e);
            block.insert("known".into(), json!(false));
            let rows: Vec<Value> = wanted
                .iter()
                .map(|v| json!({"exchange": v, "known": false}))
                .collect();
            block.insert("exchanges".into(), Value::Array(rows));
            return block;
        }
    };
    let now_ms = now.timestamp_millis();
    let rows: Vec<Value> = wanted
        .iter()
        .map(|venue| match timings.iter().find(|w| w.exchange.trim().to_uppercase() == *venue) {
            None => json!({"exchange": venue, "known": false, "is_open": false}),
            Some(w) => {
                let open = w.start_time != 0
                    && w.end_time != 0
                    && (w.start_time..=w.end_time).contains(&now_ms);
                json!({"exchange": venue, "known": true, "is_open": open, "opens_at": w.start_time, "closes_at": w.end_time})
            }
        })
        .collect();
    let any_open = rows.iter().any(|r| r["is_open"] == true);
    block.insert("known".into(), json!(true));
    block.insert("exchanges".into(), Value::Array(rows));
    block.insert("is_open".into(), json!(any_open));
    block.insert("is_holiday".into(), json!(holiday));
    block
}

pub const GROUP: ToolGroup = ToolGroup {
    key: "live",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 47,
    description: "Open a live streaming card: a list of instruments the browser subscribes to, or one derived value, such as an ATM straddle, recomputed on every tick of its legs.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    Quotes,
    Combo,
}

struct LiveTool {
    spec: ToolSpec,
    kind: Kind,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [
        ("stream_quotes", Kind::Quotes),
        ("stream_combo", Kind::Combo),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log("live", name)?;
        Some(Arc::new(LiveTool { spec, kind }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for LiveTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::Quotes => stream_quotes(ctx, &args).await,
            Kind::Combo => combo::stream_combo(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

async fn stream_quotes(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let mode = normalise_mode(args.get("mode"))?;
    let limit = if mode == "Depth" {
        MAX_LIVE_DEPTH_SYMBOLS
    } else {
        MAX_LIVE_SYMBOLS
    };
    let listed = master(&ctx.app);
    let symbols = symbols_argument(args.get("symbols"));
    let (pairs, mut notices) = symbol_pairs(&listed, symbols.as_ref(), "symbols", limit, true)?;
    let (resolved, refused): (Vec<Pair>, Vec<Pair>) =
        pairs.into_iter().partition(|(s, e)| listed(s, e));
    let refused_rows: Vec<Value> = refused
        .iter()
        .map(|(s, e)| json!({"symbol": s, "exchange": e, "reason": "no row in the instrument master, so it would never tick"}))
        .collect();
    let named = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(s, e)| format!("{} on {}", s, e))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if resolved.is_empty() {
        let message = format!(
            "None of the instruments asked for are in the instrument master ({}), so no card was opened. Check the spelling and the exchange with search_symbols, and do not tell the operator anything is streaming.",
            named(&refused)
        );
        return Ok(tool_answer("stream_quotes", &message, &notices, &[]));
    }
    let instruments = seed_instruments(ctx, &resolved, mode, &mut notices).await;
    let subscribe = subscription_list(resolved.iter().map(|(s, e)| (s.as_str(), e.as_str())));
    let market = market_block(ctx, resolved.iter().map(|(_, e)| e.as_str()));
    let mut spec = Map::new();
    spec.insert("mode".into(), json!(mode));
    spec.insert("currency".into(), json!(CURRENCY));
    spec.insert("account_mode".into(), json!(current_mode(ctx)));
    spec.insert(
        "as_of".into(),
        market.get("as_of").cloned().unwrap_or(Value::Null),
    );
    spec.insert("timezone".into(), json!(TIMEZONE));
    spec.insert(
        "instruments".into(),
        Value::Array(instruments.iter().cloned().map(Value::Object).collect()),
    );
    spec.insert("subscribe".into(), Value::Array(subscribe));
    spec.insert("market".into(), Value::Object(market.clone()));
    if !refused_rows.is_empty() {
        spec.insert("refused".into(), Value::Array(refused_rows));
    }
    if !notices.is_empty() {
        spec.insert("notices".into(), json!(notices));
    }
    let safe = match json_safe(&Value::Object(spec)) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let title = quotes_title(&instruments, mode);
    let source = "quotes_service, depth_service, market_calendar_service";
    if !ctx
        .viz
        .emit("stream_quotes", LIVE_QUOTES_VIZ, safe, &title, source)
    {
        return Ok(tool_answer(
            "stream_quotes",
            &no_sink_message("live quotes card"),
            &notices,
            &[],
        ));
    }
    let message = quotes_confirmation(&instruments, &refused, mode, &market);
    Ok(tool_answer(
        "stream_quotes",
        &message,
        &notices,
        &[label("count", &instruments.len()), label("mode", &mode)],
    ))
}

/// One card row per instrument, seeded from one snapshot (and the ladder
/// in Depth mode); a row whose seed failed stays on the card and says why.
async fn seed_instruments(
    ctx: &ToolCtx,
    pairs: &[(String, String)],
    mode: &str,
    notices: &mut Vec<String>,
) -> Vec<Map<String, Value>> {
    let request: Vec<Value> = pairs
        .iter()
        .map(|(s, e)| json!({"symbol": s, "exchange": e}))
        .collect();
    let quoted = snapshot(ctx, &request, notices).await;
    let mut rows = Vec::new();
    for (symbol, exchange) in pairs {
        let mut row = Map::new();
        row.insert("symbol".into(), json!(symbol));
        row.insert("exchange".into(), json!(exchange));
        row.insert(
            "calendar_exchange".into(),
            json!(calendar_exchange(exchange)),
        );
        row.insert(
            "is_index".into(),
            json!(INDEX_EXCHANGES.contains(&exchange.as_str())),
        );
        row.insert(
            "is_derivative".into(),
            json!(DERIVATIVE_EXCHANGES.contains(&exchange.as_str())),
        );
        let mut unavailable = Map::new();
        match quoted.get(&(symbol.clone(), exchange.clone())) {
            None => {
                unavailable.insert(
                    "seed".into(),
                    json!("the broker returned no quote for this instrument"),
                );
            }
            Some(raw) => {
                let seed = normalise_quote(raw);
                let movement = quote_move(&seed, notices, symbol);
                row.insert("seed".into(), Value::Object(seed));
                row.extend(movement);
            }
        }
        if mode == "Depth" {
            match ladder(ctx, symbol, exchange).await {
                (Some(book), _) => {
                    row.insert("depth".into(), book);
                }
                (None, Some(why)) => {
                    unavailable.insert("depth".into(), json!(why));
                }
                (None, None) => {}
            }
        }
        if !unavailable.is_empty() {
            row.insert("unavailable".into(), Value::Object(unavailable));
        }
        rows.push(row);
    }
    rows
}

fn quotes_title(instruments: &[Map<String, Value>], mode: &str) -> String {
    let names: Vec<String> = instruments
        .iter()
        .map(|r| truthy_text(r.get("symbol")))
        .collect();
    let mut listed = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 3 {
        listed = format!("{} and {} more", listed, names.len() - 3);
    }
    format!("Live {}: {}", mode, listed)
}

fn quotes_confirmation(
    instruments: &[Map<String, Value>],
    refused: &[(String, String)],
    mode: &str,
    market: &Map<String, Value>,
) -> String {
    let opened: Vec<String> = instruments
        .iter()
        .map(|row| {
            let at = format!(
                "{} {}",
                truthy_text(row.get("symbol")),
                truthy_text(row.get("exchange"))
            );
            match row
                .get("seed")
                .and_then(|s| s.get("ltp"))
                .and_then(as_number)
            {
                Some(p) => format!("{} at {}", at, format_price(&json!(p))),
                None => at,
            }
        })
        .collect();
    let noun = if instruments.len() == 1 {
        "instrument"
    } else {
        "instruments"
    };
    let mut parts = vec![format!(
        "Opened a live {} card on {} {}: {}.",
        mode,
        instruments.len(),
        noun,
        opened.join("; ")
    )];
    if !refused.is_empty() {
        let named: Vec<String> = refused
            .iter()
            .map(|(s, e)| format!("{} on {}", s, e))
            .collect();
        let verb = if refused.len() == 1 { "is" } else { "are" };
        parts.push(format!(
            "{} could not be resolved in the instrument master and {} not on the card. Say so.",
            named.join(", "),
            verb
        ));
    }
    if market.get("known") == Some(&json!(true)) && market.get("is_open") != Some(&json!(true)) {
        parts.push("Every exchange on the card is outside its trading session right now.".into());
    }
    parts.push("Those are the values the card opened with, not current ones: it is subscribed and updates on the operator's screen, so tell them what they can watch rather than quoting a price as though it were now.".into());
    parts.join(" ")
}
