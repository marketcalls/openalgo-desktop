//! Broker time inside one recorded request (OBS-03): the desktop's form of
//! the web's httpx hook, which stores each broker call's duration in
//! Flask's `g` for `utils/latency_monitor.py`.
//!
//! The monitor middleware runs a recorded request inside [`measure`]; the
//! order service wraps each broker order call (place, modify, cancel,
//! cancel all, close all) in [`broker_call`]. Broker time is the time at
//! least one such call was in flight, so the legs of a basket sent at once
//! are not counted twice and the platform's overhead (the rest of the
//! request) is never negative. Outside a recorded request, or in a task the
//! request spawned, `broker_call` only runs the call.

use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

tokio::task_local! {
    static PROBE: Arc<Mutex<Probe>>;
}

#[derive(Debug, Default)]
struct Probe {
    in_flight: u32,
    busy_since: Option<Instant>,
    busy: Duration,
    calls: u32,
    broker: Option<String>,
    signed_in: bool,
}

/// What the probe saw during one request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrokerTime {
    /// Broker order calls made.
    pub calls: u32,
    /// Time at least one of them was in flight.
    pub busy: Duration,
    /// The broker the first one went to: the one that dispatched (OBS-06),
    /// whatever the session says once the reply is written.
    pub broker: Option<String>,
    /// The request passed the signed-in user check ([`signed_in`]), so a
    /// page's order reached its handler.
    pub signed_in: bool,
}

/// Run `fut` (one request) with a probe, and report the broker time it saw.
pub async fn measure<F: Future>(fut: F) -> (F::Output, BrokerTime) {
    let probe = Arc::new(Mutex::new(Probe::default()));
    let out = PROBE.scope(probe.clone(), fut).await;
    let p = probe.lock();
    let seen = BrokerTime {
        calls: p.calls,
        busy: p.busy,
        broker: p.broker.clone(),
        signed_in: p.signed_in,
    };
    (out, seen)
}

/// The request in scope passed the signed-in user check
/// (`middleware::require_user`): what comes after is its handler. The web
/// records a page's order only there, its decorator sitting under the
/// session and CSRF checks; a request they refuse is not an order.
pub fn signed_in() {
    let _ = PROBE.try_with(|p| p.lock().signed_in = true);
}

/// Run one broker order call to `broker`, timed for the request in scope.
pub async fn broker_call<F: Future>(broker: &str, call: F) -> F::Output {
    let Ok(probe) = PROBE.try_with(Arc::clone) else {
        return call.await;
    };
    let _in_flight = InFlight::enter(probe, broker);
    call.await
}

/// One call in flight; leaving (also when the request is dropped
/// mid-call) adds the busy time once the last call has ended.
struct InFlight(Arc<Mutex<Probe>>);

impl InFlight {
    fn enter(probe: Arc<Mutex<Probe>>, broker: &str) -> Self {
        {
            let mut p = probe.lock();
            if p.in_flight == 0 {
                p.busy_since = Some(Instant::now());
            }
            p.in_flight += 1;
            p.calls += 1;
            if p.broker.is_none() {
                p.broker = Some(broker.to_string());
            }
        }
        Self(probe)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut p = self.0.lock();
        p.in_flight = p.in_flight.saturating_sub(1);
        if p.in_flight == 0 {
            if let Some(since) = p.busy_since.take() {
                p.busy += since.elapsed();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::outcome::{self, classify, PlaceOutcome};
    use crate::brokers::types::OrderResponse;
    use crate::error::{AppError, Result};

    async fn wait(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn broker_time_is_the_time_a_call_was_in_flight() {
        let ((), seen) = measure(async {
            wait(30).await; // platform work before the call
            broker_call("zerodha", wait(100)).await;
            wait(20).await; // and after it
        })
        .await;
        assert_eq!(seen.calls, 1);
        assert_eq!(seen.busy, Duration::from_millis(100));
        assert_eq!(seen.broker.as_deref(), Some("zerodha"));
    }

    /// Basket legs sent together count once: the busy span, not the sum.
    #[tokio::test(start_paused = true)]
    async fn calls_in_flight_together_count_once() {
        let ((), seen) = measure(async {
            futures_util::future::join3(
                broker_call("dhan", wait(100)),
                broker_call("dhan", wait(60)),
                async {
                    wait(80).await;
                    broker_call("dhan", wait(50)).await;
                },
            )
            .await;
        })
        .await;
        assert_eq!(seen.calls, 3);
        assert_eq!(seen.busy, Duration::from_millis(130));
    }

    /// One kind of answer an adapter gives inside a placement (LOG-08).
    async fn adapter_answer(kind: usize) -> Result<OrderResponse> {
        match kind {
            0 => Ok(OrderResponse {
                order_id: "A1".into(),
                message: None,
            }),
            // A readable refusal.
            1 => Err(outcome::refusal("Insufficient funds")),
            // A refusal after a server error in the same placement.
            2 => {
                outcome::note_server_error();
                Err(outcome::refusal("Order rejected"))
            }
            // An unmarked error after sending.
            3 => Err(AppError::Broker("connection reset".into())),
            // A step before sending failed: not sent.
            4 => outcome::before_send(async { Err(AppError::Broker("quote failed".into())) }).await,
            // A session error after a server error (review P2-4).
            _ => {
                outcome::note_server_error();
                Err(AppError::Auth("session expired".into()))
            }
        }
    }

    /// Each answer's outcome, as LOG-08 classifies it.
    fn kind_of(o: &PlaceOutcome) -> &'static str {
        match o {
            PlaceOutcome::Accepted { .. } => "accepted",
            PlaceOutcome::Refused { .. } => "refused",
            PlaceOutcome::Uncertain { .. } => "uncertain",
        }
    }

    /// The probe and `outcome::placing` are separate task-local scopes: a
    /// placement keeps its outcome through `broker_call`, whichever of the
    /// two wraps the other, and the wrapper hands back the call's own
    /// answer.
    #[tokio::test]
    async fn a_wrapped_placement_keeps_its_outcome() {
        let want = [
            "accepted",
            "refused",
            "uncertain",
            "uncertain",
            "refused",
            "uncertain",
        ];
        for (kind, want) in want.into_iter().enumerate() {
            let bare = classify(&outcome::placing(adapter_answer(kind)).await);
            assert_eq!(kind_of(&bare), want, "kind {}", kind);
            let (inner, seen) = measure(outcome::placing(broker_call(
                "zerodha",
                adapter_answer(kind),
            )))
            .await;
            assert_eq!(
                classify(&inner),
                bare,
                "probe inside the placement, kind {}",
                kind
            );
            assert_eq!(seen.calls, 1);
            let (outer, _) = measure(broker_call(
                "zerodha",
                outcome::placing(adapter_answer(kind)),
            ))
            .await;
            assert_eq!(
                classify(&outer),
                bare,
                "placement inside the probe, kind {}",
                kind
            );
        }
    }

    #[tokio::test]
    async fn outside_a_request_a_call_just_runs() {
        assert_eq!(broker_call("zerodha", async { 7 }).await, 7);
    }
}
