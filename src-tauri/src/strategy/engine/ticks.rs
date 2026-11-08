//! The tick decision path (web `engine.py` `process_tick`): decide every
//! run holding an instrument against one price, then act on what risk
//! decided.
//!
//! Deciding and acting are apart (PROD-REL-01). Whoever reads a price (the
//! tick reader, the REST poll) decides it at once, in the run's lock, so
//! every price is decided in the order it came; what the decision asks for
//! (events, exits, a stop, a page update) waits for the evaluator in
//! `tick_feed::pending`, so a slow exit never holds a decision back.

use super::{ev, exit_kind_for, stop_reason_for, StrategyModule};
use crate::risk::BreachReason;
use crate::strategy::dispatch::RunMode;
use crate::strategy::risk_adapter;
use crate::strategy::state::{ExitScope, RunState};
use crate::strategy::store::{EventFields, StrategyRow};
use crate::strategy::tick_feed::pending::{Decision, Event, LegExit, RunWork};
use serde_json::{json, Value};
use std::sync::Arc;

/// The breach events a tick writes again for as long as nothing can be
/// exited; not repeated once the run's refusal has been reported.
const REPEATING_BREACH_EVENTS: &[&str] = &[
    "leg_sl_hit",
    "leg_target_hit",
    "overall_sl_hit",
    "overall_target_hit",
    "lock_profit_triggered",
];

/// Each open leg on this instrument against the price (in the run's
/// lock): the legs to exit and the events to send. A stop-driven exit
/// trails the other open legs to entry when the strategy asks.
fn evaluate_legs(
    run: &mut RunState,
    run_id: i64,
    (symbol, exchange, ltp): (&str, &str, f64),
    trail_sl_to_entry: bool,
) -> (Vec<LegExit>, Vec<Event>) {
    let mut leg_exits: Vec<LegExit> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    let ids: Vec<i64> = run
        .legs
        .values()
        .filter(|l| l.status == "open" && l.symbol == symbol && l.exchange == exchange)
        .map(|l| l.leg_id)
        .collect();
    for id in ids {
        let Some(leg) = run.leg_mut(id) else { continue };
        let d = match risk_adapter::evaluate_leg(leg, ltp) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Run {} leg {} not evaluated: {}", run_id, id, e);
                continue;
            }
        };
        if d.trail_armed && d.stop_moved {
            events.push(Event::about(
                "leg_trail_advanced",
                format!(
                    "Trailing stop on leg {} moved to {}",
                    id,
                    d.stop_price
                        .map(crate::risk::format_price)
                        .unwrap_or_default()
                ),
                ev(Some(run_id), Some(id), "info"),
                leg.position_ref.clone(),
            ));
        }
        if let Some(reason) = d.reason {
            if let Some(kind) = exit_kind_for(reason) {
                leg_exits.push(LegExit {
                    leg_id: id,
                    kind,
                    position_ref: leg.position_ref.clone(),
                });
                events.push(Event::about(
                    if reason == BreachReason::Stop {
                        "leg_sl_hit"
                    } else {
                        "leg_target_hit"
                    },
                    d.detail.clone(),
                    ev(Some(run_id), Some(id), "warn"),
                    leg.position_ref.clone(),
                ));
            }
        }
    }
    if trail_sl_to_entry {
        if let Some(stopped) = leg_exits.iter().find(|x| x.kind == "exit_sl") {
            let moved = risk_adapter::trail_open_legs_to_entry(run, stopped.leg_id);
            if !moved.is_empty() {
                events.push(Event::about(
                    "trail_to_entry_activated",
                    format!(
                        "Stop on leg {} moved {} other legs to entry",
                        stopped.leg_id,
                        moved.len()
                    ),
                    ev(Some(run_id), None, "warn"),
                    stopped.position_ref.clone(),
                ));
            }
        }
    }
    (leg_exits, events)
}

