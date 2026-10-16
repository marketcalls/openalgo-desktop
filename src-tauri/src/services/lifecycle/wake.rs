//! Sleep and wake (ARCH-03, PLATFORM-05). While the computer sleeps the
//! monotonic clock stands still and the wall clock does not, so after a
//! sleep the wall clock has moved further than the monotonic one: that gap
//! is how a wake is noticed (a wall clock set forward looks the same, and
//! is handled the same).
//!
//! The schedulers recalculate from the injected clock on their own: each
//! acts on the slots that fell since its last look
//! (`clock::ist_slots_between`), once, or says it skipped them. What a
//! sleep leaves stale is redone here: the broker feeds reconnect at once
//! (their stall watchdog runs on the monotonic clock, so a socket that died
//! in the sleep would otherwise be noticed only after up to 90 s awake), the
//! order reconciler settles what changed at the broker meanwhile, and the
//! sandbox catches up across a session boundary.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior};

/// A wall clock this far ahead of the monotonic one is a wake.
pub const WAKE_GAP: Duration = Duration::from_secs(30);
/// How often the clocks are compared.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// The two clocks as last seen.
#[derive(Debug, Clone, Copy)]
pub struct GapWatch {
    wall: DateTime<Utc>,
    mono: Instant,
}

impl GapWatch {
    pub fn new(wall: DateTime<Utc>, mono: Instant) -> Self {
        Self { wall, mono }
    }

    /// How far the wall clock moved beyond the monotonic one since the last
    /// look, when that is more than [`WAKE_GAP`].
    pub fn look(&mut self, wall: DateTime<Utc>, mono: Instant) -> Option<Duration> {
        let wall_moved = (wall - self.wall).to_std().unwrap_or_default();
        let mono_moved = mono.saturating_duration_since(self.mono);
        *self = Self::new(wall, mono);
        let gap = wall_moved.saturating_sub(mono_moved);
        (gap > WAKE_GAP).then_some(gap)
    }
}

/// Watch for wakes (an owned task, stopped with the app).
pub fn start(ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    let token = ctx.shutdown.child_token();
    let mut watch = GapWatch::new(ctx.now(), Instant::now());
    ctx.spawn(async move {
        let mut tick = tokio::time::interval(LOOK_EVERY);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tick.tick() => {
                    let Some(ctx) = weak.upgrade() else { break };
                    if let Some(slept) = watch.look(ctx.now(), Instant::now()) {
                        woke(&ctx, slept).await;
                    }
                }
            }
        }
    });
}

/// Redo what a sleep of `slept` left stale.
pub async fn woke(ctx: &AppState, slept: Duration) {
    tracing::info!(
        "OpenAlgo was suspended for about {} min (the computer slept): reconnecting market data and checking orders",
        slept.as_secs().div_ceil(60)
    );
    ctx.websocket.reconnect();
    ctx.runtime.order_ws.reconnect();
    ctx.runtime.depth_ws.reconnect();
    ctx.reconciler.request_pass();
    match ctx.sandbox.catch_up().await {
        Ok(Some(r)) if r != Default::default() => {
            tracing::info!("Sandbox caught up after the sleep: {:?}", r)
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("The sandbox could not catch up after the sleep: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wall_clock_ahead_of_the_monotonic_one_is_a_wake() {
        let start = Utc::now();
        let mono = Instant::now();
        let mut w = GapWatch::new(start, mono);
        // Ticking along: both clocks move together.
        let s = Duration::from_secs(1);
        assert_eq!(w.look(start + chrono::Duration::seconds(1), mono + s), None);
        // A 20 s hiccup is not a wake.
        assert_eq!(
            w.look(start + chrono::Duration::seconds(22), mono + 2 * s),
            None
        );
        // An hour asleep: the monotonic clock moved one second.
        let woke = start + chrono::Duration::seconds(22 + 3600);
        assert_eq!(w.look(woke, mono + 3 * s), Some(Duration::from_secs(3599)));
        // Seen once; then the clocks agree again.
        assert_eq!(
            w.look(woke + chrono::Duration::seconds(1), mono + 4 * s),
            None
        );
        // A wall clock set back is not a wake.
        assert_eq!(w.look(start, mono + 5 * s), None);
    }
}
