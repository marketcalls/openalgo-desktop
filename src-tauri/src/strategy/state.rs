//! In-process runtime state for strategy runs (web
//! `services/strategy_module/state.py`).
//!
//! One `Mutex<RunState>` per run, in a registry keyed by run id. The stored
//! shape serialises to exactly the checkpoint's `leg_state` JSON, so a
//! snapshot round-trips through the database without translation.
//!
//! **A critical section holds in-memory bookkeeping only.** No database, no
//! broker, no emit, no await. Callers read what they need, release, and then
//! do the slow work. Every claim below checks and marks in ONE hold of the
//! run's lock: that is the whole defence against two decisions becoming two
//! orders.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// A fresh opaque identity for one position incarnation or one claim.
pub fn new_position_ref() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The outgoing position of a signal flip whose closing order has not filled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Superseded {
    pub exit_order_id: Option<i64>,
    pub exit_claim_token: Option<String>,
    pub exit_kind: Option<String>,
    pub entry_order_id: Option<i64>,
    pub position_ref: Option<String>,
    pub position: String,
    pub entry_avg: f64,
    pub qty: i64,
}

/// One leg's live state. Field names are the checkpoint's `leg_state` keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegState {
    pub leg_id: i64,
    /// `B` or `S`, always. A leg without a side is refused, never defaulted.
    pub position: String,
    pub symbol: String,
    pub exchange: String,
    pub lots: i64,
    pub qty: i64,
    pub position_ref: Option<String>,
    pub entry_order_id: Option<i64>,
    pub entry_status: String,
    pub entry_filled_qty: i64,
    pub entry_avg: f64,
    pub exit_order_id: Option<i64>,
    pub exit_claim_token: Option<String>,
    pub exit_kind: Option<String>,
    pub exit_avg: Option<f64>,
    pub ltp: Option<f64>,
    pub mtm: f64,
    pub realized_pnl: f64,
    pub status: String,
    pub tick_source: String,
    pub risk_unit: String,
    pub sl_pts: Option<f64>,
    pub target_pts: Option<f64>,
    pub trail_x: f64,
    pub trail_y: f64,
    pub effective_sl: Option<f64>,
    pub effective_target: Option<f64>,
    pub trail_active: bool,
    pub highest_price: Option<f64>,
    pub lowest_price: Option<f64>,
    pub superseded: Option<Superseded>,
}

impl LegState {
    pub fn exit_in_flight(&self) -> bool {
        self.exit_kind.is_some() || self.exit_claim_token.is_some() || self.exit_order_id.is_some()
    }

    /// Whether this leg still owns exposure, or an entry that may become one.
    pub fn requires_management(&self) -> bool {
        self.superseded.is_some()
            || self.exit_order_id.is_some()
            || self.exit_claim_token.is_some()
            || self.status == "open"
            || self.entry_status == "pending"
            || self.entry_status == "open"
    }
}

/// The resolved description of a leg to seed into run state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegSpec {
    pub leg_id: i64,
    pub position: String,
    pub symbol: String,
    pub exchange: String,
    pub lots: i64,
    pub quantity: i64,
    pub position_ref: Option<String>,
    pub sl_pts: Option<f64>,
    pub target_pts: Option<f64>,
    pub trail_x: f64,
    pub trail_y: f64,
    pub risk_unit: String,
    pub expiry: Option<String>,
    pub expiry_fallback: bool,
    pub expiry_rank: Option<String>,
}

