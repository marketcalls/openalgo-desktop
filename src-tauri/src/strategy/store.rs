//! Persistence for the strategy module (web `database/strategy_module_db.py`)
//! in the main SQLite database, with the web's table and column names.
//!
//! Six `sm_` tables: `sm_strategy` (config, legs JSON, webhook token hash),
//! `sm_strategy_run` (one activation), `sm_strategy_order` (every durable
//! order intent), `sm_strategy_checkpoint` (runtime snapshot), `sm_webhook_event`
//! (webhook audit), `sm_strategy_event` (risk-event audit trail).
//!
//! Timestamps are stored naive UTC (`YYYY-MM-DD HH:MM:SS.ffffff`) and
//! rendered with an explicit `+00:00` at the boundary. Money is stored as
//! REAL and handed out as `f64`. Children are deleted explicitly with their
//! strategy: SQLite enforces foreign keys only when asked, and a reused rowid
//! would otherwise re-attach an orphaned audit trail to the next strategy.
//!
//! Every guard that the engine depends on is one conditional statement, not a
//! read followed by a write: `claim_strategy_for_run`, the terminal run CAS,
//! the order-fact fold and the acknowledgement bind. The management writes
//! (`management.rs`) hold to the same rule and advance `revision` (SM-01).
//!
//! The `Store` methods are split by table into child modules: `strategies`,
//! `runs`, `orders`, `events`, `checkpoints`, `webhook_audit`, plus
//! `management` and `by_mode`.

use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, Row};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod by_mode;
mod checkpoints;
mod events;
mod management;
mod orders;
mod runs;
mod strategies;
mod webhook_audit;

pub use by_mode::ModedOrder;
pub use management::{
    migrate_revision, ClaimOutcome, StartClaim, ALREADY_RUNNING, CHANGED_WHILE_EDITING,
    CHANGED_WHILE_STARTING, LIVE_NOT_ENABLED, WEBHOOK_LOCKED,
};
pub(crate) use orders::frame_changes;
pub use runs::migrate_run_account;
pub use webhook_audit::WebhookEvent;

pub const WEBHOOK_TOKEN_PREFIX: &str = "oaws_";

pub const STRATEGY_KINDS: &[&str] = &["batch", "signal"];
pub const DIRECTIONS: &[&str] = &["both", "long_only", "short_only"];
pub const STRATEGY_TYPES: &[&str] = &["intraday", "positional"];
pub const RUN_MODES: &[&str] = &["live", "sandbox"];
pub const STRATEGY_STATUSES: &[&str] = &["stopped", "running", "paused", "errored"];
pub const TRIGGER_SOURCES: &[&str] = &["manual", "webhook", "scheduler"];
pub const STOP_REASONS: &[&str] = &[
    "manual",
    "scheduler",
    "overall_sl",
    "overall_target",
    "lock_profit",
    "eod",
    "expiry",
    "daily_loss_limit",
    "tick_stale",
    "recovery_failed",
    "error",
];
pub const ORDER_KINDS: &[&str] = &[
    "entry",
    "exit_sl",
    "exit_target",
    "exit_trail",
    "exit_overall_sl",
    "exit_overall_target",
    "exit_lock_profit",
    "exit_eod",
    "exit_expiry",
    "exit_daily_loss_limit",
    "exit_close_all",
    "exit_leg_manual",
    "exit_recovery",
    "exit_signal",
];
pub const ORDER_STATUSES: &[&str] = &["pending", "open", "complete", "cancelled", "rejected"];
pub const TERMINAL_ORDER_STATUSES: &[&str] = &["complete", "cancelled", "rejected"];
pub const EVENT_SEVERITIES: &[&str] = &["info", "warn", "critical"];
pub const EVENT_KINDS: &[&str] = &[
    "strategy_created",
    "strategy_updated",
    "webhook_token_rotated",
    "live_enabled",
    "live_disabled",
    "webhook_locked",
    "webhook_unlocked",
    "run_started",
    "run_paused",
    "run_resumed",
    "run_stop_requested",
    "run_stopped",
    "run_stop_failed",
    "flip_outgoing_exit_rejected",
    "close_all_manual",
    "leg_entry_placed",
    "leg_entry_filled",
    "leg_entry_rejected",
    "leg_exit_placed",
    "leg_exit_filled",
    "leg_exit_rejected",
    "leg_close_manual",
    "leg_expiry_fallback",
    "order_ack_unrecorded",
    "leg_sl_hit",
    "leg_target_hit",
    "leg_trail_armed",
    "leg_trail_advanced",
    "overall_sl_hit",
    "overall_target_hit",
    "lock_profit_armed",
    "lock_profit_floor_advanced",
    "lock_profit_triggered",
    "trail_to_entry_activated",
    "eod_squareoff",
    "expiry_squareoff",
    "tick_source_switched_to_polling",
    "tick_source_switched_to_ws",
    "tick_source_stale",
    "recovery_succeeded",
    "recovery_failed",
];
pub const WEBHOOK_RESULTS: &[&str] = &[
    "ok",
    "rejected_token",
    "rejected_ip",
    "rate_limited",
    "rejected_dedupe",
    "rejected_cooling_off",
    "rejected_invalid_action",
    "rejected_live_disabled",
    "rejected_locked",
    "rejected_payload",
    "rejected_engine_error",
];

