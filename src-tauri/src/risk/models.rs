//! Value types for the shared risk core (web `services/risk/models.py`).
//!
//! Data only. The only behaviour here is normalising untrusted input into
//! typed values. Nothing reads a clock, a database, a broker or a feed.
//!
//! Naming follows OpenAlgo: sides are `BUY` / `SELL`, stops and targets are
//! absolute prices, and the loose-dict loaders accept the scalping field
//! names (`entry_price`, `current_sl`, `initial_sl`, `target`) as aliases.

use serde_json::{Map, Value};

/// The favourable excursion a position must show before a trail may move the
/// stop. 1.0 is the value both shipped web engines use (`MIN_TRAIL_PROFIT`).
pub const DEFAULT_TRAIL_TRIGGER: f64 = 1.0;

/// Position direction, using OpenAlgo's order constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

/// How a trailing stop derives its new level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrailMode {
    /// A fixed gap behind the best price seen.
    #[default]
    Continuous,
    /// One step per completed trigger, anchored at the configured stop.
    Stepped,
}

impl TrailMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrailMode::Continuous => "continuous",
            TrailMode::Stepped => "stepped",
        }
    }
}

/// Why a rule fired. The values are the web's wire strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreachReason {
    Stop,
    Target,
    CombinedStop,
    CombinedTarget,
    LockProfit,
}

impl BreachReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            BreachReason::Stop => "sl",
            BreachReason::Target => "target",
            BreachReason::CombinedStop => "combined_sl",
            BreachReason::CombinedTarget => "combined_target",
            BreachReason::LockProfit => "lock_profit",
        }
    }
}

/// Python `float(value)` on a JSON value: numbers, and numeric text with
/// surrounding whitespace. `bool` and `null` are not numbers here.
pub fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// A usable, strictly positive, finite price. Zero is not a price: nothing on
/// an Indian exchange trades at or below zero, so a zero is a missing value.
pub fn is_price(value: Option<f64>) -> bool {
    matches!(value, Some(v) if v.is_finite() && v > 0.0)
}

/// `is_price` on a loose JSON value.
pub fn is_price_value(value: &Value) -> bool {
    is_price(value_to_f64(value))
}

/// Coerce to a float, with `default` for anything unusable or non-finite.
pub fn as_float(value: &Value, default: f64) -> f64 {
    match value_to_f64(value) {
        Some(v) if v.is_finite() => v,
        _ => default,
    }
}

/// A price, or `None` when the value is not a usable price.
pub fn as_price(value: Option<f64>) -> Option<f64> {
    if is_price(value) {
        value
    } else {
        None
    }
}

/// `as_price` on a loose JSON value.
pub fn as_price_value(value: &Value) -> Option<f64> {
    as_price(value_to_f64(value))
}

