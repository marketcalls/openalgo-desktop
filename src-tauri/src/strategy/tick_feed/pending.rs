//! Risk decisions waiting for the strategy evaluator to act on them.
//!
//! The feed's ticks are read by one task and acted on by another
//! (`tick_feed::start`). The reader decides every price of every held
//! symbol as it arrives, in the run's lock (`StrategyModule::decide_tick`),
//! so every ratchet (a leg's high and low, its trailing stop, the run's peak
//! and profit lock) and every breach is judged on the whole path, in the
//! order the prices came. Only acting waits for the evaluator: an exit to
//! place, a stop, an event to record, a page to update. While it is busy, a
//! run's later decisions are combined with those waiting
//! (`Decision::merge`), so what waits is bounded by the runs and their legs,
//! never by the prices.
//!
//! Prices used to wait instead, each symbol's folded past 32 to its first,
//! lowest, highest and latest, and that lost decisions (PROD-REL-01): a peak
//! that raised a trailing stop and the dip that then hit it were folded
//! away by a later, larger peak. No shorter path decides alike for every
//! rule: a stepped trail anchored on the live stop moves on every
//! evaluation.
//!
//! The web queues ticks in one bounded FIFO and sheds when it is full
//! (`services/strategy_module/tick_feed.py`, `TICK_QUEUE_MAX`), so a burst
//! there can drop the deciding tick itself; deciding on arrival is a
//! deliberate desktop departure.

use crate::strategy::store::EventFields;
use crate::strategy::tick_context::TickContext;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// One notification a decision raises, and the position it is about.
#[derive(Debug)]
pub(crate) struct Event {
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) fields: EventFields,
    /// A leg's position (for a stop that moved the other legs to entry,
    /// the stopped leg's); `None` for the run. A leg re-entered while its
    /// notifications wait has its own.
    position_ref: Option<String>,
}

impl Event {
    /// A notification about the run.
    pub(crate) fn run(kind: &'static str, message: String, fields: EventFields) -> Self {
        Self::about(kind, message, fields, None)
    }

    /// A notification about one position.
    pub(crate) fn about(
        kind: &'static str,
        message: String,
        fields: EventFields,
        position_ref: Option<String>,
    ) -> Self {
        Event {
            kind,
            message,
            fields,
            position_ref,
        }
    }
}

/// Notifications of a level that keeps moving: of a series waiting, only
/// the latest is recorded.
const PROGRESS: &[&str] = &["leg_trail_advanced", "lock_profit_floor_advanced"];

/// How long a decided exit or stop may wait to be acted on before health
/// warns. An order waits at most 10 s for its turn at the broker
/// (`brokers::common::ratelimit::Pacer`) and is normally answered within a
/// second or two after that, so an exit decided longer ago than this is
/// held by something else: a broker call that does not answer, a stalled
/// database, or the evaluator busy polling prices.
pub(crate) const SLOW_EXIT_AFTER: Duration = Duration::from_secs(15);

/// The strategies with an exit or stop decided and not yet acted on, and
/// how long the oldest of them has waited, in seconds (health).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExitWait {
    pub(crate) strategies: usize,
    pub(crate) oldest_s: Option<u64>,
}

impl ExitWait {
    /// How long the oldest has waited, once that is past `SLOW_EXIT_AFTER`.
    pub(crate) fn slow_for(&self) -> Option<u64> {
        self.oldest_s.filter(|s| *s >= SLOW_EXIT_AFTER.as_secs())
    }
}

/// An exit risk decided for a leg, and the position it was decided on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LegExit {
    pub(crate) leg_id: i64,
    pub(crate) kind: &'static str,
    /// The leg's position when it breached. A leg re-entered since holds
    /// another position, which this exit must not close.
    pub(crate) position_ref: Option<String>,
}