/// Ownerless webhook audit rows kept (newest first); see `record_webhook_event`.
pub const MAX_UNATTRIBUTED_WEBHOOK_EVENTS: i64 = 1000;

/// Fields a PATCH may touch. `strategy_kind` is deliberately absent.
pub const UPDATABLE_FIELDS: &[&str] = &[
    "name",
    "direction",
    "universe_tab",
    "underlying",
    "underlying_exchange",
    "strategy_type",
    "entry_time",
    "exit_time",
    "product",
    "pricetype",
    "legs",
    "overall_sl_mtm",
    "overall_target_mtm",
    "lock_profit",
    "trail_sl_to_entry",
    "scheduler",
    "daily_loss_limit_inr",
    "webhook_ip_allowlist",
];

// ---------------------------------------------------------------- migration

/// The six `sm_` tables and their indexes, each created only when absent.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sm_strategy (
            id INTEGER PRIMARY KEY,
            user_id VARCHAR(80) NOT NULL,
            name VARCHAR(200) NOT NULL,
            strategy_kind VARCHAR(20) NOT NULL DEFAULT 'batch',
            direction VARCHAR(20) NOT NULL DEFAULT 'both',
            universe_tab VARCHAR(30) NOT NULL,
            underlying VARCHAR(50) NOT NULL,
            underlying_exchange VARCHAR(20) NOT NULL,
            strategy_type VARCHAR(20) NOT NULL DEFAULT 'intraday',
            entry_time TIME,
            exit_time TIME,
            product VARCHAR(10) NOT NULL DEFAULT 'NRML',
            pricetype VARCHAR(10) NOT NULL DEFAULT 'MARKET',
            legs JSON NOT NULL DEFAULT '[]',
            overall_sl_mtm NUMERIC(18, 2),
            overall_target_mtm NUMERIC(18, 2),
            lock_profit JSON,
            trail_sl_to_entry BOOLEAN NOT NULL DEFAULT 0,
            scheduler JSON,
            live_enabled BOOLEAN NOT NULL DEFAULT 0,
            webhook_token_hash VARCHAR(64) NOT NULL UNIQUE,
            webhook_ip_allowlist JSON,
            webhook_locked BOOLEAN NOT NULL DEFAULT 0,
            daily_loss_limit_inr NUMERIC(18, 2),
            status VARCHAR(20) NOT NULL DEFAULT 'stopped',
            current_run_id INTEGER,
            created_at DATETIME NOT NULL,
            updated_at DATETIME NOT NULL,
            CONSTRAINT uq_sm_strategy_user_name UNIQUE (user_id, name)
        );
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_user_id ON sm_strategy (user_id);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_status ON sm_strategy (status);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_user_status ON sm_strategy (user_id, status);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_webhook_token_hash ON sm_strategy (webhook_token_hash);

        CREATE TABLE IF NOT EXISTS sm_strategy_run (
            id INTEGER PRIMARY KEY,
            strategy_id INTEGER NOT NULL REFERENCES sm_strategy (id) ON DELETE CASCADE,
            mode VARCHAR(10) NOT NULL,
            broker VARCHAR(50) NOT NULL DEFAULT '',
            started_at DATETIME NOT NULL,
            stopped_at DATETIME,
            stop_reason VARCHAR(30),
            stop_requested_at DATETIME,
            stop_requested_reason VARCHAR(30),
            pnl_realized NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_peak NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_trough NUMERIC(18, 2) NOT NULL DEFAULT 0,
            trigger_source VARCHAR(20) NOT NULL DEFAULT 'manual',
            webhook_event_id INTEGER,
            resolved_expiries JSON
        );
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_run_strategy_id ON sm_strategy_run (strategy_id);
        CREATE INDEX IF NOT EXISTS ix_sm_run_strategy_started ON sm_strategy_run (strategy_id, started_at);

        CREATE TABLE IF NOT EXISTS sm_strategy_order (
            id INTEGER PRIMARY KEY,
            run_id INTEGER NOT NULL REFERENCES sm_strategy_run (id) ON DELETE CASCADE,
            leg_id INTEGER NOT NULL,
            kind VARCHAR(30) NOT NULL,
            position_ref VARCHAR(32),
            broker_order_id VARCHAR(100),
            symbol VARCHAR(100) NOT NULL,
            exchange VARCHAR(20) NOT NULL,
            action VARCHAR(10) NOT NULL,
            qty INTEGER NOT NULL,
            product VARCHAR(10),
            pricetype VARCHAR(10) NOT NULL DEFAULT 'MARKET',
            price NUMERIC(18, 4) NOT NULL DEFAULT 0,
            trigger_price NUMERIC(18, 4) NOT NULL DEFAULT 0,
            status VARCHAR(20) NOT NULL DEFAULT 'pending',
            placed_at DATETIME NOT NULL,
            filled_at DATETIME,
            avg_fill_price NUMERIC(18, 4),
            filled_qty INTEGER,
            reject_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_order_run_id ON sm_strategy_order (run_id);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_order_broker_order_id ON sm_strategy_order (broker_order_id);
        CREATE INDEX IF NOT EXISTS ix_sm_order_run_placed ON sm_strategy_order (run_id, placed_at);
        CREATE INDEX IF NOT EXISTS ix_sm_order_run_leg_position ON sm_strategy_order (run_id, leg_id, position_ref);

        CREATE TABLE IF NOT EXISTS sm_strategy_checkpoint (
            id INTEGER PRIMARY KEY,
            run_id INTEGER NOT NULL REFERENCES sm_strategy_run (id) ON DELETE CASCADE,
            ts DATETIME NOT NULL,
            pnl_realized NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_unrealized NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_total NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_peak NUMERIC(18, 2) NOT NULL DEFAULT 0,
            pnl_trough NUMERIC(18, 2) NOT NULL DEFAULT 0,
            lock_floor NUMERIC(18, 2),
            trail_to_entry_active BOOLEAN NOT NULL DEFAULT 0,
            leg_state JSON NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_checkpoint_run_id ON sm_strategy_checkpoint (run_id);
        CREATE INDEX IF NOT EXISTS ix_sm_checkpoint_run_ts ON sm_strategy_checkpoint (run_id, ts);

        CREATE TABLE IF NOT EXISTS sm_webhook_event (
            id INTEGER PRIMARY KEY,
            strategy_id INTEGER REFERENCES sm_strategy (id) ON DELETE SET NULL,
            action VARCHAR(20),
            mode VARCHAR(10),
            payload JSON,
            ip VARCHAR(45),
            user_agent VARCHAR(255),
            received_at DATETIME NOT NULL,
            result VARCHAR(50) NOT NULL,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS ix_sm_webhook_event_strategy_id ON sm_webhook_event (strategy_id);
        CREATE INDEX IF NOT EXISTS ix_sm_webhook_strategy_received ON sm_webhook_event (strategy_id, received_at);

        CREATE TABLE IF NOT EXISTS sm_strategy_event (
            id INTEGER PRIMARY KEY,
            run_id INTEGER REFERENCES sm_strategy_run (id) ON DELETE CASCADE,
            strategy_id INTEGER NOT NULL REFERENCES sm_strategy (id) ON DELETE CASCADE,
            user_id VARCHAR(80) NOT NULL,
            ts DATETIME NOT NULL,
            kind VARCHAR(40) NOT NULL,
            severity VARCHAR(10) NOT NULL DEFAULT 'info',
            leg_id INTEGER,
            message TEXT NOT NULL,
            payload JSON
        );
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_event_run_id ON sm_strategy_event (run_id);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_event_strategy_id ON sm_strategy_event (strategy_id);
        CREATE INDEX IF NOT EXISTS ix_sm_strategy_event_user_id ON sm_strategy_event (user_id);
        CREATE INDEX IF NOT EXISTS ix_sm_event_strategy_ts ON sm_strategy_event (strategy_id, ts);
        CREATE INDEX IF NOT EXISTS ix_sm_event_run_ts ON sm_strategy_event (run_id, ts);";

/// Migration `072_strategy_module`: the six `sm_` tables. Idempotent.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

// ---------------------------------------------------------------- helpers

/// Naive-UTC storage form (SQLAlchemy's SQLite DateTime text).
pub fn ts(dt: DateTime<Utc>) -> String {
    dt.naive_utc().format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn parse_ts(text: &str) -> Option<NaiveDateTime> {
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(v) = NaiveDateTime::parse_from_str(text, f) {
            return Some(v);
        }
    }
    None
}

/// A stored timestamp as Python's `datetime.replace(tzinfo=UTC).isoformat()`.
pub fn iso(text: Option<&str>) -> Value {
    let Some(text) = text else {
        return Value::Null;
    };
    match parse_ts(text) {
        Some(dt) => {
            let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            let micros = dt.and_utc().timestamp_subsec_micros();
            if micros == 0 {
                Value::String(format!("{}+00:00", base))
            } else {
                Value::String(format!("{}.{:06}+00:00", base, micros))
            }
        }
        None => Value::String(text.to_string()),
    }
}

/// A stored timestamp back as UTC.
pub fn parse_utc(text: &str) -> Option<DateTime<Utc>> {
    parse_ts(text).map(|n| n.and_utc())
}

fn json_col(v: &Value) -> Option<String> {
    if v.is_null() {
        None
    } else {
        Some(v.to_string())
    }
}

fn read_json(text: Option<String>) -> Value {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn f64_json(v: Option<f64>) -> Value {
    v.and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn fnum(v: f64) -> Value {
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(json!(0.0))
}

/// `HH:MM` from a stored TIME (`09:20:00.000000`, `09:20:00` or `09:20`).
fn hhmm(text: Option<String>) -> Option<String> {
    text.and_then(|t| t.get(..5).map(str::to_string))
}

/// The TIME column form of an `HH:MM`.
fn time_col(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty())
        .map(|s| format!("{}:00.000000", s.get(..5).unwrap_or(s)))
}

fn round_to(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

fn num_param(v: &Value) -> Option<f64> {
    crate::risk::value_to_f64(v).filter(|f| f.is_finite())
}

/// A fresh webhook token: prefix plus 32 bytes of URL-safe entropy.
pub fn generate_webhook_token() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "{}{}",
        WEBHOOK_TOKEN_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// SHA-256 hex of a webhook token (an indexed, O(1) lookup key).
pub fn hash_webhook_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, PartialEq)]
pub struct StrategyRow {
    pub id: i64,
    pub user_id: String,
    pub name: String,
    pub strategy_kind: String,
    pub direction: String,
    pub universe_tab: String,
    pub underlying: String,
    pub underlying_exchange: String,
    pub strategy_type: String,
    pub entry_time: Option<String>,
    pub exit_time: Option<String>,
    pub product: String,
    pub pricetype: String,
    pub legs: Value,
    pub overall_sl_mtm: Option<f64>,
    pub overall_target_mtm: Option<f64>,
    pub lock_profit: Value,
    pub trail_sl_to_entry: bool,
    pub scheduler: Value,
    pub live_enabled: bool,
    pub webhook_token_hash: String,
    pub webhook_ip_allowlist: Value,
    pub webhook_locked: bool,
    pub daily_loss_limit_inr: Option<f64>,
    pub status: String,
    pub current_run_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    /// Advanced by every configuration write (SM-01). Not part of the
    /// web's strategy shape.
    pub revision: i64,
}

const STRATEGY_COLS: &str =
    "id, user_id, name, strategy_kind, direction, universe_tab, underlying, \
    underlying_exchange, strategy_type, entry_time, exit_time, product, pricetype, legs, \
    overall_sl_mtm, overall_target_mtm, lock_profit, trail_sl_to_entry, scheduler, live_enabled, \
    webhook_token_hash, webhook_ip_allowlist, webhook_locked, daily_loss_limit_inr, status, \
    current_run_id, created_at, updated_at, revision";

fn strategy_from(r: &Row<'_>) -> rusqlite::Result<StrategyRow> {
    Ok(StrategyRow {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        strategy_kind: r.get(3)?,
        direction: r.get(4)?,
        universe_tab: r.get(5)?,
        underlying: r.get(6)?,
        underlying_exchange: r.get(7)?,
        strategy_type: r.get(8)?,
        entry_time: hhmm(r.get(9)?),
        exit_time: hhmm(r.get(10)?),
        product: r.get(11)?,
        pricetype: r.get(12)?,
        legs: {
            let v = read_json(r.get(13)?);
            if v.is_null() {
                json!([])
            } else {
                v
            }
        },
        overall_sl_mtm: r.get(14)?,
        overall_target_mtm: r.get(15)?,
        lock_profit: read_json(r.get(16)?),
        trail_sl_to_entry: r.get(17)?,
        scheduler: read_json(r.get(18)?),
        live_enabled: r.get(19)?,
        webhook_token_hash: r.get(20)?,
        webhook_ip_allowlist: read_json(r.get(21)?),
        webhook_locked: r.get(22)?,
        daily_loss_limit_inr: r.get(23)?,
        status: r.get(24)?,
        current_run_id: r.get(25)?,
        created_at: r.get(26)?,
        updated_at: r.get(27)?,
        revision: r.get(28)?,
    })
}

impl StrategyRow {
    /// Web `strategy_to_dict`. Never carries the webhook token.
    pub fn to_dict(&self, include_legs: bool) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("strategy_kind".into(), json!(self.strategy_kind));
        m.insert("direction".into(), json!(self.direction));
        m.insert("universe_tab".into(), json!(self.universe_tab));
        m.insert("underlying".into(), json!(self.underlying));
        m.insert(
            "underlying_exchange".into(),
            json!(self.underlying_exchange),
        );
        m.insert("strategy_type".into(), json!(self.strategy_type));
        m.insert("entry_time".into(), json!(self.entry_time));
        m.insert("exit_time".into(), json!(self.exit_time));
        m.insert("product".into(), json!(self.product));
        m.insert("pricetype".into(), json!(self.pricetype));
        m.insert("overall_sl_mtm".into(), f64_json(self.overall_sl_mtm));
        m.insert(
            "overall_target_mtm".into(),
            f64_json(self.overall_target_mtm),
        );
        m.insert("lock_profit".into(), self.lock_profit.clone());
        m.insert("trail_sl_to_entry".into(), json!(self.trail_sl_to_entry));
        m.insert("scheduler".into(), self.scheduler.clone());
        m.insert("live_enabled".into(), json!(self.live_enabled));
        m.insert("webhook_locked".into(), json!(self.webhook_locked));
        m.insert(
            "webhook_ip_allowlist".into(),
            self.webhook_ip_allowlist.clone(),
        );
        m.insert(
            "daily_loss_limit_inr".into(),
            f64_json(self.daily_loss_limit_inr),
        );
        m.insert("status".into(), json!(self.status));
        m.insert("current_run_id".into(), json!(self.current_run_id));
        m.insert("created_at".into(), iso(Some(&self.created_at)));
        m.insert("updated_at".into(), iso(Some(&self.updated_at)));
        if include_legs {
            m.insert("legs".into(), self.legs.clone());
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: i64,
    pub strategy_id: i64,
    pub mode: String,
    pub broker: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub stop_reason: Option<String>,
    pub stop_requested_at: Option<String>,
    pub stop_requested_reason: Option<String>,
    pub pnl_realized: f64,
    pub pnl_peak: f64,
    pub pnl_trough: f64,
    pub trigger_source: String,
    pub webhook_event_id: Option<i64>,
    pub resolved_expiries: Value,
    /// The account at `broker` the run was opened on (LOG-07); blank when
    /// not known.
    pub broker_account: String,
}

const RUN_COLS: &str = "id, strategy_id, mode, broker, started_at, stopped_at, stop_reason, \
    stop_requested_at, stop_requested_reason, pnl_realized, pnl_peak, pnl_trough, trigger_source, \
    webhook_event_id, resolved_expiries, broker_account";

fn run_from(r: &Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        strategy_id: r.get(1)?,
        mode: r.get(2)?,
        broker: r.get(3)?,
        started_at: r.get(4)?,
        stopped_at: r.get(5)?,
        stop_reason: r.get(6)?,
        stop_requested_at: r.get(7)?,
        stop_requested_reason: r.get(8)?,
        pnl_realized: r.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
        pnl_peak: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
        pnl_trough: r.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
        trigger_source: r.get(12)?,
        webhook_event_id: r.get(13)?,
        resolved_expiries: read_json(r.get(14)?),
        broker_account: r.get(15)?,
    })
}

impl RunRow {
    pub fn is_open(&self) -> bool {
        self.stopped_at.is_none()
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "id": self.id,
            "strategy_id": self.strategy_id,
            "mode": self.mode,
            "broker": self.broker,
            "started_at": iso(Some(&self.started_at)),
            "stopped_at": iso(self.stopped_at.as_deref()),
            "stop_reason": self.stop_reason,
            "stop_requested_at": iso(self.stop_requested_at.as_deref()),
            "stop_requested_reason": self.stop_requested_reason,
            "pnl_realized": fnum(self.pnl_realized),
            "pnl_peak": fnum(self.pnl_peak),
            "pnl_trough": fnum(self.pnl_trough),
            "trigger_source": self.trigger_source,
            "webhook_event_id": self.webhook_event_id,
            "resolved_expiries": self.resolved_expiries,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    pub id: i64,
    pub run_id: i64,
    pub leg_id: i64,
    pub kind: String,
    pub position_ref: Option<String>,
    pub broker_order_id: Option<String>,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub qty: i64,
    pub product: Option<String>,
    pub pricetype: String,
    pub price: f64,
    pub trigger_price: f64,
    pub status: String,
    pub placed_at: String,
    pub filled_at: Option<String>,
    pub avg_fill_price: Option<f64>,
    pub filled_qty: Option<i64>,
    pub reject_reason: Option<String>,
}

const ORDER_COLS: &str =
    "id, run_id, leg_id, kind, position_ref, broker_order_id, symbol, exchange, \
    action, qty, product, pricetype, price, trigger_price, status, placed_at, filled_at, \
    avg_fill_price, filled_qty, reject_reason";

fn order_from(r: &Row<'_>) -> rusqlite::Result<OrderRow> {
    Ok(OrderRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        leg_id: r.get(2)?,
        kind: r.get(3)?,
        position_ref: r.get(4)?,
        broker_order_id: r.get(5)?,
        symbol: r.get(6)?,
        exchange: r.get(7)?,
        action: r.get(8)?,
        qty: r.get(9)?,
        product: r.get(10)?,
        pricetype: r.get(11)?,
        price: r.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
        trigger_price: r.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
        status: r.get(14)?,
        placed_at: r.get(15)?,
        filled_at: r.get(16)?,
        avg_fill_price: r.get(17)?,
        filled_qty: r.get(18)?,
        reject_reason: r.get(19)?,
    })
}

impl OrderRow {
    pub fn to_dict(&self) -> Value {
        json!({
            "id": self.id,
            "run_id": self.run_id,
            "leg_id": self.leg_id,
            "kind": self.kind,
            "position_ref": self.position_ref,
            "broker_order_id": self.broker_order_id,
            "symbol": self.symbol,
            "exchange": self.exchange,
            "action": self.action,
            "qty": self.qty,
            "product": self.product,
            "pricetype": self.pricetype,
            "price": fnum(self.price),
            "trigger_price": fnum(self.trigger_price),
            "status": self.status,
            "placed_at": iso(Some(&self.placed_at)),
            "filled_at": iso(self.filled_at.as_deref()),
            "avg_fill_price": f64_json(self.avg_fill_price),
            "filled_qty": self.filled_qty,
            "reject_reason": self.reject_reason,
        })
    }
}

/// A new order intent, written before the broker is called.
#[derive(Debug, Clone, Default)]
pub struct NewOrder {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub qty: i64,
    pub product: Option<String>,
    pub pricetype: String,
    pub status: String,
    pub position_ref: Option<String>,
    pub broker_order_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub run_id: Option<i64>,
    pub strategy_id: i64,
    pub user_id: String,
    pub ts: String,
    pub kind: String,
    pub severity: String,
    pub leg_id: Option<i64>,
    pub message: String,
    pub payload: Value,
}

impl EventRow {
    pub fn to_dict(&self) -> Value {
        json!({
            "id": self.id,
            "run_id": self.run_id,
            "strategy_id": self.strategy_id,
            "ts": iso(Some(&self.ts)),
            "kind": self.kind,
            "severity": self.severity,
            "leg_id": self.leg_id,
            "message": self.message,
            "payload": self.payload,
        })
    }
}

fn event_from(r: &Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        strategy_id: r.get(2)?,
        user_id: r.get(3)?,
        ts: r.get(4)?,
        kind: r.get(5)?,
        severity: r.get(6)?,
        leg_id: r.get(7)?,
        message: r.get(8)?,
        payload: read_json(r.get(9)?),
    })
}

