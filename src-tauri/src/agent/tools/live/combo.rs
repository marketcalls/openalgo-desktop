//! `stream_combo`: one derived value recomputed on every tick of its legs
//! (the web's `live.py` `stream_combo`, `STRUCTURES` and the combination
//! card). The value is always `constant + sum(multiplier x price)`, bought
//! legs positive, sold legs negative; the legs are pinned at resolution, and
//! nothing about the number is the model's to compute.

use super::{
    market_block, py_g, snapshot, strike_spacing, subscription_list, COMBO_MODE, LIVE_COMBO_VIZ,
    TIMEZONE,
};
use crate::agent::contract::json::json_safe;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::account::{current_mode, CURRENCY};
use crate::agent::tools::answer::{tool_answer, Label};
use crate::agent::tools::instrument::normalise_quote;
use crate::agent::tools::market::{data_of, master};
use crate::agent::tools::numbers::{as_number, format_price, py_round};
use crate::agent::tools::options::{normalise_int, normalise_symbol};
use crate::agent::tools::options_common::{
    choose_expiry, leg_entries, leg_label, leg_side_and_lots, leg_symbol, listed_expiries,
    master_lot_size, resolve_contract, resolve_underlying_exchange, signed_multiplier, Contract,
    BUY, DEFAULT_EXPIRY_CHOICE, EXPIRY_CHOICES, MAX_LEGS, SELL,
};
use crate::agent::tools::values::{quote, truthy_text};
use crate::services::options_service;
use serde_json::{json, Map, Value};

/// The structure of a card built from named contracts.
pub const CUSTOM_STRUCTURE: &str = "custom";

/// Exchanges with no spot instrument: the near-month future is the
/// reference price.
const NO_SPOT_EXCHANGES: [&str; 5] = ["MCX", "CDS", "BCD", "NCDEX", "NCO"];

/// One leg of a named structure before it is resolved.
#[derive(Debug, Clone, Copy)]
pub struct LegTemplate {
    pub side: &'static str,
    pub lots: i64,
    /// `true` for the underlying itself (a basis), else an option.
    pub spot: bool,
    /// `true` for `OTM{width}`, else `ATM`.
    pub out_of_money: bool,
    pub option_type: &'static str,
}

const fn option(
    side: &'static str,
    lots: i64,
    out_of_money: bool,
    option_type: &'static str,
) -> LegTemplate {
    LegTemplate {
        side,
        lots,
        spot: false,
        out_of_money,
        option_type,
    }
}

/// One named combination.
#[derive(Debug, Clone, Copy)]
pub struct Structure {
    pub name: &'static str,
    pub legs: &'static [LegTemplate],
    /// What the number means, carried onto the card.
    pub summary: &'static str,
    /// The value is quoted relative to the strike (a synthetic future).
    pub constant_strike: bool,
    /// The structure reads the `width` argument.
    pub needs_width: bool,
}

/// The structures this tool builds (the web's `STRUCTURES`), sorted by name.
pub const STRUCTURES: [Structure; 8] = [
    Structure {
        name: "basis",
        legs: &[
            option(BUY, 1, false, "CE"),
            option(SELL, 1, false, "PE"),
            LegTemplate {
                side: SELL,
                lots: 1,
                spot: true,
                out_of_money: false,
                option_type: "",
            },
        ],
        summary: "the basis: the synthetic future less spot, so the forward premium",
        constant_strike: true,
        needs_width: false,
    },
    Structure {
        name: "call_ratio",
        legs: &[option(BUY, 1, false, "CE"), option(SELL, 2, true, "CE")],
        summary: "one ATM call against two further out calls, so the net of the ratio",
        constant_strike: false,
        needs_width: true,
    },
    Structure {
        name: "call_spread",
        legs: &[option(BUY, 1, false, "CE"), option(SELL, 1, true, "CE")],
        summary: "the ATM call less the further out call, so the net debit of the spread",
        constant_strike: false,
        needs_width: true,
    },
    Structure {
        name: "put_ratio",
        legs: &[option(BUY, 1, false, "PE"), option(SELL, 2, true, "PE")],
        summary: "one ATM put against two further out puts, so the net of the ratio",
        constant_strike: false,
        needs_width: true,
    },
    Structure {
        name: "put_spread",
        legs: &[option(BUY, 1, false, "PE"), option(SELL, 1, true, "PE")],
        summary: "the ATM put less the further out put, so the net debit of the spread",
        constant_strike: false,
        needs_width: true,
    },
    Structure {
        name: "straddle",
        legs: &[option(BUY, 1, false, "CE"), option(BUY, 1, false, "PE")],
        summary: "the call and the put at the ATM strike, added together",
        constant_strike: false,
        needs_width: false,
    },
    Structure {
        name: "strangle",
        legs: &[option(BUY, 1, true, "CE"), option(BUY, 1, true, "PE")],
        summary: "the out of the money call and put, added together",
        constant_strike: false,
        needs_width: true,
    },
    Structure {
        name: "synthetic",
        legs: &[option(BUY, 1, false, "CE"), option(SELL, 1, false, "PE")],
        summary: "the synthetic future: strike plus the call less the put",
        constant_strike: true,
        needs_width: false,
    },
];

