//! Signal-mode strategies: one TradingView alert moves one leg (web
//! `services/strategy_module/signals.py`).
//!
//! ```text
//! {"action": "long_entry", "leg_id": 1}
//! {"action": "short_exit", "symbol": "RELIANCE", "exchange": "NSE"}
//! ```
//!
//! Three outcomes, and the difference is the design:
//!
//! * acted: an order was placed (200);
//! * no-op: understood and correctly did nothing (200 with a note:
//!   `already_long`, `already_short`, `no_matching_position`,
//!   `outside_entry_window`, `outside_trading_window`), because an alert
//!   engine repeats itself and a retry on an order path is how one alert
//!   becomes two positions;
//! * refused: the signal contradicts the configuration (4xx).
//!
//! The side a leg is held comes from the signal that opened it, never from
//! configuration. An opposite entry squares first, then opens. A leg returns
//! to `configured` after an exit so it can be signalled again the same day.

use super::dispatch::{build_order, entry_action, exit_action, RunMode, EXIT_PRICETYPE};
use super::resolver::{contract_exists, is_derivative_exchange, resolve_quantity};
use super::state::{ClaimId, EntryDecision, LegSpec};
use super::store::{EventFields, NewOrder, StrategyRow};
use super::StrategyModule;
use chrono::NaiveTime;
use chrono_tz::Asia::Kolkata;
use serde_json::Value;

pub const SIGNAL_ACTIONS: &[&str] = &["long_entry", "long_exit", "short_entry", "short_exit"];
pub const BATCH_ACTIONS: &[&str] = &["start", "stop"];

/// Which actions this kind of strategy accepts.
pub fn actions_for(kind: &str) -> &'static [&'static str] {
    if kind == "signal" {
        SIGNAL_ACTIONS
    } else {
        BATCH_ACTIONS
    }
}

/// What one signal did, or why it did nothing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignalResult {
    pub ok: bool,
    pub note: Option<String>,
    pub error: Option<String>,
    pub leg_id: Option<i64>,
    pub run_id: Option<i64>,
    pub flipped: bool,
}

impl SignalResult {
    pub fn acted(&self) -> bool {
        self.ok && self.note.is_none()
    }

    fn refuse(error: impl Into<String>, leg_id: Option<i64>, run_id: Option<i64>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            leg_id,
            run_id,
            ..Default::default()
        }
    }

    fn note(note: &str, leg_id: Option<i64>, run_id: Option<i64>) -> Self {
        Self {
            ok: true,
            note: Some(note.into()),
            leg_id,
            run_id,
            ..Default::default()
        }
    }
}

fn side_of(action: &str) -> &'static str {
    if action.starts_with("long") {
        "long"
    } else {
        "short"
    }
}

fn position_of(side: &str) -> &'static str {
    if side == "long" {
        "B"
    } else {
        "S"
    }
}

fn leg_id_of(leg: &Value) -> Option<i64> {
    leg.get("id")
        .or_else(|| leg.get("leg_id"))
        .and_then(Value::as_i64)
}

/// The configured leg this signal targets. `leg_id` wins; the symbol
/// fallback lets one alert template serve strategies numbered differently.
pub fn find_leg(
    strategy: &StrategyRow,
    leg_id: Option<&Value>,
    symbol: Option<&str>,
    exchange: Option<&str>,
) -> Option<Value> {
    let legs = strategy.legs.as_array()?;
    if let Some(wanted) = leg_id.filter(|v| !v.is_null()) {
        let wanted = match wanted {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        return legs
            .iter()
            .find(|l| leg_id_of(l).map(|i| i.to_string()) == Some(wanted.clone()))
            .cloned();
    }
    let symbol = symbol.filter(|s| !s.is_empty())?.to_ascii_uppercase();
    let exchange = exchange.unwrap_or("").to_ascii_uppercase();
    legs.iter()
        .find(|l| {
            l["symbol"].as_str().unwrap_or("").to_ascii_uppercase() == symbol
                && (exchange.is_empty()
                    || l["exchange"].as_str().unwrap_or("").to_ascii_uppercase() == exchange)
        })
        .cloned()
}

fn hhmm(text: Option<&str>) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text?, "%H:%M").ok()
}