/// What one price decided for one run (in the run's lock) and, while it
/// waits, every later decision combined into it, in the order acting on
/// each in turn would place them: the leg exits before any stop, the first
/// stop, then the leg exits decided with it or after it.
#[derive(Debug, Default)]
pub(crate) struct Decision {
    /// Leg exits decided before any stop.
    pub(crate) leg_exits: Vec<LegExit>,
    /// A strategy-level breach: the run stops.
    pub(crate) stop_reason: Option<&'static str>,
    /// Leg exits decided with the stop or after it. A stop can place no
    /// exit at all (its request could not be saved, or a broker
    /// acknowledgement could not be linked to its order), and these then
    /// exit the legs; a leg the stop did exit refuses them at its claim.
    pub(crate) after_stop: Vec<LegExit>,
    pub(crate) events: Vec<Event>,
}

impl Decision {
    /// What one price decided: a price that stops the run acts on its own
    /// leg exits after the stop.
    pub(crate) fn of_price(
        leg_exits: Vec<LegExit>,
        stop_reason: Option<&'static str>,
        events: Vec<Event>,
    ) -> Self {
        let mut d = Decision {
            stop_reason,
            events,
            ..Default::default()
        };
        if stop_reason.is_some() {
            d.after_stop = leg_exits;
        } else {
            d.leg_exits = leg_exits;
        }
        d
    }

    /// Combine a later decision with this one, as acting on each in turn
    /// would: its leg exits go before the waiting stop when there is none
    /// yet and after it otherwise, the first stop is kept (a later one
    /// would exit what the first did), and a position keeps its first exit
    /// (its claim refuses a second). Notifications keep their order (see
    /// `note`).
    fn merge(&mut self, later: Decision) {
        for e in later.events {
            self.note(e);
        }
        for x in later.leg_exits {
            self.keep_exit(x);
        }
        self.stop_reason = self.stop_reason.or(later.stop_reason);
        for x in later.after_stop {
            self.keep_exit(x);
        }
    }

    /// Whether acting on it places anything: a leg exit or a stop (what
    /// health counts as waiting; notifications alone are not).
    fn asks_to_act(&self) -> bool {
        !self.leg_exits.is_empty() || self.stop_reason.is_some() || !self.after_stop.is_empty()
    }

    /// Keep a leg exit where acting would place it: before the stop while
    /// none waits, after it once one does.
    fn keep_exit(&mut self, x: LegExit) {
        let waiting = self
            .leg_exits
            .iter()
            .chain(&self.after_stop)
            .any(|w| w.leg_id == x.leg_id && w.position_ref == x.position_ref);
        if waiting {
            return;
        }
        if self.stop_reason.is_some() {
            self.after_stop.push(x);
        } else {
            self.leg_exits.push(x);
        }
    }

    /// Keep a notification once per kind and position, in the order they
    /// came. A moving level's latest replaces the one before only while
    /// nothing else about that position came in between, so a trail that
    /// moved, then its stop hit, then the trail moved on reads in that
    /// order.
    fn note(&mut self, e: Event) {
        let (leg_id, position_ref) = (e.fields.leg_id, e.position_ref.clone());
        let about = |w: &Event| w.fields.leg_id == leg_id && w.position_ref == position_ref;
        if PROGRESS.contains(&e.kind) {
            let last = self.events.iter().rposition(about);
            if let Some(i) = last.filter(|&i| self.events[i].kind == e.kind) {
                self.events.remove(i);
            }
        } else if self.events.iter().any(|w| w.kind == e.kind && about(w)) {
            return;
        }
        self.events.push(e);
    }
}

/// A run's decisions not acted on yet.
pub(crate) struct RunWork {
    pub(crate) ctx: Arc<TickContext>,
    pub(crate) decided: Decision,
    /// When the run's first waiting decision came: runs are acted on in the
    /// order they decided.
    arrived: u64,
    /// When the first of them that asks for an exit or a stop was decided.
    exits_since: Option<DateTime<Utc>>,
}

/// The decisions waiting, and the runs the evaluator has taken and not yet
/// acted on.
#[derive(Default)]
struct Waiting {
    runs: HashMap<i64, RunWork>,
    arrivals: u64,
    /// Each run taken and not yet acted on, with its `exits_since`.
    acting: Vec<(i64, Option<DateTime<Utc>>)>,
}