/// The formula as a reader would say it (`24500 + NIFTY...CE - NIFTY...PE`);
/// display only, the screen evaluates from each leg's `multiplier`.
pub fn formula_text(legs: &[Map<String, Value>], constant: Option<f64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = constant.filter(|c| *c != 0.0) {
        parts.push(py_g(c));
    }
    for leg in legs {
        let multiplier = leg.get("multiplier").and_then(as_number).unwrap_or(0.0);
        let sign = if multiplier < 0.0 { "-" } else { "+" };
        let size = multiplier.abs();
        let scale = if size == 1.0 {
            String::new()
        } else {
            format!("{} x ", py_g(size))
        };
        let symbol = truthy_text(leg.get("symbol"));
        parts.push(if parts.is_empty() {
            format!("{}{}", scale, symbol)
        } else {
            format!("{} {}{}", sign, scale, symbol)
        });
    }
    parts.join(" ")
}

/// A resolved contract as a card leg.
fn leg_row(contract: &Contract, side: &str, lots: i64, origin: &str) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("symbol".into(), json!(contract.symbol));
    row.insert("exchange".into(), json!(contract.exchange));
    row.insert("segment".into(), json!(contract.segment));
    row.insert("side".into(), json!(side));
    row.insert("lots".into(), json!(lots));
    row.insert("multiplier".into(), json!(signed_multiplier(side, lots)));
    row.insert("origin".into(), json!(origin));
    row.insert("role".into(), json!("named"));
    let full = contract.to_json();
    for (source, target) in [
        ("option_type", "option_type"),
        ("strike", "strike"),
        ("expiry", "expiry"),
        ("lotSize", "lot_size"),
        ("tickSize", "tick_size"),
        ("base", "base"),
        ("underlying_exchange", "underlying_exchange"),
    ] {
        if let Some(v) = full.get(source).filter(|v| !v.is_null() && *v != "") {
            row.insert(target.into(), v.clone());
        }
    }
    row
}

pub async fn stream_combo(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let mut notices = Vec::new();
    let entries = leg_entries(args.get("legs"), "legs")?;
    let default_expiry = json!(DEFAULT_EXPIRY_CHOICE);
    let expiry = match args.get("expiry") {
        None => Some(&default_expiry),
        given => given,
    };
    if !entries.is_empty() {
        return custom_combo(ctx, &entries, args.get("underlying"), expiry, &mut notices).await;
    }
    let one = json!(1);
    let width = match args.get("width") {
        None | Some(Value::Null) => &one,
        Some(w) => w,
    };
    named_structure(ctx, args, expiry, width, &mut notices).await
}

/// The instrument the underlying is quoted on: itself, or the near-month
/// future where the exchange lists no spot.
fn spot_instrument(
    ctx: &ToolCtx,
    base: &str,
    venue: &str,
) -> Result<(String, String), ToolOutcome> {
    if !NO_SPOT_EXCHANGES.contains(&venue) {
        return Ok((base.to_string(), venue.to_string()));
    }
    options_service::near_future_symbol(&ctx.app, base, venue)
        .map(|s| (s, venue.to_string()))
        .ok_or_else(|| {
            ToolOutcome::invalid_argument(
                "underlying",
                &format!(
                    "{} has no unexpired futures contract on {}, and {} lists no spot instrument",
                    base, venue, venue
                ),
                Some("Check the underlying, or re-download the master contract."),
            )
        })
}