/// One leg's starting state. `position` must be `B` or `S`: the original
/// left it unset on signal legs, and its evaluator then read every such leg
/// as a short, so the stop fired on a favourable move.
pub fn new_leg_state(spec: &LegSpec) -> Result<LegState, String> {
    let position = spec.position.trim().to_ascii_uppercase();
    if position != "B" && position != "S" {
        return Err(format!(
            "Leg {} has an unusable position: {:?}",
            spec.leg_id, spec.position
        ));
    }
    Ok(LegState {
        leg_id: spec.leg_id,
        position,
        symbol: spec.symbol.clone(),
        exchange: spec.exchange.clone(),
        lots: spec.lots,
        qty: spec.quantity,
        position_ref: spec.position_ref.clone(),
        entry_order_id: None,
        entry_status: "pending".into(),
        entry_filled_qty: 0,
        entry_avg: 0.0,
        exit_order_id: None,
        exit_claim_token: None,
        exit_kind: None,
        exit_avg: None,
        ltp: None,
        mtm: 0.0,
        realized_pnl: 0.0,
        status: "configured".into(),
        tick_source: "ws".into(),
        risk_unit: if spec.risk_unit.is_empty() {
            "points".into()
        } else {
            spec.risk_unit.clone()
        },
        sl_pts: spec.sl_pts,
        target_pts: spec.target_pts,
        trail_x: spec.trail_x,
        trail_y: spec.trail_y,
        effective_sl: None,
        effective_target: None,
        trail_active: false,
        highest_price: None,
        lowest_price: None,
        superseded: None,
    })
}

/// One in-flight signal entry decision for a leg.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryClaim {
    pub claim_token: String,
    pub position_ref: String,
    pub position: String,
    pub held_position: Option<String>,
    pub expected_position_ref: Option<String>,
}

/// What `claim_signal_entry` decided.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryDecision {
    Claimed(EntryClaim),
    /// A no-op (`already_long`, `already_short`) or a refusal
    /// (`flip_pending`, `run_stopping`), named.
    Note(&'static str),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: i64,
    pub strategy_id: i64,
    pub pnl_realized: f64,
    pub pnl_unrealized: f64,
    pub pnl_total: f64,
    pub pnl_peak: f64,
    pub pnl_trough: f64,
    pub lock_armed: bool,
    pub lock_floor: Option<f64>,
    pub trail_to_entry_active: bool,
    pub tick_source_degraded: bool,
    /// Set after the stop request is durable; gates new signal entries.
    pub stopping: bool,
    pub signal_entry_claims: BTreeMap<String, EntryClaim>,
    pub legs: BTreeMap<String, LegState>,
}

impl RunState {
    pub fn new(run_id: i64, strategy_id: i64, legs: Vec<LegState>) -> Self {
        Self {
            run_id,
            strategy_id,
            pnl_realized: 0.0,
            pnl_unrealized: 0.0,
            pnl_total: 0.0,
            pnl_peak: 0.0,
            pnl_trough: 0.0,
            lock_armed: false,
            lock_floor: None,
            trail_to_entry_active: false,
            tick_source_degraded: false,
            stopping: false,
            signal_entry_claims: BTreeMap::new(),
            legs: legs
                .into_iter()
                .map(|l| (l.leg_id.to_string(), l))
                .collect(),
        }
    }

    pub fn leg(&self, leg_id: i64) -> Option<&LegState> {
        self.legs.get(&leg_id.to_string())
    }

    pub fn leg_mut(&mut self, leg_id: i64) -> Option<&mut LegState> {
        self.legs.get_mut(&leg_id.to_string())
    }

    pub fn open_legs(&self) -> Vec<&LegState> {
        self.legs.values().filter(|l| l.status == "open").collect()
    }

    /// Whether any actual or still-working position keeps this run managed.
    pub fn requires_management(&self) -> bool {
        !self.signal_entry_claims.is_empty()
            || self.legs.values().any(LegState::requires_management)
    }

    /// Leg ids whose live or superseded position still keeps the run active.
    pub fn managed_leg_ids(&self) -> Vec<i64> {
        self.legs
            .values()
            .filter(|l| l.requires_management())
            .map(|l| l.leg_id)
            .collect()
    }

