//! The order guard (the web's `services/agent/safety/risk.py`), pure.
//!
//! No I/O of any kind: no database, file, clock, network or log. The
//! caller hands in the trader's limits, whether the kill switch file
//! exists, the analyzer toggle, the last traded price and the funds, the
//! time, and the session's book; every decision comes back as a
//! [`Verdict`]. Nothing the model or a conversation says reaches it, so
//! no wording can talk past it. It runs inside each order tool, after the
//! trader approved and before anything is sent: approval is not a bypass.
//!
//! The rules, in this order (the order is the point):
//!
//! 1. kill switch, the stored setting or the file
//! 2. trading enabled
//! 3. analyzer mode, when the trader requires it (fails closed)
//! 4. symbol: blocklist, then allowlist
//! 5. exchange
//! 6. product
//! 7. quantity
//! 8. session order cap
//! 9. duplicate-order window
//! 10. notional cap and limit-price deviation (fail open without a price)
//! 11. affordability against available funds (fails open)
//!
//! Rules 8 and 9 and the claim are one step on the book ([`book`]); a
//! refusal after the claim (rules 10 and 11) gives the claim back.
//!
//! Desktop difference (D-007): a modified order is checked against rules
//! 1 to 3, 5 to 7 and 10 ([`check_modify`]); the web checked only 1 to 3,
//! so a modify could raise a quantity or move a price past the limits.

pub mod book;
pub mod intent;
pub mod num;

#[cfg(test)]
mod tests;

use crate::agent::safety::limits::RiskLimits;
use book::{Book, Claimed};
use chrono::{DateTime, Utc};
use intent::{arg, missing_keys_message, normalise, Intent, ORDER_KEYS, REQUIRED_KEYS};
use num::{fmt2, py_decimal, py_repr, py_str, py_text_or_empty, quantize2};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

/// The guard's outcome vocabulary. The text reaches the audit trail and
/// the model, so it is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskCode {
    Ok,
    KillSwitch,
    TradingDisabled,
    AnalyzerRequired,
    AnalyzerUnknown,
    InvalidArguments,
    InvalidSymbol,
    SymbolBlocked,
    SymbolNotAllowed,
    ExchangeNotAllowed,
    ProductNotAllowed,
    InvalidAction,
    InvalidPriceType,
    InvalidPrice,
    InvalidQuantity,
    QuantityExceeded,
    SessionCapReached,
    DuplicateOrder,
    NotionalExceeded,
    PriceDeviation,
    InsufficientFunds,
    EmptyBasket,
    UnknownOperation,
    BulkNotAllowed,
}

impl RiskCode {
    /// Every code, in the web's `RiskCode` order.
    pub const ALL: [RiskCode; 24] = [
        RiskCode::Ok,
        RiskCode::KillSwitch,
        RiskCode::TradingDisabled,
        RiskCode::AnalyzerRequired,
        RiskCode::AnalyzerUnknown,
        RiskCode::InvalidArguments,
        RiskCode::InvalidSymbol,
        RiskCode::SymbolBlocked,
        RiskCode::SymbolNotAllowed,
        RiskCode::ExchangeNotAllowed,
        RiskCode::ProductNotAllowed,
        RiskCode::InvalidAction,
        RiskCode::InvalidPriceType,
        RiskCode::InvalidPrice,
        RiskCode::InvalidQuantity,
        RiskCode::QuantityExceeded,
        RiskCode::SessionCapReached,
        RiskCode::DuplicateOrder,
        RiskCode::NotionalExceeded,
        RiskCode::PriceDeviation,
        RiskCode::InsufficientFunds,
        RiskCode::EmptyBasket,
        RiskCode::UnknownOperation,
        RiskCode::BulkNotAllowed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RiskCode::Ok => "ok",
            RiskCode::KillSwitch => "kill_switch",
            RiskCode::TradingDisabled => "trading_disabled",
            RiskCode::AnalyzerRequired => "analyzer_required",
            RiskCode::AnalyzerUnknown => "analyzer_unknown",
            RiskCode::InvalidArguments => "invalid_arguments",
            RiskCode::InvalidSymbol => "invalid_symbol",
            RiskCode::SymbolBlocked => "symbol_blocked",
            RiskCode::SymbolNotAllowed => "symbol_not_allowed",
            RiskCode::ExchangeNotAllowed => "exchange_not_allowed",
            RiskCode::ProductNotAllowed => "product_not_allowed",
            RiskCode::InvalidAction => "invalid_action",
            RiskCode::InvalidPriceType => "invalid_price_type",
            RiskCode::InvalidPrice => "invalid_price",
            RiskCode::InvalidQuantity => "invalid_quantity",
            RiskCode::QuantityExceeded => "quantity_exceeded",
            RiskCode::SessionCapReached => "session_cap_reached",
            RiskCode::DuplicateOrder => "duplicate_order",
            RiskCode::NotionalExceeded => "notional_exceeded",
            RiskCode::PriceDeviation => "price_deviation",
            RiskCode::InsufficientFunds => "insufficient_funds",
            RiskCode::EmptyBasket => "empty_basket",
            RiskCode::UnknownOperation => "unknown_operation",
            RiskCode::BulkNotAllowed => "bulk_operation_not_allowed",
        }
    }
}