/// The underlying's last traded price; when `required`, nothing usable is
/// a refusal, because every strike is chosen relative to it.
async fn spot_price(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
    required: bool,
) -> Result<Option<f64>, ToolOutcome> {
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    let response = ctx
        .service_read("quotes_service.get_quotes", "quotes", request)
        .await?;
    let price = data_of(response)
        .get("ltp")
        .and_then(as_number)
        .filter(|p| *p > 0.0);
    if price.is_some() || !required {
        return Ok(price);
    }
    Err(ToolOutcome::invalid_argument(
        "underlying",
        &format!("{} on {} returned no last traded price, so the ATM strike cannot be chosen", symbol, exchange),
        Some("Say the underlying could not be priced rather than naming a strike, and check the instrument is one this broker quotes."),
    ))
}

/// The listed call strikes of `base` on `expiry` and the ATM among them.
fn ladder(
    ctx: &ToolCtx,
    base: &str,
    expiry: &str,
    venue: &str,
    spot: f64,
) -> (Option<f64>, Option<f64>) {
    let snap = ctx.app.symbols.snapshot();
    let strikes = options_service::available_strikes(
        snap.rows(),
        base,
        expiry,
        "CE",
        &options_service::option_exchange(venue),
    );
    let atm = options_service::atm_index(&strikes, spot).and_then(|i| strikes.get(i).copied());
    (atm, strike_spacing(&strikes))
}

/// What a combination card is built from, once resolved.
struct Combo {
    structure: String,
    summary: String,
    constant_strike: bool,
    base: String,
    venue: String,
    expiry: String,
    expiry_choice: String,
    legs: Vec<Map<String, Value>>,
    spot_symbol: String,
    spot_exchange: String,
    spot: Option<f64>,
    atm_strike: Option<f64>,
    interval: Option<f64>,
    claims_atm: bool,
}

async fn named_structure(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    expiry: Option<&Value>,
    width: &Value,
    notices: &mut Vec<String>,
) -> Result<ToolOutcome, ToolOutcome> {
    let mut name = truthy_text(args.get("structure"))
        .to_lowercase()
        .replace([' ', '-'], "_");
    if name.is_empty() {
        name = "straddle".into();
    }
    let Some(chosen) = STRUCTURES.iter().find(|s| s.name == name) else {
        let names: Vec<&str> = STRUCTURES.iter().map(|s| s.name).collect();
        return Err(ToolOutcome::invalid_argument(
            "structure",
            &format!("{} is not a structure this tool builds", quote(&name)),
            Some(&format!(
                "Pass one of {}, or name the contracts themselves in 'legs'.",
                names.join(", ")
            )),
        ));
    };
    let base = normalise_symbol(args.get("underlying"), "underlying")?;
    let listed = master(&ctx.app);
    let venue = resolve_underlying_exchange(&listed, &base, args.get("exchange"), notices)?;
    let expiries = listed_expiries(ctx, &base, &venue).await?;
    if expiries.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "underlying",
            &format!("{} lists no option expiries on {}", base, options_service::option_exchange(&venue)),
            Some("Confirm the underlying has listed options, or watch the instruments themselves with stream_quotes."),
        ));
    }
    let today = crate::agent::tools::market::normalise::today(&ctx.app);
    let settled = choose_expiry(&expiries, expiry, notices, today)?;
    let steps = if chosen.needs_width {
        normalise_int(Some(width), "width", 1, 20, None)?
    } else {
        1
    };
    if !chosen.needs_width && as_number(width).is_some_and(|w| w != 1.0) {
        notices.push(format!(
            "A {} has no out of the money leg, so 'width' was not used.",
            name
        ));
    }
    let (spot_symbol, spot_exchange) = spot_instrument(ctx, &base, &venue)?;
    let spot = spot_price(ctx, &spot_symbol, &spot_exchange, true)
        .await?
        .unwrap_or_default();
    let (atm_strike, interval) = ladder(ctx, &base, &settled, &venue, spot);
    let mut legs = Vec::new();
    for (index, template) in chosen.legs.iter().enumerate() {
        let leg = if template.spot {
            spot_leg(template, &spot_symbol, &spot_exchange)
        } else {
            option_leg(ctx, template, (&base, &venue, &settled), steps, index + 1).await?
        };
        legs.push(leg);
    }
    let choice = match expiry.map(|e| truthy_text(Some(e))) {
        Some(t) if !t.is_empty() => t.to_lowercase(),
        _ => DEFAULT_EXPIRY_CHOICE.to_string(),
    };
    deliver(
        ctx,
        Combo {
            structure: name,
            summary: chosen.summary.into(),
            constant_strike: chosen.constant_strike,
            base,
            venue,
            expiry: settled,
            expiry_choice: choice,
            legs,
            spot_symbol,
            spot_exchange,
            spot: Some(spot),
            atm_strike,
            interval,
            claims_atm: chosen.legs.iter().any(|t| !t.spot && !t.out_of_money),
        },
        notices,
    )
    .await
}

