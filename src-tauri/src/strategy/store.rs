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
//! the order-fact fold and the acknowledgement bind. Management writes hold
//! to the same rule: the status they refuse on is a condition of their own
//! UPDATE (or is read inside their `BEGIN IMMEDIATE` transaction), and each
//! one advances `revision`, which a start's claim and an edit compare against
//! the revision they validated (SM-01, SM-02).

use crate::db::sqlite::SqliteDb;
use crate::error::{AppError, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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

/// Migration `072_strategy_module`: the six `sm_` tables. Idempotent.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sm_strategy (
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
        CREATE INDEX IF NOT EXISTS ix_sm_event_run_ts ON sm_strategy_event (run_id, ts);",
    )?;
    Ok(())
}

/// Migration `078_strategy_revision`: a `revision` on `sm_strategy` that every
/// management write advances, so a start or an edit can confirm, in the same
/// statement that acts, that the row it checked is still the row it changes
/// (SM-01, SM-02). Idempotent; rows that exist start at revision 0, which is
/// all a revision needs: only later changes are compared.
pub fn migrate_revision(conn: &Connection) -> Result<()> {
    if !crate::db::sqlite::migrations::column_exists(conn, "sm_strategy", "revision")? {
        conn.execute_batch(
            "ALTER TABLE sm_strategy ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
        )?;
    }
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
    /// Advanced by every management write (config, Live opt-in, webhook lock,
    /// token). Internal: not part of the web's strategy shape.
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
}

const RUN_COLS: &str = "id, strategy_id, mode, broker, started_at, stopped_at, stop_reason, \
    stop_requested_at, stop_requested_reason, pnl_realized, pnl_peak, pnl_trough, trigger_source, \
    webhook_event_id, resolved_expiries";

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

/// A start refused because the strategy was edited (or its Live opt-in,
/// webhook lock or token changed) after the start read it.
pub const CHANGED_WHILE_STARTING: &str =
    "The strategy was changed while it was starting, so it did not start. Start it again.";
/// A PATCH refused because the strategy changed after it was read.
pub const CHANGED_WHILE_EDITING: &str =
    "The strategy was changed somewhere else while you were editing it. Reload it and try again.";
pub const ALREADY_RUNNING: &str = "This strategy is already running";
pub const LIVE_NOT_ENABLED: &str =
    "This strategy is not enabled for live trading. Enable it first.";
pub const WEBHOOK_LOCKED: &str = "This strategy's webhook is locked";

/// What a start checked, claimed with the same statement that moves the
/// strategy to running (SM-01): the revision it read and validated, and the
/// destination rules it applied to that revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartClaim {
    /// The `revision` the start read before resolving its legs.
    pub revision: i64,
    /// A Live start: the strategy must still be opted in to Live.
    pub live: bool,
    /// A webhook start: the webhook must still be unlocked (kill switch).
    pub webhook: bool,
}

/// The answer to a [`StartClaim`]. Only `Claimed` moved the row; the other
/// variants explain, from a read after the refusal, why it did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    /// The strategy no longer exists.
    Missing,
    /// Another start or a run holds it.
    Running,
    /// Live was switched off after the start read the strategy.
    LiveDisabled,
    /// The webhook was locked after the start read the strategy.
    WebhookLocked,
    /// Edited after the start read it.
    Changed,
}