const EVENT_COLS: &str =
    "id, run_id, strategy_id, user_id, ts, kind, severity, leg_id, message, payload";

/// Optional fields of one audit event.
#[derive(Debug, Clone, Default)]
pub struct EventFields {
    pub run_id: Option<i64>,
    pub leg_id: Option<i64>,
    pub severity: Option<&'static str>,
    pub payload: Option<Value>,
}

/// The durable result of folding one cumulative broker order frame.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderFactFold {
    pub order_id: i64,
    pub previous_status: String,
    pub status: String,
    pub previous_filled_qty: i64,
    pub cumulative_filled_qty: i64,
    pub fill_delta: i64,
    pub previous_average_fill_price: Option<f64>,
    pub average_fill_price: Option<f64>,
    pub changed: bool,
}

impl OrderFactFold {
    pub fn terminal(&self) -> bool {
        TERMINAL_ORDER_STATUSES.contains(&self.status.as_str())
    }

    pub fn was_terminal(&self) -> bool {
        TERMINAL_ORDER_STATUSES.contains(&self.previous_status.as_str())
    }
}

// ---------------------------------------------------------------- the store

/// The strategy tables, through the main database's pool. Connections are
/// checked out per call and returned before any network await.
#[derive(Clone)]
pub struct Store {
    db: Arc<SqliteDb>,
    clock: Arc<dyn crate::clock::Clock>,
}

impl Store {
    pub fn new(db: Arc<SqliteDb>, clock: Arc<dyn crate::clock::Clock>) -> Self {
        Self { db, clock }
    }

    fn conn(&self) -> Result<crate::db::sqlite::DbConn> {
        self.db.conn()
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Run `read` with this store on the blocking pool, so a strategy's
    /// growing history is never read on an async worker, which carries the
    /// market data feed and the risk owners (the books, the history routes
    /// and `/api/v1/strategy`).
    pub async fn blocking<T, F>(&self, read: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || read(&store))
            .await
            .map_err(|e| crate::error::AppError::Internal(format!("Blocking task failed: {}", e)))?
    }

    fn utcnow(&self) -> String {
        ts(self.clock.now())
    }
}
