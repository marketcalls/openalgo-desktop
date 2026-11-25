//! Fill decisions and fills (web `sandbox/execution_engine.py`).
//!
//! [`decide`] is pure: an order and a quote in, a decision out. A fill is one
//! transaction: the order row is claimed with a conditional UPDATE on the
//! pending statuses (so of any number of simultaneous evaluators exactly one
//! fills), the terms the decision was made against are re-checked (a modify
//! that landed in between wins), the trade is written and the position and
//! funds are netted. Events go out after the commit.

use super::charge_manager;
use super::charges::Charges;
use super::config::SandboxConfig;
use super::core::{blocking, Core};
use super::db::{OrderRow, OrderTerms};
use super::events::{fill_events, order_update, Outbox};
use super::funds;
use super::locks::PositionKey;
use super::orders;
use super::positions;
use super::quotes::{quote_is_old, quote_looks_stale, Quote};
use super::types::{dec_to_db, Action, OrderStatus, PriceType, SbResult, SymbolKey};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What to do with a resting order at a quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not now.
    Wait,
    /// Fill at this price.
    Fill(Decimal),
    /// An SL whose trigger fired but whose limit is not reachable yet:
    /// `trigger pending` becomes `open`.
    ReleaseToOpen,
}

/// The web's fill rules (`_process_order`, `_process_trigger_pending_order`).
pub fn decide(order: &OrderRow, q: &Quote) -> Decision {
    let ltp = q.ltp;
    if ltp <= Decimal::ZERO || quote_looks_stale(q) {
        return Decision::Wait;
    }
    let buy = order.action == Action::Buy;
    let price = order.price.unwrap_or(Decimal::ZERO);
    let trigger = order.trigger_price.unwrap_or(Decimal::ZERO);
    let triggered = if buy { ltp >= trigger } else { ltp <= trigger };
    let limit_ok = if buy { ltp <= price } else { ltp >= price };

    match order.order_status {
        OrderStatus::TriggerPending => {
            if !triggered {
                return Decision::Wait;
            }
            match order.price_type {
                PriceType::SlM => Decision::Fill(ltp),
                PriceType::Sl if limit_ok => Decision::Fill(ltp),
                PriceType::Sl => Decision::ReleaseToOpen,
                // A MARKET/LIMIT order never rests in trigger pending; treat
                // it like an open one.
                _ => decide_open(order, q, buy, price, trigger),
            }
        }
        OrderStatus::Open => decide_open(order, q, buy, price, trigger),
        _ => Decision::Wait,
    }
}

/// [`decide`] for a quote received at `q.received_at`: a price received more
/// than `max_age` before `now` is not filled at; the order waits for a newer
/// one. (The web has no receipt time and fills at whatever it was handed.)
pub fn decide_at(order: &OrderRow, q: &Quote, now: Instant, max_age: Duration) -> Decision {
    if quote_is_old(q, now, max_age) {
        return Decision::Wait;
    }
    decide(order, q)
}

fn decide_open(
    order: &OrderRow,
    q: &Quote,
    buy: bool,
    price: Decimal,
    trigger: Decimal,
) -> Decision {
    let ltp = q.ltp;
    match order.price_type {
        PriceType::Market => {
            let side = if buy { q.ask } else { q.bid };
            Decision::Fill(if side > Decimal::ZERO { side } else { ltp })
        }
        PriceType::Limit => {
            if (buy && ltp <= price) || (!buy && ltp >= price) {
                // A resting limit fills at its limit price.
                Decision::Fill(price)
            } else {
                Decision::Wait
            }
        }
        PriceType::Sl => {
            let fire = if buy {
                ltp >= trigger && ltp <= price
            } else {
                ltp <= trigger && ltp >= price
            };
            if fire {
                Decision::Fill(ltp)
            } else {
                Decision::Wait
            }
        }
        PriceType::SlM => {
            let fire = if buy { ltp >= trigger } else { ltp <= trigger };
            if fire {
                Decision::Fill(ltp)
            } else {
                Decision::Wait
            }
        }
    }
}

/// `TRADE-YYYYMMDD-HHMMSS-<8 hex>`.
pub fn generate_trade_id(now: chrono::NaiveDateTime) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("TRADE-{}-{}", now.format("%Y%m%d-%H%M%S"), &u[..8])
}