    /// Every `(symbol, exchange)` the run needs ticks for.
    pub fn subscribed_symbols(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .legs
            .values()
            .filter(|l| l.status == "configured" || l.status == "open" || l.superseded.is_some())
            .map(|l| (l.symbol.clone(), l.exchange.clone()))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The state reduced to what a checkpoint row stores.
    pub fn snapshot_for_checkpoint(&self) -> Value {
        serde_json::json!({
            "pnl_realized": self.pnl_realized,
            "pnl_unrealized": self.pnl_unrealized,
            "pnl_total": self.pnl_total,
            "pnl_peak": self.pnl_peak,
            "pnl_trough": self.pnl_trough,
            "lock_floor": self.lock_floor,
            "trail_to_entry_active": self.trail_to_entry_active,
            "leg_state": serde_json::to_value(&self.legs).unwrap_or(Value::Null),
        })
    }
}

/// How far a leg has moved in its favour, in points, for display.
pub fn favorable_peak_points(leg: &LegState) -> f64 {
    if leg.entry_avg <= 0.0 {
        return 0.0;
    }
    if leg.position == "B" {
        leg.highest_price
            .map(|p| (p - leg.entry_avg).max(0.0))
            .unwrap_or(0.0)
    } else {
        leg.lowest_price
            .map(|p| (leg.entry_avg - p).max(0.0))
            .unwrap_or(0.0)
    }
}

/// A claim on one leg's live position, to dispatch an exit from outside the
/// lock.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitClaim {
    pub leg: LegState,
    pub claim_token: String,
}

/// A claim on a flip's outgoing position.
#[derive(Debug, Clone, PartialEq)]
pub struct SupersededClaim {
    pub leg_id: i64,
    pub position: String,
    pub position_ref: Option<String>,
    pub entry_order_id: Option<i64>,
    pub claim_token: String,
    pub symbol: String,
    pub exchange: String,
    pub quantity: i64,
    pub entry_avg: f64,
}

/// The registry of live run states. Bounded by the number of open runs: an
/// entry and its lock go together when the run ends.
#[derive(Default)]
pub struct StateRegistry {
    runs: Mutex<HashMap<i64, Arc<Mutex<RunState>>>>,
}

