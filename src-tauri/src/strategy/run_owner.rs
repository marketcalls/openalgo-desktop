//! A Live run's orders go only through the broker session it was opened on
//! (LOG-07, defence in depth; `services::live_owner`).
//!
//! The run row records its broker and account at start. Every order,
//! cancel and status read of a run goes through
//! [`StrategyModule::place_for_run`], [`StrategyModule::cancel_for_run`] or
//! [`StrategyModule::order_status_for_run`], which refuse without sending
//! when the session connected now is another broker or account. A refused
//! exit releases its claim and the run stays open and managed, as for any
//! refused exit; risk and Stop check [`StrategyModule::authorised_for`]
//! first and say so once, as they do for a missing session.
//!
//! A signal run opened before the day's login records `live` in place of a
//! broker. Its first Live order binds it to the session placing it, once
//! (web `broker_binding.py`, marketcalls/openalgo#2337); a check that only
//! asks never binds a run that has sent nothing. One that already sent
//! orders before it recorded its broker (open across the upgrade that added
//! the binding) is bound to the session that took them, which is the one
//! connected now only if it was signed in before the first of them: one
//! broker is logged in at a time, so that session was in place then.
//! Otherwise nothing is known and nothing is sent.

use super::dispatch::{DispatchResult, OrderPayload, OrderStatusResult, RunMode};
use super::StrategyModule;
use crate::services::live_owner::LiveOwner;
use chrono::{DateTime, Utc};

/// What an order of a run answers when the run's owner cannot be read.
const OWNER_UNREADABLE: &str = "OpenAlgo could not read which broker account this run belongs to, so the order was not sent. Try again.";

/// What an order of a run answers when it sent orders before recording its
/// broker and the broker that took them cannot be told (web
/// `binding_refusal` for an unbound run).
const BROKER_UNKNOWN: &str = "OpenAlgo could not confirm which broker this strategy's run trades on, so nothing was sent. Try again; if this repeats, check the run's positions at your broker.";

impl StrategyModule {
    /// Why an order of this run must not go through the session connected
    /// now, or `None` when it may (a Sandbox run, or the broker and account
    /// the run was opened on). Asking never binds a run that has sent
    /// nothing.
    pub(crate) fn owner_refusal(&self, run_id: i64, mode: RunMode) -> Option<String> {
        match mode {
            RunMode::Live => self.live_refusal(run_id, false),
            RunMode::Sandbox => None,
        }
    }

    /// Whether this run can send orders now: a session for its side, and
    /// for Live the one it was opened on.
    pub(crate) fn authorised_for(&self, run_id: i64, mode: RunMode) -> Result<(), String> {
        self.gateway.authorised(mode)?;
        self.owner_refusal(run_id, mode).map_or(Ok(()), Err)
    }

    /// Place one order of a run, unless it belongs to another broker
    /// session: then nothing is sent (a definite refusal, LOG-08). The
    /// first Live order of a run opened before any login binds it.
    pub(crate) async fn place_for_run(
        &self,
        run_id: i64,
        mode: RunMode,
        order: &OrderPayload,
    ) -> DispatchResult {
        let refused = match mode {
            RunMode::Live => self.live_refusal(run_id, true),
            RunMode::Sandbox => None,
        };
        if let Some(why) = refused {
            tracing::warn!(
                "Run {}: {} {} {} was not sent: {}",
                run_id,
                order.action,
                order.quantity,
                order.symbol,
                why
            );
            return DispatchResult::refused(why);
        }
        self.gateway.place(mode, order).await
    }

    /// Cancel one order of a run, under the same rule.
    pub(crate) async fn cancel_for_run(
        &self,
        run_id: i64,
        mode: RunMode,
        broker_order_id: &str,
    ) -> DispatchResult {
        if let Some(why) = self.owner_refusal(run_id, mode) {
            return DispatchResult::refused(why);
        }
        self.gateway.cancel(mode, broker_order_id).await
    }