/// One option leg of a named structure: the strike picked by the option
/// symbol service from the strikes the master lists, never worked out here.
async fn option_leg(
    ctx: &ToolCtx,
    template: &LegTemplate,
    (base, venue, expiry): (&str, &str, &str),
    width: i64,
    position: usize,
) -> Result<Map<String, Value>, ToolOutcome> {
    let offset = if template.out_of_money {
        format!("OTM{}", width)
    } else {
        "ATM".to_string()
    };
    let mut request = Map::new();
    request.insert("underlying".into(), json!(base));
    request.insert("exchange".into(), json!(venue));
    request.insert("expiry_date".into(), json!(expiry));
    request.insert("offset".into(), json!(offset));
    request.insert("option_type".into(), json!(template.option_type));
    let payload = ctx
        .service_read(
            "option_symbol_service.get_option_symbol",
            "optionsymbol",
            request,
        )
        .await?;
    let symbol = truthy_text(payload.get("symbol")).to_uppercase();
    let listed_on = truthy_text(payload.get("exchange")).to_uppercase();
    if symbol.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "structure",
            &format!("leg {} ({} {}) resolved to no contract", position, offset, template.option_type),
            Some("Try a nearer expiry, a smaller width, or watch the instruments themselves with stream_quotes."),
        ));
    }
    let listed = master(&ctx.app);
    let where_ = format!("the {} {} leg", offset, template.option_type);
    let contract = resolve_contract(ctx, &listed, &symbol, &listed_on, Some(&where_)).await?;
    let mut row = leg_row(&contract, template.side, template.lots, "structure");
    let right = if template.option_type == "CE" {
        "call"
    } else {
        "put"
    };
    row.insert(
        "role".into(),
        json!(format!("{}_{}", offset.to_lowercase(), right)),
    );
    Ok(row)
}

/// The underlying itself as a leg (a basis is the synthetic less spot).
fn spot_leg(template: &LegTemplate, symbol: &str, exchange: &str) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("symbol".into(), json!(symbol));
    row.insert("exchange".into(), json!(exchange));
    row.insert("segment".into(), json!("SPOT"));
    row.insert("side".into(), json!(template.side));
    row.insert("lots".into(), json!(template.lots));
    row.insert(
        "multiplier".into(),
        json!(signed_multiplier(template.side, template.lots)),
    );
    row.insert("origin".into(), json!("structure"));
    row.insert("role".into(), json!("spot"));
    row
}