impl StateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, run_id: i64) -> Option<Arc<Mutex<RunState>>> {
        self.runs.lock().get(&run_id).cloned()
    }

    /// Run `f` under the run's lock. `None` when the run has no state, which
    /// is the normal answer for a run that has already stopped.
    pub fn with_run<R>(&self, run_id: i64, f: impl FnOnce(&mut RunState) -> R) -> Option<R> {
        let cell = self.get(run_id)?;
        let mut guard = cell.lock();
        Some(f(&mut guard))
    }

    /// Install a whole state (a fresh run, or recovery's rebuild).
    pub fn install(&self, state: RunState) {
        let run_id = state.run_id;
        self.runs.lock().insert(run_id, Arc::new(Mutex::new(state)));
    }

    /// Recovery's install, refused when live state already exists so a
    /// second recovery cannot overwrite a run that is trading.
    pub fn hydrate_if_absent(&self, state: RunState) -> bool {
        let mut runs = self.runs.lock();
        if runs.contains_key(&state.run_id) {
            return false;
        }
        runs.insert(state.run_id, Arc::new(Mutex::new(state)));
        true
    }

    /// A copy of the run's state, safe to read outside the lock.
    pub fn snapshot(&self, run_id: i64) -> Option<RunState> {
        self.with_run(run_id, |s| s.clone())
    }

    /// Drop a finished run's state and its lock, together.
    pub fn clear(&self, run_id: i64) {
        self.runs.lock().remove(&run_id);
    }

    pub fn active_run_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.runs.lock().keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn len(&self) -> usize {
        self.runs.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Claim one leg for exit, or `None` if it must not be exited (again).
    ///
    /// The check and the claim marker (`exit_kind` plus a unique token) are
    /// written in ONE hold, before any dispatch: two rules firing on one leg
    /// cannot both get through, and a refused dispatch releases exactly this
    /// claim. A leg whose entry was accepted but has not filled is refused:
    /// there is no confirmed quantity to close.
    pub fn claim_leg_exit(&self, run_id: i64, leg_id: i64, kind: &str) -> Option<ExitClaim> {
        self.with_run(run_id, |run| {
            let leg = run.leg_mut(leg_id)?;
            if leg.status != "open" || leg.entry_status != "complete" || leg.exit_in_flight() {
                return None;
            }
            let token = new_position_ref();
            leg.exit_kind = Some(kind.to_string());
            leg.exit_claim_token = Some(token.clone());
            Some(ExitClaim {
                leg: leg.clone(),
                claim_token: token,
            })
        })
        .flatten()
    }

    /// Claim every exitable leg and name the unfilled ones, in one hold.
    pub fn claim_legs_for_exit(
        &self,
        run_id: i64,
        leg_ids: &[i64],
        kind: &str,
    ) -> (Vec<ExitClaim>, Vec<LegState>) {
        self.with_run(run_id, |run| {
            let mut claimed = Vec::new();
            let mut unfilled = Vec::new();
            for leg_id in leg_ids {
                let Some(leg) = run.leg_mut(*leg_id) else {
                    continue;
                };
                if leg.status != "open" || leg.exit_in_flight() {
                    continue;
                }
                if leg.entry_status != "complete" {
                    unfilled.push(leg.clone());
                    continue;
                }
                let token = new_position_ref();
                leg.exit_kind = Some(kind.to_string());
                leg.exit_claim_token = Some(token.clone());
                claimed.push(ExitClaim {
                    leg: leg.clone(),
                    claim_token: token,
                });
            }
            (claimed, unfilled)
        })
        .unwrap_or_default()
    }

    /// Bind a durable order row to the exact live-exit claim before dispatch.
    pub fn bind_live_exit(
        &self,
        run_id: i64,
        leg_id: i64,
        claim_token: &str,
        order_row_id: i64,
        position_ref: Option<&str>,
    ) -> bool {
        self.with_run(run_id, |run| {
            let Some(leg) = run.leg_mut(leg_id) else {
                return false;
            };
            if leg.exit_claim_token.as_deref() != Some(claim_token)
                || leg.exit_order_id.is_some()
                || (position_ref.is_some() && leg.position_ref.as_deref() != position_ref)
            {
                return false;
            }
            leg.exit_order_id = Some(order_row_id);
            true
        })
        .unwrap_or(false)
    }

    /// Undo only the exact live-exit claim whose order was refused (matched
    /// by its claim token or its bound order row).
    pub fn release_leg_exit(&self, run_id: i64, leg_id: i64, claim: &ClaimId) -> bool {
        self.with_run(run_id, |run| {
            let Some(leg) = run.leg_mut(leg_id) else {
                return false;
            };
            if !claim.matches(leg.exit_claim_token.as_deref(), leg.exit_order_id) {
                return false;
            }
            leg.exit_kind = None;
            leg.exit_claim_token = None;
            leg.exit_order_id = None;
            true
        })
        .unwrap_or(false)
    }

    /// Release the exact live or superseded owner of one terminal order row.
    pub fn release_order_exit(
        &self,
        run_id: i64,
        leg_id: i64,
        order_row_id: i64,
        position_ref: Option<&str>,
    ) -> Option<&'static str> {
        self.with_run(run_id, |run| {
            let leg = run.leg_mut(leg_id)?;
            if let Some(sup) = leg.superseded.as_mut() {
                if sup.exit_order_id == Some(order_row_id)
                    && (position_ref.is_none() || sup.position_ref.as_deref() == position_ref)
                {
                    sup.exit_order_id = None;
                    sup.exit_claim_token = None;
                    sup.exit_kind = None;
                    return Some("superseded");
                }
            }
            if leg.exit_order_id == Some(order_row_id)
                && (position_ref.is_none() || leg.position_ref.as_deref() == position_ref)
            {
                leg.exit_order_id = None;
                leg.exit_claim_token = None;
                leg.exit_kind = None;
                return Some("live");
            }
            None
        })
        .flatten()
    }

    /// Claim a flip's outgoing position for a fresh exit, if it is still
    /// held on `position` and no exit for it is in flight.
    pub fn claim_superseded_exit(
        &self,
        run_id: i64,
        leg_id: i64,
        position: &str,
    ) -> Option<SupersededClaim> {
        self.with_run(run_id, |run| {
            let leg = run.leg_mut(leg_id)?;
            let (symbol, exchange, lid) = (leg.symbol.clone(), leg.exchange.clone(), leg.leg_id);
            let sup = leg.superseded.as_mut()?;
            if sup.exit_claim_token.is_some() || sup.exit_order_id.is_some() {
                return None;
            }
            if !sup.position.eq_ignore_ascii_case(position) {
                return None;
            }
            let token = new_position_ref();
            sup.exit_claim_token = Some(token.clone());
            Some(SupersededClaim {
                leg_id: lid,
                position: sup.position.clone(),
                position_ref: sup.position_ref.clone(),
                entry_order_id: sup.entry_order_id,
                claim_token: token,
                symbol,
                exchange,
                quantity: sup.qty,
                entry_avg: sup.entry_avg,
            })
        })
        .flatten()
    }

    pub fn bind_superseded_exit(
        &self,
        run_id: i64,
        leg_id: i64,
        claim_token: &str,
        order_row_id: i64,
    ) -> bool {
        self.with_run(run_id, |run| {
            let Some(sup) = run.leg_mut(leg_id).and_then(|l| l.superseded.as_mut()) else {
                return false;
            };
            if sup.exit_claim_token.as_deref() != Some(claim_token) {
                return false;
            }
            sup.exit_order_id = Some(order_row_id);
            true
        })
        .unwrap_or(false)
    }

    /// Mark a flip's outgoing exit as no longer in flight. Says whether it
    /// matched.
    pub fn release_superseded_exit(&self, run_id: i64, leg_id: i64, claim: &ClaimId) -> bool {
        self.with_run(run_id, |run| {
            let Some(sup) = run.leg_mut(leg_id).and_then(|l| l.superseded.as_mut()) else {
                return false;
            };
            if !claim.matches(sup.exit_claim_token.as_deref(), sup.exit_order_id) {
                return false;
            }
            sup.exit_order_id = None;
            sup.exit_claim_token = None;
            sup.exit_kind = None;
            true
        })
        .unwrap_or(false)
    }

    /// Block new signal entries for a durably requested stop.
    pub fn mark_stopping(&self, run_id: i64) -> bool {
        self.with_run(run_id, |run| {
            run.stopping = true;
        })
        .is_some()
    }

    /// Claim one bounded signal-entry decision before any external I/O.
    pub fn claim_signal_entry(
        &self,
        run_id: i64,
        leg_id: i64,
        position: &str,
    ) -> Option<EntryDecision> {
        self.with_run(run_id, |run| {
            if run.stopping {
                return EntryDecision::Note("run_stopping");
            }
            let key = leg_id.to_string();
            if run.signal_entry_claims.contains_key(&key) {
                return EntryDecision::Note("flip_pending");
            }
            let requested = position.trim().to_ascii_uppercase();
            let leg = run.legs.get(&key);
            let live_position = leg
                .filter(|l| l.status == "open")
                .map(|l| l.position.clone());
            let superseded = leg.and_then(|l| l.superseded.clone());
            let already = if requested == "B" {
                "already_long"
            } else {
                "already_short"
            };
            if live_position.as_deref() == Some(requested.as_str()) {
                return EntryDecision::Note(already);
            }
            if live_position.is_some() && superseded.is_some() {
                return EntryDecision::Note("flip_pending");
            }
            if let Some(sup) = &superseded {
                if sup.position == requested {
                    return EntryDecision::Note(already);
                }
                return EntryDecision::Note("flip_pending");
            }
            if live_position.is_some() && leg.map(LegState::exit_in_flight).unwrap_or(false) {
                return EntryDecision::Note("flip_pending");
            }
            let claim = EntryClaim {
                claim_token: new_position_ref(),
                position_ref: new_position_ref(),
                position: requested,
                held_position: live_position,
                expected_position_ref: leg.and_then(|l| l.position_ref.clone()),
            };
            run.signal_entry_claims.insert(key, claim.clone());
            EntryDecision::Claimed(claim)
        })
    }

    /// Release only the signal-entry decision carrying this token.
    pub fn release_signal_entry_claim(&self, run_id: i64, leg_id: i64, claim_token: &str) -> bool {
        self.with_run(run_id, |run| {
            let key = leg_id.to_string();
            match run.signal_entry_claims.get(&key) {
                Some(c) if c.claim_token == claim_token => {
                    run.signal_entry_claims.remove(&key);
                    true
                }
                _ => false,
            }
        })
        .unwrap_or(false)
    }

    /// Install a claimed signal leg only over its exact expected owner.
    ///
    /// A flip squares the held side and opens the other at once, so until the
    /// closing order fills one leg id names two positions: the outgoing one is
    /// kept under `superseded` and settles from its own entry and size. The
    /// leg's realized P&L is carried forward across incarnations.
    pub fn add_leg(
        &self,
        run_id: i64,
        spec: &LegSpec,
        claim_token: &str,
        expected_position_ref: Option<&str>,
        entry_order_id: i64,
    ) -> Option<LegState> {
        self.with_run(run_id, |run| {
            if run.stopping {
                return None;
            }
            let key = spec.leg_id.to_string();
            let claim = run.signal_entry_claims.get(&key)?.clone();
            if claim.claim_token != claim_token
                || Some(claim.position_ref.as_str()) != spec.position_ref.as_deref()
                || claim.expected_position_ref.as_deref() != expected_position_ref
            {
                return None;
            }
            let previous = run.legs.get(&key).cloned();
            let current_ref = previous.as_ref().and_then(|p| p.position_ref.clone());
            if current_ref.as_deref() != expected_position_ref {
                return None;
            }
            if previous
                .as_ref()
                .map(|p| p.superseded.is_some())
                .unwrap_or(false)
            {
                return None;
            }
            let mut leg = new_leg_state(spec).ok()?;
            leg.entry_order_id = Some(entry_order_id);
            if let (Some(_), Some(prev)) = (&claim.held_position, &previous) {
                if prev.status == "open"
                    && (prev.exit_kind.is_none() || prev.exit_claim_token.is_none())
                {
                    return None;
                }
                if prev.status == "open" {
                    leg.superseded = Some(Superseded {
                        exit_order_id: prev.exit_order_id,
                        exit_claim_token: prev.exit_claim_token.clone(),
                        exit_kind: prev.exit_kind.clone(),
                        entry_order_id: prev.entry_order_id,
                        position_ref: prev.position_ref.clone(),
                        position: prev.position.clone(),
                        entry_avg: prev.entry_avg,
                        qty: prev.qty,
                    });
                }
            }
            if let Some(prev) = &previous {
                leg.realized_pnl = prev.realized_pnl;
            }
            run.legs.insert(key, leg.clone());
            Some(leg)
        })
        .flatten()
    }

    /// Apply one entry acknowledgement only to its installed incarnation and
    /// release its claim.
    pub fn finish_signal_entry(
        &self,
        run_id: i64,
        leg_id: i64,
        position_ref: &str,
        claim_token: &str,
        accepted: bool,
    ) -> bool {
        self.with_run(run_id, |run| {
            let key = leg_id.to_string();
            let claim_ok = run
                .signal_entry_claims
                .get(&key)
                .map(|c| c.claim_token == claim_token)
                .unwrap_or(false);
            let Some(leg) = run.legs.get_mut(&key) else {
                return false;
            };
            if leg.position_ref.as_deref() != Some(position_ref) || !claim_ok {
                return false;
            }
            if leg.entry_status == "pending" {
                let s = if accepted { "open" } else { "rejected" };
                leg.entry_status = s.into();
                leg.status = s.into();
            }
            run.signal_entry_claims.remove(&key);
            true
        })
        .unwrap_or(false)
    }

    /// Make one exact batch placeholder non-managed when its intent could not
    /// be persisted (the broker was never called).
    pub fn reject_entry_intent(
        &self,
        run_id: i64,
        leg_id: i64,
        position_ref: Option<&str>,
    ) -> bool {
        self.with_run(run_id, |run| {
            let Some(leg) = run.leg_mut(leg_id) else {
                return false;
            };
            if leg.position_ref.as_deref() != position_ref || leg.entry_status == "complete" {
                return false;
            }
            leg.entry_order_id = None;
            leg.entry_status = "rejected".into();
            leg.status = "rejected".into();
            true
        })
        .unwrap_or(false)
    }
}

/// What identifies an exit claim: the token taken before dispatch, or the
/// durable order row it was bound to.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimId {
    Token(String),
    Row(i64),
}

impl ClaimId {
    fn matches(&self, token: Option<&str>, row: Option<i64>) -> bool {
        match self {
            ClaimId::Token(t) => token == Some(t.as_str()),
            ClaimId::Row(r) => row == Some(*r),
        }
    }
}