/// Fresh trade ids tried when one collides with an existing trade.
const TRADE_ID_ATTEMPTS: usize = 5;

/// Write the order's one trade row, with what the fill was charged, and
/// return its id. Only a duplicate
/// trade id (the `UNIQUE` constraint) is retried with a fresh id; any other
/// failure, and a run of duplicates, is returned so the caller's
/// transaction rolls the whole fill back: an order never completes without
/// its trade.
fn insert_trade(
    tx: &Connection,
    order: &OrderRow,
    price: Decimal,
    charges: &Charges,
    now: chrono::NaiveDateTime,
    now_s: &str,
) -> rusqlite::Result<String> {
    let mut last = None;
    let c = charges.columns().map(dec_to_db);
    for _ in 0..TRADE_ID_ATTEMPTS {
        let tradeid = generate_trade_id(now);
        let r = tx.execute(
            "INSERT INTO sandbox_trades (tradeid, orderid, user_id, symbol, exchange, action,
               quantity, price, product, strategy, trade_timestamp, brokerage, stt,
               exchange_charges, sebi_charges, stamp_duty, gst, total_charges)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
               ?17, ?18)",
            params![
                tradeid,
                order.orderid,
                order.user_id,
                order.symbol,
                order.exchange,
                order.action.as_str(),
                order.quantity,
                dec_to_db(price),
                order.product.as_str(),
                order.strategy,
                now_s,
                c[0],
                c[1],
                c[2],
                c[3],
                c[4],
                c[5],
                c[6]
            ],
        );
        match r {
            Ok(_) => return Ok(tradeid),
            Err(rusqlite::Error::SqliteFailure(e, msg))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                last = Some(rusqlite::Error::SqliteFailure(e, msg));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
            Some("could not allocate a unique sandbox trade id".into()),
        )
    }))
}

/// Fill inside the caller's transaction. Returns `false` when the order is
/// no longer pending or its terms changed (nothing written), or when the
/// position the fill would open needs margin the account does not have (the
/// order is rejected).
pub(crate) fn execute_in_tx(
    tx: &Connection,
    core: &Core,
    order_id: i64,
    price: Decimal,
    expected: Option<OrderTerms>,
    outbox: &mut Outbox,
) -> rusqlite::Result<bool> {
    let Some(order) = orders::get_by_id(tx, order_id)? else {
        return Ok(false);
    };
    if !order.order_status.is_pending() {
        return Ok(false);
    }
    if let Some(t) = expected {
        if order.terms() != t {
            tracing::info!(
                "Order {} was modified while its fill was being decided; deciding again on the next price",
                order.orderid
            );
            return Ok(false);
        }
    }
    let now = core.now();
    let now_s = super::clock::ts(now);
    let claimed = tx.execute(
        "UPDATE sandbox_orders SET order_status = 'complete', average_price = ?1,
           filled_quantity = quantity, pending_quantity = 0, update_timestamp = ?2
         WHERE id = ?3 AND order_status IN ('open','trigger pending')",
        params![dec_to_db(price), now_s, order.id],
    )?;
    if claimed != 1 {
        return Ok(false);
    }
    let cfg = SandboxConfig::load(tx)?;
    // Margin follows the position. An exit placed against a position blocks
    // nothing; if the position has been closed or reduced by the time it
    // fills, the fill opens or extends one, which must hold its margin. The
    // web leaves that position at zero margin.
    let position_qty = positions::get(
        tx,
        &order.user_id,
        &order.symbol,
        &order.exchange,
        order.product.as_str(),
    )?
    .map(|p| p.quantity)
    .unwrap_or(0);
    let cv = core.contract_value(&order.symbol, &order.exchange);
    let shortfall =
        positions::margin_needed_at_fill(&cfg, &order, position_qty, cv) - order.margin_blocked;
    let mut order = order;
    if shortfall > Decimal::ZERO {
        let blocked = funds::apply(tx, &order.user_id, cfg.starting_capital, &now_s, |f| {
            funds::check_available(f, shortfall)?;
            funds::block(f, shortfall)
        })?;
        if let Err(refusal) = blocked {
            reject_claimed(tx, &cfg, &order, &refusal.0, &now_s, outbox)?;
            return Ok(false);
        }
        order.margin_blocked += shortfall;
        tx.execute(
            "UPDATE sandbox_orders SET margin_blocked = ?1 WHERE id = ?2",
            params![dec_to_db(order.margin_blocked), order.id],
        )?;
    }
    let charges = charge_manager::charge_fill(tx, &cfg, &order, price, cv, &now_s)?;
    let tradeid = insert_trade(tx, &order, price, &charges, now, &now_s)?;
    positions::apply_fill(tx, &cfg, &order, price, cv, &now_s)?;
    let discrepancy = funds::reconcile(tx, &order.user_id, true, &now_s)?;
    if discrepancy != Decimal::ZERO {
        tracing::warn!(
            "Sandbox margin reconciled by {} after filling {}",
            discrepancy,
            order.orderid
        );
    }
    let mut filled = order.clone();
    filled.average_price = Some(price);
    for e in fill_events(&filled, &tradeid, price) {
        outbox.push(e);
    }
    tracing::info!(
        "Sandbox order {} filled: {} {} {} @ {} (charges {}, brokerage {})",
        order.orderid,
        order.symbol,
        order.action.as_str(),
        order.quantity,
        price,
        charges.total(),
        charges.brokerage
    );
    Ok(true)
}

