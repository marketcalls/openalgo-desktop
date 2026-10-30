//! The payoff diagram's legs (the web's `plot_payoff`, `_named_legs`,
//! `_position_legs`, `_one_underlying` and `_price_legs` in
//! `tools/option_viz.py`).
//!
//! This file resolves and prices the legs and emits them in the Strategy
//! Builder's `StrategyLeg` shape; the browser computes and draws the curve
//! with `lib/strategyMath`, the maths `/strategybuilder` uses, so there is
//! never a second implementation to drift. Cash equity is not a leg: a
//! holding in the same underlying is named in the card, the notices and the
//! answer, because a covered call charted without its shares reads as a
//! naked short call.

use super::PAYOFF_VIZ;
use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::account::{current_mode, looks_flat, CURRENCY};
use crate::agent::tools::answer::{label, tool_answer};
use crate::agent::tools::market::{data_of, master, IST};
use crate::agent::tools::numbers::{as_number, format_price, py_round};
use crate::agent::tools::options_common::{
    leg_entries, leg_side_and_lots, leg_symbol, master_lot_size, normalise_leg_expiry,
    resolve_contract, segment_of, Contract, MAX_LEGS,
};
use crate::agent::tools::values::{truthy, truthy_text};
use crate::services::options_service::{parse_option_symbol, GreeksInput};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

const TOOL: &str = "plot_payoff";
const OPTION: &str = "OPTION";
const BUY: &str = "BUY";
const SELL: &str = "SELL";

/// One resolved leg: the instrument master's facts and the selection.
#[derive(Debug, Clone, PartialEq)]
pub struct PayoffLeg {
    pub contract: Contract,
    pub side: String,
    pub lots: i64,
    /// `named` or `position`.
    pub origin: &'static str,
    /// A held position's own average price, the entry premium.
    pub entry_price: Option<f64>,
}

/// A position that cannot be a leg, named rather than dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct Excluded {
    pub symbol: String,
    pub exchange: String,
    pub reason: String,
    pub base: String,
}

impl Excluded {
    fn to_json(&self) -> Value {
        json!({"symbol": self.symbol, "exchange": self.exchange, "reason": self.reason, "base": self.base})
    }
}

/// The contracts the operator named, read against the instrument master.
async fn named_legs(
    ctx: &ToolCtx,
    entries: &[Value],
    base: &str,
    expiry: &str,
    notices: &mut Vec<String>,
) -> Result<Vec<PayoffLeg>, ToolOutcome> {
    let listed = master(&ctx.app);
    let mut legs = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let position = i + 1;
        let (symbol, exchange) = leg_symbol(entry, position, base, expiry)?;
        let place = format!("leg {}", position);
        let contract = resolve_contract(ctx, &listed, &symbol, &exchange, Some(&place)).await?;
        let (side, lots) = leg_side_and_lots(
            entry,
            position,
            || master_lot_size(ctx, &contract.symbol, &contract.exchange),
            notices,
        )
        .await?;
        legs.push(PayoffLeg {
            contract,
            side: side.to_string(),
            lots,
            origin: "named",
            entry_price: None,
        });
    }
    Ok(legs)
}

/// The lots a held quantity is, at least one (Python's banker's `round`).
pub fn held_lots(quantity: f64, lot_size: i64) -> i64 {
    if lot_size <= 0 {
        return 1;
    }
    ((quantity.abs() / lot_size as f64).round_ties_even() as i64).max(1)
}