/// Python's `str(value or "")` for the side and mode loaders.
fn text_of(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(false) => String::new(),
        Value::Bool(true) => "True".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if n.as_f64() == Some(0.0) {
                String::new()
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// Map anything a caller might send to a side. Unknown values fall back to
/// BUY, matching the shipped scalping engine.
pub fn normalise_side(text: &str) -> Side {
    match text.trim().to_ascii_uppercase().as_str() {
        "SELL" | "S" | "SHORT" | "-1" => Side::Sell,
        _ => Side::Buy,
    }
}

pub fn normalise_side_value(value: &Value) -> Side {
    normalise_side(&text_of(value))
}

pub fn normalise_trail_mode(text: &str) -> TrailMode {
    match text.trim().to_ascii_lowercase().as_str() {
        "stepped" | "step" | "staircase" => TrailMode::Stepped,
        _ => TrailMode::Continuous,
    }
}

/// A points-based stop distance as the absolute stop price.
pub fn stop_from_points(side: Side, entry_price: f64, points: f64) -> Option<f64> {
    if !is_price(Some(entry_price)) || !is_price(Some(points)) {
        return None;
    }
    let price = match side {
        Side::Buy => entry_price - points,
        Side::Sell => entry_price + points,
    };
    (price > 0.0).then_some(price)
}

/// A points-based target distance as the absolute target price.
pub fn target_from_points(side: Side, entry_price: f64, points: f64) -> Option<f64> {
    if !is_price(Some(entry_price)) || !is_price(Some(points)) {
        return None;
    }
    let price = match side {
        Side::Buy => entry_price + points,
        Side::Sell => entry_price - points,
    };
    (price > 0.0).then_some(price)
}

/// The side from a signed net quantity, as a position book reports it.
pub fn side_from_quantity(net_quantity: f64) -> Side {
    if net_quantity < 0.0 {
        Side::Sell
    } else {
        Side::Buy
    }
}

/// First present (non-null) value among `names`.
pub(crate) fn first<'a>(state: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .filter_map(|n| state.get(*n))
        .find(|v| !v.is_null())
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Everything the core needs to judge one position against one tick.
///
/// `stop_price` is the live stop (a trail may have moved it);
/// `initial_stop_price` is the configured level a stepped trail advances
/// from. `quantity` is a magnitude; `side` is the single source of direction.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRisk {
    pub identifier: String,
    pub side: Side,
    pub entry_price: f64,
    pub quantity: f64,
    pub stop_price: Option<f64>,
    pub initial_stop_price: Option<f64>,
    pub target_price: Option<f64>,
    pub trailing_enabled: bool,
    pub trail_step: f64,
    pub trail_trigger: f64,
    pub trail_mode: TrailMode,
    pub highest_price: Option<f64>,
    pub lowest_price: Option<f64>,
}

impl Default for PositionRisk {
    fn default() -> Self {
        Self {
            identifier: String::new(),
            side: Side::Buy,
            entry_price: 0.0,
            quantity: 0.0,
            stop_price: None,
            initial_stop_price: None,
            target_price: None,
            trailing_enabled: false,
            trail_step: 0.0,
            trail_trigger: DEFAULT_TRAIL_TRIGGER,
            trail_mode: TrailMode::Continuous,
            highest_price: None,
            lowest_price: None,
        }
    }
}

impl PositionRisk {
    pub fn is_long(&self) -> bool {
        self.side == Side::Buy
    }

    /// The stop in force: the live one, else the configured one. Never the
    /// entry price: a position with no stop configured has no stop.
    pub fn effective_stop(&self) -> Option<f64> {
        as_price(self.stop_price).or_else(|| as_price(self.initial_stop_price))
    }

    /// Build from a loose dict: the scalping state shape or the canonical one.
    pub fn from_state(state: &Value) -> Self {
        let empty = Map::new();
        let state = state.as_object().unwrap_or(&empty);
        let get = |names: &[&str]| first(state, names).cloned().unwrap_or(Value::Null);

        let entry = as_float(
            &get(&["entry_price", "entry", "entry_avg", "average_price"]),
            0.0,
        );
        let side = normalise_side_value(&get(&["side", "action", "position"]));
        let stop = as_price_value(&get(&["stop_price", "current_sl", "currentSl"]));
        let mut initial_stop =
            as_price_value(&get(&["initial_stop_price", "initial_sl", "initialSl"]));
        let mut target = as_price_value(&get(&["target_price", "target", "targetPrice"]));

        // Points-configured callers (the strategy PRD, Flow) convert once, here.
        if stop.is_none() && initial_stop.is_none() {
            initial_stop =
                stop_from_points(side, entry, as_float(&get(&["sl_points", "sl_pts"]), 0.0));
        }
        if target.is_none() {
            target = target_from_points(
                side,
                entry,
                as_float(&get(&["target_points", "target_pts"]), 0.0),
            );
        }

        let trigger = first(state, &["trail_trigger", "trailing_trigger", "trail_x"]);
        let identifier = match first(state, &["identifier", "id", "symbol"]) {
            Some(v) if truthy(Some(v)) => match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
            _ => String::new(),
        };
        Self {
            identifier,
            side,
            entry_price: entry,
            quantity: as_float(&get(&["quantity", "qty"]), 0.0).abs(),
            stop_price: stop,
            initial_stop_price: initial_stop,
            target_price: target,
            trailing_enabled: truthy(first(state, &["trailing_enabled", "trailingEnabled"])),
            trail_step: as_float(
                &get(&["trail_step", "trailing_step", "trailingStep", "trail_y"]),
                0.0,
            ),
            trail_trigger: match trigger {
                Some(v) => as_float(v, DEFAULT_TRAIL_TRIGGER),
                None => DEFAULT_TRAIL_TRIGGER,
            },
            trail_mode: normalise_trail_mode(&text_of(&get(&["trail_mode", "trailMode"]))),
            highest_price: as_price_value(&get(&["highest_price", "highestPrice"])),
            lowest_price: as_price_value(&get(&["lowest_price", "lowestPrice"])),
        }
    }
}

/// What one tick did to one position.
///
/// `evaluated` is false when the tick itself was unusable; every other field
/// is then the input carried through, so a caller can always write the
/// decision back without a special case.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionDecision {
    pub identifier: String,
    pub evaluated: bool,
    pub stop_price: Option<f64>,
    pub target_price: Option<f64>,
    pub highest_price: Option<f64>,
    pub lowest_price: Option<f64>,
    pub pnl: f64,
    pub breached: bool,
    pub reason: Option<BreachReason>,
    pub detail: String,
    pub stop_moved: bool,
    pub trail_armed: bool,
}