/// Turn an order this transaction just claimed for a fill into a rejection
/// (the account cannot carry the position the fill would open), returning
/// the margin the order held.
fn reject_claimed(
    tx: &Connection,
    cfg: &SandboxConfig,
    order: &OrderRow,
    reason: &str,
    now_s: &str,
    outbox: &mut Outbox,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE sandbox_orders SET order_status = 'rejected', rejection_reason = ?1,
           average_price = NULL, filled_quantity = 0, pending_quantity = ?2,
           update_timestamp = ?3
         WHERE id = ?4",
        params![reason, order.pending_quantity, now_s, order.id],
    )?;
    if order.margin_blocked > Decimal::ZERO {
        let amount = order.margin_blocked;
        let released = funds::apply(tx, &order.user_id, cfg.starting_capital, now_s, |f| {
            funds::release(f, amount.min(f.used_margin), Decimal::ZERO, true)
        })?;
        if let Err(r) = released {
            tracing::error!(
                "Sandbox order {} rejected but its margin was not returned: {}",
                order.orderid,
                r.0
            );
        }
    }
    tracing::warn!(
        "Sandbox order {} rejected when it came to fill: {}",
        order.orderid,
        reason
    );
    outbox.push(order_update(order, OrderStatus::Rejected, reason));
    Ok(())
}

/// Mark an order rejected after a fill failed for a technical reason, and
/// return its margin. Only from a pending status.
fn reject_after_error(core: &Core, order_id: i64) {
    let mut outbox = Outbox::new();
    let reason = "Execution error: the sandbox could not record this fill";
    let r: SbResult<()> = core.db.with_tx(|tx| {
        let Some(order) = orders::get_by_id(tx, order_id)? else {
            return Ok(());
        };
        let n = tx.execute(
            "UPDATE sandbox_orders SET order_status = 'rejected', rejection_reason = ?1,
               update_timestamp = ?2
             WHERE id = ?3 AND order_status IN ('open','trigger pending')",
            params![reason, core.now_ts(), order.id],
        )?;
        if n == 1 {
            if order.margin_blocked > Decimal::ZERO {
                let cfg = SandboxConfig::load(tx)?;
                let amount = order.margin_blocked;
                let _ = funds::apply(
                    tx,
                    &order.user_id,
                    cfg.starting_capital,
                    &core.now_ts(),
                    |f| funds::release(f, amount.min(f.used_margin), Decimal::ZERO, true),
                )?;
            }
            outbox.push(order_update(&order, OrderStatus::Rejected, reason));
        }
        Ok(())
    });
    match r {
        Ok(()) => core.publish(outbox),
        Err(e) => tracing::error!("Could not mark sandbox order {} rejected: {}", order_id, e),
    }
}