/// The strategy-level breach this mark makes, if any (in the run's lock):
/// the lock profit's progress is announced, the daily loss limit is a
/// limit on the session (the realized P&L banked earlier today plus this
/// run), then the overall stop, target and lock profit, each with the
/// breach's payload.
fn run_breach(
    run: &mut RunState,
    (strategy, strategy_dict): (&StrategyRow, &Value),
    (limit, banked): (Option<f64>, Option<f64>),
    (run_id, symbol, exchange, ltp): (i64, &str, &str, f64),
    events: &mut Vec<Event>,
) -> Option<&'static str> {
    let aggregate = match risk_adapter::evaluate_run(run, strategy_dict) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Run {} not evaluated: {}", run_id, e);
            return None;
        }
    };
    if aggregate.lock_armed_now {
        events.push(Event::run(
            "lock_profit_armed",
            format!(
                "Lock profit now active with a floor of {}",
                aggregate
                    .lock_floor
                    .map(crate::risk::format_price)
                    .unwrap_or_default()
            ),
            ev(Some(run_id), None, "info"),
        ));
    } else if aggregate.lock_floor_raised {
        events.push(Event::run(
            "lock_profit_floor_advanced",
            format!(
                "Lock profit floor advanced to {}",
                aggregate
                    .lock_floor
                    .map(crate::risk::format_price)
                    .unwrap_or_default()
            ),
            ev(Some(run_id), None, "info"),
        ));
    }
    // The daily loss limit is a limit on the session, not the run.
    if let (Some(limit), Some(banked)) = (limit, banked) {
        let day = banked + run.pnl_total;
        if day <= -limit {
            events.push(Event::run(
                "overall_sl_hit",
                format!(
                    "Daily loss limit reached: the session is down {:.2} against a limit of {:.2}",
                    day.abs(),
                    limit
                ),
                ev(Some(run_id), None, "critical"),
            ));
            return Some("daily_loss_limit");
        }
    }
    if let Some(reason) = aggregate.reason.filter(|_| aggregate.breached) {
        if let Some(sr) = stop_reason_for(reason) {
            let threshold = match sr {
                "overall_sl" => -strategy.overall_sl_mtm.unwrap_or(0.0).abs(),
                "overall_target" => strategy.overall_target_mtm.unwrap_or(0.0),
                _ => aggregate.lock_floor.unwrap_or(0.0),
            };
            let legs: Vec<Value> = run
                .legs
                .values()
                .filter(|l| l.status == "open" || l.realized_pnl != 0.0)
                .map(|l| {
                    json!({
                        "symbol": l.symbol, "exchange": l.exchange, "ltp": l.ltp,
                        "mtm": (l.mtm * 100.0).round() / 100.0,
                        "tick_source": l.tick_source, "qty": l.qty, "position": l.position,
                    })
                })
                .collect();
            let kind = match sr {
                "overall_sl" => "overall_sl_hit",
                "overall_target" => "overall_target_hit",
                _ => "lock_profit_triggered",
            };
            events.push(Event::run(
                kind,
                aggregate.detail.clone(),
                EventFields {
                    run_id: Some(run_id),
                    leg_id: None,
                    severity: Some("warn"),
                    payload: Some(json!({
                        "trigger_total": (aggregate.total_pnl * 100.0).round() / 100.0,
                        "reason": sr,
                        "threshold": (threshold * 100.0).round() / 100.0,
                        "triggering_tick": {"symbol": symbol, "exchange": exchange, "ltp": ltp},
                        "legs": legs,
                    })),
                },
            ));
            return Some(sr);
        }
    }
    None
}

impl StrategyModule {
    /// Decide one price for every run holding the instrument, then act on
    /// every decision waiting: the REST poll's path (on the evaluator, so
    /// a run's tick inputs not yet in memory are read first), and a test's.
    pub async fn process_tick(self: &Arc<Self>, symbol: &str, exchange: &str, ltp: f64) {
        for run_id in self.runs_holding(symbol, exchange) {
            self.read_tick_inputs(run_id).await;
        }
        self.decide_tick(symbol, exchange, ltp);
        self.act_on_decided().await;
    }

    /// Decide one price for every run holding the instrument and leave
    /// what it asks for to the evaluator. The tick reader's path: from
    /// memory alone, never waiting (`strategy::tick_context`).
    pub(crate) fn decide_tick(self: &Arc<Self>, symbol: &str, exchange: &str, ltp: f64) {
        for run_id in self.runs_holding(symbol, exchange) {
            self.decide_for_run(run_id, symbol, exchange, ltp);
        }
    }

    /// The running runs with an open leg on the instrument.
    fn runs_holding(&self, symbol: &str, exchange: &str) -> Vec<i64> {
        let holds = |r: &mut RunState| {
            r.legs
                .values()
                .any(|l| l.status == "open" && l.symbol == symbol && l.exchange == exchange)
        };
        self.state
            .active_run_ids()
            .into_iter()
            .filter(|id| self.state.with_run(*id, holds).unwrap_or(false))
            .collect()
    }

    fn decide_for_run(self: &Arc<Self>, run_id: i64, symbol: &str, exchange: &str, ltp: f64) {
        let Some((ctx, (limit, banked))) = self.held_tick_inputs(run_id) else {
            return;
        };

        // In-memory arithmetic only.
        let decided = self.state.with_run(run_id, |run| {
            let (leg_exits, mut events) = evaluate_legs(
                run,
                run_id,
                (symbol, exchange, ltp),
                ctx.strategy.trail_sl_to_entry,
            );
            let stop_reason = run_breach(
                run,
                (&ctx.strategy, &ctx.strategy_dict),
                (limit, banked),
                (run_id, symbol, exchange, ltp),
                &mut events,
            );
            Decision::of_price(leg_exits, stop_reason, events)
        });
        if let Some(decision) = decided {
            self.feed
                .pending
                .leave(run_id, ctx, decision, self.clock.now());
        }
    }