impl PositionDecision {
    /// The legacy `evaluate_trail` return shape, field for field.
    pub fn to_trail_state(&self) -> Value {
        serde_json::json!({
            "highest_price": opt(self.highest_price),
            "lowest_price": opt(self.lowest_price),
            "current_sl": opt(self.stop_price),
            "breached": self.breached,
            "reason": self.reason.map(|r| r.as_str()),
        })
    }

    /// JSON-ready form for a REST response or a log line.
    pub fn as_dict(&self) -> Value {
        serde_json::json!({
            "identifier": self.identifier,
            "evaluated": self.evaluated,
            "stop_price": opt(self.stop_price),
            "target_price": opt(self.target_price),
            "highest_price": opt(self.highest_price),
            "lowest_price": opt(self.lowest_price),
            "pnl": num(self.pnl),
            "breached": self.breached,
            "reason": self.reason.map(|r| r.as_str()),
            "detail": self.detail,
            "stop_moved": self.stop_moved,
            "trail_armed": self.trail_armed,
        })
    }
}

fn num(v: f64) -> Value {
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn opt(v: Option<f64>) -> Value {
    v.map(num).unwrap_or(Value::Null)
}

/// One position's contribution to the aggregate mark to market.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PositionPnL {
    pub identifier: String,
    pub side: Side,
    pub entry_price: f64,
    pub quantity: f64,
    pub last_price: Option<f64>,
    pub closed: bool,
    pub realized_pnl: f64,
}

impl PositionPnL {
    pub fn from_state(state: &Value) -> Self {
        let empty = Map::new();
        let state = state.as_object().unwrap_or(&empty);
        let get = |k: &str| state.get(k).filter(|v| !v.is_null());
        let quantity = as_float(get("quantity").or(get("qty")).unwrap_or(&Value::Null), 0.0);
        let side_value = get("side").or(get("action"));
        let side = match side_value {
            Some(v) if truthy(Some(v)) => normalise_side_value(v),
            _ => side_from_quantity(quantity),
        };
        let identifier = [state.get("identifier"), state.get("symbol")]
            .into_iter()
            .flatten()
            .find(|v| truthy(Some(v)))
            .map(|v| match v {
                Value::String(s) => s.clone(),
                o => o.to_string(),
            })
            .unwrap_or_default();
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        Self {
            identifier,
            side,
            entry_price: as_float(
                get("entry_price")
                    .or(get("average_price"))
                    .unwrap_or(&Value::Null),
                0.0,
            ),
            quantity: quantity.abs(),
            last_price: as_price_value(get("last_price").or(get("ltp")).unwrap_or(&Value::Null)),
            closed: truthy(state.get("closed")) || status == "closed",
            realized_pnl: as_float(
                get("realized_pnl")
                    .or(get("realized"))
                    .unwrap_or(&Value::Null),
                0.0,
            ),
        }
    }
}