/// Fill an order at `price` (immediate execution and engine fills).
pub(crate) async fn execute(
    core: &Arc<Core>,
    order_id: i64,
    price: Decimal,
    expected: Option<OrderTerms>,
) -> SbResult<bool> {
    blocking(core, move |c| {
        let mut outbox = Outbox::new();
        let r: SbResult<bool> = c.db.with_tx(|tx| {
            Ok(execute_in_tx(
                tx,
                c,
                order_id,
                price,
                expected,
                &mut outbox,
            )?)
        });
        match r {
            Ok(filled) => {
                c.publish(outbox);
                Ok(filled)
            }
            Err(e) => {
                tracing::error!("Sandbox fill of order {} failed: {}", order_id, e);
                reject_after_error(c, order_id);
                Ok(false)
            }
        }
    })
    .await
}

/// Decide and act for one order at one quote. Returns `true` when the order
/// filled.
pub(crate) async fn process(core: &Arc<Core>, order: &OrderRow, q: &Quote) -> SbResult<bool> {
    match decide_at(order, q, Instant::now(), core.opts.stale_quote_after) {
        Decision::Wait => Ok(false),
        Decision::Fill(price) => execute(core, order.id, price, Some(order.terms())).await,
        Decision::ReleaseToOpen => {
            let id = order.id;
            blocking(core, move |c| {
                let mut outbox = Outbox::new();
                let r: SbResult<()> = c.db.with_tx(|tx| {
                    let n = tx.execute(
                        "UPDATE sandbox_orders SET order_status = 'open', update_timestamp = ?1
                         WHERE id = ?2 AND order_status = 'trigger pending'",
                        params![c.now_ts(), id],
                    )?;
                    if n == 1 {
                        if let Some(o) = orders::get_by_id(tx, id)? {
                            outbox.push(order_update(&o, OrderStatus::Open, ""));
                        }
                    }
                    Ok(())
                });
                r?;
                c.publish(outbox);
                Ok(false)
            })
            .await
        }
    }
}

/// Process one order under a *tried* position lock: when another order on
/// the same position is in flight the order is left for the next price.
async fn process_try_locked(core: &Arc<Core>, order: &OrderRow, q: &Quote) -> SbResult<bool> {
    if decide_at(order, q, Instant::now(), core.opts.stale_quote_after) == Decision::Wait {
        return Ok(false);
    }
    let key = PositionKey::new(
        &order.user_id,
        &order.exchange,
        &order.symbol,
        order.product.as_str(),
    );
    let Some(_guard) = core.locks.try_lock(key) else {
        tracing::debug!(
            "Order {} waits: another order on its position is in flight",
            order.orderid
        );
        return Ok(false);
    };
    process(core, order, q).await
}

/// Ticks: evaluate every pending order on this symbol at the tick's quote
/// (built as the web's WebSocket engine does: bid = ask = LTP, no day
/// range; see [`super::quotes::Tick::quote`]). Returns how many filled.
pub(crate) async fn on_price(core: &Arc<Core>, key: &SymbolKey, q: &Quote) -> SbResult<usize> {
    let k = key.clone();
    let pending = blocking(core, move |c| {
        Ok(c.db.with_conn(|conn| orders::pending(conn, Some(&k)))?)
    })
    .await?;
    let mut filled = 0;
    for order in &pending {
        if process_try_locked(core, order, q).await? {
            filled += 1;
        }
    }
    Ok(filled)
}

/// What one polling pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollStats {
    pub pending: usize,
    pub symbols: usize,
    pub filled: usize,
    pub gtt_fired: usize,
}

/// The polling engine's pass (web `check_and_execute_pending_orders`): every
/// pending order, quotes fetched in one batch per pass, then GTT legs.
pub(crate) async fn poll_once(core: &Arc<Core>) -> SbResult<PollStats> {
    poll_stale(core, &HashSet::new()).await
}

