//! Exiting legs at market from the symbol each leg holds, and folding the
//! broker's facts for working entries and exits (web `engine.py`
//! `_exit_legs`).

use super::{ev, StrategyModule, UNFILLED_EXIT};
use crate::strategy::dispatch::{
    build_order, exit_action, DispatchResult, OrderPayload, RunMode, EXIT_PRICETYPE,
};
use crate::strategy::state::{ClaimId, ExitClaim, ExitScope, SupersededClaim};
use crate::strategy::store::{NewOrder, StrategyRow};
use serde_json::{json, Value};

/// One position an exit is placed for: a live leg, or a flip's outgoing
/// (superseded) position, with its claim token and its owner.
struct ExitTarget {
    lid: i64,
    position: String,
    symbol: String,
    exchange: String,
    quantity: i64,
    position_ref: Option<String>,
    token: String,
    owner: &'static str,
}

impl ExitTarget {
    fn live(c: ExitClaim) -> Self {
        ExitTarget {
            lid: c.leg.leg_id,
            position: c.leg.position,
            symbol: c.leg.symbol,
            exchange: c.leg.exchange,
            quantity: c.leg.qty,
            position_ref: c.leg.position_ref,
            token: c.claim_token,
            owner: "live",
        }
    }

    fn superseded(c: SupersededClaim) -> Self {
        ExitTarget {
            lid: c.leg_id,
            position: c.position,
            symbol: c.symbol,
            exchange: c.exchange,
            quantity: c.quantity,
            position_ref: c.position_ref,
            token: c.claim_token,
            owner: "superseded",
        }
    }
}

impl StrategyModule {
    /// Exit the named legs at market, from the symbol each leg HOLDS: the
    /// position `scope` names, and a flip's outgoing position (one the
    /// strategy already chose to close).
    pub(crate) async fn exit_legs(
        &self,
        run_id: i64,
        strategy: &StrategyRow,
        (leg_ids, scope): (&[i64], ExitScope<'_>),
        kind: &str,
        mode: RunMode,
        user_id: &str,
    ) -> Vec<Value> {
        // Claimed and classified in ONE hold of the run lock.
        let (live, unfilled) = self.state.claim_legs_for_exit(run_id, leg_ids, kind, scope);
        let targets = self.exit_targets(run_id, leg_ids, live);
        let mut outcomes = Vec::new();
        for t in &targets {
            outcomes.push(
                self.place_exit(run_id, strategy, t, (kind, mode), user_id)
                    .await,
            );
        }
        for leg in unfilled {
            outcomes.push(json!({
                "leg_id": leg.leg_id,
                "ok": false,
                "symbol": leg.symbol,
                "broker_order_id": null,
                "position_ref": leg.position_ref,
                "exit_owner": "live",
                "error": UNFILLED_EXIT,
            }));
        }
        outcomes
    }

    /// What the exit is placed for: the live legs claimed, then each named
    /// leg's flip-outgoing position, claimed for exit.
    fn exit_targets(&self, run_id: i64, leg_ids: &[i64], live: Vec<ExitClaim>) -> Vec<ExitTarget> {
        let mut targets: Vec<ExitTarget> = live.into_iter().map(ExitTarget::live).collect();
        for leg_id in leg_ids {
            let position = self
                .state
                .with_run(run_id, |r| {
                    r.leg(*leg_id)
                        .and_then(|l| l.superseded.as_ref().map(|s| s.position.clone()))
                })
                .flatten();
            if let Some(p) = position {
                if let Some(c) = self.state.claim_superseded_exit(run_id, *leg_id, &p) {
                    targets.push(ExitTarget::superseded(c));
                }
            }
        }
        targets
    }

    /// One exit: recorded and bound to its claim before it is sent, then
    /// sent, and the broker's answer recorded. Answers its outcome.
    async fn place_exit(
        &self,
        run_id: i64,
        strategy: &StrategyRow,
        t: &ExitTarget,
        (kind, mode): (&str, RunMode),
        user_id: &str,
    ) -> Value {
        let action = match exit_action(&t.position) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Run {} leg {}: {}", run_id, t.lid, e);
                return json!({"leg_id": t.lid, "ok": false, "error": e,
                    "position_ref": t.position_ref, "exit_owner": t.owner});
            }
        };
        let order = build_order(
            &t.symbol,
            &t.exchange,
            action,
            t.quantity,
            &strategy.product,
            &strategy.name,
            EXIT_PRICETYPE,
        );
        let row_id = match self
            .record_exit(run_id, strategy, t, (kind, &order, action), user_id)
            .await
        {
            Ok(id) => id,
            Err(outcome) => return outcome,
        };
        let claim = ClaimId::Row(row_id);

