//! The strategy tick path under a burst (benchmarks P1, PROD-REL-01): every
//! price is decided while the evaluator is busy, in the order it came, each
//! decision places one exit, the reader never waits on the database, and a
//! tick the reader does miss is counted for health.

use super::*;
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, MarketEvent, NormalizedTick};
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::strategy::tick_feed::{Key, PriceSource};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// A market-data channel of the size a test needs: small to overrun the
/// reader, large to hold a whole burst.
pub(super) struct Burst {
    tx: broadcast::Sender<MarketEvent>,
    subscribed: Mutex<Vec<Key>>,
}

impl Burst {
    pub(super) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(capacity).0,
            subscribed: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn send(&self, symbol: &str, ltp: f64) {
        let tick = NormalizedTick {
            symbol: symbol.into(),
            exchange: "NFO".into(),
            ltp,
            ..Default::default()
        };
        let _ = self.tx.send(Arc::new(FeedEvent::Tick(tick)));
    }
}

#[async_trait::async_trait]
impl PriceSource for Burst {
    async fn subscribe(&self, keys: &[Key]) {
        self.subscribed.lock().extend_from_slice(keys);
    }
    async fn unsubscribe(&self, _keys: &[Key]) {}
    fn ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.tx.subscribe()
    }
    async fn poll(&self, _keys: &[Key]) -> Vec<(Key, f64)> {
        vec![]
    }
}

/// A NIFTY option leg (`position` B or S, `expiry` weekly or monthly,
/// `option_type` CE or PE, at `offset` from the money) with `risk`.
pub(super) fn option_leg(
    id: i64,
    (position, expiry, option_type, offset): (&str, &str, &str, &str),
    risk: Value,
) -> Value {
    let mut leg = json!({
        "id": id, "segment": "options", "expiry": expiry, "lots": 1,
        "position": position, "option_type": option_type, "strike_mode": "atm",
        "atm_offset": offset, "trail": {"x": 0, "y": 0},
    });
    for (k, v) in risk.as_object().unwrap() {
        leg[k] = v.clone();
    }
    leg
}

/// A long weekly NIFTY call at `offset` from the money, with `risk`.
fn long_call(offset: &str, risk: Value) -> Value {
    option_leg(1, ("B", "weekly", "CE", offset), risk)
}

/// A run of one long call bought at 100; answers its run and contract.
async fn held(t: &T, name: &str, offset: &str, risk: Value) -> (i64, String) {
    let sid = t.make(config(name, json!([long_call(offset, risk)]), json!({})));
    let run = t.start_filled(sid, 100.0).await;
    (run, t.leg(run, 1).symbol)
}

fn sold(t: &T, symbol: &str) -> bool {
    t.gw.placed()
        .iter()
        .any(|o| o.symbol == symbol && o.action == "SELL")
}

/// Wait (bounded) until `f` holds.
pub(super) async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..500 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f()
}

/// The kinds of the run's exit orders, in the order they were recorded.
pub(super) fn exit_kinds(t: &T, run: i64) -> Vec<String> {
    t.orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .map(|o| o.kind)
        .collect()
}

/// The strategy tick path's health now.
pub(super) fn health(t: &T) -> Value {
    t.m.feed.health(t.clock.now())
}

fn prices_read(t: &T) -> u64 {
    health(t)["ticks_queued"].as_u64().unwrap_or(0)
}

/// Send `ticks` in order while the evaluator is held placing another run's
/// exit, wait until the reader has read every one, then let the evaluator
/// go. Far more than 32 prices of one symbol wait at once.
pub(super) async fn send_while_held(t: &T, burst: &Burst, ticks: &[(String, f64)]) {
    let (_, blocker) = held(t, "Blocker", "OTM3", json!({"sl_pts": 10})).await;
    let gate = Arc::new(Gate::default());
    *t.gw.place_gate.lock() = Some(gate.clone());
    let read = prices_read(t);
    burst.send(&blocker, 85.0);
    gate.entered().await;
    for (symbol, ltp) in ticks {
        burst.send(symbol, *ltp);
        tokio::task::yield_now().await;
    }
    let all = read + 1 + ticks.len() as u64;
    assert!(
        eventually(|| prices_read(t) == all).await,
        "the reader read {} of {} prices",
        prices_read(t),
        all
    );
    *t.gw.place_gate.lock() = None;
    gate.release();
}