/// A polling pass over the pending orders and GTT legs of every symbol not
/// in `fresh`: symbols whose own feed is live are left to their ticks.
pub(crate) async fn poll_stale(
    core: &Arc<Core>,
    fresh: &HashSet<SymbolKey>,
) -> SbResult<PollStats> {
    let pending = blocking(core, |c| {
        Ok(c.db.with_conn(|conn| orders::pending(conn, None))?)
    })
    .await?;
    let mut by_symbol: BTreeMap<SymbolKey, Vec<OrderRow>> = BTreeMap::new();
    let mut polled = 0;
    for o in pending.iter() {
        let key = SymbolKey::new(o.symbol.clone(), o.exchange.clone());
        if fresh.contains(&key) {
            continue;
        }
        polled += 1;
        by_symbol.entry(key).or_default().push(o.clone());
    }
    let mut stats = PollStats {
        pending: polled,
        symbols: by_symbol.len(),
        ..Default::default()
    };
    if !by_symbol.is_empty() {
        let keys: Vec<SymbolKey> = by_symbol.keys().cloned().collect();
        let quotes: HashMap<SymbolKey, Quote> = core.quotes.quotes(&keys).await;
        for (key, orders) in by_symbol {
            let Some(q) = quotes.get(&key) else {
                continue;
            };
            for o in &orders {
                if process_try_locked(core, o, q).await? {
                    stats.filled += 1;
                }
            }
        }
    }
    stats.gtt_fired = super::gtt::poll_active_except(core, fresh).await?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn order(
        action: Action,
        pt: PriceType,
        status: OrderStatus,
        price: Option<Decimal>,
        trig: Option<Decimal>,
    ) -> OrderRow {
        OrderRow {
            id: 1,
            orderid: "1".into(),
            user_id: "u".into(),
            strategy: None,
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action,
            quantity: 1,
            price,
            trigger_price: trig,
            price_type: pt,
            product: super::super::types::Product::Mis,
            order_status: status,
            average_price: None,
            filled_quantity: 0,
            pending_quantity: 1,
            rejection_reason: None,
            margin_blocked: Decimal::ZERO,
            gtt_leg_id: None,
            order_timestamp: String::new(),
            update_timestamp: String::new(),
        }
    }

    #[test]
    fn market_uses_the_far_side_when_present() {
        let q = Quote {
            ltp: dec!(100),
            bid: dec!(99.9),
            ask: dec!(100.1),
            ..Quote::default()
        };
        let b = order(
            Action::Buy,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        let s = order(
            Action::Sell,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        assert_eq!(decide(&b, &q), Decision::Fill(dec!(100.1)));
        assert_eq!(decide(&s, &q), Decision::Fill(dec!(99.9)));
        assert_eq!(
            decide(
                &b,
                &Quote {
                    ltp: dec!(100),
                    ..Quote::default()
                }
            ),
            Decision::Fill(dec!(100))
        );
    }

    #[test]
    fn stale_or_zero_quotes_wait() {
        let b = order(
            Action::Buy,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        assert_eq!(decide(&b, &Quote::default()), Decision::Wait);
        let stale = Quote {
            ltp: dec!(90),
            high: dec!(110),
            low: dec!(100),
            ..Quote::default()
        };
        assert_eq!(decide(&b, &stale), Decision::Wait);
    }

    // SB-06: a price received longer ago than the limit is not filled at;
    // the same price received again fills.
    #[test]
    fn a_quote_older_than_the_limit_waits_and_a_refreshed_one_fills() {
        let now = Instant::now();
        let max_age = Duration::from_secs(30);
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s)).expect("uptime");
        let b = order(
            Action::Buy,
            PriceType::Limit,
            OrderStatus::Open,
            Some(dec!(100)),
            None,
        );
        let old = Quote::ltp(dec!(99)).received(ago(31));
        assert_eq!(decide_at(&b, &old, now, max_age), Decision::Wait);
        let refreshed = old.received(now);
        assert_eq!(
            decide_at(&b, &refreshed, now, max_age),
            Decision::Fill(dec!(100))
        );
        // At the limit it still counts as current.
        assert_eq!(
            decide_at(&b, &old.received(ago(30)), now, max_age),
            Decision::Fill(dec!(100))
        );
        // A market order waits on an old quote too.
        let m = order(
            Action::Sell,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        assert_eq!(decide_at(&m, &old, now, max_age), Decision::Wait);
        assert_eq!(
            decide_at(&m, &refreshed, now, max_age),
            Decision::Fill(dec!(99))
        );
        // A quote without a receipt time is not judged.
        assert_eq!(
            decide_at(&b, &Quote::ltp(dec!(99)), now, max_age),
            Decision::Fill(dec!(100))
        );
    }
}