/// The operator's open futures and options as legs; cash and anything past
/// [`MAX_LEGS`] go to `excluded`. A derivative that cannot be resolved
/// fails the call rather than drawing an understated payoff.
async fn position_legs(
    ctx: &ToolCtx,
    excluded: &mut Vec<Excluded>,
) -> Result<Vec<PayoffLeg>, ToolOutcome> {
    let payload = ctx
        .service_read(
            "positionbook_service.get_positionbook",
            "positionbook",
            Map::new(),
        )
        .await?;
    let rows: Vec<Value> = match data_of(payload) {
        Value::Array(rows) => rows.into_iter().filter(Value::is_object).collect(),
        _ => Vec::new(),
    };
    let listed = master(&ctx.app);
    let mut legs = Vec::new();
    for row in rows {
        let quantity = row.get("quantity").and_then(as_number);
        let symbol = truthy_text(row.get("symbol")).trim().to_uppercase();
        let exchange = truthy_text(row.get("exchange")).trim().to_uppercase();
        let Some(quantity) = quantity else { continue };
        if symbol.is_empty() || looks_flat(row.get("quantity").unwrap_or(&Value::Null)) {
            continue;
        }
        let reason = if segment_of(&symbol, None)?.is_empty() {
            Some("a cash position rather than a derivative".to_string())
        } else if legs.len() >= MAX_LEGS {
            Some(format!("past the {} legs one diagram carries", MAX_LEGS))
        } else {
            None
        };
        if let Some(reason) = reason {
            excluded.push(Excluded {
                base: symbol.clone(),
                symbol,
                exchange,
                reason,
            });
            continue;
        }
        let contract = resolve_contract(ctx, &listed, &symbol, &exchange, None).await?;
        let lots = held_lots(quantity, contract.lot_size);
        legs.push(PayoffLeg {
            side: if quantity > 0.0 { BUY } else { SELL }.to_string(),
            lots,
            origin: "position",
            entry_price: row
                .get("average_price")
                .and_then(as_number)
                .filter(|a| *a > 0.0),
            contract,
        });
    }
    Ok(legs)
}

/// Keep the legs of one underlying (one curve has one axis): the base with
/// the most legs, ties to the greater name; the rest are named, not lost.
pub fn one_underlying(legs: Vec<PayoffLeg>) -> (Vec<PayoffLeg>, Vec<PayoffLeg>) {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<PayoffLeg>> = HashMap::new();
    for leg in legs {
        let base = leg.contract.base.clone();
        if !groups.contains_key(&base) {
            order.push(base.clone());
        }
        groups.entry(base).or_default().push(leg);
    }
    let winner = order
        .iter()
        .max_by(|a, b| {
            let (la, lb) = (groups[*a].len(), groups[*b].len());
            la.cmp(&lb).then_with(|| a.cmp(b))
        })
        .cloned()
        .unwrap_or_default();
    let chosen = groups.remove(&winner).unwrap_or_default();
    let rest = order
        .iter()
        .filter(|b| **b != winner)
        .flat_map(|b| groups.remove(b).unwrap_or_default())
        .collect();
    (chosen, rest)
}

/// What the broker answered for the legs: last prices by (symbol,
/// exchange) and a forward per expiry.
#[derive(Debug, Default)]
pub struct Market {
    pub quotes: HashMap<(String, String), f64>,
    pub forwards: HashMap<String, f64>,
}

/// The legs in `StrategyLeg` shape, the spot, and the mean implied
/// volatility (a leg with none of its own falls back to it). `implied_vol` inverts
/// an option's market price against its forward.
pub fn priced_legs(
    legs: &[PayoffLeg],
    market: &Market,
    implied_vol: impl Fn(&PayoffLeg, f64, f64) -> f64,
    notices: &mut Vec<String>,
) -> (Vec<Value>, Option<f64>, Option<f64>) {
    let Some(first) = legs.first() else {
        return (Vec::new(), None, None);
    };
    let spot = market
        .quotes
        .get(&(
            first.contract.base.clone(),
            first.contract.underlying_exchange.clone(),
        ))
        .copied();
    let mut priced = Vec::new();
    let mut implied_vols = Vec::new();
    for (i, leg) in legs.iter().enumerate() {
        let c = &leg.contract;
        let quoted = market
            .quotes
            .get(&(c.symbol.clone(), c.exchange.clone()))
            .copied();
        let Some(price) = leg.entry_price.or(quoted) else {
            notices.push(format!("{} is not quoting, so it could not be priced and is not on the curve. The diagram is therefore incomplete.", c.symbol));
            continue;
        };
        let option = c.segment == OPTION;
        if option && c.strike.is_none() {
            notices.push(format!("{} has no strike in the instrument master, so it could not be priced and is not on the curve. The diagram is therefore incomplete.", c.symbol));
            continue;
        }
        let forward = market
            .forwards
            .get(&c.expiry)
            .copied()
            .filter(|f| *f != 0.0);
        let leg_vol = match (option, forward, quoted.filter(|m| *m != 0.0)) {
            (true, Some(f), Some(m)) => implied_vol(leg, f, m),
            _ => 0.0,
        };
        if leg_vol > 0.0 {
            implied_vols.push(leg_vol);
        }
        priced.push(strategy_leg(
            i + 1,
            leg,
            price,
            leg_vol,
            quoted,
            spot,
            forward,
        ));
    }
    let atm_vol = (!implied_vols.is_empty()).then(|| {
        py_round(
            implied_vols.iter().sum::<f64>() / implied_vols.len() as f64,
            4,
        )
    });
    (priced, spot, atm_vol)
}

