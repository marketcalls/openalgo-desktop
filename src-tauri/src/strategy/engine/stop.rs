//! Stopping a run and closing a leg (web `engine.py` `stop_run`,
//! `close_leg`): request the exits, then finalise once fills confirm the
//! run is flat.

use super::{ev, StopOutcome, StrategyModule, STATE_UNAVAILABLE};
use crate::strategy::state::ExitScope;
use crate::strategy::store::{RunRow, StrategyRow, ORDER_KINDS};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};

/// The kind a stop's exit orders are recorded as: `exit_<reason>` when it
/// is a known kind, else `exit_close_all` (and always for a manual stop).
fn stop_exit_kind(reason: &str) -> String {
    if reason == "manual" {
        "exit_close_all".to_string()
    } else {
        let k = format!("exit_{}", reason);
        if ORDER_KINDS.contains(&k.as_str()) {
            k
        } else {
            "exit_close_all".to_string()
        }
    }
}

impl StrategyModule {
    /// Request a stop, exit every owned position, and finalise only once
    /// flat. Boxed: a synchronous fill inside it can reach it again.
    pub fn stop_run<'a>(
        &'a self,
        run_id: i64,
        user_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, StopOutcome> {
        Box::pin(async move { self.stop_run_inner(run_id, user_id, reason).await })
    }

    async fn stop_run_inner(&self, run_id: i64, user_id: &str, reason: &str) -> StopOutcome {
        let (run_row, strategy) = match self.active_run(run_id, user_id) {
            Ok(r) => r,
            Err(refused) => return refused,
        };
        let strategy_id = run_row.strategy_id;
        let mode = self.mode_of(&run_row.mode);

        // Durable before every broker call.
        let reason = match self
            .record_stop_request(run_id, strategy_id, user_id, reason)
            .await
        {
            Ok(r) => r,
            Err(refused) => return refused,
        };

        if self.reconcile_acks(run_id, true).await > 0 {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                "Stop remains pending because one or more broker acknowledgements could not be linked to their order rows. Nothing was closed on a guess; reconciliation will retry.",
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(
                "Some broker acknowledgements could not be linked to their orders; the run remains managed",
                vec![],
            );
        }

        let Some(snapshot) = self.state.snapshot(run_id) else {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                "Stop remains pending because the run's live state is unavailable; nothing was assumed closed",
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(STATE_UNAVAILABLE, vec![]);
        };
        let still_held = snapshot.requires_management();

        if let Err(e) = self.authorised_for(run_id, mode) {
            return self
                .stop_without_session(run_id, (strategy_id, user_id), &reason, still_held, e)
                .await;
        }
        self.note_actionable_again(strategy_id, user_id, run_id)
            .await;

        // A working entry is possible future exposure, not a position to
        // reverse: cancel, then fold a broker fact.
        self.reconcile_working_exits(run_id, mode).await;
        self.cancel_and_reconcile_working_entries(run_id, mode)
            .await;

        if let Ok(Some(r)) = self.store.get_run(run_id) {
            if r.stopped_at.is_some() {
                return StopOutcome {
                    ok: true,
                    ..Default::default()
                };
            }
        }
        let Some(snapshot) = self.state.snapshot(run_id) else {
            return StopOutcome::pending(STATE_UNAVAILABLE, vec![]);
        };
        let managed = snapshot.managed_leg_ids();
        let kind = stop_exit_kind(&reason);
        let exits = self
            .exit_legs(
                run_id,
                &strategy,
                (&managed, ExitScope::Held),
                &kind,
                mode,
                user_id,
            )
            .await;

        self.stop_after_exits(run_id, (strategy_id, user_id), &reason, exits)
            .await
    }

    /// The open run and its strategy, which a stop or a close acts on. A
    /// run that has ended answers "not active" and a strategy that is not
    /// the user's "not found"; a read that failed is an error (SM-04), so a
    /// stop is never told that a run it could not read is not running.
    fn active_run(&self, run_id: i64, user_id: &str) -> Result<(RunRow, StrategyRow), StopOutcome> {
        let run = match self.store.get_run(run_id) {
            Ok(Some(r)) if r.stopped_at.is_none() => r,
            Ok(_) => return Err(StopOutcome::err("Run is not active")),
            Err(e) => return Err(StopOutcome::read_failed(format_args!("run {}", run_id), e)),
        };
        match self.store.get_strategy(run.strategy_id, user_id) {
            Ok(Some(strategy)) => Ok((run, strategy)),
            Ok(None) => Err(StopOutcome::err("Strategy not found")),
            Err(e) => Err(StopOutcome::read_failed(
                format_args!("strategy {} of run {}", run.strategy_id, run_id),
                e,
            )),
        }
    }

