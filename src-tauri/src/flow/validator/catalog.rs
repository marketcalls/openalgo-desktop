//! The vocabularies the validator checks against: the node types, which of
//! them trigger, branch or only decorate, the fields each needs, and the
//! constants an order field may hold. Copied from the module-level constants
//! of the web's `services/flow_workflow_validator.py`, in its order; the
//! order constants themselves come from `brokers::common::mapping`, the
//! desktop's one copy of `docs/prompt/order-constants.md`.

use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};

/// The node types the editor can render and the executor can dispatch, kept
/// in lockstep with the editor's `components/flow/nodes/index.ts` (pinned by
/// a test).
pub const VALID_NODE_TYPES: [&str; 61] = [
    "andGate",
    "barOffset",
    "basketOrder",
    "calendar",
    "cancelAllOrders",
    "cancelOrder",
    "closePositions",
    "delay",
    "expiry",
    "fundCheck",
    "funds",
    "getDepth",
    "getOrderStatus",
    "getQuote",
    "group",
    "history",
    "holdings",
    "holidays",
    "httpRequest",
    "indicator",
    "intervals",
    "log",
    "margin",
    "mathExpression",
    "modifyOrder",
    "multiQuotes",
    "notGate",
    "openPosition",
    "optionChain",
    "optionSymbol",
    "optionsMultiOrder",
    "optionsOrder",
    "orGate",
    "orderBook",
    "orderUpdateTrigger",
    "placeOrder",
    "positionBook",
    "positionCheck",
    "priceAlert",
    "priceCondition",
    "priorPeriodOhlc",
    "smartOrder",
    "splitOrder",
    "start",
    "strategyPnl",
    "subscribeDepth",
    "subscribeLtp",
    "subscribeQuote",
    "symbol",
    "syntheticFuture",
    "telegramAlert",
    "timeCondition",
    "timeWindow",
    "timings",
    "tradeBook",
    "unsubscribe",
    "varCondition",
    "variable",
    "waitUntil",
    "webhookTrigger",
    "whatsappAlert",
];

/// The nodes a run starts from, sorted as the web's "Add one of:" lists them.
pub const TRIGGER_NODE_TYPES: [&str; 4] = [
    "orderUpdateTrigger",
    "priceAlert",
    "start",
    "webhookTrigger",
];

/// Nodes that exist only in the editor. They are never dispatched, so they
/// are exempt from reachability.
pub const DECORATIVE_NODE_TYPES: [&str; 1] = ["group"];

/// The logic gates, which combine the results of their inputs (the
/// executor's `GATE_NODE_TYPES`).
pub(crate) const GATE_NODE_TYPES: [&str; 3] = ["andGate", "orGate", "notGate"];

/// Most nodes a workflow may hold.
pub const MAX_NODES: usize = 500;
/// Most edges a workflow may hold.
pub const MAX_EDGES: usize = 1000;

/// The source handles a branching node may emit; the executor treats yes/no
/// as synonyms of true/false. Sorted, as the error's `expected` lists them.
pub(crate) const BRANCHING_HANDLES: [&str; 4] = ["false", "no", "true", "yes"];

/// Nodes that fork into true/false branches. `priceAlert` renders the same
/// handles as the condition nodes, so leaving it out rejected a valid
/// price-alert workflow on its own edges.
pub const BRANCHING_NODE_TYPES: [&str; 10] = [
    "positionCheck",
    "fundCheck",
    "priceCondition",
    "varCondition",
    "timeWindow",
    "timeCondition",
    "andGate",
    "orGate",
    "notGate",
    "priceAlert",
];

/// Fields a node cannot execute without, checked at the strict level.
pub const REQUIRED_NODE_FIELDS: [(&str, &[&str]); 35] = [
    ("placeOrder", &["symbol", "exchange", "action", "quantity"]),
    ("smartOrder", &["symbol", "exchange", "action", "quantity"]),
    ("optionsOrder", &["underlying", "action", "quantity"]),
    ("modifyOrder", &["orderId"]),
    ("cancelOrder", &["orderId"]),
    ("getOrderStatus", &["orderId"]),
    ("getQuote", &["symbol", "exchange"]),
    ("getDepth", &["symbol", "exchange"]),
    ("history", &["symbol", "exchange", "interval"]),
    ("indicator", &["indicatorName"]),
    ("priorPeriodOhlc", &["symbol", "exchange"]),
    ("barOffset", &["symbol", "exchange"]),
    ("openPosition", &["symbol", "exchange"]),
    ("telegramAlert", &["message"]),
    ("whatsappAlert", &["message"]),
    ("mathExpression", &["expression"]),
    ("varCondition", &["leftValue", "operator"]),
    ("priceCondition", &["symbol", "exchange", "operator"]),
    // Condition gates that fail open when unconfigured: positionCheck with no
    // symbol reads a zero-quantity position, so `not_exists` is always true.
    ("positionCheck", &["symbol", "exchange", "condition"]),
    // Broker-mutating nodes. An empty one reaches the broker as a malformed
    // order rather than failing at import.
    (
        "splitOrder",
        &["symbol", "exchange", "action", "quantity", "splitSize"],
    ),
    ("basketOrder", &["orders"]),
    ("optionsMultiOrder", &["underlying", "quantity"]),
    // `price` is required only for level conditions: see PRICE_ALERT_CONDITIONS.
    ("priceAlert", &["symbol", "exchange", "condition"]),
    ("subscribeLtp", &["symbol", "exchange"]),
    ("subscribeQuote", &["symbol", "exchange"]),
    ("subscribeDepth", &["symbol", "exchange"]),
    ("optionSymbol", &["underlying", "optionType"]),
    ("optionChain", &["underlying"]),
    ("syntheticFuture", &["underlying"]),
    ("expiry", &["symbol", "exchange"]),
    ("symbol", &["symbol", "exchange"]),
    ("multiQuotes", &["symbols"]),
    ("httpRequest", &["url"]),
    ("variable", &["variableName"]),
    ("waitUntil", &["targetTime"]),
];

