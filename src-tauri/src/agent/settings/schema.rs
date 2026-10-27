//! The Agent's settings: every key, its kind, its shipped default and its
//! bounds (the web's `services/agent/settings.py` `_SPEC`, in the same
//! order, which is the order `GET /agent/api/settings` lists them in).
//!
//! Every default is the safe end of its switch: trading off, analyzer-only
//! off (as the web ships it), bulk destructive operations off, the kill
//! switch released, voice off, external data on (it is read-only).

/// How a stored value is read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Money,
    Percent,
    Text,
    Enum,
    Set,
}

/// A shipped default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shipped {
    Bool(bool),
    Int(i64),
    /// A decimal as Python writes it (`"500000.00"`, `"5"`).
    Decimal(&'static str),
    Text(&'static str),
    Set(&'static [&'static str]),
    /// `<app data folder>/agent_kill_switch` (decision 2 of the 1.0.4
    /// plan; the web's is `<repository root>/agent_kill_switch`).
    KillSwitchFile,
}

/// One row of the schema.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub kind: Kind,
    pub default: Shipped,
    /// Inclusive bounds for `Int`, `Money`, `Percent`.
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// Permitted values for `Enum`.
    pub choices: &'static [&'static str],
}

pub const TRADING_ENABLED: &str = "trading_enabled";
pub const REQUIRE_ANALYZER_MODE: &str = "require_analyzer_mode";
pub const SYSTEM_PROMPT: &str = "system_prompt";
pub const DEFAULT_REASONING_EFFORT: &str = "default_reasoning_effort";
pub const KILL_SWITCH: &str = "kill_switch";
pub const KILL_SWITCH_FILE: &str = "kill_switch_file";
pub const VOICE_TRADING_ENABLED: &str = "voice_trading_enabled";
pub const VOICE_AGENT_NAME: &str = "voice_agent_name";
pub const MCP_ENABLED: &str = "mcp_enabled";

/// `off`, `low`, `medium`, `high`: a model's thinking effort.
pub const REASONING_EFFORTS: &[&str] = &["off", "low", "medium", "high"];

/// The exchanges an order may go to: every OpenAlgo exchange except the
/// quote-only indices (the web's `TRADABLE_EXCHANGES`).
pub const TRADABLE_EXCHANGES: &[&str] = &[
    "BCD", "BFO", "BSE", "CDS", "CRYPTO", "MCX", "NCDEX", "NCO", "NFO", "NSE",
];

/// The product types (the web's `VALID_PRODUCT_TYPES`), sorted.
pub const PRODUCT_TYPES: &[&str] = &["CNC", "MIS", "NRML"];

const fn field(key: &'static str, kind: Kind, default: Shipped) -> Field {
    Field {
        key,
        kind,
        default,
        min: None,
        max: None,
        choices: &[],
    }
}

const fn bounded(
    key: &'static str,
    kind: Kind,
    default: Shipped,
    min: i64,
    max: Option<i64>,
) -> Field {
    Field {
        key,
        kind,
        default,
        min: Some(min),
        max,
        choices: &[],
    }
}

/// Every setting, in the web's order.
pub const FIELDS: &[Field] = &[
    field(TRADING_ENABLED, Kind::Bool, Shipped::Bool(false)),
    field(REQUIRE_ANALYZER_MODE, Kind::Bool, Shipped::Bool(false)),
    field(SYSTEM_PROMPT, Kind::Text, Shipped::Text("")),
    Field {
        key: DEFAULT_REASONING_EFFORT,
        kind: Kind::Enum,
        default: Shipped::Text("off"),
        min: None,
        max: None,
        choices: REASONING_EFFORTS,
    },
    bounded(
        "max_orders_per_session",
        Kind::Int,
        Shipped::Int(20),
        0,
        None,
    ),
    bounded(
        "max_order_quantity",
        Kind::Int,
        Shipped::Int(10000),
        0,
        None,
    ),
    bounded(
        "max_order_value",
        Kind::Money,
        Shipped::Decimal("500000.00"),
        0,
        None,
    ),
    bounded(
        "max_price_deviation_pct",
        Kind::Percent,
        Shipped::Decimal("5"),
        0,
        Some(100),
    ),
    bounded(
        "duplicate_order_window_seconds",
        Kind::Int,
        Shipped::Int(10),
        0,
        None,
    ),
    field(
        "allowed_exchanges",
        Kind::Set,
        Shipped::Set(TRADABLE_EXCHANGES),
    ),
    field("allowed_products", Kind::Set, Shipped::Set(PRODUCT_TYPES)),
    field("symbol_allowlist", Kind::Set, Shipped::Set(&[])),
    field("symbol_blocklist", Kind::Set, Shipped::Set(&[])),
    bounded(
        "max_funds_utilization_pct",
        Kind::Percent,
        Shipped::Decimal("100"),
        0,
        Some(100),
    ),
    field("allow_bulk_destructive", Kind::Bool, Shipped::Bool(false)),
    field(KILL_SWITCH, Kind::Bool, Shipped::Bool(false)),
    field(KILL_SWITCH_FILE, Kind::Text, Shipped::KillSwitchFile),
    field("voice_enabled", Kind::Bool, Shipped::Bool(false)),
    field("voice_provider", Kind::Text, Shipped::Text("openai")),
    field("voice_model", Kind::Text, Shipped::Text("gpt-live-1")),
    field("voice_speaker", Kind::Text, Shipped::Text("marin")),
    field(VOICE_AGENT_NAME, Kind::Text, Shipped::Text("Vega")),
    field(VOICE_TRADING_ENABLED, Kind::Bool, Shipped::Bool(false)),
    bounded(
        "voice_confirm_window_seconds",
        Kind::Int,
        Shipped::Int(30),
        5,
        Some(300),
    ),
    bounded(
        "voice_idle_timeout_seconds",
        Kind::Int,
        Shipped::Int(180),
        30,
        Some(3600),
    ),
    field(MCP_ENABLED, Kind::Bool, Shipped::Bool(true)),
];

/// The voice block, seeded as rows by migration 098 as the web's
/// `migrate_agent_voice.py` seeds them, so a later release can never turn a
/// microphone on, or let speech reach an order, by changing a default.
pub const VOICE_KEYS: &[&str] = &[
    "voice_enabled",
    "voice_provider",
    "voice_model",
    "voice_speaker",
    VOICE_AGENT_NAME,
    VOICE_TRADING_ENABLED,
    "voice_confirm_window_seconds",
    "voice_idle_timeout_seconds",
];

/// The schema row for a key.
pub fn field_of(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// The kill switch file's name inside the app's data folder.
pub const KILL_SWITCH_FILE_NAME: &str = "agent_kill_switch";

/// The longest system prompt override a trader may save (D-014): the
/// built-in sections it replaces come to about 20,000 characters, and the
/// whole prompt is capped at 40,000, so a longer override would be dropped
/// whole at the next message with only a log line to say so.
pub const MAX_SYSTEM_PROMPT_CHARS: usize = 20_000;