    /// Act on every decision waiting, run by run in the order they
    /// decided, until none is left.
    pub(crate) async fn act_on_decided(&self) {
        loop {
            let waiting = self.feed.pending.take();
            if waiting.is_empty() {
                return;
            }
            for (run_id, work) in waiting {
                self.act_on_run(run_id, work).await;
                self.feed.pending.acted(run_id);
            }
        }
    }

    /// What was decided for one run, outside its lock (everything here
    /// reaches the database or broker): the page watching it updated, the
    /// events recorded, the leg exits decided before any stop placed, each
    /// for the position it was decided on, the stop, then the leg exits
    /// decided with it or after it. Those are what exits a leg when the
    /// stop placed nothing; a leg the stop exited refuses them at its claim.
    async fn act_on_run(&self, run_id: i64, work: RunWork) {
        let RunWork { ctx, decided, .. } = work;
        let (strategy, mode) = (&ctx.strategy, ctx.mode);
        // The run's state is copied only for a page that watches it.
        if self.broadcast.has_subscribers(strategy.id) {
            self.broadcast
                .push_delta(self.state.snapshot(run_id), false)
                .await;
        }
        let acting = !decided.leg_exits.is_empty()
            || decided.stop_reason.is_some()
            || !decided.after_stop.is_empty();
        // A run that cannot act (no session, or one other than its own)
        // keeps receiving prices and fires again on every tick. Once that
        // has been said, the breach is not written again on each tick
        // until the run can act (web `_REPEATING_BREACH_EVENTS`,
        // marketcalls/openalgo#2337).
        let repeating =
            acting && self.is_unactionable(run_id) && self.authorised_for(run_id, mode).is_err();
        for e in decided.events {
            if repeating && REPEATING_BREACH_EVENTS.contains(&e.kind) {
                continue;
            }
            self.emit(strategy.id, &strategy.user_id, e.kind, &e.message, e.fields)
                .await;
        }
        if !decided.leg_exits.is_empty() {
            self.act_on_risk(run_id, (strategy, mode), None, decided.leg_exits)
                .await;
        }
        if decided.stop_reason.is_some() {
            self.act_on_risk(run_id, (strategy, mode), decided.stop_reason, vec![])
                .await;
        }
        if !decided.after_stop.is_empty() {
            self.act_on_risk(run_id, (strategy, mode), None, decided.after_stop)
                .await;
        }
    }

    /// What risk decided, once the lock is released: a strategy-level
    /// breach stops the run; leg exits are placed, unless there is no
    /// broker session (recorded once per episode, never pretended). Each
    /// leg's claim takes only the position its exit was decided on: the
    /// exits before it are placed first, and a leg re-entered meanwhile
    /// holds another position.
    async fn act_on_risk(
        &self,
        run_id: i64,
        (strategy, mode): (&StrategyRow, RunMode),
        stop_reason: Option<&'static str>,
        leg_exits: Vec<LegExit>,
    ) {
        let strategy_id = strategy.id;
        let user_id = strategy.user_id.clone();
        if let Some(reason) = stop_reason {
            // A strategy-level breach closes everything.
            let r = self.stop_run(run_id, &user_id, reason).await;
            if r.ok {
                self.note_actionable_again(strategy_id, &user_id, run_id)
                    .await;
            }
            return;
        }
        if leg_exits.is_empty() {
            return;
        }
        if self.gateway.authorised(mode).is_err() {
            // The 3 AM window: record once per episode, never pretend to exit.
            if self.claim_unactionable(run_id) {
                tracing::warn!(
                    "Run {} has risk to act on but no broker session; positions left open",
                    run_id
                );
                self.emit(
                    strategy_id,
                    &user_id,
                    "leg_exit_rejected",
                    "Risk triggered but there is no broker session, so nothing could be exited. Positions are still open. Log in to your broker to restore the session.",
                    ev(Some(run_id), None, "critical"),
                )
                .await;
            }
            return;
        }
        if let Some(why) = self.owner_refusal(run_id, mode) {
            // LOG-07: the session is another broker or account than the
            // run's; its exits are not sent there. Said once per episode.
            if self.claim_unactionable(run_id) {
                self.emit(
                    strategy_id,
                    &user_id,
                    "leg_exit_rejected",
                    &format!(
                        "Risk triggered, but nothing was exited. {} Positions are still open.",
                        why
                    ),
                    ev(Some(run_id), None, "critical"),
                )
                .await;
            }
            return;
        }
        self.note_actionable_again(strategy_id, &user_id, run_id)
            .await;
        for x in leg_exits {
            let decided = ExitScope::DecidedOn(x.position_ref.as_deref());
            self.exit_legs(
                run_id,
                strategy,
                (&[x.leg_id], decided),
                x.kind,
                mode,
                &user_id,
            )
            .await;
        }
    }
}
