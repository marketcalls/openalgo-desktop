//! Push channel for live strategy runs (web
//! `services/strategy_module/broadcast.py`).
//!
//! Six events, all on the default namespace, all addressed to the room
//! `strategy:{id}`, all carrying the same envelope (`type`, `strategy_id`,
//! `run_id`, `ts` in IST ISO 8601, `ts_ms` epoch milliseconds):
//!
//! | Event | Adds |
//! |---|---|
//! | `strategy_snapshot` | live figures and every leg |
//! | `strategy_delta` | live figures and the open legs (throttled to 100 ms) |
//! | `strategy_event` | `event` (an event row) |
//! | `strategy_order_update` | `order` (an order row) |
//! | `strategy_run_update` | `run` (a run row) |
//! | `strategy_terminal` | `stop_reason`, `pnl_realized` |
//!
//! Nothing here is load bearing for money: every push swallows its own
//! failure. The throttle map is bounded (dropped on terminal, swept past
//! 256 entries, reset if still over).

use super::risk_adapter::run_pnl;
use super::state::{favorable_peak_points, LegState, RunState};
use crate::clock::Clock;
use async_trait::async_trait;
use chrono::DateTime;
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const ROOM_PREFIX: &str = "strategy:";
pub const EVENT_SNAPSHOT: &str = "strategy_snapshot";
pub const EVENT_DELTA: &str = "strategy_delta";
pub const EVENT_EVENT: &str = "strategy_event";
pub const EVENT_ORDER_UPDATE: &str = "strategy_order_update";
pub const EVENT_RUN_UPDATE: &str = "strategy_run_update";
pub const EVENT_TERMINAL: &str = "strategy_terminal";

pub const DELTA_MIN_INTERVAL: Duration = Duration::from_millis(100);
pub const MAX_TRACKED_STRATEGIES: usize = 256;
pub const THROTTLE_IDLE: Duration = Duration::from_secs(900);

pub fn room_for(strategy_id: i64) -> String {
    format!("{}{}", ROOM_PREFIX, strategy_id)
}

/// Where the frames go. Production is the Socket.IO server; tests record.
#[async_trait]
pub trait RoomEmitter: Send + Sync {
    /// Whether anyone is in the room. Fail open when it cannot be told.
    fn has_subscribers(&self, room: &str) -> bool;
    async fn emit_to(&self, room: &str, event: &str, payload: Value);
}

/// The production emitter over the app's Socket.IO handle.
pub struct SocketRooms {
    ui: Arc<crate::events::subscribers::SocketEmitter>,
}

impl SocketRooms {
    pub fn new(ui: Arc<crate::events::subscribers::SocketEmitter>) -> Self {
        Self { ui }
    }
}

#[async_trait]
impl RoomEmitter for SocketRooms {
    fn has_subscribers(&self, room: &str) -> bool {
        match self.ui.io() {
            Some(io) => !io.within(room.to_string()).sockets().is_empty(),
            None => false,
        }
    }

    async fn emit_to(&self, room: &str, event: &str, payload: Value) {
        if let Some(io) = self.ui.io() {
            if let Err(e) = io
                .to(room.to_string())
                .emit(event.to_string(), &payload)
                .await
            {
                tracing::debug!("Strategy broadcast {} to {} failed: {}", event, room, e);
            }
        }
    }
}

/// Builds and sends the six frames.
pub struct Broadcaster {
    rooms: Arc<dyn RoomEmitter>,
    clock: Arc<dyn Clock>,
    last_delta: Mutex<HashMap<i64, Instant>>,
}

