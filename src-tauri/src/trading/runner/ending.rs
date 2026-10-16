//! Ending a run: pause (the position stays), close (Stop: the run's
//! position is closed on the side it traded, then the run ends, or its
//! close stays pending), and removing a deployment.

use super::inbox::Message;
use super::orders::Target;
use super::{books, orders, store};
use super::{
    side_name, side_words, CloseFailure, Refusal, RunEntry, RunInfo, Runner, BUSY_DELETE_REFUSAL,
    CLOSE_PENDING_TAIL, CLOSE_TIMEOUT_MESSAGE, CLOSING_MESSAGE, NOTHING_CLOSED,
    RUNNING_DELETE_REFUSAL,
};
use crate::strategy::dispatch::{Book, RunMode};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Runner {
    /// Close the page, stop the pump, and say so in the run's log.
    fn teardown(&self, entry: &RunEntry, why: &str) {
        entry.halted.store(true, Ordering::SeqCst);
        entry.inbox.push(Message::Halt);
        entry.inbox.close();
        entry.cancel.cancel();
        if let Some(h) = entry.pump.lock().take() {
            h.abort();
        }
        if let Some(host) = self.host.read().clone() {
            host.close(&entry.info.run_id);
        }
        self.write_log(&entry.info, &[format!("=== {} ===", why)]);
    }

    /// End a run and leave its position exactly where it is.
    pub fn pause(&self, given: &str) -> Result<String, Refusal> {
        self.pause_for(given, true, "Paused")
    }

    fn pause_for(
        &self,
        given: &str,
        refuse_while_closing: bool,
        why: &str,
    ) -> Result<String, Refusal> {
        let run_id = self.as_run_id(given);
        let entry = {
            let mut reg = self.reg.lock();
            if reg.stopping.contains(&run_id) {
                return Err(Refusal::new(409, "That run is already stopping"));
            }
            if refuse_while_closing && reg.closing.contains(&run_id) {
                return Err(Refusal::new(409, CLOSING_MESSAGE));
            }
            let Some(entry) = reg.runs.remove(&run_id) else {
                return Err(Refusal::new(409, "That run is not running"));
            };
            reg.stopping.insert(run_id.clone());
            entry
        };
        self.teardown(&entry, why);
        self.reg.lock().stopping.remove(&run_id);
        tracing::info!("The OpenScript run {} was paused", run_id);
        Ok(format!("{} paused", entry.info.script))
    }

    /// Close what the run holds, then end it. A close that did not happen
    /// leaves the run running and managed: its page is resumed, so the
    /// script sends its own exits again. A close that left an order of this
    /// run working (an exit not yet filled, an entry whose cancel the broker
    /// has not confirmed) leaves the run close pending instead: halted, so
    /// nothing new is sent beside an order that may still fill, until Stop is
    /// pressed again or the run is paused.
    pub async fn close(&self, given: &str) -> Result<String, Refusal> {
        let run_id = self.as_run_id(given);
        let entry = {
            let mut reg = self.reg.lock();
            if reg.stopping.contains(&run_id) {
                return Err(Refusal::new(409, "That run is already stopping"));
            }
            if reg.closing.contains(&run_id) {
                return Err(Refusal::new(409, CLOSING_MESSAGE));
            }
            let Some(entry) = reg.runs.get(&run_id).cloned() else {
                return Err(Refusal::new(409, "That run is not running"));
            };
            reg.closing.insert(run_id.clone());
            entry
        };
        let outcome = self.close_claimed(&entry).await;
        let mut reg = self.reg.lock();
        reg.closing.remove(&run_id);
        match outcome {
            Ok(()) => {
                if let Some(e) = reg.runs.remove(&run_id) {
                    drop(reg);
                    self.teardown(&e, "Closed and stopped");
                }
                tracing::info!("Closed and stopped the OpenScript run {}", run_id);
                Ok(format!("{} closed and stopped", entry.info.script))
            }
            Err(failed) => {
                drop(reg);
                if failed.pending {
                    entry.close_pending.store(true, Ordering::SeqCst);
                } else {
                    entry.close_pending.store(false, Ordering::SeqCst);
                    entry.halted.store(false, Ordering::SeqCst);
                    entry.inbox.push(Message::Resume);
                }
                self.write_log(
                    &entry.info,
                    &[format!("The close did not complete: {}", failed.message)],
                );
                Err(Refusal::new(409, failed.message))
            }
        }
    }

    async fn close_claimed(&self, entry: &Arc<RunEntry>) -> Result<(), CloseFailure> {
        // Nothing new is sent from here on.
        entry.halted.store(true, Ordering::SeqCst);
        entry.inbox.push(Message::Halt);
        let _held = entry.order_lock.lock().await;
        let info = &entry.info;
        let (wait, look) = {
            let o = self.options.read();
            (o.close_wait, o.close_look)
        };
        let deadline = Instant::now() + wait;

        // Nothing of this run may still be working when it measures itself.
        self.settle(entry, deadline, look).await?;

        // What its own fills add up to, capped by what the destination holds.
        let held: Vec<store::Holding> = store::holdings(
            &self
                .read_own(info)
                .map_err(|m| CloseFailure::resume(format!("{} {}", m, NOTHING_CLOSED)))?,
        )
        .into_iter()
        .filter(|h| h.quantity != 0)
        .collect();
        let sizes = self
            .exit_sizes(info, &held, deadline, look)
            .await
            .map_err(CloseFailure::resume)?;
        let exits = self.send_exits(entry, &sizes).await?;
        if exits.is_empty() {
            return Ok(());
        }
        self.watch_exits(entry, &exits, deadline, look).await
    }

    /// Send the exit of each holding, in turn. A refusal before any exit
    /// went out resumes the run; after one, or with an exit whose outcome
    /// is unknown, the close stays pending.
    async fn send_exits(
        &self,
        entry: &Arc<RunEntry>,
        sizes: &[(&store::Holding, i64)],
    ) -> Result<Vec<i64>, CloseFailure> {
        let info = &entry.info;
        let mut exits = Vec::new();
        for &(h, units) in sizes {
            let placement = orders::Placement {
                intent_id: -1,
                tag: "close".into(),
                action: if h.quantity > 0 { "SELL" } else { "BUY" },
                quantity: units,
                pricetype: "MARKET",
                price: None,
                trigger_price: None,
            };
            let target = Target {
                strategy: &info.run_id,
                symbol: &h.symbol,
                exchange: &h.exchange,
                product: &h.product,
            };
            // An exit already out may still fill: the run waits, it does not resume.
            let failed = |m: String| {
                if exits.is_empty() {
                    CloseFailure::resume(m)
                } else {
                    CloseFailure::pending(format!("{} {}", m, CLOSE_PENDING_TAIL))
                }
            };
            let (id, result) = match self
                .send(info, &target, &placement, Some(entry.as_ref()), true)
                .await
            {
                Ok(sent) => sent,
                Err(e) => {
                    return Err(failed(format!(
                        "This strategy's position could not be closed: {}.",
                        e
                    )))
                }
            };
            if result.uncertain {
                // It may be at the broker: a Stop pressed again must not send
                // a second one (LOG-08). The order book settles it.
                return Err(CloseFailure::pending(format!(
                    "The {} did not confirm the order closing this strategy's position, so it may have been placed. It is not sent again until the order book settles it. {}",
                    if info.mode == RunMode::Live {
                        "broker"
                    } else {
                        "sandbox"
                    },
                    CLOSE_PENDING_TAIL
                )));
            }
            if !result.ok {
                return Err(failed(format!(
                    "This strategy's position could not be closed: {}.{}",
                    result
                        .error
                        .unwrap_or_else(|| "the order was refused".into()),
                    if exits.is_empty() {
                        " It is still running and still holding it. Close the position yourself, or try again."
                    } else {
                        ""
                    }
                )));
            }
            exits.push(id);
        }
        Ok(exits)
    }

    /// Watch the exits to a fill, not sent and forgotten: an exit accepted
    /// here and rejected at the destination leaves the very position Stop
    /// was pressed to be rid of.
    async fn watch_exits(
        &self,
        entry: &Arc<RunEntry>,
        exits: &[i64],
        deadline: Instant,
        look: Duration,
    ) -> Result<(), CloseFailure> {
        let info = &entry.info;
        loop {
            self.refresh_orders_until(entry, Some(deadline)).await;
            let rows = self
                .read_own(info)
                .map_err(|m| CloseFailure::pending(format!("{} {}", m, CLOSE_PENDING_TAIL)))?;
            let mine: Vec<&store::OrderRow> =
                rows.iter().filter(|r| exits.contains(&r.id)).collect();
            if mine.iter().all(|r| r.status == "filled") {
                return Ok(());
            }
            if let Some(r) = mine
                .iter()
                .find(|r| matches!(r.status.as_str(), "rejected" | "cancelled" | "expired"))
            {
                let working = mine
                    .iter()
                    .any(|o| !store::TERMINAL.contains(&o.status.as_str()));
                let filled_some = mine.iter().any(|o| o.filled_quantity > 0);
                if working || filled_some {
                    // Part of the close happened, or may still: the position
                    // is no longer what the script believes it holds.
                    return Err(CloseFailure::pending(format!(
                        "The order closing this strategy's position was {} after {} of {} filled. {}",
                        r.status, r.filled_quantity, r.quantity, CLOSE_PENDING_TAIL
                    )));
                }
                return Err(CloseFailure::resume(format!(
                    "The order closing this strategy's position was {}. It is still running and still holding it. Close the position yourself, or try again.",
                    r.status
                )));
            }
            if Instant::now() >= deadline {
                return Err(CloseFailure::pending(CLOSE_TIMEOUT_MESSAGE));
            }
            tokio::time::sleep(look).await;
        }
    }

    /// Cancel every order of this run still working, then wait until the
    /// destination says each is finished, folding what came of it, so a
    /// fill that lands during the Stop is counted. A cancel request is
    /// intent, not proof. An order still working, or unreadable, at the
    /// deadline leaves the run close pending, because its fill may still
    /// come and the size to close is not known.
    ///
    /// **An order this run cannot follow blocks the close.** A row still
    /// open with no number from the destination (its answer could not be
    /// saved, the app stopped between sending it and saving the answer, or
    /// Semi-Auto holds it for approval) may fill, and its fill would never
    /// be counted, so the close does not go ahead, before anything is
    /// cancelled. The row is in the ledger, so this holds across a restart.
    async fn settle(
        &self,
        entry: &RunEntry,
        deadline: Instant,
        look: Duration,
    ) -> Result<(), CloseFailure> {
        let info = &entry.info;
        let services = self.services();
        self.refresh_orders_until(entry, Some(deadline)).await;
        // An order whose answer is not saved cannot be followed, so its fill
        // could not be counted: nothing is measured until it is saved.
        let unsaved: Vec<String> = entry
            .unsaved
            .lock()
            .iter()
            .map(|a| {
                a.orderid
                    .clone()
                    .unwrap_or_else(|| "without a number".into())
            })
            .collect();
        if !unsaved.is_empty() {
            return Err(CloseFailure::pending(format!(
                "The answer for order {} of this strategy has not been saved yet, so its fill cannot be counted and nothing was closed. {}",
                unsaved.join(", "),
                CLOSE_PENDING_TAIL
            )));
        }
        let rows = self
            .read_own(info)
            .map_err(|m| CloseFailure::resume(format!("{} {}", m, NOTHING_CLOSED)))?;
        let working: Vec<&store::OrderRow> = rows
            .iter()
            .filter(|o| !store::TERMINAL.contains(&o.status.as_str()))
            .collect();
        let unnumbered: Vec<String> = working
            .iter()
            .filter(|o| o.orderid.is_none())
            .map(|o| format!("{} {} {}", o.action, o.quantity, o.symbol))
            .collect();
        if !unnumbered.is_empty() {
            let from = if info.mode == RunMode::Live {
                "your broker"
            } else {
                "the sandbox"
            };
            self.write_log(
                info,
                &[format!(
                    "Not closed: {} of this strategy {} no order number from {}, so {} cannot be followed.",
                    unnumbered.join(", "),
                    if unnumbered.len() == 1 { "has" } else { "have" },
                    from,
                    if unnumbered.len() == 1 { "it" } else { "they" }
                )],
            );
            return Err(CloseFailure::pending(format!(
                "The order {} of this strategy has no order number from {} (it was not confirmed, its answer was not saved, or it is waiting for approval in the Action Center), so this close cannot follow it and nothing was closed. {}",
                unnumbered.join(", "),
                from,
                CLOSE_PENDING_TAIL
            )));
        }
        for o in &working {
            if let Some(oid) = o.orderid.as_deref() {
                let r = services.cancel(info.mode, oid).await;
                self.write_log(
                    info,
                    &[if r.ok {
                        format!("Asked for order {} to be cancelled before closing.", oid)
                    } else {
                        format!(
                            "Order {} could not be cancelled this time: {}",
                            oid,
                            r.error.unwrap_or_default()
                        )
                    }],
                );
            }
        }
        if working.is_empty() {
            return Ok(());
        }
        self.wait_settled(entry, deadline, look).await
    }

    /// Wait until no order of this run is working (each cancel asked for
    /// has ended), or the deadline.
    async fn wait_settled(
        &self,
        entry: &RunEntry,
        deadline: Instant,
        look: Duration,
    ) -> Result<(), CloseFailure> {
        let info = &entry.info;
        loop {
            self.refresh_orders_until(entry, Some(deadline)).await;
            let still: Vec<String> = self
                .read_own(info)
                .map_err(|m| CloseFailure::pending(format!("{} {}", m, CLOSE_PENDING_TAIL)))?
                .into_iter()
                .filter(|o| !store::TERMINAL.contains(&o.status.as_str()))
                .filter_map(|o| o.orderid)
                .collect();
            if still.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CloseFailure::pending(format!(
                    "{} {} of this strategy {} still working or could not be read, so {} may still come and nothing was closed. {}",
                    if still.len() == 1 { "Order" } else { "Orders" },
                    still.join(", "),
                    if still.len() == 1 { "is" } else { "are" },
                    if still.len() == 1 { "its fill" } else { "their fills" },
                    CLOSE_PENDING_TAIL
                )));
            }
            tokio::time::sleep(look).await;
        }
    }

    /// What the destination itself holds in each contract the run's own fills
    /// hold, in `held`'s order. Read again until the close deadline while a
    /// book cannot be read or does not mention a contract: a book that has
    /// not caught up with a fill is not a flat account (LOG-04b). Still
    /// unknown at the deadline, nothing is sent and the run resumes.
    async fn destination_sizes(
        &self,
        info: &RunInfo,
        held: &[store::Holding],
        deadline: Instant,
        look: Duration,
    ) -> Result<Vec<i64>, String> {
        let services = self.services();
        let delivery = held.iter().any(|h| h.product.eq_ignore_ascii_case("CNC"));
        loop {
            let read = async {
                let positions = services
                    .book(info.mode, Book::Positions)
                    .await
                    .map_err(|e| ("positions", e))?;
                let holdings = if delivery {
                    Some(
                        services
                            .holdings(info.mode)
                            .await
                            .map_err(|e| ("holdings", e))?,
                    )
                } else {
                    None
                };
                Ok::<_, (&str, Value)>((positions, holdings))
            }
            .await;
            let last_try = Instant::now() >= deadline;
            match read {
                Ok((positions, holdings)) => {
                    let sizes: Vec<Option<i64>> = held
                        .iter()
                        .map(|h| {
                            books::destination_net(
                                &positions,
                                holdings.as_ref(),
                                &h.symbol,
                                &h.exchange,
                                &h.product,
                            )
                        })
                        .collect();
                    match sizes.iter().position(Option::is_none) {
                        None => return Ok(sizes.into_iter().flatten().collect()),
                        Some(n) if last_try => {
                            let h = &held[n];
                            self.write_log(
                                info,
                                &[format!(
                                    "Your positions {} do not show {} {} {}, though this strategy's own orders add up to {}. Nothing was sent.",
                                    side_words(info.mode),
                                    h.product,
                                    h.symbol,
                                    h.exchange,
                                    h.quantity
                                )],
                            );
                            return Err(format!(
                                "Your positions {} do not show {} yet, though this strategy's own orders hold {} {}, so nothing was sent and nothing was closed. It is still running and still holding its position. Try Stop again in a moment. If you have already closed it yourself, Pause the strategy instead.",
                                side_words(info.mode),
                                h.symbol,
                                side_name(h.quantity),
                                h.quantity.unsigned_abs()
                            ));
                        }
                        Some(_) => {}
                    }
                }
                Err((what, e)) if last_try => {
                    self.write_log(
                        info,
                        &[format!(
                            "Your {} {} could not be read: {}",
                            what,
                            side_words(info.mode),
                            e.get("message").and_then(Value::as_str).unwrap_or_default()
                        )],
                    );
                    return Err(format!(
                        "Your {} {} could not be read just now, so nothing was sent to close this strategy's position. It is still running and still holding it. Try Stop again in a moment.",
                        what,
                        side_words(info.mode)
                    ));
                }
                Err(_) => {}
            }
            tokio::time::sleep(look).await;
        }
    }

    /// What a Stop may send for each contract the run's own fills hold: never
    /// more than, nor against, what the destination itself holds there (its
    /// position book, plus settled holdings for delivery). Every contract is
    /// decided before anything is sent, so a conflict on one sends nothing.
    async fn exit_sizes<'a>(
        &self,
        info: &RunInfo,
        held: &'a [store::Holding],
        deadline: Instant,
        look: Duration,
    ) -> Result<Vec<(&'a store::Holding, i64)>, String> {
        if held.is_empty() {
            return Ok(Vec::new());
        }
        let actuals = self.destination_sizes(info, held, deadline, look).await?;
        let mut out = Vec::new();
        for (h, actual) in held.iter().zip(actuals) {
            match orders::exit_cap(h.quantity, actual) {
                orders::ExitCap::Nothing => {}
                orders::ExitCap::DestinationFlat => self.write_log(
                    info,
                    &[format!(
                        "Your account {} holds no {} {} position, though this strategy's own orders add up to {}. It may have been closed outside this strategy, so nothing was sent for it.",
                        side_words(info.mode),
                        h.product,
                        h.symbol,
                        h.quantity
                    )],
                ),
                orders::ExitCap::Opposite => {
                    return Err(format!(
                        "This strategy's own orders add up to {} {} {}, but your account {} is {} {}. Nothing was sent, so that position was not added to. It is still running. Check the position and close it yourself if needed, then Stop it again or Pause it.",
                        side_name(h.quantity),
                        h.quantity.unsigned_abs(),
                        h.symbol,
                        side_words(info.mode),
                        side_name(actual),
                        actual.unsigned_abs()
                    ));
                }
                orders::ExitCap::Close { units, capped } => {
                    if capped {
                        self.write_log(
                            info,
                            &[format!(
                                "Your account {} holds {} {}, less than the {} this strategy's own orders add up to, so only {} is being closed.",
                                side_words(info.mode),
                                actual.unsigned_abs(),
                                h.symbol,
                                h.quantity.unsigned_abs(),
                                units
                            )],
                        );
                    }
                    out.push((h, units));
                }
            }
        }
        Ok(out)
    }

    /// The page ended by itself (a diagnostic stopped the script) or stopped
    /// answering: drop the run and leave the position where it is.
    pub fn end_run(&self, run_id: &str, why: &str) {
        let entry = {
            let mut reg = self.reg.lock();
            if reg.closing.contains(run_id) || reg.stopping.contains(run_id) {
                return;
            }
            reg.runs.remove(run_id)
        };
        if let Some(e) = entry {
            self.teardown(&e, why);
            tracing::warn!("The OpenScript run {} ended: {}", run_id, why);
        }
    }

    /// Pause every run (logout, shutdown). Positions are left as they are.
    pub fn pause_all(&self, why: &str) -> Vec<String> {
        let ids: Vec<String> = self.reg.lock().runs.keys().cloned().collect();
        let mut done = Vec::new();
        for id in ids {
            if self.pause_for(&id, false, why).is_ok() {
                done.push(id);
            }
        }
        done
    }

    /// Refuse new starts, pause every run, stop the scheduler.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        let paused = self.pause_all("Paused because OpenAlgo is closing");
        if !paused.is_empty() {
            tracing::info!("Paused {} OpenScript run(s) on the way out", paused.len());
        }
        let handle = self.scheduler.lock().take();
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
    }

    // ------------------------------------------------------------- removal

    /// Remove one deployment's settings and schedule, refusing while it runs.
    pub fn remove_deployment(&self, given: &str) -> Result<String, Refusal> {
        let conn = self
            .db
            .conn()
            .map_err(|_| Refusal::new(500, "The run settings could not be read just now."))?;
        let found = store::read(&conn, given).ok().flatten();
        drop(conn);
        let Some(found) = found else {
            return Err(Refusal::new(
                404,
                format!("{} has no run settings saved.", given),
            ));
        };
        let run_id = found.deployment.clone();
        {
            let mut reg = self.reg.lock();
            if reg.runs.contains_key(&run_id) {
                return Err(Refusal::new(409, RUNNING_DELETE_REFUSAL));
            }
            if reg.starting.contains(&run_id)
                || reg.stopping.contains(&run_id)
                || reg.closing.contains(&run_id)
                || reg.deleting.contains(&run_id)
            {
                return Err(Refusal::new(409, BUSY_DELETE_REFUSAL));
            }
            reg.deleting.insert(run_id.clone());
        }
        let out = (|| {
            let mut conn = self
                .db
                .conn()
                .map_err(|_| Refusal::new(500, "These run settings could not be removed."))?;
            let msg = match store::delete(&mut conn, &run_id) {
                Ok(Ok((_, msg))) => msg,
                Ok(Err(why)) => return Err(Refusal::new(500, why)),
                Err(e) => {
                    tracing::error!("Could not remove {}: {}", run_id, e);
                    return Err(Refusal::new(
                        500,
                        "These run settings could not be removed.",
                    ));
                }
            };
            // The schedule goes with it, under either name it was saved by.
            for name in [run_id.as_str(), given] {
                if let Err(e) = store::delete_schedule(&conn, name) {
                    tracing::error!("Could not remove the schedule for {}: {}", name, e);
                }
            }
            Ok(msg)
        })();
        self.reg.lock().deleting.remove(&run_id);
        out
    }
}