    /// Records the stop request durably (the stored reason wins), marks the
    /// run stopping and announces it. A request that cannot be saved
    /// places no exit (`Err`: the stop's answer).
    async fn record_stop_request(
        &self,
        run_id: i64,
        strategy_id: i64,
        user_id: &str,
        reason: &str,
    ) -> Result<String, StopOutcome> {
        if !self.store.request_run_stop(run_id, reason).unwrap_or(false) {
            return Err(StopOutcome {
                ok: false,
                stop_pending: false,
                error: Some(
                    "Could not save the stop request, so no exit order was placed. Try again."
                        .into(),
                ),
                exits: vec![],
                ..Default::default()
            });
        }
        let reason = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.stop_requested_reason)
            .unwrap_or_else(|| reason.to_string());
        self.state.mark_stopping(run_id);
        self.emit(
            strategy_id,
            user_id,
            "run_stop_requested",
            &format!(
                "Stop requested ({}); exit orders are being attempted",
                reason
            ),
            ev(Some(run_id), None, "info"),
        )
        .await;
        Ok(reason)
    }

    /// No broker session, or not the one the run was opened on (LOG-07): a
    /// run with nothing held is finished; otherwise the stop stays pending,
    /// the positions stay managed, and the trader is told once.
    async fn stop_without_session(
        &self,
        run_id: i64,
        (strategy_id, user_id): (i64, &str),
        reason: &str,
        still_held: bool,
        e: String,
    ) -> StopOutcome {
        if !still_held {
            return self
                .finish_stop(run_id, strategy_id, user_id, reason, vec![])
                .await;
        }
        if self.claim_unactionable(run_id) {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                &format!(
                    "Stop remains pending: {} Positions remain open and managed, and the stop is retried once that session is back.",
                    e
                ),
                ev(Some(run_id), None, "critical"),
            )
            .await;
        }
        StopOutcome::pending(format!("{} The run remains managed.", e), vec![])
    }

    /// What the stop's exits found: a run already finished by a fill, exits
    /// refused while positions are held (the run stays open and managed),
    /// exits still working (pending), or flat (finished).
    async fn stop_after_exits(
        &self,
        run_id: i64,
        (strategy_id, user_id): (i64, &str),
        reason: &str,
        exits: Vec<Value>,
    ) -> StopOutcome {
        if let Ok(Some(r)) = self.store.get_run(run_id) {
            if r.stopped_at.is_some() {
                return StopOutcome {
                    ok: true,
                    exits,
                    ..Default::default()
                };
            }
        }
        let Some(snapshot) = self.state.snapshot(run_id) else {
            self.emit(
            strategy_id,
            user_id,
            "run_stop_failed",
            "Stop remains pending because the run's live state is unavailable; nothing was assumed closed",
            ev(Some(run_id), None, "critical"),
        )
        .await;
            return StopOutcome::pending(STATE_UNAVAILABLE, exits);
        };
        let still_held = snapshot.requires_management();
        // An unconfirmed exit is not a refusal: it may be working at the
        // broker, and the stop waits for it like any placed exit.
        let refused = exits
            .iter()
            .filter(|e| e["ok"] != json!(true) && e["unconfirmed"] != json!(true))
            .count();
        if refused > 0 && still_held {
            // The positions are still at the broker: finalising would release
            // the strategy, drop the state and unsubscribe the prices.
            self.emit(
            strategy_id,
            user_id,
            "run_stop_failed",
            &format!(
                "Stop refused for {} position(s); the run remains open, managed, and can be stopped again",
                refused
            ),
            ev(Some(run_id), None, "critical"),
        )
        .await;
            return StopOutcome::pending(
            format!(
                "{} of {} exit order(s) were refused. The run is still open and still managed; retry the stop.",
                refused,
                exits.len()
            ),
            exits,
        );
        }
        if still_held {
            return StopOutcome {
                ok: true,
                stop_pending: true,
                exits,
                ..Default::default()
            };
        }
        self.finish_stop(run_id, strategy_id, user_id, reason, exits)
            .await
    }

    async fn finish_stop(
        &self,
        run_id: i64,
        strategy_id: i64,
        user_id: &str,
        reason: &str,
        exits: Vec<Value>,
    ) -> StopOutcome {
        let persisted = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.stop_requested_reason)
            .unwrap_or_else(|| reason.to_string());
        if self
            .finalise(
                run_id,
                strategy_id,
                user_id,
                &persisted,
                &format!("Run stopped ({})", persisted),
            )
            .await
        {
            StopOutcome {
                ok: true,
                exits,
                ..Default::default()
            }
        } else {
            StopOutcome::pending(
                "The run is flat but its final stop could not be saved; retry the stop",
                exits,
            )
        }
    }

    /// Continue a durable stop (after an entry fill, or on the scheduler's
    /// periodic retry). `None` when no stop is pending.
    pub async fn reconcile_pending_stop(&self, run_id: i64) -> Option<StopOutcome> {
        // Read on the blocking pool: asked after every entry fill and
        // settled order, from the order reconciler's pass (PROD-REL-02).
        let (run, strategy) = self
            .store
            .blocking(move |s| {
                let run = s.get_run(run_id)?;
                let strategy = match &run {
                    Some(r) if r.stopped_at.is_none() && r.stop_requested_reason.is_some() => {
                        s.get_strategy_unscoped(r.strategy_id).ok().flatten()
                    }
                    _ => None,
                };
                Ok((run, strategy))
            })
            .await
            .ok()?;
        let run = run?;
        if run.stopped_at.is_some() {
            return None;
        }
        let reason = run.stop_requested_reason.clone()?;
        let Some(strategy) = strategy else {
            return Some(StopOutcome::pending(
                "The strategy owning this pending stop is unavailable",
                vec![],
            ));
        };
        Some(self.stop_run(run_id, &strategy.user_id, &reason).await)
    }

    /// Exit one leg; the run continues with the rest. Never triggers
    /// trail-to-entry: an operator's close is an override, not a signal.
    pub async fn close_leg(&self, run_id: i64, leg_id: i64, user_id: &str) -> StopOutcome {
        let (run_row, strategy) = match self.active_run(run_id, user_id) {
            Ok(r) => r,
            Err(refused) => return refused,
        };
        let strategy_id = run_row.strategy_id;
        let mode = self.mode_of(&run_row.mode);
        if let Err(e) = self.authorised_for(run_id, mode) {
            return StopOutcome::err(e);
        }
        let exits = self
            .exit_legs(
                run_id,
                &strategy,
                (&[leg_id], ExitScope::Held),
                "exit_leg_manual",
                mode,
                user_id,
            )
            .await;
        if exits.is_empty() {
            return StopOutcome::err("That leg is not open");
        }
        if exits.iter().any(|e| e["unconfirmed"] == json!(true)) {
            return StopOutcome {
                ok: false,
                stop_pending: false,
                error: Some(
                    "The exit was sent, but the broker did not confirm it. It may have been placed, so it is not sent again; check the order book. The leg stays managed until the broker's order book settles it."
                        .into(),
                ),
                exits,
                ..Default::default()
            };
        }
        // Non-empty is not success: a refused exit is still a held position.
        if exits.iter().any(|e| e["ok"] != json!(true)) {
            let errors = exits
                .iter()
                .filter(|e| e["ok"] != json!(true))
                .map(|e| e["error"].as_str().unwrap_or("refused").to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return StopOutcome {
                ok: false,
                stop_pending: false,
                error: Some(format!("Exit refused: {}", errors)),
                exits,
                ..Default::default()
            };
        }
        self.emit(
            strategy_id,
            user_id,
            "leg_close_manual",
            &format!("Operator requested closure of leg {}", leg_id),
            ev(Some(run_id), Some(leg_id), "info"),
        )
        .await;
        let still_open = self
            .state
            .with_run(run_id, |r| !r.open_legs().is_empty())
            .unwrap_or(false);
        let mut stopped = false;
        if !still_open {
            stopped = self
                .finalise(run_id, strategy_id, user_id, "manual", "All legs closed")
                .await;
        }
        let run_closed = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .map(|r| r.stopped_at.is_some())
            .unwrap_or(false);
        StopOutcome {
            ok: true,
            exits,
            run_stopped: Some(stopped || run_closed),
            ..Default::default()
        }
    }

    /// Close the run row, release the strategy and drop the live state, once,
    /// and only when nothing is managed any more. Peak and trough are written
    /// on every path.
    pub async fn finalise(
        &self,
        run_id: i64,
        strategy_id: i64,
        user_id: &str,
        reason: &str,
        message: &str,
    ) -> bool {
        let figures = match self.state.with_run(run_id, |live| {
            if live.requires_management() {
                None
            } else {
                Some((live.pnl_realized, live.pnl_peak, live.pnl_trough))
            }
        }) {
            Some(Some(f)) => f,
            Some(None) => return false,
            None => (0.0, 0.0, 0.0),
        };
        let mut finished = self
            .store
            .finish_run_and_release_strategy(
                run_id,
                strategy_id,
                reason,
                figures.0,
                figures.1,
                figures.2,
            )
            .unwrap_or(false);
        if !finished {
            finished = self
                .store
                .finish_detached_run(run_id, strategy_id, reason, figures.0, figures.1, figures.2)
                .unwrap_or(false);
        }
        if !finished {
            return false;
        }
        self.emit(
            strategy_id,
            user_id,
            "run_stopped",
            message,
            ev(Some(run_id), None, "info"),
        )
        .await;
        // The final figures, forced past the throttle.
        self.broadcast
            .push_delta(self.state.snapshot(run_id), true)
            .await;
        if let Ok(Some(run)) = self.store.get_run(run_id) {
            self.broadcast
                .push_run_update(strategy_id, run.to_dict())
                .await;
        }
        self.broadcast
            .push_terminal(strategy_id, run_id, reason, figures.0)
            .await;
        self.release_unactionable(run_id);
        self.feed.remove_run(run_id).await;
        self.state.clear(run_id);
        self.tick_contexts.forget(run_id);
        // Every stop arms the webhook cooling-off window.
        self.webhook
            .note_run_stopped(strategy_id, std::time::Instant::now());
        true
    }

    pub(super) async fn note_actionable_again(&self, strategy_id: i64, user_id: &str, run_id: i64) {
        if !self.release_unactionable(run_id) {
            return;
        }
        self.emit(
            strategy_id,
            user_id,
            "recovery_succeeded",
            "Broker session restored; this run can act on its risk rules again.",
            ev(Some(run_id), None, "warn"),
        )
        .await;
    }
}