        let result = self.place_for_run(run_id, mode, &order).await;
        self.record_acknowledgement(row_id, &result, strategy.id, user_id, run_id, t.lid)
            .await;
        if result.uncertain {
            // The exit may be at the broker: the claim stays with its row
            // so no tick, stop retry or close sends a second one. Only the
            // order reconciler releases it (LOG-08).
            return json!({
                "leg_id": t.lid,
                "ok": false,
                "unconfirmed": true,
                "error": result.error,
                "position_ref": t.position_ref,
                "exit_owner": t.owner,
            });
        }
        if !result.ok {
            // Release the claim so a later attempt is not mistaken for a
            // duplicate and the leg skipped for the rest of the session.
            self.release_refused_exit(run_id, t, &claim, result.broker_order_id.as_deref())
                .await;
        }
        self.emit_exit_answer((strategy, user_id), run_id, t, (action, kind), &result)
            .await;
        if result.ok {
            self.replay_for(result.broker_order_id.as_deref()).await;
        }
        json!({
            "leg_id": t.lid,
            "ok": result.ok,
            "error": result.error,
            "position_ref": t.position_ref,
            "exit_owner": t.owner,
        })
    }

    /// Records the exit's order and binds it to the target's claim before it
    /// is sent. An exit with no row is not sent (P2-6): it could not be
    /// found again if its answer were lost, so it could be sent twice; the
    /// claim is released and the leg stays managed, so the next breach,
    /// stop retry or close tries again. A claim that changed before
    /// dispatch rejects the row. `Err` is the exit's outcome.
    async fn record_exit(
        &self,
        run_id: i64,
        strategy: &StrategyRow,
        t: &ExitTarget,
        (kind, order, action): (&str, &OrderPayload, &str),
        user_id: &str,
    ) -> Result<i64, Value> {
        let lid = t.lid;
        let row = self.store.record_order(
            run_id,
            lid,
            kind,
            &NewOrder {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                action: action.into(),
                qty: t.quantity,
                product: Some(order.product.clone()),
                pricetype: EXIT_PRICETYPE.into(),
                status: "pending".into(),
                position_ref: t.position_ref.clone(),
                broker_order_id: None,
            },
        );
        let claim = ClaimId::Token(t.token.clone());
        let row_id = match row {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Exit row for run {} leg {} not written: {}", run_id, lid, e);
                self.release_exit_claim(run_id, t, &claim);
                self.emit(
                    strategy.id,
                    user_id,
                    "leg_exit_rejected",
                    &format!(
                        "Exit for leg {} was not sent: its order could not be recorded. The leg stays managed and the exit is tried again; check the disk and the app's data folder.",
                        lid
                    ),
                    ev(Some(run_id), Some(lid), "critical"),
                )
                .await;
                return Err(json!({"leg_id": lid, "ok": false,
                    "position_ref": t.position_ref, "exit_owner": t.owner,
                    "error": "The exit could not be recorded, so it was not sent"}));
            }
        };
        let bound = if t.owner == "superseded" {
            self.state
                .bind_superseded_exit(run_id, lid, &t.token, row_id)
        } else {
            self.state
                .bind_live_exit(run_id, lid, &t.token, row_id, t.position_ref.as_deref())
        };
        if !bound {
            let reason = if t.owner == "superseded" {
                "Outgoing position exit claim changed before dispatch"
            } else {
                "Live position exit claim changed before dispatch"
            };
            let _ = self
                .store
                .update_order(row_id, Some("rejected"), None, Some(reason));
            self.release_exit_claim(run_id, t, &claim);
            self.emit(
                strategy.id,
                user_id,
                "leg_exit_rejected",
                &format!(
                    "{} exit claim changed on leg {} before dispatch",
                    if t.owner == "superseded" {
                        "Superseded"
                    } else {
                        "Live"
                    },
                    lid
                ),
                ev(Some(run_id), Some(lid), "critical"),
            )
            .await;
            return Err(json!({"leg_id": lid, "ok": false,
                "position_ref": t.position_ref, "exit_owner": t.owner,
                "error": format!("The {} position changed before its exit could be placed", t.owner)}));
        }
        Ok(row_id)
    }

    /// Releases the target's exit claim: the flip-outgoing claim or the
    /// leg's.
    fn release_exit_claim(&self, run_id: i64, t: &ExitTarget, claim: &ClaimId) {
        if t.owner == "superseded" {
            self.state.release_superseded_exit(run_id, t.lid, claim);
        } else {
            self.state.release_leg_exit(run_id, t.lid, claim);
        }
    }

    /// A refused exit's claim goes back; a flip-outgoing exit refused is
    /// reported.
    async fn release_refused_exit(
        &self,
        run_id: i64,
        t: &ExitTarget,
        claim: &ClaimId,
        broker_order_id: Option<&str>,
    ) {
        if t.owner == "superseded" {
            if self.state.release_superseded_exit(run_id, t.lid, claim) {
                self.report_flip_outgoing_exit_rejected(run_id, t.lid, "refused", broker_order_id)
                    .await;
            }
        } else {
            self.state.release_leg_exit(run_id, t.lid, claim);
        }
    }

    /// The `leg_exit_placed` or `leg_exit_rejected` event for an answered
    /// exit.
    async fn emit_exit_answer(
        &self,
        (strategy, user_id): (&StrategyRow, &str),
        run_id: i64,
        t: &ExitTarget,
        (action, kind): (&str, &str),
        result: &DispatchResult,
    ) {
        self.emit(
            strategy.id,
            user_id,
            if result.ok {
                "leg_exit_placed"
            } else {
                "leg_exit_rejected"
            },
            &if result.ok {
                format!(
                    "Exit {} {} {} placed ({})",
                    action, t.quantity, t.symbol, kind
                )
            } else {
                format!(
                    "Exit rejected on leg {}: {}",
                    t.lid,
                    result.error.clone().unwrap_or_default()
                )
            },
            ev(
                Some(run_id),
                Some(t.lid),
                if result.ok { "info" } else { "critical" },
            ),
        )
        .await;
    }

    pub(crate) async fn report_flip_outgoing_exit_rejected(
        &self,
        run_id: i64,
        leg_id: i64,
        ended: &str,
        broker_order_id: Option<&str>,
    ) {
        let Ok(Some(run)) = self.store.get_run(run_id) else {
            return;
        };
        if run.stopped_at.is_some() {
            return;
        }
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
            return;
        };
        let order = broker_order_id
            .map(|b| format!(" order {}", b))
            .unwrap_or_default();
        self.emit(
            run.strategy_id,
            &strategy.user_id,
            "flip_outgoing_exit_rejected",
            &format!(
                "Outgoing exit{} for leg {} was {}. The old side is still held, remains managed, and can be closed again.",
                order, leg_id, ended
            ),
            ev(Some(run_id), Some(leg_id), "critical"),
        )
        .await;
    }

    /// Cancel each accepted working entry through the run's pipe, then fold
    /// one broker status fact. Cancellation is intent, never proof.
    pub(super) async fn cancel_and_reconcile_working_entries(&self, run_id: i64, mode: RunMode) {
        let working = [
            "pending",
            "open",
            "working",
            "trigger pending",
            "trigger_pending",
        ];
        let rows = self.store.list_orders(run_id).unwrap_or_default();
        for row in rows.iter().filter(|r| {
            r.kind == "entry" && working.contains(&r.status.trim().to_ascii_lowercase().as_str())
        }) {
            let Some(bid) = row.broker_order_id.clone().filter(|b| !b.is_empty()) else {
                continue;
            };
            let c = self.cancel_for_run(run_id, mode, &bid).await;
            if !c.ok {
                tracing::warn!(
                    "Working entry {} on run {} could not be cancelled: {}",
                    bid,
                    run_id,
                    c.error.unwrap_or_default()
                );
            }
            let status = self.order_status_for_run(run_id, mode, &bid).await;
            if status.ok {
                self.apply_order_snapshot(&bid, &status.order).await;
            }
        }
    }

    /// Fold one broker fact for each accepted exit still awaiting a frame.
    pub(super) async fn reconcile_working_exits(&self, run_id: i64, mode: RunMode) {
        let working = [
            "pending",
            "open",
            "working",
            "trigger pending",
            "trigger_pending",
        ];
        let rows = self.store.list_orders(run_id).unwrap_or_default();
        for row in rows.iter().filter(|r| {
            r.kind != "entry" && working.contains(&r.status.trim().to_ascii_lowercase().as_str())
        }) {
            let Some(bid) = row.broker_order_id.clone().filter(|b| !b.is_empty()) else {
                continue;
            };
            let status = self.order_status_for_run(run_id, mode, &bid).await;
            if status.ok {
                self.apply_order_snapshot(&bid, &status.order).await;
            }
        }
    }
}