/// Destructive operations that touch one named thing: allowed whenever
/// trading is on and the analyzer policy is met.
pub const TARGETED_OPERATIONS: [&str; 5] = [
    "cancel_order",
    "modify_order",
    "cancel_gtt_order",
    "modify_gtt_order",
    "close_position",
];

/// Destructive operations that sweep the whole account: switched off
/// until the trader turns `allow_bulk_destructive` on.
pub const BULK_OPERATIONS: [&str; 3] =
    ["cancel_all_orders", "close_all_positions", "square_off_all"];

/// A guard decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub allowed: bool,
    /// One sentence a trader or the model can act on.
    pub reason: String,
    pub code: RiskCode,
    /// The numbers behind it, already JSON-safe; `warnings` lists every
    /// check skipped for want of a price or funds.
    pub details: Map<String, Value>,
}

impl Verdict {
    /// The line a tool result, notice or log carries; a refusal names its
    /// code so it is not mistaken for a passing fault and retried.
    pub fn as_message(&self) -> String {
        if self.allowed {
            if self.reason.is_empty() {
                "Allowed by the risk guard.".into()
            } else {
                self.reason.clone()
            }
        } else {
            format!(
                "Blocked by the risk guard [{}]: {}",
                self.code.as_str(),
                self.reason
            )
        }
    }

    /// `{allowed, code, reason, details}`.
    pub fn as_dict(&self) -> Value {
        json!({
            "allowed": self.allowed,
            "code": self.code.as_str(),
            "reason": self.reason,
            "details": self.details,
        })
    }

    /// The claim this verdict holds, for a release or a commit.
    pub fn claim_token(&self) -> Option<&str> {
        self.details.get("claim_token").and_then(Value::as_str)
    }
}

/// What the guard is told about the world outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    /// The kill switch file exists.
    pub kill_switch_file_present: bool,
    /// The platform's analyzer toggle; `None` when it could not be read.
    pub analyzer_mode: Option<bool>,
}