/// A combination of the contracts the trader named.
async fn custom_combo(
    ctx: &ToolCtx,
    entries: &[Value],
    underlying: Option<&Value>,
    expiry: Option<&Value>,
    notices: &mut Vec<String>,
) -> Result<ToolOutcome, ToolOutcome> {
    let base = truthy_text(underlying).to_uppercase();
    let mut shorthand = truthy_text(expiry).to_uppercase();
    // A shorthand leg (`24500CE`) cannot expand against an expiry keyword.
    // The web compares the upper-cased text with the lower-case keywords, so
    // it never clears one; the desktop compares case-insensitively.
    if EXPIRY_CHOICES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&shorthand))
    {
        shorthand.clear();
    }
    let listed = master(&ctx.app);
    let mut legs = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let index = i + 1;
        let (symbol, venue) = leg_symbol(entry, index, &base, &shorthand)?;
        let contract = resolve_contract(
            ctx,
            &listed,
            &symbol,
            &venue,
            Some(&format!("leg {}", index)),
        )
        .await?;
        let (side, lots) = leg_side_and_lots(
            entry,
            index,
            || master_lot_size(ctx, &contract.symbol, &contract.exchange),
            notices,
        )
        .await?;
        legs.push(leg_row(&contract, side, lots, "named"));
    }
    let base_of = |leg: &Map<String, Value>| truthy_text(leg.get("base"));
    let mut underlyings: Vec<String> = legs.iter().map(base_of).collect();
    underlyings.sort();
    underlyings.dedup();
    let first = legs.first().cloned().unwrap_or_default();
    if underlyings.len() > 1 {
        let named: Vec<String> = underlyings
            .iter()
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        notices.push(format!(
            "The legs span {}, so the card shows their combined value against {}, which is the underlying it is centred on.",
            named.join(", "),
            base_of(&first)
        ));
    }
    let combo_base = base_of(&first);
    let combo_venue = truthy_text(first.get("underlying_exchange"));
    let (spot_symbol, spot_exchange) = spot_instrument(ctx, &combo_base, &combo_venue)?;
    let spot = spot_price(ctx, &spot_symbol, &spot_exchange, false).await?;
    let settled = truthy_text(first.get("expiry"));
    let (mut atm_strike, mut interval) = (None, None);
    if let Some(s) = spot.filter(|_| !settled.is_empty() && !combo_base.is_empty()) {
        (atm_strike, interval) = ladder(ctx, &combo_base, &settled, &combo_venue, s);
    }
    deliver(
        ctx,
        Combo {
            structure: CUSTOM_STRUCTURE.into(),
            summary: "the contracts the operator named, bought legs added and sold legs subtracted"
                .into(),
            constant_strike: false,
            base: combo_base,
            venue: combo_venue,
            expiry: settled,
            expiry_choice: "named".into(),
            legs,
            spot_symbol,
            spot_exchange,
            spot,
            atm_strike,
            interval,
            claims_atm: false,
        },
        notices,
    )
    .await
}