impl Waiting {
    /// The runs with an exit or stop decided and not yet acted on (a run
    /// being acted on with a newer decision waiting is one run), and when
    /// the oldest of those was decided.
    fn exits(&self) -> (usize, Option<DateTime<Utc>>) {
        let since = self
            .runs
            .iter()
            .map(|(id, work)| (*id, work.exits_since))
            .chain(self.acting.iter().copied());
        let mut runs = HashSet::new();
        let mut oldest: Option<DateTime<Utc>> = None;
        for (run_id, at) in since {
            if let Some(at) = at {
                runs.insert(run_id);
                oldest = Some(oldest.map_or(at, |o| o.min(at)));
            }
        }
        (runs.len(), oldest)
    }
}

/// The decisions waiting for the evaluator, its wake-up, and what the
/// strategy tick path read, combined and missed (health).
#[derive(Default)]
pub(crate) struct Pending {
    waiting: Mutex<Waiting>,
    /// Woken on every decision left; the evaluator acts on all waiting.
    pub(super) wake: Arc<Notify>,
    /// Prices of held symbols read since start.
    read: AtomicU64,
    /// Decisions combined with ones still waiting for the evaluator (each
    /// was decided on its own price; only its follow-up was combined).
    combined: AtomicU64,
    /// Ticks the reader missed because the market-data channel overran it:
    /// lost to the strategy module.
    lagged: AtomicU64,
    /// `lagged` as of the last health alert.
    alerted: AtomicU64,
}

impl Pending {
    pub(super) fn note_read(&self) {
        self.read.fetch_add(1, Ordering::Relaxed);
    }

    /// Leave one price's decision for a run, decided at `now`, to the
    /// evaluator and wake it.
    pub(crate) fn leave(
        &self,
        run_id: i64,
        ctx: Arc<TickContext>,
        decision: Decision,
        now: DateTime<Utc>,
    ) {
        {
            let mut guard = self.waiting.lock();
            let waiting = &mut *guard;
            let acts = decision.asks_to_act().then_some(now);
            if let Some(work) = waiting.runs.get_mut(&run_id) {
                work.decided.merge(decision);
                work.exits_since = work.exits_since.or(acts);
                self.combined.fetch_add(1, Ordering::Relaxed);
            } else {
                let mut decided = Decision::default();
                decided.merge(decision);
                waiting.arrivals += 1;
                let arrived = waiting.arrivals;
                waiting.runs.insert(
                    run_id,
                    RunWork {
                        ctx,
                        decided,
                        arrived,
                        exits_since: acts,
                    },
                );
            }
        }
        self.wake.notify_one();
    }

    /// Everything waiting, run by run in the order they first decided. A
    /// run taken counts as waiting until `acted`.
    pub(crate) fn take(&self) -> Vec<(i64, RunWork)> {
        let mut runs: Vec<(i64, RunWork)> = {
            let mut guard = self.waiting.lock();
            let waiting = &mut *guard;
            let taken = waiting.runs.iter().map(|(id, w)| (*id, w.exits_since));
            waiting.acting.extend(taken);
            waiting.runs.drain().collect()
        };
        runs.sort_by_key(|(_, work)| work.arrived);
        runs
    }

    /// The run taken has been acted on.
    pub(crate) fn acted(&self, run_id: i64) {
        let mut waiting = self.waiting.lock();
        if let Some(i) = waiting.acting.iter().position(|(id, _)| *id == run_id) {
            waiting.acting.swap_remove(i);
        }
    }

    /// Drop every decision waiting (shutdown).
    pub(super) fn clear(&self) {
        let mut waiting = self.waiting.lock();
        waiting.runs.clear();
        waiting.acting.clear();
    }

    pub(super) fn note_lagged(&self, n: u64) {
        self.lagged.fetch_add(n, Ordering::Relaxed);
        tracing::warn!(
            "The strategy tick reader missed {} prices: the market-data channel overran it",
            n
        );
    }

    /// The missed count when it grew since the last call (one alert per
    /// growth).
    pub(super) fn missed_since_alert(&self) -> Option<u64> {
        let missed = self.lagged.load(Ordering::Relaxed);
        (self.alerted.fetch_max(missed, Ordering::Relaxed) < missed).then_some(missed)
    }