fn verdict(allowed: bool, code: RiskCode, reason: String, details: Value) -> Verdict {
    Verdict {
        allowed,
        reason,
        code,
        details: match details {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    }
}

pub(crate) fn blocked(code: RiskCode, reason: String, details: Value) -> Verdict {
    verdict(false, code, reason, details)
}

fn allowed(reason: String, details: Map<String, Value>) -> Verdict {
    verdict(true, RiskCode::Ok, reason, Value::Object(details))
}

/// Rules 1 to 3.
fn gate(limits: &RiskLimits, gate: &Gate) -> Option<Verdict> {
    if limits.kill_switch_engaged {
        return Some(blocked(
            RiskCode::KillSwitch,
            "The agent kill switch is engaged. Release it in the agent settings to resume trading."
                .into(),
            json!({"source": "setting"}),
        ));
    }
    if gate.kill_switch_file_present {
        let path = limits.kill_switch_path.to_string_lossy().into_owned();
        return Some(blocked(
            RiskCode::KillSwitch,
            format!(
                "The agent kill switch file {} exists. Delete it to resume trading.",
                path
            ),
            json!({"source": "file", "kill_switch_file": path}),
        ));
    }
    if !limits.trading_enabled {
        return Some(blocked(
            RiskCode::TradingDisabled,
            "Agent trading is switched off. Enable it in the agent settings first.".into(),
            json!({}),
        ));
    }
    if limits.require_analyzer_mode {
        match gate.analyzer_mode {
            None => return Some(blocked(
                RiskCode::AnalyzerUnknown,
                "Analyzer mode is required but the platform's analyzer setting could not be read, so this order cannot be confirmed as a sandbox order.".into(),
                json!({}),
            )),
            Some(false) => return Some(blocked(
                RiskCode::AnalyzerRequired,
                "The agent is restricted to analyzer mode and the platform is in live mode. Switch the platform to analyzer mode, or turn off require_analyzer_mode in the agent settings.".into(),
                json!({}),
            )),
            Some(true) => {}
        }
    }
    None
}

fn sorted(set: &std::collections::BTreeSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

/// Rule 4.
fn check_symbol(intent: &Intent, limits: &RiskLimits) -> Option<Verdict> {
    if limits.symbol_blocklist.contains(&intent.symbol) {
        return Some(blocked(
            RiskCode::SymbolBlocked,
            format!("{} is on the agent's symbol blocklist.", intent.symbol),
            json!({"symbol": intent.symbol}),
        ));
    }
    if !limits.symbol_allowlist.is_empty() && !limits.symbol_allowlist.contains(&intent.symbol) {
        return Some(blocked(
            RiskCode::SymbolNotAllowed,
            format!("{} is not on the agent's symbol allowlist.", intent.symbol),
            json!({"symbol": intent.symbol, "allowed": sorted(&limits.symbol_allowlist)}),
        ));
    }
    None
}

/// Rules 5 to 7.
fn check_venue_and_size(intent: &Intent, limits: &RiskLimits) -> Option<Verdict> {
    if !limits.allowed_exchanges.contains(&intent.exchange) {
        return Some(blocked(
            RiskCode::ExchangeNotAllowed,
            format!(
                "The agent may not trade on {}. Allowed: {}.",
                intent.exchange,
                sorted(&limits.allowed_exchanges).join(", ")
            ),
            json!({"exchange": intent.exchange, "allowed": sorted(&limits.allowed_exchanges)}),
        ));
    }
    if !limits.allowed_products.contains(&intent.product) {
        return Some(blocked(
            RiskCode::ProductNotAllowed,
            format!(
                "The agent may not use product {}. Allowed: {}.",
                intent.product,
                sorted(&limits.allowed_products).join(", ")
            ),
            json!({"product": intent.product, "allowed": sorted(&limits.allowed_products)}),
        ));
    }
    if intent.quantity > limits.max_order_quantity {
        return Some(blocked(
            RiskCode::QuantityExceeded,
            format!(
                "Quantity {} exceeds the per-order limit of {}.",
                intent.quantity, limits.max_order_quantity
            ),
            json!({"quantity": intent.quantity, "limit": limits.max_order_quantity}),
        ));
    }
    None
}

/// Rule 10: the notional cap and the limit-price deviation. Without a
/// reference price both are skipped with a warning (fails open: the
/// broker runs its own checks).
fn check_pricing(
    intent: &Intent,
    limits: &RiskLimits,
    warnings: &mut Vec<String>,
) -> Option<Verdict> {
    let Some(reference) = intent.reference_price() else {
        warnings.push(format!(
            "No reference price for {}, so the notional cap and the price-deviation check were skipped.",
            intent.symbol
        ));
        return None;
    };
    let notional = reference * Decimal::from(intent.quantity);
    if notional > limits.max_order_value {
        return Some(blocked(
            RiskCode::NotionalExceeded,
            format!(
                "The order is worth about {}, above the per-order limit of {}.",
                fmt2(notional),
                limits.max_order_value
            ),
            json!({
                "notional": notional.to_string(),
                "limit": limits.max_order_value.to_string(),
                "reference_price": reference.to_string(),
                "quantity": intent.quantity,
            }),
        ));
    }
    let order_price = intent.deviation_price()?;
    let Some(ltp) = intent.ltp else {
        warnings.push(format!(
            "No last traded price for {}, so the price-deviation check was skipped.",
            intent.symbol
        ));
        return None;
    };
    let deviation = (order_price - ltp).abs() / ltp * Decimal::ONE_HUNDRED;
    if deviation > limits.max_price_deviation_pct {
        return Some(blocked(
            RiskCode::PriceDeviation,
            format!(
                "The order price {} is {}% away from the last traded price {}, beyond the {}% limit.",
                order_price,
                fmt2(deviation),
                ltp,
                limits.max_price_deviation_pct
            ),
            json!({
                "order_price": order_price.to_string(),
                "ltp": ltp.to_string(),
                "deviation_pct": quantize2(deviation).to_string(),
                "limit_pct": limits.max_price_deviation_pct.to_string(),
            }),
        ));
    }
    None
}

/// The cash a leg needs: a buy's reference price times its quantity. A
/// sell needs none here (its margin is the broker's to compute).
fn cash_required(intent: &Intent) -> Decimal {
    if intent.action != "BUY" {
        return Decimal::ZERO;
    }
    intent
        .reference_price()
        .map(|r| r * Decimal::from(intent.quantity))
        .unwrap_or(Decimal::ZERO)
}

/// Rule 11, against `max_funds_utilization_pct` of the available funds.
/// Unknown funds skip it with a warning (fails open).
fn check_affordability(
    required: Decimal,
    available: Option<Decimal>,
    limits: &RiskLimits,
    warnings: &mut Vec<String>,
    label: &str,
) -> Option<Verdict> {
    if required <= Decimal::ZERO {
        return None;
    }
    let Some(available) = available else {
        warnings.push(format!(
            "Available funds are unknown, so the affordability check for {} was skipped.",
            label
        ));
        return None;
    };
    let usable = available * limits.max_funds_utilization_pct / Decimal::ONE_HUNDRED;
    if required > usable {
        return Some(blocked(
            RiskCode::InsufficientFunds,
            format!(
                "{} needs about {} but only {} of the {} available may be used ({}% of funds).",
                label,
                fmt2(required),
                fmt2(usable),
                fmt2(available),
                limits.max_funds_utilization_pct
            ),
            json!({
                "required": quantize2(required).to_string(),
                "usable": quantize2(usable).to_string(),
                "available": quantize2(available).to_string(),
                "utilization_pct": limits.max_funds_utilization_pct.to_string(),
            }),
        ));
    }
    None
}

fn refused_after_claim(book: &mut Book, token: &Claimed, refusal: Verdict) -> Verdict {
    if let Claimed::Token(t) = token {
        book.rollback(Some(t));
    }
    refusal
}

fn allowed_details(
    fields: Vec<(&str, Value)>,
    book: &Book,
    limits: &RiskLimits,
    token: &Claimed,
    warnings: Vec<String>,
) -> Map<String, Value> {
    let mut details: Map<String, Value> = fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    details.insert("orders_claimed".into(), json!(book.orders_claimed));
    details.insert(
        "orders_allowed".into(),
        json!(limits.max_orders_per_session),
    );
    if let Claimed::Token(t) = token {
        details.insert("claim_token".into(), json!(t));
    }
    if !warnings.is_empty() {
        details.insert("warnings".into(), json!(warnings));
    }
    details
}

/// One order (the web's `check_order`). `token` claims it under that
/// identity; `None` is a dry run that claims nothing.
pub fn check_order(
    limits: &RiskLimits,
    gate_in: &Gate,
    book: &mut Book,
    args: &Map<String, Value>,
    now: DateTime<Utc>,
    token: Option<&str>,
) -> Verdict {
    if let Some(refused) = gate(limits, gate_in) {
        return refused;
    }
    let intent = match normalise(args) {
        Ok(i) => i,
        Err(e) => return verdict(false, e.code, e.reason, Value::Object(e.details)),
    };
    if let Some(refused) =
        check_symbol(&intent, limits).or_else(|| check_venue_and_size(&intent, limits))
    {
        return refused;
    }
    let claimed = match book.claim(std::slice::from_ref(&intent), limits, now, token) {
        Ok(c) => c,
        Err(refused) => return refused,
    };
    let mut warnings = Vec::new();
    if let Some(refused) = check_pricing(&intent, limits, &mut warnings) {
        return refused_after_claim(book, &claimed, refused);
    }
    let required = cash_required(&intent);
    if let Some(refused) = check_affordability(
        required,
        intent.available_funds,
        limits,
        &mut warnings,
        &intent.describe(),
    ) {
        return refused_after_claim(book, &claimed, refused);
    }
    let fields = vec![
        ("symbol", json!(intent.symbol)),
        ("exchange", json!(intent.exchange)),
        ("action", json!(intent.action)),
        ("quantity", json!(intent.quantity)),
        ("product", json!(intent.product)),
        ("price_type", json!(intent.price_type)),
    ];
    let details = allowed_details(fields, book, limits, &claimed, warnings);
    allowed(
        format!("{} passed every risk check.", intent.describe()),
        details,
    )
}

/// A multi-leg basket (the web's `check_basket`): per-leg rules leg by leg,
/// one claim for every leg or none, one affordability check on the sum.
/// No Agent tool sends a basket; it is kept so every code stays pinned.
pub fn check_basket(
    limits: &RiskLimits,
    gate_in: &Gate,
    book: &mut Book,
    legs: &[Value],
    available_funds: &Value,
    now: DateTime<Utc>,
    token: Option<&str>,
) -> Verdict {
    if let Some(refused) = gate(limits, gate_in) {
        return refused;
    }
    if legs.is_empty() {
        return blocked(
            RiskCode::EmptyBasket,
            "The basket contains no legs.".into(),
            json!({}),
        );
    }
    let intents = match basket_intents(legs, limits) {
        Ok(i) => i,
        Err(refused) => return refused,
    };
    let claimed = match book.claim(&intents, limits, now, token) {
        Ok(c) => c,
        Err(refused) => return refused,
    };
    let mut warnings = Vec::new();
    for (i, intent) in intents.iter().enumerate() {
        if let Some(refused) = check_pricing(intent, limits, &mut warnings) {
            let leg = prefixed(i + 1, refused);
            return refused_after_claim(book, &claimed, leg);
        }
    }
    let funds = py_decimal(available_funds)
        .or_else(|| intents.iter().filter_map(|l| l.available_funds).max());
    let required = intents.iter().map(cash_required).sum();
    let label = format!("this basket of {} legs", intents.len());
    if let Some(refused) = check_affordability(required, funds, limits, &mut warnings, &label) {
        return refused_after_claim(book, &claimed, refused);
    }
    let fields = vec![
        ("legs", json!(intents.len())),
        (
            "symbols",
            json!(intents.iter().map(|l| l.symbol.clone()).collect::<Vec<_>>()),
        ),
    ];
    let details = allowed_details(fields, book, limits, &claimed, warnings);
    allowed(
        format!("All {} legs passed every risk check.", intents.len()),
        details,
    )
}

/// "Leg {i}: {reason}", with the leg number first in the details.
fn prefixed(index: usize, refused: Verdict) -> Verdict {
    let mut details = Map::new();
    details.insert("leg".into(), json!(index));
    details.extend(refused.details);
    verdict(
        false,
        refused.code,
        format!("Leg {}: {}", index, refused.reason),
        Value::Object(details),
    )
}

fn basket_intents(legs: &[Value], limits: &RiskLimits) -> Result<Vec<Intent>, Verdict> {
    let mut intents = Vec::with_capacity(legs.len());
    for (i, leg) in legs.iter().enumerate() {
        let index = i + 1;
        let Value::Object(fields) = leg else {
            return Err(blocked(
                RiskCode::InvalidArguments,
                format!("Leg {} is not a mapping of order fields.", index),
                json!({"leg": index}),
            ));
        };
        let unknown: Vec<&String> = fields
            .keys()
            .filter(|k| !ORDER_KEYS.contains(&k.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !unknown.is_empty() {
            let names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
            return Err(blocked(
                RiskCode::InvalidArguments,
                format!(
                    "Leg {} carries unrecognised field(s): {}.",
                    index,
                    names.join(", ")
                ),
                json!({"leg": index, "unknown": names}),
            ));
        }
        let missing: Vec<&str> = REQUIRED_KEYS
            .iter()
            .copied()
            .filter(|k| !fields.contains_key(*k))
            .collect();
        if !missing.is_empty() {
            return Err(blocked(
                RiskCode::InvalidArguments,
                format!("Leg {}: {}", index, missing_keys_message(&missing)),
                json!({"leg": index}),
            ));
        }
        match normalise(fields) {
            Ok(intent) => intents.push(intent),
            Err(e) => {
                return Err(prefixed(
                    index,
                    verdict(false, e.code, e.reason, Value::Object(e.details)),
                ))
            }
        }
    }
    for (i, intent) in intents.iter().enumerate() {
        if let Some(refused) =
            check_symbol(intent, limits).or_else(|| check_venue_and_size(intent, limits))
        {
            return Err(prefixed(i + 1, refused));
        }
    }
    Ok(intents)
}

/// A destructive operation that places nothing (the web's
/// `check_destructive`): the gate, then its class. Never consumes the
/// session cap: cancelling is how a session gets out of trouble.
pub fn check_destructive(
    limits: &RiskLimits,
    gate_in: &Gate,
    operation: &Value,
    details: Map<String, Value>,
) -> Verdict {
    if let Some(refused) = gate(limits, gate_in) {
        return refused;
    }
    let name = py_text_or_empty(operation).trim().to_lowercase();
    let known =
        TARGETED_OPERATIONS.contains(&name.as_str()) || BULK_OPERATIONS.contains(&name.as_str());
    let with = |op: String| {
        let mut d = Map::new();
        d.insert("operation".into(), Value::String(op));
        d.extend(details.clone());
        d
    };
    if !known {
        let mut all: Vec<&str> = TARGETED_OPERATIONS
            .iter()
            .chain(BULK_OPERATIONS.iter())
            .copied()
            .collect();
        all.sort_unstable();
        return blocked(
            RiskCode::UnknownOperation,
            format!(
                "{} is not a destructive operation this guard knows. Known operations: {}.",
                py_repr(operation),
                all.join(", ")
            ),
            Value::Object(with(py_str(operation))),
        );
    }
    if BULK_OPERATIONS.contains(&name.as_str()) && !limits.allow_bulk_destructive {
        return blocked(
            RiskCode::BulkNotAllowed,
            format!(
                "{} affects the whole account and account-wide operations are switched off. Enable allow_bulk_destructive in the agent settings to permit it.",
                name
            ),
            Value::Object(with(name.clone())),
        );
    }
    allowed(
        format!("{} passed every risk check.", name),
        with(name.clone()),
    )
}

/// A modification (D-007, desktop): the gate, then the modified order's
/// exchange, product and quantity (rules 5 to 7) and its notional and
/// price deviation (rule 10). No claim: a modify is not a new order, so it
/// never consumes the session cap or meets the duplicate window, and funds
/// are the broker's to judge for a change to an order already accepted.
pub fn check_modify(
    limits: &RiskLimits,
    gate_in: &Gate,
    args: &Map<String, Value>,
    details: Map<String, Value>,
) -> Verdict {
    if let Some(refused) = gate(limits, gate_in) {
        return refused;
    }
    let intent = match normalise(args) {
        Ok(i) => i,
        Err(e) => return verdict(false, e.code, e.reason, Value::Object(e.details)),
    };
    let mut warnings = Vec::new();
    if let Some(refused) = check_venue_and_size(&intent, limits)
        .or_else(|| check_pricing(&intent, limits, &mut warnings))
    {
        return refused;
    }
    let mut d = Map::new();
    d.insert("operation".into(), json!("modify_order"));
    d.extend(details);
    if !warnings.is_empty() {
        d.insert("warnings".into(), json!(warnings));
    }
    allowed("modify_order passed every risk check.".into(), d)
}

/// Give back a claim whose order was never sent.
pub fn release(book: &mut Book, verdict: &Verdict) -> bool {
    book.rollback(verdict.claim_token())
}

/// The claimed order was dispatched: keep its count and fingerprint.
pub fn commit(book: &mut Book, verdict: &Verdict) -> bool {
    book.commit(verdict.claim_token())
}

/// The order fields of a modification, as the guard reads them.
pub fn modify_args(order: &Map<String, Value>, ltp: Option<&Value>) -> Map<String, Value> {
    let mut args = Map::new();
    for key in ["symbol", "exchange", "action", "quantity", "product"] {
        args.insert(key.into(), arg(order, key).clone());
    }
    args.insert("price_type".into(), arg(order, "pricetype").clone());
    args.insert("price".into(), arg(order, "price").clone());
    args.insert("trigger_price".into(), arg(order, "trigger_price").clone());
    if let Some(ltp) = ltp {
        args.insert("ltp".into(), ltp.clone());
    }
    args
}