/// Nodes that need at least one of a set of interchangeable fields.
/// fundCheck reads `minAvailable`, or the legacy `threshold`; with neither,
/// its guard compares against zero and passes on any balance at all.
pub const EITHER_REQUIRED_FIELDS: [(&str, &[&[&str]]); 1] =
    [("fundCheck", &[&["minAvailable", "threshold"]])];

/// The price monitor's conditions and the fields each needs, keyed on the
/// canonical condition the monitor compares on (the web's
/// `CONDITIONAL_REQUIRED_FIELDS["priceAlert"]["condition"]`). Sorted by
/// condition, as the `unknown_condition` error lists them.
pub(crate) const PRICE_ALERT_CONDITIONS: [(&str, &[&str]); 13] = [
    ("crossing", &["price"]),
    ("crossing_down", &["price"]),
    ("crossing_up", &["price"]),
    ("entering_channel", &["priceLower", "priceUpper"]),
    ("exiting_channel", &["priceLower", "priceUpper"]),
    ("greater_than", &["price"]),
    ("inside_channel", &["priceLower", "priceUpper"]),
    ("less_than", &["price"]),
    ("moving_down", &[]),
    ("moving_down_percent", &["percentage"]),
    ("moving_up", &[]),
    ("moving_up_percent", &["percentage"]),
    ("outside_channel", &["priceLower", "priceUpper"]),
];

/// The executor's variable operations, matched exactly (lowercase, no
/// surrounding spaces).
pub(crate) const VALID_VARIABLE_OPERATIONS: [&str; 11] = [
    "set",
    "get",
    "add",
    "subtract",
    "multiply",
    "divide",
    "increment",
    "decrement",
    "parse_json",
    "stringify",
    "append",
];

/// Order nodes whose static price type decides which prices they need.
pub(crate) const PRICED_ORDER_NODE_TYPES: [&str; 6] = [
    "placeOrder",
    "smartOrder",
    "optionsOrder",
    "optionsMultiOrder",
    "basketOrder",
    "splitOrder",
];

/// Fields that must be a positive number wherever they appear (SmartOrder
/// quantity may be zero: a target-position square-off). The web iterates a
/// `frozenset` here, whose order varies between processes; this is the
/// order the fields are listed in.
pub(crate) const POSITIVE_NUMBER_FIELDS: [&str; 3] = ["quantity", "splitSize", "lots"];

/// The methods the httpRequest node implements, sorted.
pub const HTTP_METHODS: [&str; 5] = ["DELETE", "GET", "PATCH", "POST", "PUT"];
/// The longest httpRequest timeout, in milliseconds; the executor clamps to
/// it and the validator refuses beyond it.
pub const HTTP_TIMEOUT_MAX_MS: u32 = 60_000;
/// The shortest httpRequest timeout that can describe a real remote call.
pub const HTTP_TIMEOUT_MIN_MS: u32 = 1_000;

/// A vocabulary an order field is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vocabulary {
    Exchange,
    Action,
    Product,
    PriceType,
    OptionType,
    VariableOperation,
    StrikeMode,
}

impl Vocabulary {
    /// Whether `canonical` (already trimmed and cased) is in the vocabulary.
    pub(crate) fn contains(self, canonical: &str) -> bool {
        self.sorted().contains(&canonical)
    }

    /// The vocabulary, sorted, as the error's `expected` lists it.
    pub(crate) fn sorted(self) -> Vec<&'static str> {
        let mut values: Vec<&'static str> = match self {
            Vocabulary::Exchange => Exchange::ALL.iter().map(|e| e.as_str()).collect(),
            Vocabulary::Action => Action::ALL.iter().map(|a| a.as_str()).collect(),
            Vocabulary::Product => Product::ALL.iter().map(|p| p.as_str()).collect(),
            Vocabulary::PriceType => PriceType::ALL.iter().map(|p| p.as_str()).collect(),
            Vocabulary::OptionType => vec!["CE", "PE"],
            Vocabulary::VariableOperation => VALID_VARIABLE_OPERATIONS.to_vec(),
            Vocabulary::StrikeMode => crate::flow::node_contracts::VALID_LEG_STRIKE_MODES.to_vec(),
        };
        values.sort_unstable();
        values
    }
}

/// The vocabulary each node-level field is drawn from, in the web's
/// `ENUM_FIELDS` order (which is the order their errors appear in).
pub(crate) const ENUM_FIELDS: [(&str, Vocabulary); 6] = [
    ("exchange", Vocabulary::Exchange),
    ("action", Vocabulary::Action),
    ("product", Vocabulary::Product),
    ("priceType", Vocabulary::PriceType),
    ("pricetype", Vocabulary::PriceType),
    ("operation", Vocabulary::VariableOperation),
];

/// Whether `node_type` is one of the listed types.
pub(crate) fn is_one_of(list: &[&str], node_type: &str) -> bool {
    list.contains(&node_type)
}

/// The fields a node type always needs at the strict level.
pub(crate) fn required_fields(node_type: &str) -> &'static [&'static str] {
    REQUIRED_NODE_FIELDS
        .iter()
        .find(|(t, _)| *t == node_type)
        .map_or(&[], |(_, fields)| fields)
}