/// The heading: `NIFTY 09SEP26 24500 straddle`; named contracts are a
/// `combination`.
fn combo_label(c: &Combo) -> String {
    let words = if c.structure == CUSTOM_STRUCTURE {
        "combination".to_string()
    } else {
        c.structure.replace('_', " ")
    };
    let at = match (c.claims_atm, c.atm_strike) {
        (true, Some(s)) => py_g(s),
        _ => String::new(),
    };
    [c.base.clone(), c.expiry.clone(), at, words]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What seeding a card's legs produced.
struct Seeded {
    /// The strike every option leg shares, when the value is quoted
    /// against it.
    constant: Option<f64>,
    /// The seeded value, when every leg had a traded price.
    value: Option<f64>,
    /// Legs that had a traded price.
    priced: usize,
    spot_seed: Option<Value>,
    subscribe: Vec<Value>,
}

/// Seed the legs from one snapshot, the constant when every option leg
/// shares one strike, and the value when every leg priced.
async fn seed_legs(ctx: &ToolCtx, c: &mut Combo, notices: &mut Vec<String>) -> Seeded {
    let mut pairs: Vec<(String, String)> = c
        .legs
        .iter()
        .map(|l| (truthy_text(l.get("symbol")), truthy_text(l.get("exchange"))))
        .collect();
    pairs.push((c.spot_symbol.clone(), c.spot_exchange.clone()));
    let subscribe = subscription_list(pairs.iter().map(|(s, e)| (s.as_str(), e.as_str())));
    let quoted = snapshot(ctx, &subscribe, notices).await;
    let mut seeded = 0;
    for leg in c.legs.iter_mut() {
        let key = (
            truthy_text(leg.get("symbol")),
            truthy_text(leg.get("exchange")),
        );
        if let Some(raw) = quoted.get(&key) {
            let seed = normalise_quote(raw);
            if seed.get("ltp").and_then(as_number).is_some() {
                seeded += 1;
            }
            leg.insert("seed".into(), Value::Object(seed));
        }
    }
    let mut constant = None;
    if c.constant_strike {
        let mut strikes: Vec<f64> = c
            .legs
            .iter()
            .filter(|l| l.get("segment") == Some(&json!("OPTION")))
            .filter_map(|l| l.get("strike").and_then(as_number))
            .collect();
        strikes.sort_by(f64::total_cmp);
        strikes.dedup();
        if let [only] = strikes.as_slice() {
            constant = Some(*only);
        } else {
            notices.push("The option legs sit at different strikes, so the value is shown as the net premium rather than as a price.".into());
        }
    }
    let mut value = None;
    if seeded == c.legs.len() && !c.legs.is_empty() {
        let mut total = constant.unwrap_or(0.0);
        for leg in &c.legs {
            let price = leg
                .get("seed")
                .and_then(|s| s.get("ltp"))
                .and_then(as_number)
                .unwrap_or(0.0);
            total += leg.get("multiplier").and_then(as_number).unwrap_or(0.0) * price;
        }
        value = Some(py_round(total, 4));
    }
    let spot_seed = quoted
        .get(&(c.spot_symbol.clone(), c.spot_exchange.clone()))
        .map(|q| Value::Object(normalise_quote(q)));
    Seeded {
        constant,
        value,
        priced: seeded,
        spot_seed,
        subscribe,
    }
}

async fn deliver(
    ctx: &ToolCtx,
    mut c: Combo,
    notices: &mut Vec<String>,
) -> Result<ToolOutcome, ToolOutcome> {
    if c.legs.len() > MAX_LEGS {
        notices.push(format!(
            "{} of {} legs were dropped, because one card carries at most {}.",
            c.legs.len() - MAX_LEGS,
            c.legs.len(),
            MAX_LEGS
        ));
        c.legs.truncate(MAX_LEGS);
    }
    let seeded = seed_legs(ctx, &mut c, notices).await;
    let value = seeded.value;
    let spec = combo_spec(ctx, &c, seeded, notices);
    let safe = match json_safe(&Value::Object(spec.clone())) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let label = truthy_text(spec.get("label"));
    let source = "quotes_service, option_symbol_service, expiry_service, symbol_service, market_calendar_service";
    if !ctx
        .viz
        .emit("stream_combo", LIVE_COMBO_VIZ, safe, &label, source)
    {
        let message = no_sink_message("live combination card");
        return Ok(tool_answer("stream_combo", &message, notices, &[]));
    }
    let labels: [Label<'_>; 2] = [
        ("underlying", (!c.base.is_empty()).then(|| c.base.clone())),
        ("expiry", (!c.expiry.is_empty()).then(|| c.expiry.clone())),
    ];
    let message = combo_confirmation(&spec, &c.legs, value, c.spot);
    Ok(tool_answer("stream_combo", &message, notices, &labels))
}

/// The combination card (viz kind `live_combo`), in the web's key order.
fn combo_spec(ctx: &ToolCtx, c: &Combo, s: Seeded, notices: &[String]) -> Map<String, Value> {
    let mut spot_row = Map::new();
    spot_row.insert("symbol".into(), json!(c.spot_symbol));
    spot_row.insert("exchange".into(), json!(c.spot_exchange));
    if let Some(seed) = s.spot_seed {
        spot_row.insert("seed".into(), seed);
    }
    if let Some(spot) = c.spot {
        spot_row.insert("ltp".into(), json!(spot));
    }
    let mut lot_sizes: Vec<i64> = c
        .legs
        .iter()
        .filter_map(|l| l.get("lot_size").and_then(as_number))
        .filter(|l| *l != 0.0)
        .map(|l| l as i64)
        .collect();
    lot_sizes.sort_unstable();
    lot_sizes.dedup();
    let mut exchanges: Vec<String> = c
        .legs
        .iter()
        .map(|l| truthy_text(l.get("exchange")))
        .collect();
    exchanges.push(c.spot_exchange.clone());
    let market = market_block(ctx, exchanges.iter().map(String::as_str));
    let mut spec = Map::new();
    spec.insert("structure".into(), json!(c.structure));
    spec.insert("summary".into(), json!(c.summary));
    spec.insert("label".into(), json!(combo_label(c)));
    spec.insert("underlying".into(), json!(c.base));
    spec.insert("underlying_exchange".into(), json!(c.venue));
    spec.insert("expiry".into(), json!(c.expiry));
    spec.insert("expiry_choice".into(), json!(c.expiry_choice));
    spec.insert("mode".into(), json!(COMBO_MODE));
    spec.insert("currency".into(), json!(CURRENCY));
    spec.insert("account_mode".into(), json!(current_mode(ctx)));
    spec.insert(
        "as_of".into(),
        market.get("as_of").cloned().unwrap_or(Value::Null),
    );
    spec.insert("timezone".into(), json!(TIMEZONE));
    spec.insert("spot".into(), Value::Object(spot_row));
    spec.insert(
        "legs".into(),
        Value::Array(c.legs.iter().cloned().map(Value::Object).collect()),
    );
    let expression = formula_text(&c.legs, s.constant);
    spec.insert("formula".into(), json!({"kind": "signed_sum", "constant": s.constant, "per": "unit", "expression": expression}));
    spec.insert(
        "seed".into(),
        json!({"value": s.value, "complete": s.value.is_some(), "legs_priced": s.priced}),
    );
    spec.insert("subscribe".into(), Value::Array(s.subscribe));
    spec.insert("market".into(), Value::Object(market));
    if let [only] = lot_sizes.as_slice() {
        spec.insert("lot_size".into(), json!(only));
    }
    if let Some(strike) = c.atm_strike {
        let threshold = c
            .interval
            .filter(|i| *i != 0.0)
            .map(|i| py_round(i / 2.0, 4));
        spec.insert(
            "atm".into(),
            json!({
                "strike": strike,
                "strike_interval": c.interval,
                "spot_at_resolution": c.spot,
                "roll_threshold": threshold,
                "pinned": true,
                "claims_atm": c.claims_atm,
            }),
        );
    }
    if !notices.is_empty() {
        spec.insert("notices".into(), json!(notices));
    }
    spec
}

fn combo_confirmation(
    spec: &Map<String, Value>,
    legs: &[Map<String, Value>],
    value: Option<f64>,
    spot: Option<f64>,
) -> String {
    let label = truthy_text(spec.get("label"));
    let summary = truthy_text(spec.get("summary"));
    let described = legs.iter().map(leg_label).collect::<Vec<_>>().join("; ");
    let mut parts = vec![if described.is_empty() {
        format!("Opened a live card on the {}.", label)
    } else {
        format!("Opened a live card on the {}: {}.", label, described)
    }];
    match value {
        Some(v) => parts.push(format!(
            "It is {}, seeded at {}{}",
            summary,
            format_price(&json!(v)),
            spot.map_or(".".to_string(), |s| format!(" with spot {}.", format_price(&json!(s))))
        )),
        None => parts.push(format!(
            "It is {}. Not every leg had a traded price, so the card opens incomplete and fills in on the first tick.",
            summary
        )),
    }
    if let Some(atm) = spec.get("atm") {
        let threshold = atm
            .get("roll_threshold")
            .and_then(as_number)
            .filter(|t| *t != 0.0);
        if let (Some(true), Some(t), Some(s)) = (
            atm.get("claims_atm").and_then(Value::as_bool),
            threshold,
            atm.get("strike").and_then(as_number),
        ) {
            parts.push(format!(
                "The legs are pinned at the {} strike. If spot moves more than {} away the card says the strike is no longer the ATM rather than relabelling itself, so it keeps meaning what it meant when it opened.",
                py_g(s),
                py_g(t)
            ));
        }
    }
    let market = spec.get("market");
    if market.and_then(|m| m.get("known")) == Some(&json!(true))
        && market.and_then(|m| m.get("is_open")) != Some(&json!(true))
    {
        parts.push("The exchanges on the card are outside their trading session right now.".into());
    }
    parts.push("The value recomputes on every tick on the operator's screen, so describe what they can watch rather than quoting the seeded number as though it were current.".into());
    parts.join(" ")
}