    /// The counts for health at `now`: the strategies with an exit or stop
    /// decided and not yet acted on and how long the oldest has waited,
    /// `warn` once any tick was missed or while an exit has waited past
    /// `SLOW_EXIT_AFTER`. The read and combined counts keep the keys they
    /// had when prices waited (`ticks_queued`, `ticks_folded`); the Health
    /// page names them.
    pub(super) fn health(&self, now: DateTime<Utc>) -> Value {
        let lagged = self.lagged.load(Ordering::Relaxed);
        let wait = self.exit_wait(now);
        let slow = wait.slow_for().is_some();
        json!({
            "status": if lagged == 0 && !slow { "pass" } else { "warn" },
            "ticks_queued": self.read.load(Ordering::Relaxed),
            "ticks_folded": self.combined.load(Ordering::Relaxed),
            "ticks_missed": lagged,
            "exits_waiting": wait.strategies,
            "oldest_exit_wait_s": wait.oldest_s,
            "exits_slow": slow,
        })
    }

    /// The exits and stops decided and not yet acted on at `now`.
    pub(super) fn exit_wait(&self, now: DateTime<Utc>) -> ExitWait {
        let (strategies, oldest) = self.waiting.lock().exits();
        ExitWait {
            strategies,
            oldest_s: oldest.map(|at| (now - at).to_std().unwrap_or_default().as_secs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(leg_id: i64, kind: &'static str) -> LegExit {
        LegExit {
            leg_id,
            kind,
            position_ref: Some(format!("position-{}", leg_id)),
        }
    }

    fn event(kind: &'static str, leg_id: Option<i64>, message: &str) -> Event {
        let position = leg_id.map(|l| format!("position-{}", l));
        event_on(kind, (leg_id, position), message)
    }

    /// A notification about one position of a leg, or (`None`) the run's.
    fn event_on(
        kind: &'static str,
        (leg_id, position_ref): (Option<i64>, Option<String>),
        message: &str,
    ) -> Event {
        let fields = EventFields {
            leg_id,
            ..Default::default()
        };
        Event::about(kind, message.to_string(), fields, position_ref)
    }

    fn seen(d: &Decision) -> Vec<(&str, Option<i64>, &str)> {
        d.events
            .iter()
            .map(|e| (e.kind, e.fields.leg_id, e.message.as_str()))
            .collect()
    }

    fn exits(d: &Decision) -> Vec<(i64, &'static str)> {
        d.leg_exits.iter().map(|x| (x.leg_id, x.kind)).collect()
    }

    fn combined(decisions: Vec<Decision>) -> Decision {
        let mut d = Decision::default();
        for later in decisions {
            d.merge(later);
        }
        d
    }

    fn price(leg_exits: Vec<LegExit>, stop_reason: Option<&'static str>) -> Decision {
        Decision::of_price(leg_exits, stop_reason, vec![])
    }

    /// A leg keeps its first exit, the first stop is kept, and the leg
    /// exits after it wait behind it rather than being dropped.
    #[test]
    fn a_leg_keeps_its_first_exit_and_exits_after_a_stop_wait_behind_it() {
        let d = combined(vec![
            price(vec![exit(1, "exit_sl")], None),
            price(vec![exit(1, "exit_target"), exit(2, "exit_target")], None),
            price(vec![], Some("overall_sl")),
            price(
                vec![exit(2, "exit_sl"), exit(3, "exit_sl")],
                Some("lock_profit"),
            ),
            price(vec![exit(3, "exit_target"), exit(4, "exit_target")], None),
        ]);
        assert_eq!(exits(&d), vec![(1, "exit_sl"), (2, "exit_target")]);
        assert_eq!(d.stop_reason, Some("overall_sl"), "the first stop");
        let after: Vec<(i64, &str)> = d.after_stop.iter().map(|x| (x.leg_id, x.kind)).collect();
        assert_eq!(after, vec![(3, "exit_sl"), (4, "exit_target")]);
    }

    /// A stop can place nothing (its request could not be saved), so the
    /// stopping price's own leg exits are acted on after it; a leg the
    /// stop did exit refuses them at its claim.
    #[test]
    fn a_price_that_stops_the_run_acts_on_its_own_leg_exits_after_the_stop() {
        let d = combined(vec![Decision::of_price(
            vec![exit(1, "exit_sl")],
            Some("lock_profit"),
            vec![event("leg_sl_hit", Some(1), "hit")],
        )]);
        assert!(d.leg_exits.is_empty());
        assert_eq!(d.stop_reason, Some("lock_profit"));
        assert_eq!(d.after_stop, vec![exit(1, "exit_sl")]);
        assert_eq!(d.events.len(), 1, "its events are still recorded");
    }

    /// A leg re-entered while its first exit waits is another position: its
    /// own exit is kept beside the first, which is dropped when acted on.
    #[test]
    fn a_re_entered_position_keeps_its_own_exit() {
        let mut again = exit(1, "exit_sl");
        again.position_ref = Some("position-1b".into());
        let d = combined(vec![
            price(vec![exit(1, "exit_sl")], None),
            price(vec![again.clone()], None),
        ]);
        assert_eq!(d.leg_exits, vec![exit(1, "exit_sl"), again]);
    }

    /// The trail moved to 105, the 104 hit it, then the trail moved on
    /// to 145: the log keeps that order, and of each series of moves only
    /// the latest.
    #[test]
    fn a_notification_is_kept_once_and_a_moving_level_keeps_its_latest_in_order() {
        let d = combined(vec![
            Decision {
                events: vec![event("leg_trail_advanced", Some(1), "to 105")],
                ..Default::default()
            },
            Decision {
                leg_exits: vec![exit(1, "exit_sl")],
                events: vec![event("leg_sl_hit", Some(1), "at 104")],
                ..Default::default()
            },
            Decision {
                events: vec![
                    event("leg_sl_hit", Some(1), "at 103"),
                    event("leg_trail_advanced", Some(1), "to 145"),
                    event("leg_trail_advanced", Some(2), "to 98"),
                    event("leg_trail_advanced", Some(1), "to 146"),
                ],
                ..Default::default()
            },
            Decision {
                stop_reason: Some("overall_sl"),
                events: vec![event("overall_sl_hit", None, "down")],
                ..Default::default()
            },
            Decision {
                events: vec![event("lock_profit_triggered", None, "after the stop")],
                ..Default::default()
            },
        ]);
        assert_eq!(
            seen(&d),
            vec![
                ("leg_trail_advanced", Some(1), "to 105"),
                ("leg_sl_hit", Some(1), "at 104"),
                ("leg_trail_advanced", Some(2), "to 98"),
                ("leg_trail_advanced", Some(1), "to 146"),
                ("overall_sl_hit", None, "down"),
                ("lock_profit_triggered", None, "after the stop"),
            ]
        );
    }

    /// Notifications are kept per position: a leg re-entered while its
    /// first position's hit waits has its own hit told, and a second leg's
    /// stop moving the others to entry is told beside the first's.
    #[test]
    fn each_position_keeps_its_own_notifications() {
        let (first, again) = (Some("position-1".into()), Some("position-1b".into()));
        let d = combined(vec![
            Decision {
                events: vec![
                    event_on("leg_sl_hit", (Some(1), first.clone()), "first"),
                    event_on("trail_to_entry_activated", (None, first.clone()), "by 1"),
                ],
                ..Default::default()
            },
            Decision {
                events: vec![
                    event_on("leg_sl_hit", (Some(1), first.clone()), "first again"),
                    event_on("trail_to_entry_activated", (None, first), "by 1 again"),
                    event_on("leg_sl_hit", (Some(1), again), "re-entered"),
                    event_on(
                        "trail_to_entry_activated",
                        (None, Some("position-2".into())),
                        "by 2",
                    ),
                ],
                ..Default::default()
            },
        ]);
        assert_eq!(
            seen(&d),
            vec![
                ("leg_sl_hit", Some(1), "first"),
                ("trail_to_entry_activated", None, "by 1"),
                ("leg_sl_hit", Some(1), "re-entered"),
                ("trail_to_entry_activated", None, "by 2"),
            ]
        );
    }

    #[test]
    fn counts_survive_and_a_miss_alerts_once() {
        let p = Pending::default();
        p.note_read();
        p.note_read();
        assert!(p.take().is_empty());
        p.note_lagged(3);
        assert_eq!(p.missed_since_alert(), Some(3));
        assert_eq!(p.missed_since_alert(), None, "once per growth");
        let h = p.health(Utc::now());
        assert_eq!(h["ticks_queued"], 2);
        assert_eq!(h["ticks_missed"], 3);
        assert_eq!(h["exits_waiting"], 0);
        assert_eq!(h["oldest_exit_wait_s"], Value::Null);
        assert_eq!(h["status"], "warn");
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        const LEGS: [i64; 3] = [1, 2, 3];

        /// What acting places: a position is exited once (its claim), a
        /// stop exits the legs it can that are not yet exited, and the
        /// first stop's reason is the one kept. What a stop can exit is
        /// fixed for a case: every leg, some (an entry not yet filled), or
        /// none (its request could not be saved, or a broker
        /// acknowledgement could not be linked).
        #[derive(Debug, Default, PartialEq)]
        struct Placed {
            exits: Vec<(i64, &'static str)>,
            stop: Option<&'static str>,
            stop_exits: Vec<i64>,
        }

        impl Placed {
            fn new(stop_exits: &[i64]) -> Self {
                Placed {
                    stop_exits: stop_exits.to_vec(),
                    ..Default::default()
                }
            }

            fn exit(&mut self, leg_id: i64, kind: &'static str) {
                if self.exits.iter().all(|(l, _)| *l != leg_id) {
                    self.exits.push((leg_id, kind));
                }
            }

            fn stop(&mut self, reason: &'static str) {
                self.stop.get_or_insert(reason);
                for leg in self.stop_exits.clone() {
                    self.exit(leg, "stop");
                }
            }

            /// One price acted on alone: its stop, then its own leg exits
            /// (a leg the stop exited refuses them).
            fn act_alone(&mut self, (exits, stop): &Price) {
                if let Some(reason) = *stop {
                    self.stop(reason);
                }
                exits.iter().for_each(|&(l, k)| self.exit(l, k));
            }

            /// The evaluator on combined decisions (`act_on_run`): the leg
            /// exits before the stop, the stop, the leg exits after it.
            fn act_combined(&mut self, d: &Decision) {
                d.leg_exits.iter().for_each(|x| self.exit(x.leg_id, x.kind));
                if let Some(reason) = d.stop_reason {
                    self.stop(reason);
                }
                d.after_stop
                    .iter()
                    .for_each(|x| self.exit(x.leg_id, x.kind));
            }
        }

        /// One price's leg exits and stop, as decided.
        type Price = (Vec<(i64, &'static str)>, Option<&'static str>);

        fn priced() -> impl Strategy<Value = Price> {
            let leg_exit = (
                prop::sample::select(LEGS.to_vec()),
                prop::sample::select(vec!["exit_sl", "exit_target"]),
            );
            (
                prop::collection::vec(leg_exit, 0..3),
                prop::option::weighted(
                    0.15,
                    prop::sample::select(vec!["overall_sl", "lock_profit"]),
                ),
            )
        }

        fn decided((exits, stop): &Price) -> Decision {
            let exits = exits.iter().map(|&(l, k)| exit(l, k)).collect();
            Decision::of_price(exits, *stop, vec![])
        }

        proptest! {
            /// However the evaluator's turns cut a run of decisions, and
            /// whatever a stop manages to exit, acting on each turn's
            /// combined decisions places exactly what acting on every
            /// price in turn places.
            #[test]
            fn combining_decisions_places_what_acting_on_each_does(
                prices in prop::collection::vec(priced(), 1..40),
                cuts in prop::collection::vec(any::<bool>(), 40),
                stop_exits in prop::sample::subsequence(LEGS.to_vec(), 0..=LEGS.len()),
            ) {
                let mut alone = Placed::new(&stop_exits);
                for p in &prices {
                    alone.act_alone(p);
                }
                let mut turns = Placed::new(&stop_exits);
                let mut waiting = Decision::default();
                for (i, p) in prices.iter().enumerate() {
                    waiting.merge(decided(p));
                    if cuts[i] {
                        turns.act_combined(&std::mem::take(&mut waiting));
                    }
                }
                turns.act_combined(&waiting);
                prop_assert_eq!(turns, alone);
            }
        }
    }
}
