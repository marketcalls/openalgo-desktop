//! Exits: the worker a breach starts, placing the exit's child orders
//! (only what was neither filled nor is still working), recording the
//! attempt on the stop's row, and clearing the stop.

use super::{
    chunks, exit_chunk, run_mode, ChildState, ExitAttempt, ExitChild, ExitOutcome, Flatness, Gen,
    LegPosition, RiskMonitor, BOOK_LAG_GRACE_MS, SCALPING_STRATEGY, SL_UPDATE_EVENT,
};
use crate::risk::BreachReason;
use crate::scalping::store::SlState;
use crate::strategy::dispatch::{DispatchResult, OrderPayload};
use crate::strategy::tick_feed::Key;
use serde_json::json;
use std::sync::Arc;

/// The order's state after the broker's answer: working when accepted,
/// unconfirmed when it may be at the broker (never sent again until the
/// order reconciler settles it, LOG-08), none when refused.
fn answer_state(r: &DispatchResult) -> Option<ChildState> {
    if r.ok {
        Some(ChildState::Working)
    } else if r.uncertain {
        Some(ChildState::Unconfirmed)
    } else {
        None
    }
}

impl RiskMonitor {
    pub(super) async fn run_exit(
        self: Arc<Self>,
        key: String,
        gen: Gen,
        sl: SlState,
        reason: Option<BreachReason>,
        ltp: f64,
    ) {
        let outcome = self.execute_exit(&key, gen, &sl).await;
        let why = match reason {
            Some(BreachReason::Target) => "target",
            _ => "stop loss",
        };
        match &outcome {
            ExitOutcome::Flat => {
                tracing::info!(
                    "Scalping {} for {} found the position already flat; cleared",
                    why,
                    sl.symbol
                );
                self.clear(&key, gen, &sl).await;
            }
            ExitOutcome::Placed {
                orders,
                unconfirmed,
            } => {
                tracing::info!(
                    "Scalping {} exit for {} sent in {} order(s) near {:.2} ({} unconfirmed); the stop stays until the position is flat",
                    why,
                    sl.symbol,
                    orders + unconfirmed,
                    ltp,
                    unconfirmed
                );
            }
            ExitOutcome::Waiting => {
                tracing::debug!(
                    "Scalping {} for {}: exit orders already cover the position",
                    why,
                    sl.symbol
                );
            }
            ExitOutcome::Refused(msg) => {
                tracing::error!(
                    "Scalping {} exit for {} was refused and the position is still open; it stays protected and will be retried: {}",
                    why,
                    sl.symbol,
                    msg
                );
            }
            ExitOutcome::Stale => {
                tracing::info!(
                    "Scalping {} for {} was changed while its exit was being decided; nothing was sent",
                    why,
                    sl.symbol
                );
                // Nothing went out: the new stop decides without a cooldown.
                self.inner.lock().last_exit.remove(&key);
            }
        }
        // Release the claim only after the clear, so no tick in between can
        // send a second exit for a leg that was just closed.
        self.inner.lock().inflight.remove(&key);
    }

    /// A position the book does not show: while exit orders still work the
    /// reconciler follows them; otherwise the leg is not flat on the book's
    /// word alone (an empty or lagging book right after an entry, a stale
    /// zero row, an unreadable row) and stays protected.
    fn unknown_position(
        &self,
        key: &str,
        sl: &SlState,
        unreadable: bool,
        known: Option<&ExitAttempt>,
    ) -> ExitOutcome {
        if known.is_some_and(ExitAttempt::working) {
            return ExitOutcome::Waiting;
        }
        self.alert_once(
            key,
            "unknown_position",
            format!(
                "Scalping stop for {} {}: the broker's position book does not show this position{}, so OpenAlgo cannot tell whether it is still open. The stop stays and no exit is sent until the book shows it. Check the position at the broker.",
                sl.symbol,
                sl.exchange,
                if unreadable { " with a readable quantity" } else { "" }
            ),
        );
        ExitOutcome::Refused("The position book does not show this position yet".into())
    }