/// One leg as `StrategyLeg` declares it, in the web's key order.
fn strategy_leg(
    n: usize,
    leg: &PayoffLeg,
    price: f64,
    implied_vol: f64,
    market: Option<f64>,
    spot: Option<f64>,
    forward: Option<f64>,
) -> Value {
    let c = &leg.contract;
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("agent-{}", n)));
    m.insert("segment".into(), json!(c.segment));
    m.insert("side".into(), json!(leg.side));
    m.insert("lots".into(), json!(leg.lots));
    m.insert("lotSize".into(), json!(c.lot_size));
    m.insert("expiry".into(), json!(c.expiry));
    m.insert("price".into(), json!(py_round(price, 6)));
    m.insert("iv".into(), json!(py_round(implied_vol, 4)));
    m.insert("active".into(), json!(true));
    m.insert("symbol".into(), json!(c.symbol));
    m.insert("exchange".into(), json!(c.exchange));
    m.insert("contractValid".into(), json!(true));
    m.insert("origin".into(), json!(leg.origin));
    let option = c.segment == OPTION;
    if option {
        m.insert("strike".into(), json!(c.strike));
        m.insert("optionType".into(), json!(c.option_type));
    }
    if let Some(tick) = c.tick_size {
        m.insert("tickSize".into(), json!(tick));
    }
    if let Some(ts) = c.expiry_ts.filter(|t| *t != 0) {
        m.insert("expiryTs".into(), json!(ts));
    }
    if let Some(mp) = market {
        m.insert("marketPrice".into(), json!(py_round(mp, 6)));
    }
    if let Some(s) = spot {
        m.insert("referenceUnderlying".into(), json!(s));
    }
    if let (true, Some(f)) = (option, forward) {
        m.insert("forwardPrice".into(), json!(f));
    }
    Value::Object(m)
}

/// The broker's quotes for every leg and the underlying (one batched
/// call), and one forward per expiry (a failed forward is skipped).
async fn market_for(ctx: &ToolCtx, legs: &[PayoffLeg]) -> Result<Market, ToolOutcome> {
    let mut market = Market::default();
    let Some(first) = legs.first() else {
        return Ok(market);
    };
    let (base, venue) = (&first.contract.base, &first.contract.underlying_exchange);
    let mut pairs: Vec<Value> = legs
        .iter()
        .map(|l| json!({"symbol": l.contract.symbol, "exchange": l.contract.exchange}))
        .collect();
    pairs.push(json!({"symbol": base, "exchange": venue}));
    let mut request = Map::new();
    request.insert("symbols".into(), Value::Array(pairs));
    let response = ctx
        .service_read("quotes_service.get_multiquotes", "multiquotes", request)
        .await?;
    for row in response
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(ltp) = row
            .get("data")
            .and_then(|d| d.get("ltp"))
            .and_then(as_number)
        {
            let key = (
                truthy_text(row.get("symbol")),
                truthy_text(row.get("exchange")),
            );
            market.quotes.insert(key, ltp);
        }
    }
    let expiries: BTreeSet<&str> = legs
        .iter()
        .map(|l| l.contract.expiry.as_str())
        .filter(|e| !e.is_empty())
        .collect();
    for expiry in expiries {
        let mut request = Map::new();
        request.insert("underlying".into(), json!(base));
        request.insert("exchange".into(), json!(venue));
        request.insert("expiry_date".into(), json!(expiry));
        match ctx
            .service_read(
                "synthetic_future_service.calculate_synthetic_future",
                "syntheticfuture",
                request,
            )
            .await
        {
            Ok(payload) => {
                if let Some(v) = payload
                    .get("synthetic_future_price")
                    .and_then(as_number)
                    .filter(|v| *v != 0.0)
                {
                    market.forwards.insert(expiry.to_string(), v);
                }
            }
            Err(e) => tracing::warn!("Payoff: no forward for {} {}: {}", base, expiry, e.text()),
        }
    }
    Ok(market)
}