impl ClaimOutcome {
    /// The trader-facing reason for a refusal; `None` for `Claimed`.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            ClaimOutcome::Claimed => None,
            ClaimOutcome::Missing => Some("Strategy not found"),
            ClaimOutcome::Running => Some(ALREADY_RUNNING),
            ClaimOutcome::LiveDisabled => Some(LIVE_NOT_ENABLED),
            ClaimOutcome::WebhookLocked => Some(WEBHOOK_LOCKED),
            ClaimOutcome::Changed => Some(CHANGED_WHILE_STARTING),
        }
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

    fn utcnow(&self) -> String {
        ts(self.clock.now())
    }

    // ------------------------------------------------------------ strategies

    /// Create a strategy and issue its webhook token, returned once.
    pub fn create_strategy(&self, user_id: &str, config: &Value) -> Result<(StrategyRow, String)> {
        let name = config["name"].as_str().unwrap_or_default().to_string();
        let conn = self.conn()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sm_strategy WHERE user_id = ?1 AND name = ?2)",
            params![user_id, name],
            |r| r.get(0),
        )?;
        if exists {
            return Err(AppError::Validation(format!(
                "A strategy named '{}' already exists",
                name
            )));
        }
        let token = generate_webhook_token();
        let now = self.utcnow();
        let s = |k: &str, d: &str| {
            config
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or(d)
                .to_string()
        };
        conn.execute(
            "INSERT INTO sm_strategy (user_id, name, strategy_kind, direction, universe_tab, \
             underlying, underlying_exchange, strategy_type, entry_time, exit_time, product, \
             pricetype, legs, overall_sl_mtm, overall_target_mtm, lock_profit, trail_sl_to_entry, \
             scheduler, live_enabled, webhook_token_hash, webhook_ip_allowlist, webhook_locked, \
             daily_loss_limit_inr, status, current_run_id, created_at, updated_at) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, 0, \
              ?19, ?20, 0, ?21, 'stopped', NULL, ?22, ?22)",
            params![
                user_id,
                name,
                s("strategy_kind", "batch"),
                s("direction", "both"),
                s("universe_tab", "weekly_monthly"),
                s("underlying", ""),
                s("underlying_exchange", ""),
                s("strategy_type", "intraday"),
                time_col(&config["entry_time"]),
                time_col(&config["exit_time"]),
                s("product", "NRML"),
                s("pricetype", "MARKET"),
                config.get("legs").cloned().unwrap_or(json!([])).to_string(),
                num_param(&config["overall_sl_mtm"]),
                num_param(&config["overall_target_mtm"]),
                json_col(&config["lock_profit"]),
                config["trail_sl_to_entry"].as_bool().unwrap_or(false),
                json_col(&config["scheduler"]),
                hash_webhook_token(&token),
                json_col(&config["webhook_ip_allowlist"]),
                num_param(&config["daily_loss_limit_inr"]),
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        let row = self
            .get_strategy_unscoped(id)?
            .ok_or_else(|| AppError::Internal("created strategy vanished".into()))?;
        Ok((row, token))
    }

    /// Every strategy for a user, newest first, with its last finalised run.
    pub fn list_strategies(
        &self,
        user_id: &str,
        status: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut sql = format!(
            "SELECT {} FROM sm_strategy WHERE user_id = ?1",
            STRATEGY_COLS
        );
        let mut args: Vec<String> = vec![user_id.to_string()];
        if let Some(s) = status {
            args.push(s.to_string());
            sql.push_str(&format!(" AND status = ?{}", args.len()));
        }
        if let Some(q) = q {
            args.push(format!(
                "%{}%",
                q.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            ));
            sql.push_str(&format!(
                " AND name LIKE ?{} ESCAPE '\\' COLLATE NOCASE",
                args.len()
            ));
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), strategy_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut data = row.to_dict(false);
            let last: Option<(i64, Option<f64>, Option<String>)> = conn
                .query_row(
                    "SELECT id, pnl_realized, stopped_at FROM sm_strategy_run \
                     WHERE strategy_id = ?1 AND stopped_at IS NOT NULL \
                     ORDER BY stopped_at DESC, id DESC LIMIT 1",
                    [row.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            data["last_finalized_run"] = match last {
                None => Value::Null,
                Some((id, pnl, stopped)) => json!({
                    "id": id,
                    "pnl_realized": fnum(pnl.unwrap_or(0.0)),
                    "stopped_at": iso(stopped.as_deref()),
                }),
            };
            out.push(data);
        }
        Ok(out)
    }

    /// One strategy, scoped to its owner.
    pub fn get_strategy(&self, strategy_id: i64, user_id: &str) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy WHERE id = ?1 AND user_id = ?2",
                    STRATEGY_COLS
                ),
                params![strategy_id, user_id],
                strategy_from,
            )
            .optional()?)
    }

    /// One strategy without an owner filter. Engine only.
    pub fn get_strategy_unscoped(&self, strategy_id: i64) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy WHERE id = ?1", STRATEGY_COLS),
                [strategy_id],
                strategy_from,
            )
            .optional()?)
    }

    /// Every strategy id (scheduler sync).
    pub fn all_strategy_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id FROM sm_strategy ORDER BY id")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Update a stopped strategy with already-validated `changes`, as one
    /// transaction (SM-02): the owner, the status, the `revision` the caller
    /// validated against and the name's uniqueness are checked under the write
    /// lock, then every field, `updated_at` and the revision commit together
    /// or not at all.
    pub fn update_strategy(
        &self,
        strategy_id: i64,
        user_id: &str,
        revision: i64,
        changes: &Map<String, Value>,
    ) -> Result<StrategyRow> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT status, strategy_kind, revision FROM sm_strategy \
                 WHERE id = ?1 AND user_id = ?2",
                params![strategy_id, user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((status, kind, stored_revision)) = current else {
            return Err(AppError::NotFound("Strategy not found".into()));
        };
        if status == "running" {
            return Err(AppError::Validation(
                "Stop the strategy before editing it".into(),
            ));
        }
        if let Some(wanted) = changes.get("strategy_kind").and_then(Value::as_str) {
            if wanted != kind {
                return Err(AppError::Validation(
                    "A strategy cannot change between batch and signal. The two kinds do not \
                     share a leg shape, so every leg would describe the wrong kind of contract. \
                     Create a new strategy instead."
                        .into(),
                ));
            }
        }
        if stored_revision != revision {
            return Err(AppError::Validation(CHANGED_WHILE_EDITING.into()));
        }
        if let Some(name) = changes.get("name").and_then(Value::as_str) {
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sm_strategy WHERE user_id = ?1 AND name = ?2 \
                 AND id != ?3)",
                params![user_id, name, strategy_id],
                |r| r.get(0),
            )?;
            if taken {
                return Err(AppError::Validation(format!(
                    "A strategy named '{}' already exists",
                    name
                )));
            }
        }
        for (field, value) in changes {
            if !UPDATABLE_FIELDS.contains(&field.as_str()) {
                continue;
            }
            let sql = format!("UPDATE sm_strategy SET {} = ?1 WHERE id = ?2", field);
            let param: rusqlite::types::Value = match field.as_str() {
                "entry_time" | "exit_time" => time_col(value).into(),
                "legs" | "lock_profit" | "scheduler" | "webhook_ip_allowlist" => {
                    json_col(value).into()
                }
                "overall_sl_mtm" | "overall_target_mtm" | "daily_loss_limit_inr" => {
                    num_param(value).into()
                }
                "trail_sl_to_entry" => i64::from(value.as_bool().unwrap_or(false)).into(),
                _ => value.as_str().map(str::to_string).into(),
            };
            tx.execute(&sql, params![param, strategy_id])?;
        }
        tx.execute(
            "UPDATE sm_strategy SET updated_at = ?1, revision = revision + 1 WHERE id = ?2",
            params![self.utcnow(), strategy_id],
        )?;
        let row = tx
            .query_row(
                &format!("SELECT {} FROM sm_strategy WHERE id = ?1", STRATEGY_COLS),
                [strategy_id],
                strategy_from,
            )
            .optional()?
            .ok_or_else(|| AppError::NotFound("Strategy not found".into()))?;
        tx.commit()?;
        Ok(row)
    }

    /// Delete a stopped strategy and every row that belongs to it. The
    /// status is checked inside the delete's own transaction, so a start
    /// that claims the strategy cannot slip between the check and the delete.
    pub fn delete_strategy(&self, strategy_id: i64, user_id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM sm_strategy WHERE id = ?1 AND user_id = ?2",
                params![strategy_id, user_id],
                |r| r.get(0),
            )
            .optional()?;
        match status.as_deref() {
            None => return Err(AppError::NotFound("Strategy not found".into())),
            Some("running") => {
                return Err(AppError::Validation(
                    "Stop the strategy before deleting it".into(),
                ))
            }
            Some(_) => {}
        }
        tx.execute(
            "DELETE FROM sm_strategy_checkpoint WHERE run_id IN \
             (SELECT id FROM sm_strategy_run WHERE strategy_id = ?1)",
            [strategy_id],
        )?;
        tx.execute(
            "DELETE FROM sm_strategy_order WHERE run_id IN \
             (SELECT id FROM sm_strategy_run WHERE strategy_id = ?1)",
            [strategy_id],
        )?;
        tx.execute(
            "DELETE FROM sm_strategy_event WHERE strategy_id = ?1",
            [strategy_id],
        )?;
        tx.execute(
            "DELETE FROM sm_webhook_event WHERE strategy_id = ?1",
            [strategy_id],
        )?;
        tx.execute(
            "DELETE FROM sm_strategy_run WHERE strategy_id = ?1",
            [strategy_id],
        )?;
        tx.execute("DELETE FROM sm_strategy WHERE id = ?1", [strategy_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_strategy_status(
        &self,
        strategy_id: i64,
        status: &str,
        run_id: Option<i64>,
    ) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy SET status = ?1, current_run_id = ?2 WHERE id = ?3",
            params![status, run_id, strategy_id],
        )? == 1)
    }

    /// Stopped to running, in one conditional UPDATE that also checks what
    /// the start decided on (SM-01): the revision it read, the Live opt-in for
    /// a Live start and the webhook lock for a webhook start. A Live disable,
    /// a kill switch or an edit that lands while the start awaits its legs
    /// therefore refuses the claim instead of being overtaken by it. Only the
    /// caller that made the move gets `Claimed`.
    pub fn claim_strategy_for_run(
        &self,
        strategy_id: i64,
        claim: StartClaim,
    ) -> Result<ClaimOutcome> {
        let conn = self.conn()?;
        let moved = conn.execute(
            "UPDATE sm_strategy SET status = 'running' WHERE id = ?1 AND status = 'stopped' \
             AND revision = ?2 AND (?3 = 0 OR live_enabled = 1) AND (?4 = 0 OR webhook_locked = 0)",
            params![strategy_id, claim.revision, claim.live, claim.webhook],
        )?;
        if moved == 1 {
            return Ok(ClaimOutcome::Claimed);
        }
        // The UPDATE above decided; this read only names the reason.
        let row: Option<(String, bool, bool)> = conn
            .query_row(
                "SELECT status, live_enabled, webhook_locked FROM sm_strategy WHERE id = ?1",
                [strategy_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            None => ClaimOutcome::Missing,
            Some((status, _, _)) if status != "stopped" => ClaimOutcome::Running,
            Some((_, live_enabled, _)) if claim.live && !live_enabled => ClaimOutcome::LiveDisabled,
            Some((_, _, locked)) if claim.webhook && locked => ClaimOutcome::WebhookLocked,
            Some(_) => ClaimOutcome::Changed,
        })
    }

    pub fn release_strategy(&self, strategy_id: i64) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy SET status = 'stopped', current_run_id = NULL WHERE id = ?1",
            [strategy_id],
        )? == 1)
    }

    /// Issue a new webhook token (returned once). The owner check is the
    /// UPDATE's own condition, and the revision moves so a start that read
    /// the old token's strategy does not complete.
    pub fn rotate_webhook_token(&self, strategy_id: i64, user_id: &str) -> Result<String> {
        let token = generate_webhook_token();
        let conn = self.conn()?;
        let moved = conn.execute(
            "UPDATE sm_strategy SET webhook_token_hash = ?1, updated_at = ?2, \
             revision = revision + 1 WHERE id = ?3 AND user_id = ?4",
            params![
                hash_webhook_token(&token),
                self.utcnow(),
                strategy_id,
                user_id
            ],
        )?;
        if moved != 1 {
            return Err(AppError::NotFound("Strategy not found".into()));
        }
        Ok(token)
    }

    /// Switch the Live opt-in of a strategy that is not running, in one
    /// conditional UPDATE (SM-01): a start that claims first makes this
    /// refuse, and this landing first makes a Live start's claim refuse.
    pub fn set_live_enabled(&self, strategy_id: i64, user_id: &str, enabled: bool) -> Result<()> {
        let conn = self.conn()?;
        let moved = conn.execute(
            "UPDATE sm_strategy SET live_enabled = ?1, revision = revision + 1 \
             WHERE id = ?2 AND user_id = ?3 AND status != 'running'",
            params![enabled, strategy_id, user_id],
        )?;
        if moved == 1 {
            return Ok(());
        }
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sm_strategy WHERE id = ?1 AND user_id = ?2)",
            params![strategy_id, user_id],
            |r| r.get(0),
        )?;
        if exists {
            Err(AppError::Validation(
                "Stop the strategy before changing its mode".into(),
            ))
        } else {
            Err(AppError::NotFound("Strategy not found".into()))
        }
    }

    /// Lock or unlock the webhook (the kill switch's first step). Allowed
    /// while running; the revision moves so a webhook start already under
    /// way when the lock lands does not complete.
    pub fn set_webhook_locked(&self, strategy_id: i64, user_id: &str, locked: bool) -> Result<()> {
        let conn = self.conn()?;
        let moved = conn.execute(
            "UPDATE sm_strategy SET webhook_locked = ?1, revision = revision + 1 \
             WHERE id = ?2 AND user_id = ?3",
            params![locked, strategy_id, user_id],
        )?;
        if moved != 1 {
            return Err(AppError::NotFound("Strategy not found".into()));
        }
        Ok(())
    }

    /// Resolve an inbound webhook token through its digest (unique index).
    pub fn get_strategy_by_webhook_token(&self, token: &str) -> Result<Option<StrategyRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy WHERE webhook_token_hash = ?1",
                    STRATEGY_COLS
                ),
                [hash_webhook_token(token)],
                strategy_from,
            )
            .optional()?)
    }

    // ------------------------------------------------------------ runs

    pub fn create_run(
        &self,
        strategy_id: i64,
        mode: &str,
        broker: &str,
        trigger_source: &str,
        webhook_event_id: Option<i64>,
        resolved_expiries: Option<&Value>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sm_strategy_run (strategy_id, mode, broker, started_at, trigger_source, \
             webhook_event_id, resolved_expiries) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                strategy_id,
                mode,
                broker,
                self.utcnow(),
                trigger_source,
                webhook_event_id,
                resolved_expiries.and_then(json_col),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_run(&self, run_id: i64) -> Result<Option<RunRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy_run WHERE id = ?1", RUN_COLS),
                [run_id],
                run_from,
            )
            .optional()?)
    }

    pub fn list_runs(&self, strategy_id: i64, limit: i64) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_run WHERE strategy_id = ?1 \
             ORDER BY started_at DESC, id DESC LIMIT ?2",
            RUN_COLS
        ))?;
        let rows = stmt
            .query_map(params![strategy_id, limit.max(1)], run_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(RunRow::to_dict).collect())
    }

    pub fn list_open_runs(&self) -> Result<Vec<RunRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_run WHERE stopped_at IS NULL ORDER BY id",
            RUN_COLS
        ))?;
        let rows = stmt
            .query_map([], run_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn finish_fields(&self) -> String {
        self.utcnow()
    }

    /// Finish one active run and release only its current strategy, in one
    /// transaction. False when the run was already finished or the strategy
    /// now points at another run: neither row changes then.
    pub fn finish_run_and_release_strategy(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
        pnl_realized: f64,
        pnl_peak: f64,
        pnl_trough: f64,
    ) -> Result<bool> {
        let now = self.finish_fields();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let finished = tx.execute(
            "UPDATE sm_strategy_run SET stopped_at = ?1, stop_reason = ?2, stop_requested_at = NULL, \
             stop_requested_reason = NULL, pnl_realized = ?3, pnl_peak = ?4, pnl_trough = ?5 \
             WHERE id = ?6 AND strategy_id = ?7 AND stopped_at IS NULL",
            params![
                now,
                stop_reason,
                round_to(pnl_realized, 2),
                round_to(pnl_peak, 2),
                round_to(pnl_trough, 2),
                run_id,
                strategy_id
            ],
        )?;
        if finished != 1 {
            tx.rollback()?;
            return Ok(false);
        }
        let released = tx.execute(
            "UPDATE sm_strategy SET status = 'stopped', current_run_id = NULL \
             WHERE id = ?1 AND current_run_id = ?2",
            params![strategy_id, run_id],
        )?;
        if released != 1 {
            tx.rollback()?;
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    /// Finish a residual run that is not the strategy's current run.
    pub fn finish_detached_run(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
        pnl_realized: f64,
        pnl_peak: f64,
        pnl_trough: f64,
    ) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy_run SET stopped_at = ?1, stop_reason = ?2, stop_requested_at = NULL, \
             stop_requested_reason = NULL, pnl_realized = ?3, pnl_peak = ?4, pnl_trough = ?5 \
             WHERE id = ?6 AND strategy_id = ?7 AND stopped_at IS NULL AND NOT EXISTS \
             (SELECT 1 FROM sm_strategy WHERE id = ?7 AND current_run_id = ?6)",
            params![
                self.utcnow(),
                stop_reason,
                round_to(pnl_realized, 2),
                round_to(pnl_peak, 2),
                round_to(pnl_trough, 2),
                run_id,
                strategy_id
            ],
        )? == 1)
    }

    /// Close a never-linked run, then release only its empty strategy claim.
    pub fn finish_unlinked_run_and_release_claim(
        &self,
        run_id: i64,
        strategy_id: i64,
        stop_reason: &str,
    ) -> Result<bool> {
        if !self.finish_detached_run(run_id, strategy_id, stop_reason, 0.0, 0.0, 0.0)? {
            return Ok(false);
        }
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE sm_strategy SET status = 'stopped', current_run_id = NULL \
             WHERE id = ?1 AND status = 'running' AND current_run_id IS NULL",
            [strategy_id],
        )? == 1)
    }

    /// Persist a stop request while leaving the run active until it is flat.
    /// True when a request is (now or already) recorded on an open run.
    pub fn request_run_stop(&self, run_id: i64, reason: &str) -> Result<bool> {
        let conn = self.conn()?;
        let updated = conn.execute(
            "UPDATE sm_strategy_run SET stop_requested_at = ?1, stop_requested_reason = ?2 \
             WHERE id = ?3 AND stopped_at IS NULL AND stop_requested_at IS NULL \
             AND stop_requested_reason IS NULL",
            params![self.utcnow(), reason, run_id],
        )?;
        if updated == 1 {
            return Ok(true);
        }
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM sm_strategy_run WHERE id = ?1 AND stopped_at IS NULL \
                 AND stop_requested_at IS NOT NULL AND stop_requested_reason IS NOT NULL",
                [run_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(existing.is_some())
    }

    /// What earlier runs banked since `since` (a daily loss limit's session).
    pub fn realized_pnl_since(
        &self,
        strategy_id: i64,
        since: DateTime<Utc>,
        exclude_run_id: Option<i64>,
    ) -> Result<f64> {
        let conn = self.conn()?;
        let total: Option<f64> = conn.query_row(
            "SELECT SUM(pnl_realized) FROM sm_strategy_run WHERE strategy_id = ?1 \
             AND started_at >= ?2 AND id != ?3",
            params![strategy_id, ts(since), exclude_run_id.unwrap_or(-1)],
            |r| r.get(0),
        )?;
        Ok(total.unwrap_or(0.0))
    }

    // ------------------------------------------------------------ orders

    /// Write an order row at placement time, before the broker answers.
    pub fn record_order(&self, run_id: i64, leg_id: i64, kind: &str, o: &NewOrder) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sm_strategy_order (run_id, leg_id, kind, position_ref, broker_order_id, \
             symbol, exchange, action, qty, product, pricetype, price, trigger_price, status, \
             placed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0, ?12, ?13)",
            params![
                run_id,
                leg_id,
                kind,
                o.position_ref,
                o.broker_order_id,
                o.symbol,
                o.exchange,
                o.action,
                o.qty,
                o.product,
                if o.pricetype.is_empty() {
                    "MARKET"
                } else {
                    &o.pricetype
                },
                if o.status.is_empty() {
                    "pending"
                } else {
                    &o.status
                },
                self.utcnow(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Write the broker's answer onto an order row. False when no row moved.
    pub fn update_order(
        &self,
        order_id: i64,
        status: Option<&str>,
        broker_order_id: Option<&str>,
        reject_reason: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let now = self.utcnow();
        let n = conn.execute(
            "UPDATE sm_strategy_order SET \
               status = COALESCE(?1, status), \
               filled_at = CASE WHEN ?1 = 'complete' AND filled_at IS NULL THEN ?2 ELSE filled_at END, \
               broker_order_id = COALESCE(?3, broker_order_id), \
               reject_reason = COALESCE(?4, reject_reason) \
             WHERE id = ?5",
            params![status, now, broker_order_id, reject_reason, order_id],
        )?;
        Ok(n == 1)
    }

    pub fn get_order(&self, order_id: i64) -> Result<Option<OrderRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM sm_strategy_order WHERE id = ?1", ORDER_COLS),
                [order_id],
                order_from,
            )
            .optional()?)
    }

    /// The strategy order carrying this broker reference (the "is it ours"
    /// test on every order update).
    pub fn get_order_by_broker_id(&self, broker_order_id: &str) -> Result<Option<OrderRow>> {
        if broker_order_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy_order WHERE broker_order_id = ?1 ORDER BY id LIMIT 1",
                    ORDER_COLS
                ),
                [broker_order_id],
                order_from,
            )
            .optional()?)
    }

    pub fn list_orders(&self, run_id: i64) -> Result<Vec<OrderRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_order WHERE run_id = ?1 ORDER BY placed_at ASC, id ASC",
            ORDER_COLS
        ))?;
        let rows = stmt
            .query_map([run_id], order_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_orders_for_strategy(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> Result<Vec<OrderRow>> {
        let conn = self.conn()?;
        let cols = ORDER_COLS
            .split(", ")
            .map(|c| format!("o.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_order o JOIN sm_strategy_run r ON o.run_id = r.id \
             WHERE r.strategy_id = ?1 AND (?2 IS NULL OR o.run_id = ?2) \
             ORDER BY o.placed_at ASC, o.id ASC",
            cols
        ))?;
        let rows = stmt
            .query_map(params![strategy_id, run_id], order_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Atomically fold one cumulative broker frame into an order row (web
    /// `fold_order_broker_frame`). Quantity evidence is cumulative, a working
    /// frame never reopens a terminal row, and a dead row is upgraded only by
    /// a `complete` frame carrying strictly more filled quantity. The UPDATE
    /// is conditional on the facts this read saw; a loser retries.
    pub fn fold_order_broker_frame(
        &self,
        order_id: i64,
        status: &str,
        avg_fill_price: Option<f64>,
        filled_qty: Option<i64>,
        reject_reason: Option<&str>,
    ) -> Result<Option<OrderFactFold>> {
        let mut incoming = status.trim().to_ascii_lowercase();
        if !["open", "complete", "cancelled", "rejected"].contains(&incoming.as_str()) {
            incoming = "open".into();
        }
        let incoming_qty = filled_qty.unwrap_or(0).max(0);
        for _ in 0..8 {
            let Some(row) = self.get_order(order_id)? else {
                return Ok(None);
            };
            let raw_status = row.status.clone();
            let previous_status = raw_status.trim().to_ascii_lowercase();
            let previous_qty = row.filled_qty.unwrap_or(0).max(0);
            let previous_price = row.avg_fill_price;
            let mut evidence = incoming_qty;
            if incoming == "complete" && evidence <= 0 {
                // A complete order with an omitted quantity traded in full.
                evidence = row.qty.max(0);
            }
            let cumulative = previous_qty.max(evidence);
            let delta = cumulative - previous_qty;
            let next_status = if previous_status == "complete" {
                "complete".to_string()
            } else if previous_status == "cancelled" || previous_status == "rejected" {
                if incoming == "complete" && delta > 0 {
                    "complete".to_string()
                } else {
                    previous_status.clone()
                }
            } else if TERMINAL_ORDER_STATUSES.contains(&incoming.as_str()) {
                incoming.clone()
            } else {
                "open".to_string()
            };
            let status_changed = next_status != previous_status;
            if !status_changed && delta <= 0 {
                return Ok(Some(OrderFactFold {
                    order_id,
                    previous_status: previous_status.clone(),
                    status: previous_status,
                    previous_filled_qty: previous_qty,
                    cumulative_filled_qty: previous_qty,
                    fill_delta: 0,
                    previous_average_fill_price: previous_price,
                    average_fill_price: previous_price,
                    changed: false,
                }));
            }
            let set_fill = delta > 0;
            let set_filled_at = next_status == "complete" && previous_status != "complete";
            let set_reason = reject_reason.is_some()
                && (next_status == "cancelled" || next_status == "rejected");
            let conn = self.conn()?;
            let n = conn.execute(
                "UPDATE sm_strategy_order SET status = ?1, \
                   filled_qty = CASE WHEN ?2 THEN ?3 ELSE filled_qty END, \
                   avg_fill_price = CASE WHEN ?2 THEN ?4 ELSE avg_fill_price END, \
                   filled_at = CASE WHEN ?5 THEN ?6 ELSE filled_at END, \
                   reject_reason = CASE WHEN ?7 THEN ?8 ELSE reject_reason END \
                 WHERE id = ?9 AND status = ?10 AND filled_qty IS ?11",
                params![
                    next_status,
                    set_fill,
                    cumulative,
                    avg_fill_price,
                    set_filled_at,
                    self.utcnow(),
                    set_reason,
                    reject_reason,
                    order_id,
                    raw_status,
                    row.filled_qty,
                ],
            )?;
            if n != 1 {
                continue;
            }
            return Ok(Some(OrderFactFold {
                order_id,
                previous_status,
                status: next_status,
                previous_filled_qty: previous_qty,
                cumulative_filled_qty: cumulative,
                fill_delta: delta,
                previous_average_fill_price: previous_price,
                average_fill_price: if set_fill {
                    avg_fill_price
                } else {
                    previous_price
                },
                changed: true,
            }));
        }
        tracing::error!(
            "Could not fold broker facts for strategy order {} after concurrent updates",
            order_id
        );
        Ok(None)
    }

    /// Repair one exact pending row from its durable acknowledgement witness.
    /// Returns `repaired`, `already_bound`, `conflict` or `missing`.
    pub fn bind_order_acknowledgement(
        &self,
        order_id: i64,
        run_id: i64,
        leg_id: i64,
        broker_order_id: Option<&str>,
        status: &str,
        reject_reason: Option<&str>,
    ) -> Result<&'static str> {
        let desired = status.trim().to_ascii_lowercase();
        if desired != "open" && desired != "rejected" {
            return Ok("conflict");
        }
        let desired_id = broker_order_id.map(str::trim).filter(|s| !s.is_empty());
        if desired == "open" && desired_id.is_none() {
            return Ok("conflict");
        }
        let Some(row) = self
            .get_order(order_id)?
            .filter(|r| r.run_id == run_id && r.leg_id == leg_id)
        else {
            return Ok("missing");
        };
        let current_id = row
            .broker_order_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if current_id.is_some() && current_id != desired_id {
            return Ok("conflict");
        }
        if let Some(id) = desired_id {
            if let Some(other) = self.get_order_by_broker_id(id)? {
                if other.id != order_id {
                    return Ok("conflict");
                }
            }
        }
        let current_status = row.status.trim().to_ascii_lowercase();
        if current_status != "pending" {
            if current_id == desired_id
                && (desired == "open" || (desired == "rejected" && current_status == "rejected"))
            {
                return Ok("already_bound");
            }
            return Ok("conflict");
        }
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE sm_strategy_order SET status = ?1, \
               broker_order_id = COALESCE(?2, broker_order_id), \
               reject_reason = CASE WHEN ?1 = 'rejected' THEN ?3 ELSE reject_reason END \
             WHERE id = ?4 AND run_id = ?5 AND leg_id = ?6 AND status = ?7 \
               AND broker_order_id IS ?8",
            params![
                desired,
                desired_id,
                reject_reason,
                order_id,
                run_id,
                leg_id,
                row.status,
                row.broker_order_id
            ],
        )?;
        Ok(if n == 1 { "repaired" } else { "conflict" })
    }

    // ------------------------------------------------------------ events

    pub fn record_event(
        &self,
        strategy_id: i64,
        user_id: &str,
        kind: &str,
        message: &str,
        f: &EventFields,
    ) -> Result<EventRow> {
        let conn = self.conn()?;
        let now = self.utcnow();
        conn.execute(
            "INSERT INTO sm_strategy_event (run_id, strategy_id, user_id, ts, kind, severity, \
             leg_id, message, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                f.run_id,
                strategy_id,
                user_id,
                now,
                kind,
                f.severity.unwrap_or("info"),
                f.leg_id,
                message,
                f.payload.as_ref().and_then(json_col),
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(EventRow {
            id,
            run_id: f.run_id,
            strategy_id,
            user_id: user_id.to_string(),
            ts: now,
            kind: kind.to_string(),
            severity: f.severity.unwrap_or("info").to_string(),
            leg_id: f.leg_id,
            message: message.to_string(),
            payload: f.payload.clone().unwrap_or(Value::Null),
        })
    }

    pub fn list_events(
        &self,
        strategy_id: i64,
        run_id: Option<i64>,
        kind: Option<&str>,
        severity: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_event WHERE strategy_id = ?1 \
             AND (?2 IS NULL OR run_id = ?2) AND (?3 IS NULL OR kind = ?3) \
             AND (?4 IS NULL OR severity = ?4) ORDER BY ts DESC, id DESC LIMIT ?5",
            EVENT_COLS
        ))?;
        let rows = stmt
            .query_map(
                params![strategy_id, run_id, kind, severity, limit.max(1)],
                event_from,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.iter().map(EventRow::to_dict).collect())
    }

    /// The append-only lost-acknowledgement witnesses of one run, oldest first.
    pub fn list_order_ack_events(&self, run_id: i64) -> Result<Vec<EventRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_event WHERE run_id = ?1 AND kind = 'order_ack_unrecorded' \
             ORDER BY id ASC",
            EVENT_COLS
        ))?;
        let rows = stmt
            .query_map([run_id], event_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------ checkpoints

    pub fn write_checkpoint(&self, run_id: i64, snapshot: &Value) -> Result<()> {
        let f = |k: &str| round_to(snapshot[k].as_f64().unwrap_or(0.0), 2);
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sm_strategy_checkpoint (run_id, ts, pnl_realized, pnl_unrealized, \
             pnl_total, pnl_peak, pnl_trough, lock_floor, trail_to_entry_active, leg_state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run_id,
                self.utcnow(),
                f("pnl_realized"),
                f("pnl_unrealized"),
                f("pnl_total"),
                f("pnl_peak"),
                f("pnl_trough"),
                snapshot["lock_floor"].as_f64().map(|v| round_to(v, 2)),
                snapshot["trail_to_entry_active"].as_bool().unwrap_or(false),
                snapshot
                    .get("leg_state")
                    .cloned()
                    .unwrap_or(json!({}))
                    .to_string(),
            ],
        )?;
        Ok(())
    }

    fn checkpoint_dict(r: &Row<'_>) -> rusqlite::Result<Value> {
        let lock_floor: Option<f64> = r.get(8)?;
        let ts: String = r.get(2)?;
        let id: i64 = r.get(0)?;
        let run_id: i64 = r.get(1)?;
        let money = |i: usize| -> rusqlite::Result<Value> {
            Ok(fnum(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0)))
        };
        let (realized, unrealized, total, peak, trough) =
            (money(3)?, money(4)?, money(5)?, money(6)?, money(7)?);
        let trail: bool = r.get(9)?;
        let mut legs = read_json(r.get(10)?);
        if legs.is_null() {
            legs = json!({});
        }
        Ok(json!({
            "id": id,
            "run_id": run_id,
            "ts": iso(Some(&ts)),
            "pnl_realized": realized,
            "pnl_unrealized": unrealized,
            "pnl_total": total,
            "pnl_peak": peak,
            "pnl_trough": trough,
            "lock_floor": f64_json(lock_floor),
            "trail_to_entry_active": trail,
            "leg_state": legs,
        }))
    }

    const CHECKPOINT_COLS: &'static str =
        "c.id, c.run_id, c.ts, c.pnl_realized, c.pnl_unrealized, \
        c.pnl_total, c.pnl_peak, c.pnl_trough, c.lock_floor, c.trail_to_entry_active, c.leg_state";

    pub fn latest_checkpoint(&self, run_id: i64) -> Result<Option<Value>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM sm_strategy_checkpoint c WHERE c.run_id = ?1 \
                     ORDER BY c.ts DESC, c.id DESC LIMIT 1",
                    Self::CHECKPOINT_COLS
                ),
                [run_id],
                Self::checkpoint_dict,
            )
            .optional()?)
    }

    /// A run's checkpoints, oldest first, narrowed to a run of `strategy_id`.
    pub fn list_checkpoints(
        &self,
        run_id: i64,
        limit: i64,
        strategy_id: Option<i64>,
    ) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM sm_strategy_checkpoint c JOIN sm_strategy_run r ON c.run_id = r.id \
             WHERE c.run_id = ?1 AND (?2 IS NULL OR r.strategy_id = ?2) \
             ORDER BY c.ts ASC, c.id ASC LIMIT ?3",
            Self::CHECKPOINT_COLS
        ))?;
        let rows = stmt
            .query_map(
                params![run_id, strategy_id, limit.max(1)],
                Self::checkpoint_dict,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Keep only the newest `keep` checkpoints for a run.
    pub fn prune_checkpoints(&self, run_id: i64, keep: i64) -> Result<usize> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "DELETE FROM sm_strategy_checkpoint WHERE run_id = ?1 AND id NOT IN \
             (SELECT id FROM sm_strategy_checkpoint WHERE run_id = ?1 ORDER BY ts DESC, id DESC LIMIT ?2)",
            params![run_id, keep.max(1)],
        )?)
    }

    pub fn count_checkpoints(&self, run_id: i64) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM sm_strategy_checkpoint WHERE run_id = ?1",
            [run_id],
            |r| r.get(0),
        )?)
    }

    // ------------------------------------------------------------ webhook audit

    /// Audit one inbound webhook, whatever the outcome. Ownerless rows (an
    /// unknown token) are capped at the newest `MAX_UNATTRIBUTED_WEBHOOK_EVENTS`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_webhook_event(
        &self,
        result: &str,
        strategy_id: Option<i64>,
        action: Option<&str>,
        mode: Option<&str>,
        payload: Option<&Value>,
        ip: Option<&str>,
        user_agent: Option<&str>,
        error: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        let ua = user_agent
            .map(|u| u.chars().take(255).collect::<String>())
            .filter(|u| !u.is_empty());
        conn.execute(
            "INSERT INTO sm_webhook_event (strategy_id, action, mode, payload, ip, user_agent, \
             received_at, result, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                strategy_id,
                action.map(|a| a.chars().take(20).collect::<String>()),
                mode.map(|m| m.chars().take(10).collect::<String>()),
                payload.and_then(json_col),
                ip.map(|i| i.chars().take(45).collect::<String>()),
                ua,
                self.utcnow(),
                result,
                error
            ],
        )?;
        let id = conn.last_insert_rowid();
        if strategy_id.is_none() {
            conn.execute(
                "DELETE FROM sm_webhook_event WHERE strategy_id IS NULL AND id NOT IN \
                 (SELECT id FROM sm_webhook_event WHERE strategy_id IS NULL ORDER BY id DESC LIMIT ?1)",
                [MAX_UNATTRIBUTED_WEBHOOK_EVENTS],
            )?;
        }
        Ok(id)
    }

    pub fn list_webhook_events(&self, strategy_id: i64, limit: i64) -> Result<Vec<Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, strategy_id, action, mode, payload, ip, user_agent, received_at, result, \
             error FROM sm_webhook_event WHERE strategy_id = ?1 \
             ORDER BY received_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![strategy_id, limit.max(1)], |r| {
                let received: String = r.get(7)?;
                Ok(json!({
                    "id": (r.get::<_, i64>(0)?),
                    "strategy_id": (r.get::<_, Option<i64>>(1)?),
                    "action": (r.get::<_, Option<String>>(2)?),
                    "mode": (r.get::<_, Option<String>>(3)?),
                    "payload": read_json(r.get(4)?),
                    "ip": (r.get::<_, Option<String>>(5)?),
                    "user_agent": (r.get::<_, Option<String>>(6)?),
                    "received_at": iso(Some(&received)),
                    "result": (r.get::<_, String>(8)?),
                    "error": (r.get::<_, Option<String>>(9)?),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_unattributed_webhook_events(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM sm_webhook_event WHERE strategy_id IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// The username that owns the API key (single-user: one row).
    pub fn api_key_owner(&self) -> Result<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row("SELECT name FROM api_keys ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Raw SQL for tests and recovery tooling that need to place the store in
    /// a state no product path produces.
    pub fn execute_raw(&self, sql: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(sql)?;
        Ok(())
    }
}