/// Wait until no exit or stop decided is still waiting to be acted on.
pub(super) async fn settled(t: &T) {
    assert!(
        eventually(|| health(t)["exits_waiting"].as_u64().unwrap_or(0) == 0).await,
        "{}",
        health(t)
    );
}

fn ticks(symbol: &str, prices: &[f64]) -> Vec<(String, f64)> {
    prices.iter().map(|p| (symbol.to_string(), *p)).collect()
}

/// `head`, then `price` `n` times.
fn then_flat(head: &[f64], price: f64, n: usize) -> Vec<f64> {
    let mut v = head.to_vec();
    v.extend(std::iter::repeat_n(price, n));
    v
}

/// The module with a burst source large enough to hold every price sent.
async fn burst_module() -> (T, Arc<Burst>) {
    let burst = Burst::new(8192);
    let t = t_with(
        ist(2026, 10, 7, 10, 0),
        Some(burst.clone() as Arc<dyn PriceSource>),
    );
    t.m.start().await;
    (t, burst)
}

/// The messages of the strategy's events of `kind`.
fn messages(t: &T, sid: i64, kind: &str) -> Vec<String> {
    t.events(sid)
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| e["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// While the evaluator is held placing one run's exit, the feed floods
/// three other held contracts, each with one spike in it: through a stop
/// (to 85 under 90), through a target (to 115 over 110), and up through a
/// trailing stop's trigger (to 150, trailing it to 140) with the noise
/// after it under the trail. Before, one consumer read and evaluated: held
/// in the placement it stopped reading, the channel overran and the spikes
/// were skipped (`Lagged`, logged at debug), so three positions got no
/// exit. Now the reader keeps reading and deciding while the evaluator is
/// held, and every spike's exit is placed once it is free.
#[tokio::test]
async fn a_spike_is_decided_even_while_the_evaluator_is_held() {
    let burst = Burst::new(16);
    let t = t_with(
        ist(2026, 10, 7, 10, 0),
        Some(burst.clone() as Arc<dyn PriceSource>),
    );
    t.m.start().await;
    let (_, blocker) = held(&t, "Blocker", "OTM3", json!({"sl_pts": 10})).await;
    let (stop_run, stop) = held(&t, "Stop", "ATM", json!({"sl_pts": 10})).await;
    let (target_run, target) = held(&t, "Target", "OTM1", json!({"target_pts": 10})).await;
    let (trail_run, trail) = held(
        &t,
        "Trail",
        "OTM2",
        json!({"sl_pts": 10, "trail": {"x": 10, "y": 0}}),
    )
    .await;

    let gate = Arc::new(Gate::default());
    *t.gw.place_gate.lock() = Some(gate.clone());
    burst.send(&blocker, 85.0);
    gate.entered().await;
    for i in 0..200 {
        let noise = 100.0 + f64::from(i % 5) * 0.1;
        burst.send(&stop, if i == 50 { 85.0 } else { noise });
        burst.send(&target, if i == 50 { 115.0 } else { noise });
        burst.send(&trail, if i == 50 { 150.0 } else { noise });
        tokio::task::yield_now().await;
    }
    *t.gw.place_gate.lock() = None;
    gate.release();

    for _ in 0..500 {
        if [&blocker, &stop, &target, &trail]
            .iter()
            .all(|s| sold(&t, s))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for (name, symbol) in [("stop", &stop), ("target", &target), ("trail", &trail)] {
        assert!(sold(&t, symbol), "the {} spike was never decided", name);
    }
    let kind = |run: i64| {
        t.orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .map(|o| o.kind)
    };
    assert_eq!(kind(stop_run).as_deref(), Some("exit_sl"));
    assert_eq!(kind(target_run).as_deref(), Some("exit_target"));
    assert_eq!(kind(trail_run).as_deref(), Some("exit_sl"));
    let h = health(&t);
    assert_eq!(h["ticks_missed"], 0, "{}", h);
    assert!(h["ticks_folded"].as_u64().unwrap() > 0, "{}", h);
    t.m.shutdown().await;
}

/// PROD-REL-01, long: bought at 100 with a 90 stop and a 5-point trail.
/// The 110 raises the stop to 105 and the 104 after it hits it, though a
/// later 150 and 29 prices of 149 arrive while the evaluator is busy.
/// Folded to its first, lowest, highest and latest (100, 104, 150, 149),
/// the path lost the 110: the 104 met a 90 stop and the 145 the 150 earned
/// was never hit, so the position stayed open.
#[tokio::test]
async fn a_trail_raised_by_an_earlier_peak_exits_on_the_dip_after_it() {
    let (t, burst) = burst_module().await;
    let risk = json!({"sl_pts": 10, "trail": {"x": 5, "y": 0}});
    let (run, symbol) = held(&t, "Trail", "ATM", risk).await;
    let path = then_flat(&[100.0, 110.0, 104.0, 150.0], 149.0, 29);
    send_while_held(&t, &burst, &ticks(&symbol, &path)).await;

    assert!(
        eventually(|| !exit_kinds(&t, run).is_empty()).await,
        "the dip under the trailed stop was never decided"
    );
    settled(&t).await;
    assert_eq!(exit_kinds(&t, run), vec!["exit_sl"], "exactly one exit");
    let sid = t.run(run).strategy_id;
    let hits = messages(&t, sid, "leg_sl_hit");
    assert!(
        hits.len() == 1 && hits[0].contains("last price 104 ") && hits[0].contains("the stop 105 "),
        "decided at the 104, against the stop the 110 earned: {:?}",
        hits
    );
    let leg = t.leg(run, 1);
    assert_eq!(leg.highest_price, Some(150.0), "every price was seen");
    assert_eq!(leg.effective_sl, Some(145.0));
    t.m.shutdown().await;
}

/// The short mirror: sold at 100 with a 110 stop and a 5-point trail. The
/// 90 lowers the stop to 95 and the 96 after it hits it, though a later 50
/// and 29 prices of 51 wait with it.
#[tokio::test]
async fn a_short_trail_lowered_by_an_earlier_trough_exits_on_the_bounce_after_it() {
    let (t, burst) = burst_module().await;
    let leg = option_leg(
        1,
        ("S", "weekly", "CE", "ATM"),
        json!({"sl_pts": 10, "trail": {"x": 5, "y": 0}}),
    );
    let sid = t.make(config("Short trail", json!([leg]), json!({})));
    let run = t.start_filled(sid, 100.0).await;
    let symbol = t.leg(run, 1).symbol;
    let path = then_flat(&[100.0, 90.0, 96.0, 50.0], 51.0, 29);
    send_while_held(&t, &burst, &ticks(&symbol, &path)).await;

    assert!(
        eventually(|| !exit_kinds(&t, run).is_empty()).await,
        "the bounce over the trailed stop was never decided"
    );
    settled(&t).await;
    assert_eq!(exit_kinds(&t, run), vec!["exit_sl"], "exactly one exit");
    let hits = messages(&t, sid, "leg_sl_hit");
    assert!(
        hits.len() == 1 && hits[0].contains("last price 96 ") && hits[0].contains("the stop 95 "),
        "{:?}",
        hits
    );
    let bought_back =
        t.gw.placed()
            .into_iter()
            .filter(|o| o.symbol == symbol && o.action == "BUY")
            .count();
    assert_eq!(bought_back, 1);
    t.m.shutdown().await;
}

/// A stepped trail (every 10 points of favour moves the 90 stop up 5): the
/// 120 earns two steps (stop 100) and the 99 after it hits it, though a
/// later 200 and 29 prices of 199 wait with it. Folded, the 99 met the 90.
#[tokio::test]
async fn a_stepped_trail_exits_on_the_dip_after_the_steps_it_earned() {
    let (t, burst) = burst_module().await;
    let risk = json!({"sl_pts": 10, "trail": {"x": 10, "y": 5}});
    let (run, symbol) = held(&t, "Stepped", "ATM", risk).await;
    let path = then_flat(&[100.0, 120.0, 99.0, 200.0], 199.0, 29);
    send_while_held(&t, &burst, &ticks(&symbol, &path)).await;

    assert!(
        eventually(|| !exit_kinds(&t, run).is_empty()).await,
        "the dip under the stepped stop was never decided"
    );
    settled(&t).await;
    assert_eq!(exit_kinds(&t, run), vec!["exit_sl"], "exactly one exit");
    let sid = t.run(run).strategy_id;
    let hits = messages(&t, sid, "leg_sl_hit");
    assert!(
        hits.len() == 1 && hits[0].contains("last price 99 ") && hits[0].contains("the stop 100 "),
        "{:?}",
        hits
    );
    assert_eq!(t.leg(run, 1).effective_sl, Some(140.0), "ten steps at 200");
    t.m.shutdown().await;
}

/// The deciding peak and dip arrive between many folds: 40 prices first,
/// then 110 and 104, then 150 and a hundred prices of 149. Each fold kept
/// the earlier low and the latest high, so the pair was lost.
#[tokio::test]
async fn a_peak_and_dip_between_many_folds_are_still_decided() {
    let (t, burst) = burst_module().await;
    let risk = json!({"sl_pts": 10, "trail": {"x": 5, "y": 0}});
    let (run, symbol) = held(&t, "Long burst", "ATM", risk).await;
    let mut path = then_flat(&[100.0], 101.0, 40);
    path.extend(then_flat(&[110.0, 104.0, 150.0], 149.0, 100));
    send_while_held(&t, &burst, &ticks(&symbol, &path)).await;

    assert!(
        eventually(|| !exit_kinds(&t, run).is_empty()).await,
        "the dip was never decided"
    );
    settled(&t).await;
    assert_eq!(exit_kinds(&t, run), vec!["exit_sl"], "exactly one exit");
    t.m.shutdown().await;
}

/// Two long legs (a call with a 5-point trail, a put with a 90 stop) and
/// their prices interleaved. `trail_to_entry`: whether a stop on one moves
/// the other's stop to its entry.
async fn two_legs(t: &T, trail_to_entry: bool) -> (i64, String, String) {
    let call = option_leg(
        1,
        ("B", "weekly", "CE", "ATM"),
        json!({"sl_pts": 10, "trail": {"x": 5, "y": 0}}),
    );
    let put = option_leg(2, ("B", "weekly", "PE", "ATM"), json!({"sl_pts": 10}));
    let sid = t.make(config(
        "Two legs",
        json!([call, put]),
        json!({"trail_sl_to_entry": trail_to_entry}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    (run, t.leg(run, 1).symbol, t.leg(run, 2).symbol)
}

/// A partial exit: the call's trailed stop is hit on the dip after its
/// peak and only the call is exited; the put, whose prices interleave and
/// never reach its stop, stays open and the run keeps running.
#[tokio::test]
async fn only_the_leg_whose_trail_was_hit_is_exited() {
    let (t, burst) = burst_module().await;
    let (run, call, put) = two_legs(&t, false).await;
    let call_path = then_flat(&[100.0, 110.0, 104.0, 150.0], 149.0, 29);
    let mut sent = Vec::new();
    for (i, p) in call_path.iter().enumerate() {
        sent.push((call.clone(), *p));
        sent.push((put.clone(), 100.5 + (i % 3) as f64 * 0.1));
    }
    send_while_held(&t, &burst, &sent).await;

    assert!(
        eventually(|| !exit_kinds(&t, run).is_empty()).await,
        "the call's dip was never decided"
    );
    settled(&t).await;
    let exits: Vec<(i64, String)> = t
        .orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .map(|o| (o.leg_id, o.kind))
        .collect();
    assert_eq!(exits, vec![(1, "exit_sl".to_string())]);
    assert_eq!(t.leg(run, 2).status, "open");
    assert_eq!(t.run(run).stop_requested_reason, None);
    t.m.shutdown().await;
}

/// Trail to entry is decided in order across symbols: the call's stop is
/// hit at 104, which moves the put's stop to its entry (100), so the put's
/// 99.5 after it exits the put too. Folded, the call's 110 and 104 were
/// lost, nothing moved, and the 99.5 met the put's 90.
#[tokio::test]
async fn a_trailed_stop_moves_the_other_leg_to_entry_before_its_next_price() {
    let (t, burst) = burst_module().await;
    let (run, call, put) = two_legs(&t, true).await;
    let mut sent = ticks(&call, &[100.0, 110.0, 104.0]);
    sent.extend(ticks(&put, &[100.5, 100.4, 100.6, 99.5]));
    sent.extend(ticks(&call, &then_flat(&[150.0], 149.0, 29)));
    sent.extend(ticks(&put, &[101.0; 30]));
    send_while_held(&t, &burst, &sent).await;

    assert!(
        eventually(|| exit_kinds(&t, run).len() == 2).await,
        "exits: {:?}",
        exit_kinds(&t, run)
    );
    settled(&t).await;
    let exits: Vec<(i64, String)> = t
        .orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .map(|o| (o.leg_id, o.kind))
        .collect();
    assert_eq!(
        exits,
        vec![(1, "exit_sl".to_string()), (2, "exit_sl".to_string())]
    );
    let sid = t.run(run).strategy_id;
    assert_eq!(messages(&t, sid, "trail_to_entry_activated").len(), 1);
    t.m.shutdown().await;
}

/// Aggregate risk over two legs: a trailing profit lock (active at 650,
/// floor 325, trailing 325 behind the peak). The call's 110 makes 650 and
/// activates it; the put's 99 and the call's 104 take the run to 195, under
/// the floor, so the run stops and both legs are exited once each. Folded,
/// the 110 was lost: the lock activated only at the later 150, with a floor
/// the 149s never reached.
#[tokio::test]
async fn a_profit_lock_across_two_legs_stops_the_run_on_the_reversal() {
    let (t, burst) = burst_module().await;
    let call = option_leg(1, ("B", "weekly", "CE", "ATM"), json!({}));
    let put = option_leg(2, ("B", "weekly", "PE", "ATM"), json!({}));
    let lock = json!({"lock_profit": {"mode": "lock_and_trail", "if_profit_reaches": 650,
                                       "lock_profit": 325, "trail_step": 325}});
    let sid = t.make(config("Locked pair", json!([call, put]), lock));
    let run = t.start_filled(sid, 100.0).await;
    let (call, put) = (t.leg(run, 1).symbol, t.leg(run, 2).symbol);
    let mut sent = ticks(&call, &[100.0, 110.0]);
    sent.extend(ticks(&put, &[99.0]));
    sent.extend(ticks(&call, &then_flat(&[104.0, 150.0], 149.0, 29)));
    sent.extend(ticks(&put, &[99.0; 30]));
    send_while_held(&t, &burst, &sent).await;

    assert!(
        eventually(|| exit_kinds(&t, run).len() == 2).await,
        "exits: {:?}",
        exit_kinds(&t, run)
    );
    settled(&t).await;
    assert_eq!(
        t.run(run).stop_requested_reason.as_deref(),
        Some("lock_profit")
    );
    let mut exits: Vec<(i64, String)> = t
        .orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .map(|o| (o.leg_id, o.kind))
        .collect();
    exits.sort();
    assert_eq!(
        exits,
        vec![
            (1, "exit_lock_profit".to_string()),
            (2, "exit_lock_profit".to_string())
        ]
    );
    let hits = messages(&t, sid, "lock_profit_triggered");
    assert!(
        hits.len() == 1 && hits[0].contains(" 195 "),
        "decided at 195: {:?}",
        hits
    );
    t.m.shutdown().await;
}

/// Two long legs at 100, the call with a 90 stop, and a combined stop of
/// 325 (5 points): the call's 94 breaches the combined stop, the put's 110
/// brings the run back inside it, and the call's 89 then hits its own
/// stop, all while the evaluator is busy. Answers the run and its legs'
/// exits once acted on.
async fn stop_then_leg_exit(t: &T, burst: &Burst, refuse_stop: bool) -> (i64, Vec<(i64, String)>) {
    let call = option_leg(1, ("B", "weekly", "CE", "ATM"), json!({"sl_pts": 10}));
    let put = option_leg(2, ("B", "weekly", "PE", "ATM"), json!({}));
    let limit = json!({"overall_sl_mtm": 325});
    let sid = t.make(config("Stop then leg", json!([call, put]), limit));
    let run = t.start_filled(sid, 100.0).await;
    if refuse_stop {
        t.db.conn()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_stop BEFORE UPDATE OF stop_requested_reason \
                 ON sm_strategy_run WHEN NEW.id = {} \
                 BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
                run
            ))
            .unwrap();
    }
    let (call, put) = (t.leg(run, 1).symbol, t.leg(run, 2).symbol);
    let mut sent = ticks(&call, &[94.0]);
    sent.extend(ticks(&put, &[110.0]));
    sent.extend(ticks(&call, &[89.0]));
    send_while_held(t, burst, &sent).await;
    eventually(|| !exit_kinds(t, run).is_empty()).await;
    settled(t).await;
    let exits = t
        .orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .map(|o| (o.leg_id, o.kind))
        .collect();
    (run, exits)
}

/// A stop that placed nothing (its request could not be saved) does not
/// take the leg exits decided after it down with it: the call's own stop,
/// hit after the combined stop while the run was back inside it, is
/// placed. Before, a leg exit decided behind a waiting stop was dropped,
/// and the call stayed open until a later price breached again.
#[tokio::test]
async fn a_leg_exit_decided_after_a_stop_that_placed_nothing_is_placed() {
    let (t, burst) = burst_module().await;
    let (run, exits) = stop_then_leg_exit(&t, &burst, true).await;
    assert_eq!(exits, vec![(1, "exit_sl".to_string())]);
    assert_eq!(
        t.run(run).stop_requested_reason,
        None,
        "the stop was not saved"
    );
    assert_eq!(t.leg(run, 2).status, "open");
    t.m.shutdown().await;
}

/// The same path with a stop that exits every leg: the call's own exit,
/// decided after the stop, is refused by the leg's claim, so each leg is
/// exited once, by the stop.
#[tokio::test]
async fn a_leg_exit_decided_after_a_stop_that_exited_the_leg_is_not_placed_again() {
    let (t, burst) = burst_module().await;
    let (run, exits) = stop_then_leg_exit(&t, &burst, false).await;
    let stop_kind = "exit_overall_sl".to_string();
    assert_eq!(exits, vec![(1, stop_kind.clone()), (2, stop_kind)]);
    assert_eq!(
        t.run(run).stop_requested_reason.as_deref(),
        Some("overall_sl")
    );
    t.m.shutdown().await;
}

/// Health counts the strategies whose exit or stop waits to be acted on,
/// not decisions that only move a level or record nothing, counts a run
/// being acted on once when a newer decision of it waits, and warns once
/// the oldest exit has waited past 15 seconds. Before, every price left a
/// decision (`runs_waiting` was rarely 0), a run taken and decided again
/// counted twice, and the status stayed pass while an exit sat behind a
/// broker call that did not answer.
#[tokio::test]
async fn health_says_how_long_a_decided_exit_has_waited() {
    let (t, burst) = burst_module().await;
    let (_, blocker) = held(&t, "Blocker", "OTM3", json!({"sl_pts": 10})).await;
    let (_, other) = held(&t, "Other", "ATM", json!({"sl_pts": 10})).await;
    let gate = Arc::new(Gate::default());
    *t.gw.place_gate.lock() = Some(gate.clone());
    burst.send(&blocker, 85.0);
    gate.entered().await;
    // Nothing to act on: another strategy's price inside its stop, and a
    // newer one for the strategy whose exit is being placed.
    let read = prices_read(&t);
    burst.send(&other, 100.5);
    burst.send(&blocker, 95.0);
    assert!(eventually(|| prices_read(&t) == read + 2).await);
    let h = health(&t);
    assert_eq!(h["exits_waiting"], 1, "{}", h);
    assert_eq!(h["status"], "pass", "{}", h);

    t.clock.advance(chrono::Duration::seconds(20));
    burst.send(&other, 85.0);
    assert!(eventually(|| prices_read(&t) == read + 3).await);
    let h = health(&t);
    assert_eq!(h["exits_waiting"], 2, "{}", h);
    assert_eq!(h["oldest_exit_wait_s"], 20, "{}", h);
    assert_eq!(h["exits_slow"], true, "{}", h);
    assert_eq!(h["status"], "warn", "{}", h);

    *t.gw.place_gate.lock() = None;
    gate.release();
    settled(&t).await;
    let h = health(&t);
    assert_eq!(h["oldest_exit_wait_s"], Value::Null, "{}", h);
    assert_eq!(h["status"], "pass", "{}", h);
    t.m.shutdown().await;
}

/// An exit that waits past 15 seconds to be placed raises a health alert
/// at the next sample, saying how long and for how many strategies, kept
/// current while it waits, and resolved by OpenAlgo once the exits are
/// placed. Before, only the Strategy Prices card turned amber while the
/// Health page was open, and nothing was listed for a trader who was not
/// looking at it.
#[tokio::test]
async fn a_slow_exit_raises_a_health_alert_until_it_is_placed() {
    use openalgo_desktop_lib::db::sqlite::{logs::LogsDb, monitor};
    use openalgo_desktop_lib::services::strategy_ticks;

    let dir = tempfile::tempdir().unwrap();
    let logs = LogsDb::new(&dir.path().join("logs.db")).unwrap();
    let (t, burst) = burst_module().await;
    // One health sample's alerts, and what is listed after it.
    let sample = |t: &T| {
        let c = logs.conn().unwrap();
        strategy_ticks::alert(&c, &t.m.feed, t.clock.now()).unwrap();
        monitor::active_alerts(&c).unwrap()
    };
    let (_, blocker) = held(&t, "Blocker", "OTM3", json!({"sl_pts": 10})).await;
    let (_, other) = held(&t, "Other", "ATM", json!({"sl_pts": 10})).await;
    let gate = Arc::new(Gate::default());
    *t.gw.place_gate.lock() = Some(gate.clone());
    burst.send(&blocker, 85.0);
    gate.entered().await;
    assert!(
        sample(&t).is_empty(),
        "an exit being placed now is not slow"
    );

    t.clock.advance(chrono::Duration::seconds(20));
    let open = sample(&t);
    assert_eq!(open.len(), 1, "{:?}", open);
    assert_eq!(open[0].alert_type, "strategy_exits_slow");
    assert_eq!(open[0].severity, "warn");
    assert_eq!(open[0].metric_value, 20.0);
    assert!(
        open[0]
            .message
            .starts_with("A strategy exit decided 20 seconds ago is not placed yet"),
        "{}",
        open[0].message
    );

    // A second strategy's exit waits behind the first: one alert, saying
    // what waits now.
    let read = prices_read(&t);
    burst.send(&other, 85.0);
    assert!(eventually(|| prices_read(&t) == read + 1).await);
    t.clock.advance(chrono::Duration::seconds(10));
    let open = sample(&t);
    assert_eq!(open.len(), 1, "{:?}", open);
    assert!(
        open[0].message.starts_with(
            "2 strategies have an exit that is not placed yet; the oldest was decided 30 seconds ago"
        ),
        "{}",
        open[0].message
    );

    *t.gw.place_gate.lock() = None;
    gate.release();
    settled(&t).await;
    assert!(sample(&t).is_empty(), "resolved once the exits are placed");
    t.m.shutdown().await;
}

/// The tick reader never waits on the database. With every pooled
/// connection held, so a read waits its 10 s for one, a run whose banked
/// P&L must be read for a new session and a run whose tick inputs were
/// forgotten get prices first; a third run's price after theirs is still
/// decided at once. Before, the reader read both on the spot and held every
/// run's prices behind each read, until the market-data channel overran.
/// The reads it asked for land once the database answers.
#[tokio::test]
async fn a_stalled_database_read_never_holds_another_runs_prices() {
    let burst = Burst::new(4096);
    let t = t_with(
        ist(2026, 10, 7, 10, 0),
        Some(burst.clone() as Arc<dyn PriceSource>),
    );
    // The reader and the evaluator only: the checkpoint writer and the
    // scheduler read the database on the runtime itself.
    openalgo_desktop_lib::strategy::tick_feed::start(&t.m);
    let risk = json!({"sl_pts": 10});
    let limit = json!({"daily_loss_limit_inr": 100000});
    let sid = t.make(config(
        "Banked",
        json!([long_call("ATM", risk.clone())]),
        limit,
    ));
    let banked_run = t.start_filled(sid, 100.0).await;
    let banked = t.leg(banked_run, 1).symbol;
    let (forgotten_run, forgotten) = held(&t, "Forgotten", "OTM1", risk.clone()).await;
    let (free_run, free) = held(&t, "Free", "OTM2", risk).await;
    t.m.tick_contexts.forget(forgotten_run);
    t.clock.set(ist(2026, 10, 8, 10, 0));
    let pool: Vec<_> = (0..8).map(|_| t.db.conn().unwrap()).collect();

    for symbol in [&banked, &forgotten, &free] {
        burst.send(symbol, 101.0);
    }
    let decided = |run: i64, ltp: f64| t.leg(run, 1).ltp == Some(ltp);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !decided(free_run, 101.0) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        decided(free_run, 101.0),
        "a price waited behind another run's database read"
    );
    assert!(
        decided(banked_run, 101.0),
        "judged banking nothing meanwhile"
    );
    assert!(
        !decided(forgotten_run, 101.0),
        "not judged without its inputs"
    );

    drop(pool);
    assert!(
        eventually(|| t.m.tick_contexts.len() == 3).await,
        "the read the reader asked for never landed"
    );
    burst.send(&forgotten, 102.0);
    assert!(eventually(|| decided(forgotten_run, 102.0)).await);
    t.m.shutdown().await;
}

/// A tick the reader itself misses (the channel overran it) is counted and
/// turns the strategy tick health to warn; the web logs it at debug only.
#[tokio::test]
async fn a_missed_tick_is_counted_for_health() {
    let burst = Burst::new(4);
    let t = t_with(
        ist(2026, 10, 7, 10, 0),
        Some(burst.clone() as Arc<dyn PriceSource>),
    );
    t.m.start().await;
    let (_, symbol) = held(&t, "Held", "ATM", json!({"sl_pts": 10})).await;
    // Sent without yielding: the reader cannot run in between.
    for _ in 0..20 {
        burst.send(&symbol, 100.5);
    }
    for _ in 0..500 {
        if health(&t)["ticks_missed"].as_u64().unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let h = health(&t);
    assert_eq!(h["ticks_missed"], 16, "{}", h);
    assert_eq!(h["status"], "warn");
    assert_eq!(t.m.feed.ticks_missed_unreported(), Some(16));
    assert_eq!(t.m.feed.ticks_missed_unreported(), None, "alerted once");
    t.m.shutdown().await;
}