/// Aggregate mark to market, named as the web's strategy P&L service names it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PnLSummary {
    pub realized: f64,
    pub unrealized: f64,
    pub total: f64,
    pub priced: usize,
    pub unpriced: usize,
}

/// Portfolio limits and the ratchet state they need carried between ticks.
///
/// `combined_stoploss` is a magnitude: 5000 and -5000 mean the same loss.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AggregateRisk {
    pub combined_stoploss: Option<f64>,
    pub combined_target: Option<f64>,
    pub lock_profit_at: Option<f64>,
    pub lock_profit_floor: Option<f64>,
    pub lock_trail_step: Option<f64>,
    pub lock_armed: bool,
    pub lock_floor: Option<f64>,
    pub peak_pnl: f64,
    pub trough_pnl: f64,
    pub stop_bypassed: bool,
}

/// Web `_optional_float`: a finite number or `None`.
pub fn optional_float(value: Option<&Value>) -> Option<f64> {
    value.and_then(value_to_f64).filter(|v| v.is_finite())
}

impl AggregateRisk {
    pub fn from_state(state: &Value) -> Self {
        let empty = Map::new();
        let state = state.as_object().unwrap_or(&empty);
        let get = |names: &[&str]| first(state, names);
        Self {
            combined_stoploss: optional_float(get(&[
                "combined_stoploss",
                "overall_sl_mtm",
                "max_loss",
            ])),
            combined_target: optional_float(get(&[
                "combined_target",
                "overall_target_mtm",
                "max_profit",
            ])),
            lock_profit_at: optional_float(get(&["lock_profit_at", "if_profit_reaches"])),
            lock_profit_floor: optional_float(get(&["lock_profit_floor", "lock_profit"])),
            lock_trail_step: optional_float(get(&["lock_trail_step", "trail_step"])),
            lock_armed: truthy(get(&["lock_armed"])),
            lock_floor: optional_float(get(&["lock_floor"])),
            peak_pnl: as_float(get(&["peak_pnl", "pnl_peak"]).unwrap_or(&Value::Null), 0.0),
            trough_pnl: as_float(
                get(&["trough_pnl", "pnl_trough"]).unwrap_or(&Value::Null),
                0.0,
            ),
            stop_bypassed: truthy(get(&["stop_bypassed", "trail_to_entry_active"])),
        }
    }
}

/// What one aggregate evaluation decided. The ratchets (`peak_pnl`,
/// `trough_pnl`, `lock_armed`, `lock_floor`) are always returned.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AggregateDecision {
    pub total_pnl: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub peak_pnl: f64,
    pub trough_pnl: f64,
    pub lock_armed: bool,
    pub lock_floor: Option<f64>,
    pub lock_armed_now: bool,
    pub lock_floor_raised: bool,
    pub breached: bool,
    pub reason: Option<BreachReason>,
    pub detail: String,
}

/// One stop relocation the caller should apply.
#[derive(Debug, Clone, PartialEq)]
pub struct StopMove {
    pub identifier: String,
    pub previous_stop: Option<f64>,
    pub new_stop: f64,
}

/// Which stops trail to entry, and which were deliberately left alone.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrailToEntryDecision {
    pub moves: Vec<StopMove>,
    pub skipped_not_improving: Vec<String>,
    pub skipped_through_price: Vec<String>,
    pub skipped_no_entry: Vec<String>,
    pub detail: String,
}

impl TrailToEntryDecision {
    pub fn moved(&self) -> usize {
        self.moves.len()
    }
}

/// Compact plain-text price for a detail message. No currency symbol.
pub fn format_price(value: f64) -> String {
    let text = format!("{:.4}", value);
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".into()
    } else {
        trimmed.to_string()
    }
}