impl StrategyModule {
    /// Why this signal is outside the strategy's trading window, if it is.
    /// Exits stay allowed before the entry window: a carried position must
    /// always be closable.
    fn window_note(&self, strategy: &StrategyRow, action: &str) -> Option<&'static str> {
        if strategy.strategy_type != "intraday" {
            return None;
        }
        let now = self.clock.now().with_timezone(&Kolkata).time();
        if let Some(exit) = hhmm(strategy.exit_time.as_deref()) {
            if now >= exit {
                return Some("outside_trading_window");
            }
        }
        if action.ends_with("_entry") {
            if let Some(entry) = hhmm(strategy.entry_time.as_deref()) {
                if now < entry {
                    return Some("outside_entry_window");
                }
            }
        }
        None
    }

    /// Apply one signal to one leg.
    pub async fn handle_signal(
        &self,
        strategy: &StrategyRow,
        action: &str,
        leg_id: Option<&Value>,
        symbol: Option<&str>,
        exchange: Option<&str>,
    ) -> SignalResult {
        if !SIGNAL_ACTIONS.contains(&action) {
            return SignalResult::refuse(
                format!("Unknown signal action: '{}'", action),
                None,
                None,
            );
        }
        let side = side_of(action);
        let allowed = match strategy.direction.as_str() {
            "long_only" => side == "long",
            "short_only" => side == "short",
            _ => true,
        };
        if !allowed {
            return SignalResult::refuse(
                format!(
                    "This strategy is {}; a {} signal is not accepted",
                    strategy.direction, side
                ),
                None,
                None,
            );
        }
        let Some(leg) = find_leg(strategy, leg_id, symbol, exchange) else {
            return SignalResult::refuse("No leg matches this signal", None, None);
        };
        let lid = leg_id_of(&leg).unwrap_or(1);
        let leg_side = leg["side"].as_str().unwrap_or("both").to_ascii_lowercase();
        if leg_side != "both" && leg_side != side {
            return SignalResult::refuse(
                format!("Leg {} only accepts {} signals", lid, leg_side),
                Some(lid),
                None,
            );
        }
        if let Some(note) = self.window_note(strategy, action) {
            return SignalResult::note(note, Some(lid), None);
        }
        let run_id = match self.day_run(strategy).await {
            Ok(id) => id,
            Err(e) => return SignalResult::refuse(e, Some(lid), None),
        };
        if action.ends_with("_entry") {
            if let Ok(Some(run)) = self.store.get_run(run_id) {
                if run.stop_requested_reason.is_some() {
                    return SignalResult {
                        ok: false,
                        note: Some("run_stopping".into()),
                        leg_id: Some(lid),
                        run_id: Some(run_id),
                        ..Default::default()
                    };
                }
            }
            return self.signal_enter(strategy, run_id, &leg, lid, side).await;
        }
        self.signal_exit(strategy, run_id, &leg, lid, side).await
    }

    /// The run this signal belongs to, opening one if the session has none.
    /// Serialised per strategy so two alerts on one bar join one run.
    async fn day_run(&self, strategy: &StrategyRow) -> Result<i64, String> {
        let lock = self.day_run_lock(strategy.id);
        let _held = lock.lock().await;
        let current = self
            .store
            .get_strategy_unscoped(strategy.id)
            .ok()
            .flatten()
            .unwrap_or_else(|| strategy.clone());
        if let Some(run_id) = current.current_run_id {
            if let Ok(Some(run)) = self.store.get_run(run_id) {
                if run.stopped_at.is_none() {
                    let started = super::store::parse_utc(&run.started_at);
                    let stale = started
                        .map(|s| {
                            super::session::started_before_today(
                                s,
                                self.clock.now(),
                                self.session.0,
                                self.session.1,
                            )
                        })
                        .unwrap_or(false);
                    if !stale {
                        return Ok(run_id);
                    }
                    // A signal run IS a trading day: roll a stale one through
                    // the durable stop, and only confirmed flatness opens a
                    // replacement.
                    tracing::info!("Rolling signal run {} through end-of-day stop", run_id);
                    let r = self.stop_run(run_id, &current.user_id, "eod").await;
                    if !r.ok || r.stop_pending {
                        return Ok(run_id);
                    }
                    self.emit(
                        strategy.id,
                        &current.user_id,
                        "eod_squareoff",
                        "Previous day's run closed on the first signal of a new day",
                        EventFields {
                            run_id: Some(run_id),
                            severity: Some("warn"),
                            ..Default::default()
                        },
                    )
                    .await;
                }
            }
        }
        // Mode is the strategy's own opt-in: live only if enabled.
        let mode = if current.live_enabled {
            RunMode::Live
        } else {
            RunMode::Sandbox
        };
        if !self
            .store
            .claim_strategy_for_run(strategy.id)
            .unwrap_or(false)
        {
            if let Ok(Some(r)) = self.store.get_strategy_unscoped(strategy.id) {
                if let Some(id) = r.current_run_id {
                    return Ok(id);
                }
            }
            return Err("This strategy is already running".into());
        }
        let broker = {
            let b = self.gateway.broker_name(mode);
            if b.is_empty() {
                mode.as_str().to_string()
            } else {
                b
            }
        };
        let run_id =
            match self
                .store
                .create_run(strategy.id, mode.as_str(), &broker, "webhook", None, None)
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(
                        "Could not open a signal run for strategy {}: {}",
                        strategy.id,
                        e
                    );
                    let _ = self.store.release_strategy(strategy.id);
                    return Err("Could not open a run".into());
                }
            };
        // State before the link: once the row names this run, any reader may
        // act on it.
        self.state
            .install(super::state::RunState::new(run_id, strategy.id, vec![]));
        if !matches!(
            self.store
                .set_strategy_status(strategy.id, "running", Some(run_id)),
            Ok(true)
        ) {
            self.state.clear(run_id);
            let _ = self
                .store
                .finish_unlinked_run_and_release_claim(run_id, strategy.id, "error");
            return Err("Could not link the new signal run; no order was placed".into());
        }
        self.emit(
            strategy.id,
            &current.user_id,
            "run_started",
            &format!("Signal run opened in {} mode", mode.as_str()),
            EventFields {
                run_id: Some(run_id),
                ..Default::default()
            },
        )
        .await;
        Ok(run_id)
    }

    async fn signal_enter(
        &self,
        strategy: &StrategyRow,
        run_id: i64,
        leg: &Value,
        lid: i64,
        side: &str,
    ) -> SignalResult {
        // A gap in order facts is being repaired: no new exposure until the
        // book has been read (ARCH-01). Exit signals are never held by this.
        if let Some(why) = self.entry_refusal(self.run_mode(run_id)) {
            return SignalResult::refuse(why, Some(lid), Some(run_id));
        }
        let claim = match self
            .state
            .claim_signal_entry(run_id, lid, position_of(side))
        {
            None => return SignalResult::refuse("No active run", Some(lid), Some(run_id)),
            Some(EntryDecision::Note(n)) => {
                return SignalResult {
                    ok: n != "run_stopping",
                    note: Some(n.into()),
                    leg_id: Some(lid),
                    run_id: Some(run_id),
                    ..Default::default()
                }
            }
            Some(EntryDecision::Claimed(c)) => c,
        };
        let token = claim.claim_token.clone();
        let result = self
            .signal_enter_claimed(strategy, run_id, leg, lid, side, claim)
            .await;
        if self.state.release_signal_entry_claim(run_id, lid, &token) {
            // Every refusal before the entry finished leaves the claim here; a
            // stop may have become durable meanwhile.
            self.reconcile_pending_stop(run_id).await;
        }
        result
    }

    async fn signal_enter_claimed(
        &self,
        strategy: &StrategyRow,
        run_id: i64,
        leg: &Value,
        lid: i64,
        side: &str,
        claim: super::state::EntryClaim,
    ) -> SignalResult {
        let held = claim
            .held_position
            .as_deref()
            .map(|p| if p == "B" { "long" } else { "short" });
        // Refused before anything is squared: a refusal must cost nothing.
        if let Some(e) = reject_uncarryable_short(strategy, leg, side) {
            return SignalResult::refuse(format!("Leg {}: {}", lid, e), Some(lid), None);
        }
        let mut flipped = false;
        if let Some(held) = held {
            // Opposite side: square first, then open.
            let closed = self.signal_exit(strategy, run_id, leg, lid, held).await;
            if !closed.ok || closed.note.is_some() {
                return closed;
            }
            flipped = true;
        }
        let spec = match self.resolve_signal_leg(leg, lid, side) {
            Ok(mut s) => {
                s.position_ref = Some(claim.position_ref.clone());
                s
            }
            Err(e) => return SignalResult::refuse(format!("Leg {}: {}", lid, e), Some(lid), None),
        };
        match self
            .signal_place_entry(strategy, run_id, &spec, &claim)
            .await
        {
            Ok(()) => SignalResult {
                ok: true,
                leg_id: Some(lid),
                run_id: Some(run_id),
                flipped,
                ..Default::default()
            },
            Err(e) => SignalResult::refuse(e, Some(lid), Some(run_id)),
        }
    }

    /// A signal leg in run-state shape. The side comes from the signal.
    fn resolve_signal_leg(&self, leg: &Value, lid: i64, side: &str) -> Result<LegSpec, String> {
        let symbol = leg["symbol"].as_str().unwrap_or("").to_ascii_uppercase();
        let exchange = leg["exchange"].as_str().unwrap_or("").to_ascii_uppercase();
        let raw_qty = leg
            .get("qty")
            .or_else(|| leg.get("quantity"))
            .and_then(crate::risk::value_to_f64)
            .unwrap_or(0.0);
        if symbol.is_empty() || exchange.is_empty() || raw_qty <= 0.0 {
            return Err("symbol, exchange and quantity are all required".into());
        }
        let g = self.symbols.snapshot();
        if !contract_exists(&g, &symbol, &exchange) {
            return Err(format!("{} is not a contract on {}", symbol, exchange));
        }
        let mode = leg["qty_mode"].as_str().unwrap_or("units");
        let (quantity, _lot) = resolve_quantity(&g, raw_qty as i64, mode, &symbol, &exchange)?;
        let trail = &leg["trail"];
        Ok(LegSpec {
            leg_id: lid,
            position: position_of(side).into(),
            symbol,
            exchange,
            lots: if mode == "lots" { raw_qty as i64 } else { 1 },
            quantity,
            position_ref: None,
            sl_pts: leg["sl_pts"].as_f64(),
            target_pts: leg["target_pts"].as_f64(),
            trail_x: trail["x"].as_f64().unwrap_or(0.0),
            trail_y: trail["y"].as_f64().unwrap_or(0.0),
            risk_unit: leg["risk_unit"].as_str().unwrap_or("points").to_string(),
            ..Default::default()
        })
    }

    async fn signal_place_entry(
        &self,
        strategy: &StrategyRow,
        run_id: i64,
        spec: &LegSpec,
        claim: &super::state::EntryClaim,
    ) -> Result<(), String> {
        let mode = self.run_mode(run_id);
        self.gateway.authorised(mode)?;
        let action = entry_action(&spec.position);
        let order = build_order(
            &spec.symbol,
            &spec.exchange,
            action,
            spec.quantity,
            &strategy.product,
            &strategy.name,
            &strategy.pricetype,
        );
        let row_id = match self.store.record_order(
            run_id,
            spec.leg_id,
            "entry",
            &NewOrder {
                symbol: spec.symbol.clone(),
                exchange: spec.exchange.clone(),
                action: action.into(),
                qty: spec.quantity,
                product: Some(order.product.clone()),
                pricetype: order.pricetype.clone(),
                status: "pending".into(),
                position_ref: spec.position_ref.clone(),
                broker_order_id: None,
            },
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Signal entry row not written: {}", e);
                self.emit(
                    strategy.id,
                    &strategy.user_id,
                    "leg_entry_rejected",
                    &format!(
                        "Signal entry for leg {} not placed: its order row could not be written",
                        spec.leg_id
                    ),
                    EventFields {
                        run_id: Some(run_id),
                        leg_id: Some(spec.leg_id),
                        severity: Some("critical"),
                        payload: None,
                    },
                )
                .await;
                return Err("Could not record the order before placing it".into());
            }
        };
        if self
            .state
            .add_leg(
                run_id,
                spec,
                &claim.claim_token,
                claim.expected_position_ref.as_deref(),
                row_id,
            )
            .is_none()
        {
            let _ = self.store.update_order(
                row_id,
                Some("rejected"),
                None,
                Some("Signal entry claim changed before dispatch"),
            );
            return Err("The position changed before its entry could be placed".into());
        }
        let symbols = vec![(spec.symbol.clone(), spec.exchange.clone())];
        self.feed.add_run(run_id, &symbols).await;
        let result = self.gateway.place(mode, &order).await;
        self.record_acknowledgement(
            row_id,
            &result,
            strategy.id,
            &strategy.user_id,
            run_id,
            spec.leg_id,
        )
        .await;
        if result.uncertain {
            // The entry may be at the broker: the leg stays pending on its
            // row and the claim stays, so a repeated alert cannot send a
            // second entry, until the order reconciler settles it (LOG-08).
            self.state
                .hold_signal_entry_claim(run_id, spec.leg_id, &claim.claim_token, row_id);
            return Err(result.error.unwrap_or_else(|| {
                crate::brokers::common::outcome::UNCERTAIN_MESSAGE.into()
            }));
        }
        self.state.finish_signal_entry(
            run_id,
            spec.leg_id,
            spec.position_ref.as_deref().unwrap_or(""),
            &claim.claim_token,
            result.ok,
        );
        self.emit(
            strategy.id,
            &strategy.user_id,
            "leg_entry_placed",
            &format!(
                "Signal {} {} {}{}",
                action,
                spec.quantity,
                spec.symbol,
                if result.ok {
                    String::new()
                } else {
                    format!(" rejected: {}", result.error.clone().unwrap_or_default())
                }
            ),
            EventFields {
                run_id: Some(run_id),
                leg_id: Some(spec.leg_id),
                severity: Some(if result.ok { "info" } else { "warn" }),
                payload: None,
            },
        )
        .await;
        if result.ok {
            self.replay_for(result.broker_order_id.as_deref()).await;
            Ok(())
        } else {
            self.reconcile_pending_stop(run_id).await;
            Err(result.error.unwrap_or_else(|| "Order rejected".into()))
        }
    }

    fn run_mode(&self, run_id: i64) -> RunMode {
        self.store
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| RunMode::parse(&r.mode))
            .unwrap_or(RunMode::Sandbox)
    }

    /// Close a leg held on `side`, or say it was not held.
    fn signal_exit<'a>(
        &'a self,
        strategy: &'a StrategyRow,
        run_id: i64,
        leg: &'a Value,
        lid: i64,
        side: &'a str,
    ) -> futures_util::future::BoxFuture<'a, SignalResult> {
        Box::pin(async move {
            let _ = leg;
            let held = self
                .state
                .with_run(run_id, |run| {
                    run.leg(lid).filter(|l| l.status == "open").map(|l| {
                        if l.position == "B" {
                            "long"
                        } else {
                            "short"
                        }
                    })
                })
                .flatten();
            if held != Some(side) {
                // A flip whose closing order was refused leaves the outgoing
                // side held: an exit for it is real.
                if let Some(c) = self
                    .state
                    .claim_superseded_exit(run_id, lid, position_of(side))
                {
                    let placed = self
                        .signal_place_exit(
                            strategy,
                            run_id,
                            lid,
                            &c.position,
                            &c.symbol,
                            &c.exchange,
                            c.quantity,
                            c.position_ref.clone(),
                            &c.claim_token,
                            true,
                        )
                        .await;
                    return match placed {
                        Ok(()) => SignalResult {
                            ok: true,
                            leg_id: Some(lid),
                            run_id: Some(run_id),
                            ..Default::default()
                        },
                        Err((e, claim)) => {
                            // `None`: unconfirmed, the claim stays (LOG-08).
                            if let Some(claim) = claim {
                                if self.state.release_superseded_exit(run_id, lid, &claim) {
                                    self.report_flip_outgoing_exit_rejected(
                                        run_id, lid, "refused", None,
                                    )
                                    .await;
                                }
                            }
                            SignalResult::refuse(e, Some(lid), Some(run_id))
                        }
                    };
                }
                return SignalResult::note("no_matching_position", Some(lid), Some(run_id));
            }
            // Claim before dispatching: a repeated exit alert must not send a
            // second closing order.
            let Some(c) = self.state.claim_leg_exit(run_id, lid, "exit_signal") else {
                let unfilled = self
                    .state
                    .with_run(run_id, |r| {
                        r.leg(lid)
                            .is_some_and(|l| l.status == "open" && l.entry_status != "complete")
                    })
                    .unwrap_or(false);
                if unfilled {
                    return SignalResult::refuse(
                        "The entry for this leg has been accepted but not filled, so there is no confirmed quantity to exit. Retry once it fills.",
                        Some(lid),
                        Some(run_id),
                    );
                }
                return SignalResult::note("no_matching_position", Some(lid), Some(run_id));
            };
            match self
                .signal_place_exit(
                    strategy,
                    run_id,
                    lid,
                    &c.leg.position,
                    &c.leg.symbol,
                    &c.leg.exchange,
                    c.leg.qty,
                    c.leg.position_ref.clone(),
                    &c.claim_token,
                    false,
                )
                .await
            {
                Ok(()) => SignalResult {
                    ok: true,
                    leg_id: Some(lid),
                    run_id: Some(run_id),
                    ..Default::default()
                },
                Err((e, claim)) => {
                    // Leave the leg exitable: its stop, target and square-off
                    // all skip a leg that looks like it has an exit in flight.
                    // An unconfirmed exit (`None`) keeps its claim (LOG-08).
                    if let Some(claim) = claim {
                        self.state.release_leg_exit(run_id, lid, &claim);
                    }
                    SignalResult::refuse(e, Some(lid), Some(run_id))
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn signal_place_exit(
        &self,
        strategy: &StrategyRow,
        run_id: i64,
        lid: i64,
        position: &str,
        symbol: &str,
        exchange: &str,
        quantity: i64,
        position_ref: Option<String>,
        claim_token: &str,
        superseded: bool,
    ) -> Result<(), (String, Option<ClaimId>)> {
        let mut claim = ClaimId::Token(claim_token.to_string());
        let mode = self.run_mode(run_id);
        if let Err(e) = self.gateway.authorised(mode) {
            return Err((e, Some(claim)));
        }
        let action = exit_action(position).map_err(|e| (e, Some(claim.clone())))?;
        let order = build_order(
            symbol,
            exchange,
            action,
            quantity,
            &strategy.product,
            &strategy.name,
            EXIT_PRICETYPE,
        );
        let row = self.store.record_order(
            run_id,
            lid,
            "exit_signal",
            &NewOrder {
                symbol: symbol.into(),
                exchange: exchange.into(),
                action: action.into(),
                qty: quantity,
                product: Some(order.product.clone()),
                pricetype: EXIT_PRICETYPE.into(),
                status: "pending".into(),
                position_ref: position_ref.clone(),
                broker_order_id: None,
            },
        );
        let row_id = row.ok();
        match row_id {
            Some(row_id) => {
                let bound = if superseded {
                    self.state
                        .bind_superseded_exit(run_id, lid, claim_token, row_id)
                } else {
                    self.state.bind_live_exit(
                        run_id,
                        lid,
                        claim_token,
                        row_id,
                        position_ref.as_deref(),
                    )
                };
                if !bound {
                    let _ = self.store.update_order(
                        row_id,
                        Some("rejected"),
                        None,
                        Some(if superseded {
                            "Outgoing position exit claim changed before dispatch"
                        } else {
                            "Live position exit claim changed before dispatch"
                        }),
                    );
                    return Err((
                        "The position changed before its exit could be placed".into(),
                        Some(claim),
                    ));
                }
                claim = ClaimId::Row(row_id);
            }
            None => {
                // Placed anyway: getting flat wins over the audit row.
                self.emit(
                    strategy.id,
                    &strategy.user_id,
                    "leg_exit_placed",
                    &format!(
                        "Signal exit for leg {} is being placed without an order row: it could not be written",
                        lid
                    ),
                    EventFields {
                        run_id: Some(run_id),
                        leg_id: Some(lid),
                        severity: Some("critical"),
                        payload: None,
                    },
                )
                .await;
            }
        }
        let result = self.gateway.place(mode, &order).await;
        if let Some(row_id) = row_id {
            self.record_acknowledgement(
                row_id,
                &result,
                strategy.id,
                &strategy.user_id,
                run_id,
                lid,
            )
            .await;
        }
        self.emit(
            strategy.id,
            &strategy.user_id,
            "leg_exit_placed",
            &format!(
                "Signal {} {} {}{}",
                action,
                quantity,
                symbol,
                if result.ok {
                    String::new()
                } else if result.uncertain {
                    " sent, but not confirmed by the broker; not sent again".to_string()
                } else {
                    format!(" rejected: {}", result.error.clone().unwrap_or_default())
                }
            ),
            EventFields {
                run_id: Some(run_id),
                leg_id: Some(lid),
                severity: Some(if result.ok { "info" } else { "warn" }),
                payload: None,
            },
        )
        .await;
        if result.ok {
            if row_id.is_some() {
                self.replay_for(result.broker_order_id.as_deref()).await;
            }
            Ok(())
        } else if result.uncertain {
            // The exit may be at the broker: its claim stays bound to the
            // row until the order reconciler settles it (LOG-08).
            Err((
                result
                    .error
                    .unwrap_or_else(|| crate::brokers::common::outcome::UNCERTAIN_MESSAGE.into()),
                None,
            ))
        } else {
            Err((
                result.error.unwrap_or_else(|| "Order rejected".into()),
                Some(claim),
            ))
        }
    }
}

/// A short that the product cannot carry: cash sold short under anything
/// but MIS is a naked short delivery.
fn reject_uncarryable_short(strategy: &StrategyRow, leg: &Value, side: &str) -> Option<String> {
    if side != "short" || strategy.product.eq_ignore_ascii_case("MIS") {
        return None;
    }
    let exchange = leg["exchange"].as_str().unwrap_or("").to_ascii_uppercase();
    if exchange.is_empty() || is_derivative_exchange(&exchange) {
        return None;
    }
    Some(format!(
        "cash cannot be held short overnight, and product {} carries the position. Use MIS for an intraday short.",
        strategy.product.to_ascii_uppercase()
    ))
}