/// The platform's Black-76 implied volatility of an option at its market
/// price against a forward; 0 when it cannot be solved.
fn implied_volatility(ctx: &ToolCtx, leg: &PayoffLeg, forward: f64, market: f64) -> f64 {
    let c = &leg.contract;
    let Ok(parsed) = parse_option_symbol(&c.symbol) else {
        tracing::warn!("Payoff: no implied volatility for {}", c.symbol);
        return 0.0;
    };
    let reply =
        GreeksInput::new(&c.symbol, &c.exchange, &parsed, forward, market).calculate(ctx.app.now());
    if reply.body.get("status").and_then(Value::as_str) != Some("success") {
        tracing::warn!("Payoff: no implied volatility for {}", c.symbol);
        return 0.0;
    }
    reply
        .body
        .get("implied_volatility")
        .and_then(as_number)
        .unwrap_or(0.0)
}

/// Where the legs came from, as the answer names it.
fn origin_text(named: bool, held: bool) -> &'static str {
    if held && !named {
        "the operator's open positions"
    } else {
        "the named legs"
    }
}

/// The answer when there is nothing to chart.
fn nothing_to_chart(excluded: &[Excluded]) -> String {
    let mut text = "There is nothing to chart: no contracts were named and the position book holds no open futures or options, so the operator is flat in derivatives. Say so rather than drawing an empty diagram.".to_string();
    if !excluded.is_empty() {
        let held: Vec<String> = excluded
            .iter()
            .map(|r| format!("{} ({})", r.symbol, r.reason))
            .collect();
        text.push_str(&format!(" They do hold {}.", held.join(", ")));
    }
    text
}