    /// Read one order of a run, under the same rule: its answer is folded
    /// into the run as a fill or a cancellation, so a fact about the same
    /// order id read from another broker would describe a position the run
    /// does not hold (web `fetch_order_status` with the run's binding).
    pub(crate) async fn order_status_for_run(
        &self,
        run_id: i64,
        mode: RunMode,
        broker_order_id: &str,
    ) -> OrderStatusResult {
        if let Some(why) = self.owner_refusal(run_id, mode) {
            return OrderStatusResult {
                ok: false,
                order: serde_json::Value::Null,
                error: Some(why),
            };
        }
        self.gateway.order_status(mode, broker_order_id).await
    }

    /// The refusal for a Live run; `first_order` binds a run that has sent
    /// nothing to the session placing it.
    fn live_refusal(&self, run_id: i64, first_order: bool) -> Option<String> {
        let owner = match self.recorded_owner(run_id) {
            Ok(Some(owner)) => owner,
            Ok(None) => return None,
            Err(why) => return Some(why),
        };
        let active = LiveOwner::active(self.gateway.as_ref());
        if owner.knows_broker() {
            return owner.refusal(active.as_ref());
        }
        match self.store.first_dispatched_at(run_id) {
            // Nothing at any broker yet: nothing a session could reach
            // wrongly. Its first order binds it; with no session that order
            // is refused for that reason by the caller.
            Ok(None) => match active.filter(|_| first_order) {
                Some(active) => self.bind_and_judge(run_id, &active, "its first Live order"),
                None => None,
            },
            Ok(Some(first)) => self.bind_earlier_orders(run_id, first, active.as_ref()),
            Err(e) => {
                tracing::error!("Could not read the orders of run {}: {}", run_id, e);
                Some(OWNER_UNREADABLE.into())
            }
        }
    }

    /// The broker session the run recorded; `Err` with the trader's
    /// sentence when the run cannot be read.
    fn recorded_owner(&self, run_id: i64) -> Result<Option<LiveOwner>, String> {
        match self.store.get_run(run_id) {
            Ok(run) => Ok(run.map(|r| LiveOwner::new(r.broker, r.broker_account))),
            Err(e) => {
                tracing::error!("Could not read the broker of run {}: {}", run_id, e);
                Err(OWNER_UNREADABLE.into())
            }
        }
    }

    /// An unbound run that sent orders before recording its broker: bound
    /// to the session connected now when that session was signed in before
    /// the first of them; otherwise refused, nothing known.
    fn bind_earlier_orders(
        &self,
        run_id: i64,
        first: DateTime<Utc>,
        active: Option<&LiveOwner>,
    ) -> Option<String> {
        // No session: the caller's own session check refuses.
        let active = active?;
        let took_them = self
            .gateway
            .session_since(RunMode::Live)
            .is_some_and(|since| since <= first);
        if !took_them {
            tracing::warn!(
                "Run {} sent orders before recording its broker; the broker that took them is not known",
                run_id
            );
            return Some(BROKER_UNKNOWN.into());
        }
        self.bind_and_judge(run_id, active, "the orders it sent earlier")
    }

    /// Record `active` as the run's broker session unless another caller
    /// did first, then judge by whichever bind won (a racing order keeps
    /// the binding it won).
    fn bind_and_judge(&self, run_id: i64, active: &LiveOwner, by: &str) -> Option<String> {
        match self.store.bind_run_owner(run_id, active) {
            Ok(true) => {
                tracing::info!("Run {} bound to broker {} by {}", run_id, active.broker, by)
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("Could not record the broker of run {}: {}", run_id, e);
                return Some(OWNER_UNREADABLE.into());
            }
        }
        match self.recorded_owner(run_id) {
            Ok(Some(owner)) if owner.knows_broker() => owner.refusal(Some(active)),
            // Stopped meanwhile, or not bound: nothing to send it through.
            Ok(_) => Some(BROKER_UNKNOWN.into()),
            Err(why) => Some(why),
        }
    }
}