fn num(v: Option<f64>) -> Value {
    v.filter(|f| f.is_finite())
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn num0(v: f64) -> Value {
    if v.is_finite() {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(json!(0.0))
    } else {
        json!(0.0)
    }
}

/// One leg, the same shape in a snapshot and a delta.
pub fn leg_payload(leg: &LegState) -> Value {
    json!({
        "leg_id": leg.leg_id,
        "symbol": leg.symbol,
        "exchange": leg.exchange,
        "position": leg.position,
        "lots": leg.lots,
        "qty": leg.qty,
        "status": leg.status,
        "entry_status": leg.entry_status,
        "exit_kind": leg.exit_kind,
        "ltp": num(leg.ltp),
        "entry_avg": num0(leg.entry_avg),
        "mtm": num0(leg.mtm),
        "realized_pnl": num0(leg.realized_pnl),
        "effective_sl": num(leg.effective_sl),
        "effective_target": num(leg.effective_target),
        "trail_active": leg.trail_active,
        "favorable_points": num0(favorable_peak_points(leg)),
        "tick_source": leg.tick_source,
    })
}

/// The run's live P&L and ratchets. Realized and unrealized are recomputed
/// from the legs, never read from a figure an earlier pass wrote.
fn figures(run: &RunState) -> serde_json::Map<String, Value> {
    let (realized, unrealized) = run_pnl(run).unwrap_or((run.pnl_realized, run.pnl_unrealized));
    let mut m = serde_json::Map::new();
    m.insert("mtm_realized".into(), num0(realized));
    m.insert("mtm_unrealized".into(), num0(unrealized));
    m.insert("mtm_total".into(), num0(realized + unrealized));
    m.insert("peak".into(), num0(run.pnl_peak));
    m.insert("trough".into(), num0(run.pnl_trough));
    m.insert("lock_armed".into(), json!(run.lock_armed));
    m.insert("lock_floor".into(), num(run.lock_floor));
    m.insert(
        "trail_to_entry_active".into(),
        json!(run.trail_to_entry_active),
    );
    m.insert(
        "tick_source_degraded".into(),
        json!(run.tick_source_degraded),
    );
    m
}

impl Broadcaster {
    pub fn new(rooms: Arc<dyn RoomEmitter>, clock: Arc<dyn Clock>) -> Self {
        Self {
            rooms,
            clock,
            last_delta: Mutex::new(HashMap::new()),
        }
    }

    /// The fields every frame carries.
    pub fn envelope(
        &self,
        kind: &str,
        strategy_id: i64,
        run_id: Option<i64>,
    ) -> serde_json::Map<String, Value> {
        let now: DateTime<chrono_tz::Tz> = self.clock.now().with_timezone(&Kolkata);
        let mut m = serde_json::Map::new();
        m.insert("type".into(), json!(kind));
        m.insert("strategy_id".into(), json!(strategy_id));
        m.insert("run_id".into(), json!(run_id));
        m.insert(
            "ts".into(),
            json!(now.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string()),
        );
        m.insert("ts_ms".into(), json!(now.timestamp_millis()));
        m
    }

    fn state_payload(&self, run: &RunState, kind: &str, open_only: bool) -> Value {
        let mut legs: Vec<&LegState> = if open_only {
            run.open_legs()
        } else {
            run.legs.values().collect()
        };
        legs.sort_by_key(|l| l.leg_id);
        let mut payload = self.envelope(kind, run.strategy_id, Some(run.run_id));
        payload.extend(figures(run));
        payload.insert(
            "legs".into(),
            Value::Array(legs.into_iter().map(leg_payload).collect()),
        );
        Value::Object(payload)
    }

    /// The whole of a run's live state (also the REST snapshot body).
    pub fn snapshot_payload(&self, run: &RunState) -> Value {
        self.state_payload(run, "snapshot", false)
    }

    pub fn delta_payload(&self, run: &RunState) -> Value {
        self.state_payload(run, "delta", true)
    }

    pub fn has_subscribers(&self, strategy_id: i64) -> bool {
        self.rooms.has_subscribers(&room_for(strategy_id))
    }

    fn admit_delta(&self, strategy_id: i64, force: bool) -> bool {
        let now = Instant::now();
        let mut map = self.last_delta.lock();
        if map.len() > MAX_TRACKED_STRATEGIES {
            map.retain(|_, at| now.saturating_duration_since(*at) <= THROTTLE_IDLE);
            if map.len() > MAX_TRACKED_STRATEGIES {
                tracing::warn!(
                    "Strategy delta throttle is tracking {} strategies; resetting it",
                    map.len()
                );
                map.clear();
            }
        }
        if !force {
            if let Some(last) = map.get(&strategy_id) {
                if now.saturating_duration_since(*last) < DELTA_MIN_INTERVAL {
                    return false;
                }
            }
        }
        map.insert(strategy_id, now);
        true
    }

    /// Entries in the throttle map (hygiene tests).
    pub fn tracked(&self) -> usize {
        self.last_delta.lock().len()
    }

    pub fn forget_strategy(&self, strategy_id: i64) {
        self.last_delta.lock().remove(&strategy_id);
    }

    pub async fn push_snapshot(&self, run: Option<RunState>) -> bool {
        let Some(run) = run else {
            return false;
        };
        if !self.has_subscribers(run.strategy_id) {
            return false;
        }
        let payload = self.snapshot_payload(&run);
        self.rooms
            .emit_to(&room_for(run.strategy_id), EVENT_SNAPSHOT, payload)
            .await;
        true
    }

    /// Push a run's live figures and open legs, subject to the throttle.
    /// `force` exempts the one-off deltas (after a fill, the last of a run).
    pub async fn push_delta(&self, run: Option<RunState>, force: bool) -> bool {
        let Some(run) = run else {
            return false;
        };
        // Before the throttle: an unwatched run must not consume its window.
        if !self.has_subscribers(run.strategy_id) {
            return false;
        }
        if !self.admit_delta(run.strategy_id, force) {
            return false;
        }
        let payload = self.delta_payload(&run);
        self.rooms
            .emit_to(&room_for(run.strategy_id), EVENT_DELTA, payload)
            .await;
        true
    }

    async fn push_one(
        &self,
        event: &str,
        strategy_id: i64,
        payload: serde_json::Map<String, Value>,
    ) -> bool {
        if !self.has_subscribers(strategy_id) {
            return false;
        }
        self.rooms
            .emit_to(&room_for(strategy_id), event, Value::Object(payload))
            .await;
        true
    }

    pub async fn push_event(&self, strategy_id: i64, event: Value) -> bool {
        let run_id = event.get("run_id").and_then(Value::as_i64);
        let mut p = self.envelope("event", strategy_id, run_id);
        p.insert("event".into(), event);
        self.push_one(EVENT_EVENT, strategy_id, p).await
    }

    pub async fn push_order_update(&self, strategy_id: i64, order: Value) -> bool {
        let run_id = order.get("run_id").and_then(Value::as_i64);
        let mut p = self.envelope("order_update", strategy_id, run_id);
        p.insert("order".into(), order);
        self.push_one(EVENT_ORDER_UPDATE, strategy_id, p).await
    }

    pub async fn push_run_update(&self, strategy_id: i64, run: Value) -> bool {
        let run_id = run.get("id").and_then(Value::as_i64);
        let mut p = self.envelope("run_update", strategy_id, run_id);
        p.insert("run".into(), run);
        self.push_one(EVENT_RUN_UPDATE, strategy_id, p).await
    }

    /// The run's last word. Drops the throttle entry whether or not it went.
    pub async fn push_terminal(
        &self,
        strategy_id: i64,
        run_id: i64,
        stop_reason: &str,
        pnl_realized: f64,
    ) -> bool {
        let mut p = self.envelope("terminal", strategy_id, Some(run_id));
        p.insert("stop_reason".into(), json!(stop_reason));
        p.insert("pnl_realized".into(), num0(pnl_realized));
        let sent = self.push_one(EVENT_TERMINAL, strategy_id, p).await;
        self.forget_strategy(strategy_id);
        sent
    }
}