/// The `payoff` spec, JSON-safe.
fn payoff_spec(
    ctx: &ToolCtx,
    head: &Contract,
    spot: f64,
    atm_vol: Option<f64>,
    legs: Vec<Value>,
    excluded: &[Excluded],
    notices: &[String],
) -> Map<String, Value> {
    let as_of = ctx.app.now().with_timezone(&IST);
    let mut spec = Map::new();
    spec.insert("underlying".into(), json!(head.base));
    spec.insert(
        "underlying_exchange".into(),
        json!(head.underlying_exchange),
    );
    spec.insert("spot".into(), json!(spot));
    spec.insert("atm_iv".into(), json!(atm_vol));
    spec.insert("currency".into(), json!(CURRENCY));
    spec.insert("mode".into(), json!(current_mode(ctx)));
    spec.insert(
        "as_of".into(),
        json!(as_of.format("%Y-%m-%dT%H:%M:%S%:z").to_string()),
    );
    spec.insert("timezone".into(), json!("Asia/Kolkata"));
    spec.insert("legs".into(), Value::Array(legs));
    if !excluded.is_empty() {
        let rows = excluded.iter().map(Excluded::to_json).collect();
        spec.insert("excluded".into(), Value::Array(rows));
    }
    if !notices.is_empty() {
        spec.insert("notices".into(), json!(notices));
    }
    match json_safe(&Value::Object(spec)) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `plot_payoff`: the named legs, the operator's own open positions, or
/// both, priced and emitted for the browser to draw.
pub async fn plot_payoff(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let entries = leg_entries(args.get("legs"), "legs")?;
    let base = truthy_text(args.get("underlying")).trim().to_uppercase();
    let mut expiry = truthy_text(args.get("expiry_date")).trim().to_uppercase();
    if !expiry.is_empty() {
        expiry = normalise_leg_expiry(Some(&json!(expiry)), "expiry_date")?;
    }
    let mut notices: Vec<String> = Vec::new();
    let mut excluded: Vec<Excluded> = Vec::new();
    let mut sources = vec!["symbol_service", "quotes_service"];
    let named = named_legs(ctx, &entries, &base, &expiry, &mut notices).await?;
    let include = args.get("include_open_positions").is_some_and(truthy);
    let mut held = Vec::new();
    if named.is_empty() || include {
        held = position_legs(ctx, &mut excluded).await?;
        sources.push("positionbook_service");
    }
    let origin = origin_text(!named.is_empty(), !held.is_empty());
    let raw: Vec<PayoffLeg> = named.into_iter().chain(held).collect();
    if raw.is_empty() {
        return Ok(tool_answer(TOOL, &nothing_to_chart(&excluded), &[], &[]));
    }
    let (chosen, dropped) = one_underlying(raw);
    let head = chosen[0].contract.clone();
    for row in &dropped {
        notices.push(format!("{} is on {}, a different underlying, so it is not on this curve. One payoff diagram has one underlying on its axis.", row.contract.symbol, row.contract.base));
    }
    for row in excluded.iter().filter(|r| r.base == head.base) {
        notices.push(format!("The operator also holds {}, which is {} and cannot be a leg of this curve. The diagram therefore understates a covered or hedged position: say so.", row.symbol, row.reason));
    }
    let market = market_for(ctx, &chosen).await?;
    let (resolved, spot, atm_vol) = priced_legs(
        &chosen,
        &market,
        |l, f, m| implied_volatility(ctx, l, f, m),
        &mut notices,
    );
    if resolved.is_empty() {
        return Ok(tool_answer(TOOL, "None of the legs could be priced against the broker session, so no payoff was drawn. Check the contracts are still quoting.", &notices, &[]));
    }
    let Some(spot) = spot.filter(|s| *s > 0.0) else {
        return Ok(tool_answer(TOOL, &format!("{} on {} returned no last traded price, so the curve has nothing to centre on and none was drawn. Say the underlying could not be priced rather than describing a shape.", head.base, head.underlying_exchange), &notices, &[]));
    };
    if !market.forwards.is_empty() {
        sources.push("synthetic_future_service");
    }
    let spec = payoff_spec(
        ctx,
        &head,
        spot,
        atm_vol,
        resolved.clone(),
        &excluded,
        &notices,
    );
    let title = format!("{} payoff at expiry", head.base);
    if !ctx
        .viz
        .emit(TOOL, PAYOFF_VIZ, spec, &title, &sources.join(", "))
    {
        return Ok(tool_answer(
            TOOL,
            &no_sink_message("payoff diagram"),
            &notices,
            &[],
        ));
    }
    Ok(tool_answer(
        TOOL,
        &payoff_message(&resolved, &head.base, origin, spot),
        &notices,
        &[label("underlying", &head.base)],
    ))
}

/// The answer once the diagram is on screen.
pub fn payoff_message(resolved: &[Value], base: &str, origin: &str, spot: f64) -> String {
    let legs = resolved
        .iter()
        .map(|l| {
            format!(
                "{} {}x{} {} at {}",
                truthy_text(l.get("side")),
                l.get("lots").cloned().unwrap_or(Value::Null),
                l.get("lotSize").cloned().unwrap_or(Value::Null),
                truthy_text(l.get("symbol")),
                format_price(l.get("price").unwrap_or(&Value::Null))
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Drew the payoff at expiry for {} {} on {}, from {}, priced against spot {}: {}. The operator can see the curve, its breakevens and its maximum profit and loss, so read the shape rather than restating the legs.",
        resolved.len(),
        if resolved.len() == 1 { "leg" } else { "legs" },
        base,
        origin,
        format_price(&json!(spot)),
        legs
    )
}