    /// A zero row with no exit orders of this leg is flat only once the book
    /// has shown zero for BOOK_LAG_GRACE_MS: right after an entry the book
    /// can still carry an earlier, closed row of the same contract at zero
    /// (review P1-4). True while that grace runs; any other row resets it.
    fn zero_row_too_new(&self, key: &str, zero_row: bool) -> bool {
        if !zero_row {
            self.inner.lock().zero_since.remove(key);
            return false;
        }
        let now = self.clock.now();
        let first = *self
            .inner
            .lock()
            .zero_since
            .entry(key.to_string())
            .or_insert(now);
        (now - first).num_milliseconds() < BOOK_LAG_GRACE_MS
    }

    /// How much to exit now and the attempt it continues, decided on the
    /// stop this exit was claimed for and only that one (LOG-06: the
    /// trader may have edited or replaced it meanwhile); or the outcome
    /// when nothing is to be sent.
    fn exit_quantity(
        &self,
        key: &str,
        gen: Gen,
        net: i64,
    ) -> Result<(i64, Option<ExitAttempt>), ExitOutcome> {
        let now = self.clock.now();
        let attempt = {
            let inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) != Some(gen) {
                return Err(ExitOutcome::Stale);
            }
            inner.attempts.get(key).cloned()
        };
        Ok(match attempt {
            Some(a) => {
                let same_side = a.net0.signum() == net.signum();
                if net == 0 {
                    if a.working() {
                        // The book is ahead of the order book: let the
                        // reconciler see the children end.
                        return Err(ExitOutcome::Waiting);
                    }
                    // `flatness` already refused a zero that the exit
                    // orders did not make.
                    return Err(ExitOutcome::Flat);
                }
                let left = a.unexited();
                if left > 0 && same_side {
                    (left.min(net.abs()), Some(a))
                } else if a.working() {
                    return Err(ExitOutcome::Waiting);
                } else if same_side
                    && a.settled_at
                        .is_some_and(|t| (now - t).num_milliseconds() < BOOK_LAG_GRACE_MS)
                {
                    // Every exit filled; the position book has not caught up.
                    return Err(ExitOutcome::Waiting);
                } else {
                    // The book has had time (or the position changed side):
                    // it is the position to protect now.
                    (net.abs(), None)
                }
            }
            None => {
                if net == 0 {
                    return Err(ExitOutcome::Flat);
                }
                (net.abs(), None)
            }
        })
    }

    /// Writes an exit order into the attempt before it is sent, as an
    /// order with no answer yet: a quit or crash while it is in flight
    /// leaves it unconfirmed and looked up in the book, never forgotten and
    /// sent again (review P2-5). Answers its index, or the refusal when it
    /// could not be saved (it is then not sent).
    fn remember_child(
        &self,
        key: &str,
        sl: &SlState,
        a: &mut ExitAttempt,
        (action, q, net): (&str, i64, i64),
        first: bool,
    ) -> Result<usize, String> {
        let pending = ExitChild {
            order_id: None,
            client_tag: None,
            action: action.to_string(),
            qty: q,
            filled: 0,
            state: ChildState::Unconfirmed,
            placed_at: self.clock.now(),
            net_before: Some(net),
        };
        a.children.push(pending.clone());
        a.settled_at = None;
        let idx = a.children.len() - 1;
        let (_, saved) = self.update_attempt(key, sl.id, |slot| match slot {
            Some(c) if c.id == a.id => {
                c.children.push(pending);
                c.settled_at = None;
            }
            _ => *slot = Some(a.clone()),
        });
        if !saved {
            // Not sent: it could not be remembered.
            a.children.pop();
            self.update_attempt(key, sl.id, |slot| {
                if let Some(c) = slot.as_mut().filter(|c| c.id == a.id) {
                    if c.children.len() == idx + 1 && c.children[idx].order_id.is_none() {
                        c.children.pop();
                    }
                }
            });
            self.alert_once(
                key,
                "persist",
                format!(
                    "Scalping stop for {}: its exit could not be saved, so {} was not sent. The stop stays; check the disk and the app's data folder, then check the order book at the broker.",
                    sl.symbol,
                    if first { "it" } else { "the rest of it" }
                ),
            );
            return Err("The exit could not be saved before sending".into());
        }
        Ok(idx)
    }

    /// Records the broker's answer on the order, in the row and in memory;
    /// a definite refusal never reached the broker, so that order is
    /// removed. Answers whether the row was written.
    fn record_answer(
        &self,
        key: &str,
        row_id: i64,
        a: &mut ExitAttempt,
        idx: usize,
        (state, r): (Option<ChildState>, &DispatchResult),
    ) -> bool {
        let order_id = r.broker_order_id.clone().filter(|i| !i.is_empty());
        let tag = r.client_tag.clone();
        // The answer goes onto that order, as it stands now (a reconcile
        // pass may already have found it in the book).
        let apply = |ch: &mut ExitChild| {
            let Some(st) = state else {
                return;
            };
            if ch.order_id.is_none() {
                ch.order_id = order_id.clone();
            }
            if ch.client_tag.is_none() {
                ch.client_tag = tag.clone();
            }
            if ch.state == ChildState::Unconfirmed && ch.order_id.is_some() {
                ch.state = st;
            }
        };
        let (_, saved) = self.update_attempt(key, row_id, |slot| {
            if let Some(c) = slot.as_mut().filter(|c| c.id == a.id) {
                match state {
                    // A definite refusal never reached the broker.
                    None => {
                        if c.children.len() == idx + 1 && c.children[idx].order_id.is_none() {
                            c.children.pop();
                        }
                    }
                    Some(_) => {
                        if let Some(ch) = c.children.get_mut(idx) {
                            apply(ch);
                        }
                    }
                }
            }
        });
        match state {
            None => {
                a.children.pop();
            }
            Some(_) => {
                if let Some(ch) = a.children.get_mut(idx) {
                    apply(ch);
                }
            }
        }
        saved
    }

    /// Exit orders were sent but could not be saved: the trader is told,
    /// with whether the rest of the exit was held back.
    fn alert_unsaved(&self, key: &str, sl: &SlState, rest_held: bool) {
        self.alert_once(
            key,
            "persist",
            format!(
                "Scalping stop for {}: exit orders were sent but could not be saved{}. The stop stays; check the order book at the broker.",
                sl.symbol,
                if rest_held { ", so the rest of the exit was not sent" } else { "" }
            ),
        );
    }

    /// Exit one leg in the mode it was opened in: read its net position,
    /// then send whole-lot, freeze-safe MARKET orders for what is neither
    /// filled nor still working from earlier exits of this leg.
    pub async fn execute_exit(&self, key: &str, gen: Gen, sl: &SlState) -> ExitOutcome {
        let mode = run_mode(&sl.mode);
        if let Err(e) = self.gateway.authorised(mode) {
            return ExitOutcome::Refused(e);
        }
        if self.inner.lock().broken.contains(key) {
            return ExitOutcome::Refused(
                "The exit orders sent earlier for this stop could not be read back".into(),
            );
        }
        let known = self.inner.lock().attempts.get(key).cloned();
        let (pos, flat) = match self.read_leg(mode, sl, known.as_ref()).await {
            Ok(r) => r,
            Err(msg) => return ExitOutcome::Refused(msg),
        };
        // A zero row with no exit orders of this leg: flat only once the
        // book has shown zero for BOOK_LAG_GRACE_MS. Right after an entry
        // the book can still carry an earlier, closed row of the same
        // contract at zero (review P1-4).
        if self.zero_row_too_new(key, pos == LegPosition::Net(0) && known.is_none()) {
            return ExitOutcome::Waiting;
        }
        let net = match flat {
            Flatness::Open(n) => n,
            Flatness::Flat => 0,
            Flatness::Unknown => {
                let unreadable = pos == LegPosition::Unreadable;
                return self.unknown_position(key, sl, unreadable, known.as_ref());
            }
        };
        let (qty, attempt) = match self.exit_quantity(key, gen, net) {
            Ok(q) => q,
            Err(outcome) => return outcome,
        };
        let action = if net > 0 { "SELL" } else { "BUY" };
        let chunk = match exit_chunk(&self.symbols, &self.freeze, &sl.symbol, &sl.exchange, qty) {
            Ok(c) => c,
            Err(e) => return ExitOutcome::Refused(e),
        };
        let mut a = attempt.unwrap_or_else(|| ExitAttempt {
            id: crate::strategy::state::new_position_ref(),
            net0: net,
            children: Vec::new(),
            settled_at: None,
        });
        let mut refused: Option<String> = None;
        let (mut accepted, mut unconfirmed) = (0usize, 0usize);
        let parts = chunks(qty, chunk);
        let last = parts.len().saturating_sub(1);
        for (i, q) in parts.into_iter().enumerate() {
            let order = OrderPayload {
                symbol: sl.symbol.clone(),
                exchange: sl.exchange.clone(),
                action: action.to_string(),
                quantity: q,
                product: sl.product.clone(),
                pricetype: "MARKET".into(),
                strategy: SCALPING_STRATEGY.into(),
            };
            let idx = match self.remember_child(key, sl, &mut a, (action, q, net), i == 0) {
                Ok(idx) => idx,
                Err(msg) => {
                    refused = Some(msg);
                    break;
                }
            };
            let r = self.gateway.place(mode, &order).await;
            let state = answer_state(&r);
            match state {
                Some(ChildState::Working) => accepted += 1,
                Some(_) => unconfirmed += 1,
                None => {}
            }
            let saved = self.record_answer(key, sl.id, &mut a, idx, (state, &r));
            if state.is_none() {
                refused = Some(
                    r.error
                        .unwrap_or_else(|| "The exit order was rejected".into()),
                );
                break;
            }
            if !saved {
                // The orders already sent are followed in memory; no more
                // are sent while they cannot be saved.
                self.alert_unsaved(key, sl, i < last);
                if i < last {
                    refused = Some("The exit orders could not be saved".into());
                }
                break;
            }
        }
        match refused {
            Some(msg) => ExitOutcome::Refused(msg),
            None => ExitOutcome::Placed {
                orders: accepted,
                unconfirmed,
            },
        }
    }

    /// Change a leg's attempt in memory and write it to the stop's row in
    /// the same hold, so two writers (a reconcile pass folding fills, an
    /// exit adding an order, the trader's "not placed") never overwrite
    /// each other's orders. Answers the attempt as it now stands and
    /// whether the row was written (memory has the change either way).
    pub(super) fn update_attempt(
        &self,
        key: &str,
        row_id: i64,
        change: impl FnOnce(&mut Option<ExitAttempt>),
    ) -> (Option<ExitAttempt>, bool) {
        let mut inner = self.inner.lock();
        let mut slot = inner.attempts.remove(key);
        change(&mut slot);
        let saved = match &slot {
            Some(a) => {
                inner.attempts.insert(key.to_string(), a.clone());
                self.persist_attempt(row_id, a)
            }
            None => true,
        };
        (slot, saved)
    }

    /// Forget a closed leg: its row, its state, its subscription when no
    /// other leg needs the instrument, then tell the terminal. Only for the
    /// stop the exit was decided on: a replacement is never touched.
    pub(super) async fn clear(&self, key: &str, gen: Gen, sl: &SlState) -> bool {
        {
            let inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) != Some(gen) {
                return false;
            }
        }
        match self.store.delete_sl_exact(gen.0, gen.1) {
            Ok(true) => {}
            Ok(false) => {
                // Edited since: the new stop stays, and decides next tick.
                return false;
            }
            Err(e) => {
                tracing::error!(
                    "Scalping stop for {} could not be removed: {}",
                    sl.symbol,
                    e
                );
            }
        }
        let symkey: Key = (sl.symbol.clone(), sl.exchange.clone());
        let unsub = {
            let mut inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) == Some(gen) {
                inner.states.remove(key);
            }
            inner.prune();
            let needed = inner.wanted().contains(&symkey);
            if !needed && inner.subscribed.remove(&symkey) {
                Some(symkey)
            } else {
                None
            }
        };
        if let (Some(src), Some(k)) = (&self.prices, unsub) {
            src.unsubscribe(std::slice::from_ref(&k)).await;
        }
        self.ui
            .emit(
                SL_UPDATE_EVENT,
                json!({
                    "symbol": sl.symbol,
                    "exchange": sl.exchange,
                    "product": sl.product,
                    "cleared": true,
                }),
            )
            .await;
        true
    }
}
