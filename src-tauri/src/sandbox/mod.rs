//! Sandbox (analyzer mode) engine: the desktop port of the web's `sandbox/`
//! package, in its own `sandbox.db`.
//!
//! With analyzer mode on, every order and account service hands the request
//! to [`Sandbox`] before any broker call. The engine never knows about
//! brokers: prices come from a [`QuoteSource`] (snapshots) and a
//! [`TickSource`] (live LTP), instruments from a [`SymbolSource`], and time
//! from the injected [`Clock`] read in IST.
//!
//! What it does, as the web does:
//! * 1 crore default capital; margin `|qty| x price / leverage` blocked on
//!   placement and netted against an opposite position; released on cancel,
//!   reduce and close; every funds change a compare-and-set.
//! * Validation with the web's messages (product/exchange compatibility, lot
//!   size, the MIS time gate, the CNC sell holdings check; a refused CNC sell
//!   is recorded as a `rejected` order with its reason).
//! * Order ids are 14-digit strings; statuses are exactly `open`,
//!   `trigger pending`, `complete`, `cancelled`, `rejected`.
//! * MARKET fills at LTP (bid/ask when the quote has them); a marketable
//!   LIMIT fills at LTP; resting LIMIT, SL and SL-M fill from ticks, with a
//!   polling fallback every 5 s while the feed has been silent for 30 s.
//! * Weighted-average netting, realized P&L on reduce, the average reset to
//!   the fill price on reversal, `today_realized_pnl` and
//!   `accumulated_realized_pnl`.
//! * T+1 settlement of CNC to holdings at 00:00 IST; MIS auto square-off at
//!   NSE/BSE/NFO/BFO 15:15, CDS/BCD 16:45, MCX 23:30, NCDEX 17:00 plus a
//!   one-minute backup; today's P&L reset at the 03:00 boundary; the 23:59
//!   snapshot; the optional weekly reset; catch-up of anything missed while
//!   the app was closed; GTT single and OCO.
//! * Events on the crate bus: `order.update`, `sandbox.order_filled`,
//!   `sandbox.auto_squareoff`, `sandbox.t1_settlement`, `gtt.triggered`,
//!   `gtt.expired`.
//!
//! # Public API (for the API-parity wave)
//!
//! Every call returns `Result<T, SandboxError>`. `Ok(T)` serialises to the
//! web's analyze-mode body with HTTP 200; `Err(e)` serialises with
//! [`SandboxError::body`] and `e.http_status` (400 validation and business
//! refusals, 404 unknown order or GTT, 409 position busy or a refused margin
//! release, 500 storage failures). Request schema validation (missing
//! fields, marshmallow-style field errors) stays in the API layer.
//!
//! | Endpoint | Call | Reply |
//! |---|---|---|
//! | `placeorder` | [`Sandbox::place_order`], [`Sandbox::place_order_with_quote`] | [`replies::OrderPlaced`] |
//! | `placesmartorder` | [`Sandbox::place_smart_order`] | [`replies::SmartOrderReply`] |
//! | `modifyorder` | [`Sandbox::modify_order`] | [`replies::OrderMessage`] |
//! | `cancelorder` | [`Sandbox::cancel_order`] | [`replies::OrderMessage`] |
//! | `cancelallorder` | [`Sandbox::cancel_all_orders`] | [`replies::CancelAllReply`] |
//! | `closeposition` (one) | [`Sandbox::close_position`] | [`replies::OrderMessage`] |
//! | `closeposition` (all) | [`Sandbox::close_all_positions`] | [`replies::CloseAllReply`] |
//! | `orderbook` | [`Sandbox::orderbook`] | [`replies::OrderbookReply`] |
//! | `tradebook` | [`Sandbox::tradebook`] | [`replies::TradebookReply`] |
//! | `positionbook` | [`Sandbox::positionbook`] | [`replies::PositionbookReply`] |
//! | `holdings` | [`Sandbox::holdings`] | [`replies::HoldingsReply`] |
//! | `funds` | [`Sandbox::funds`] | [`replies::FundsReply`] |
//! | `orderstatus` | [`Sandbox::order_status`] | [`replies::OrderStatusReply`] |
//! | `openposition` | [`Sandbox::open_position`] | [`replies::OpenPositionReply`] |
//! | `pnl/symbols` | [`Sandbox::pnl_symbols`] | [`replies::PnlSymbolsReply`] |
//! | `placegttorder` | [`Sandbox::place_gtt`] | [`replies::GttReply`] |
//! | `modifygttorder` | [`Sandbox::modify_gtt`] | [`replies::GttReply`] |
//! | `cancelgttorder` | [`Sandbox::cancel_gtt`] | [`replies::GttReply`] |
//! | `gttorderbook` | [`Sandbox::gtt_orderbook`] | [`replies::GttOrderbookReply`] |
//! | `/sandbox/api/configs` | [`Sandbox::configs`] | `serde_json::Value` |
//! | `/sandbox/update` | [`Sandbox::update_config`] | [`replies::SettingsMessage`] |
//! | `/sandbox/reset` | [`Sandbox::reset`] | [`replies::SettingsMessage`] |
//! | `/sandbox/squareoff-status` | [`Sandbox::squareoff_status`] | [`replies::SquareOffStatusReply`] |
//! | `/sandbox/reload-squareoff` | [`Sandbox::reload_squareoff`] | [`replies::SquareOffStatusReply`] |
//! | `/sandbox/mypnl/api/data` | [`Sandbox::mypnl`] | [`replies::MyPnlReply`] |
//!
//! Lifecycle: [`Sandbox::set_analyzer_mode`] starts the engine task (after a
//! catch-up) when analyzer mode turns on and stops it when it turns off;
//! [`Sandbox::shutdown`] on app exit. Tests and adapters can also drive the
//! engine directly: [`Sandbox::on_tick`], [`Sandbox::poll_once`],
//! [`Sandbox::run_due_jobs`], [`Sandbox::catch_up`],
//! [`Sandbox::square_off_now`], [`Sandbox::t1_settlement`].

pub mod catch_up;
pub mod clock;
pub mod config;
mod core;
pub mod db;
pub mod engine;
pub mod events;
pub mod execution;
pub mod funds;
pub mod gtt;
pub mod holdings;
pub mod locks;
pub mod orders;
pub mod positions;
pub mod quotes;
pub mod replies;
pub mod session;
pub mod squareoff;
pub mod types;

pub use self::core::{EngineCmd, SandboxOptions, TradingDayFn};
pub use catch_up::CatchUpReport;
pub use clock::Clock;
pub use db::SandboxDb;
pub use engine::EngineHandle;
pub use execution::PollStats;
pub use gtt::GttRequest;
pub use orders::{ModifyRequest, OrderRequest, SmartOrderRequest};
pub use quotes::{BroadcastTicks, Quote, QuoteSource, StaticQuoteSource, Tick, TickSource};
pub use squareoff::SweepReport;
pub use types::{SandboxError, SbResult, StaticSymbols, SymbolKey, SymbolMeta, SymbolSource};

use self::core::{blocking, Core};
use crate::events::EventBus;
use clock::{display_seconds, ts};
use config::SandboxConfig;
use parking_lot::Mutex as SyncMutex;
use replies::*;
use rust_decimal::Decimal;
use squareoff::Scheduler;
use std::path::Path;
use std::sync::Arc;
use types::{float, money, pct};

/// What the engine runs against.
#[derive(Clone)]
pub struct SandboxDeps {
    pub symbols: Arc<dyn SymbolSource>,
    pub quotes: Arc<dyn QuoteSource>,
    pub clock: Arc<dyn Clock>,
    /// The crate event bus; `None` publishes nothing (tests).
    pub bus: Option<Arc<EventBus>>,
}

pub(crate) struct SandboxInner {
    core: Arc<Core>,
    scheduler: SyncMutex<Option<Scheduler>>,
    engine: tokio::sync::Mutex<Option<EngineHandle>>,
}

impl SandboxInner {
    pub(crate) fn invalidate_schedule(&self) {
        *self.scheduler.lock() = None;
    }

    pub(crate) async fn run_due_jobs_inner(&self) -> SbResult<Vec<String>> {
        let now = self.core.now();
        let due = {
            let mut guard = self.scheduler.lock();
            match guard.as_mut() {
                Some(s) => s.due(now),
                None => {
                    let cfg = self.core.config()?;
                    *guard = Some(Scheduler::build(&cfg, self.core.opts.session_expiry, now));
                    Vec::new()
                }
            }
        };
        let mut ran = Vec::with_capacity(due.len());
        for (id, kind) in due {
            if let Err(e) = squareoff::run_job(&self.core, kind).await {
                tracing::warn!("Sandbox job {} failed: {}", id, e.message);
            }
            ran.push(id);
        }
        Ok(ran)
    }
}

/// The sandbox engine handle. Cheap to clone.
#[derive(Clone)]
pub struct Sandbox {
    inner: Arc<SandboxInner>,
}

impl Sandbox {
    /// Open `sandbox.db` at `path` (created and migrated if needed).
    pub fn open(path: &Path, deps: SandboxDeps, opts: SandboxOptions) -> SbResult<Self> {
        let db = SandboxDb::open(path)?;
        Ok(Self::with_db(Arc::new(db), deps, opts))
    }

    /// An in-memory sandbox (tests).
    pub fn in_memory(deps: SandboxDeps, opts: SandboxOptions) -> SbResult<Self> {
        let db = SandboxDb::open_in_memory()?;
        Ok(Self::with_db(Arc::new(db), deps, opts))
    }

    pub fn with_db(db: Arc<SandboxDb>, deps: SandboxDeps, opts: SandboxOptions) -> Self {
        let core = Arc::new(Core {
            db,
            symbols: deps.symbols,
            quotes: deps.quotes,
            clock: deps.clock,
            bus: deps.bus,
            locks: locks::PositionLocks::new(),
            opts,
            engine_tx: parking_lot::RwLock::new(None),
            sweep_lock: tokio::sync::Mutex::new(()),
            t1_lock: tokio::sync::Mutex::new(()),
            catch_up_lock: tokio::sync::Mutex::new(()),
            mode_lock: tokio::sync::Mutex::new(()),
        });
        Self {
            inner: Arc::new(SandboxInner {
                core,
                scheduler: SyncMutex::new(None),
                engine: tokio::sync::Mutex::new(None),
            }),
        }
    }

    fn core(&self) -> &Arc<Core> {
        &self.inner.core
    }

    /// The underlying database (tests and diagnostics).
    pub fn db(&self) -> &Arc<SandboxDb> {
        &self.core().db
    }

    /// The signed-in user the sandbox books belong to.
    pub fn user_id(&self) -> &str {
        self.core().user()
    }

    /// Entries in the position lock table (resource tests).
    pub fn lock_table_len(&self) -> usize {
        self.core().locks.len()
    }

    // -- orders ------------------------------------------------------------

    /// `placeorder`.
    pub async fn place_order(&self, req: OrderRequest) -> SbResult<OrderPlaced> {
        orders::place(self.core(), req, None).await
    }

    /// `placeorder` with a quote the caller already fetched (basket orders
    /// price every leg with one multiquotes call).
    pub async fn place_order_with_quote(
        &self,
        req: OrderRequest,
        quote: Quote,
    ) -> SbResult<OrderPlaced> {
        orders::place(self.core(), req, Some(quote)).await
    }

    /// `placesmartorder`: reach `position_size`, deciding under the
    /// position's lock.
    pub async fn place_smart_order(&self, req: SmartOrderRequest) -> SbResult<SmartOrderReply> {
        let core = self.core();
        let product = if req.product.trim().is_empty() {
            "MIS".to_string()
        } else {
            req.product.trim().to_ascii_uppercase()
        };
        let exchange = req.exchange.trim().to_ascii_uppercase();
        let symbol = req.symbol.trim().to_string();
        let guard = core
            .locks
            .lock(locks::PositionKey::new(
                core.user(),
                &exchange,
                &symbol,
                &product,
            ))
            .await;
        let (rows, _) = positions::open_positions(core, false).await?;
        let current = rows
            .iter()
            .find(|p| p.symbol == symbol && p.exchange == exchange && p.product.as_str() == product)
            .map(|p| p.quantity)
            .unwrap_or(0);
        let target = req.position_size;
        let original_qty = req.quantity;
        let (action, quantity) = if target == 0 && current == 0 && original_qty != 0 {
            (req.action.clone(), original_qty)
        } else if target == current {
            let message = if original_qty == 0 {
                "No OpenPosition Found. Not placing Exit order."
            } else {
                "Positions Already Matched. No Action needed."
            };
            return Ok(SmartOrderReply::NoAction(Message::new(message)));
        } else if target == 0 && current > 0 {
            ("SELL".to_string(), current.abs())
        } else if target == 0 && current < 0 {
            ("BUY".to_string(), current.abs())
        } else if current == 0 {
            (
                if target > 0 { "BUY" } else { "SELL" }.to_string(),
                target.abs(),
            )
        } else {
            let diff = target - current;
            (
                if diff > 0 { "BUY" } else { "SELL" }.to_string(),
                diff.abs(),
            )
        };
        let price_type = if req.price_type.trim().is_empty() {
            "MARKET".to_string()
        } else {
            req.price_type.clone()
        };
        let order = OrderRequest {
            symbol,
            exchange,
            action,
            quantity,
            price: req.price,
            trigger_price: req.trigger_price,
            price_type,
            product,
            strategy: req.strategy.clone(),
        };
        let placed = orders::place_locked(core, &guard, order, None, None).await?;
        Ok(SmartOrderReply::Placed(placed))
    }

    /// `modifyorder`.
    pub async fn modify_order(&self, orderid: &str, m: ModifyRequest) -> SbResult<OrderMessage> {
        orders::modify(self.core(), orderid, m).await
    }

    /// `cancelorder`.
    pub async fn cancel_order(&self, orderid: &str) -> SbResult<OrderMessage> {
        orders::cancel(self.core(), orderid).await
    }

    /// `cancelallorder`: every `open` and `trigger pending` order of the
    /// session.
    pub async fn cancel_all_orders(&self) -> SbResult<CancelAllReply> {
        orders::cancel_all(self.core()).await
    }

    /// `closeposition` for one position.
    pub async fn close_position(
        &self,
        symbol: &str,
        exchange: &str,
        product: &str,
    ) -> SbResult<OrderMessage> {
        positions::close_position(
            self.core(),
            symbol.trim(),
            &exchange.trim().to_ascii_uppercase(),
            &product.trim().to_ascii_uppercase(),
        )
        .await
    }

    /// `closeposition` without a symbol: close everything open.
    pub async fn close_all_positions(&self) -> SbResult<CloseAllReply> {
        let (rows, _) = positions::open_positions(self.core(), true).await?;
        if rows.is_empty() {
            return Ok(CloseAllReply::Nothing(Message::new(
                "No open positions to close",
            )));
        }
        let mut closed = 0i64;
        let mut pending = 0i64;
        let mut failed = 0i64;
        for p in rows.iter().filter(|p| p.quantity != 0) {
            match positions::close_position(self.core(), &p.symbol, &p.exchange, p.product.as_str())
                .await
            {
                // Closed only once the close filled (SB-05); a close still
                // waiting for a price is pending.
                Ok(m) if squareoff::close_filled(self.core(), &m.orderid).await => closed += 1,
                Ok(_) => pending += 1,
                Err(_) => failed += 1,
            }
        }
        let mut message = format!("Closed {closed} positions");
        if pending > 0 {
            message.push_str(&format!(
                " ({pending} close orders are waiting for a price and fill when one arrives)"
            ));
        }
        if failed > 0 {
            message.push_str(&format!(" (Failed to close {failed} positions)"));
        }
        Ok(CloseAllReply::Closed {
            status: SUCCESS,
            message,
            closed_positions: closed,
            failed_closures: failed,
            mode: ANALYZE,
        })
    }

    // -- books -------------------------------------------------------------

    /// `orderbook`: the current session's orders, newest first, with the
    /// statistics block.
    pub async fn orderbook(&self) -> SbResult<OrderbookReply> {
        blocking(self.core(), |c| {
            let since = ts(c.session_start());
            let rows =
                c.db.with_conn(|conn| orders::orders_since(conn, c.user(), &since))?;
            Ok(orders::orderbook_reply(&rows))
        })
        .await
    }

    /// `tradebook`: the current session's trades.
    pub async fn tradebook(&self) -> SbResult<TradebookReply> {
        blocking(self.core(), |c| {
            let since = ts(c.session_start());
            let rows =
                c.db.with_conn(|conn| positions::trades_since(conn, c.user(), &since))?;
            Ok(positions::tradebook_reply(&rows))
        })
        .await
    }

    /// `positionbook`: the session's positions marked to market.
    pub async fn positionbook(&self) -> SbResult<PositionbookReply> {
        let (rows, _) = positions::open_positions(self.core(), true).await?;
        Ok(positions::positionbook_reply(self.core(), &rows))
    }

    /// `holdings`.
    pub async fn holdings(&self) -> SbResult<HoldingsReply> {
        holdings::holdings(self.core(), true).await
    }

    /// `funds` (runs the weekly auto-reset check first, as the web does).
    pub async fn funds(&self) -> SbResult<FundsReply> {
        blocking(self.core(), |c| {
            c.db.with_tx(|tx| {
                let cfg = SandboxConfig::load(tx)?;
                let now = c.now();
                let now_s = ts(now);
                let f = funds::ensure(tx, c.user(), cfg.starting_capital, &now_s)?;
                if let Some(day) = cfg.reset_day {
                    use chrono::Datelike;
                    let reset_at = now.date().and_time(cfg.reset_time);
                    let last = clock::parse_ts(&f.last_reset_date).unwrap_or(now);
                    if now.weekday() == day && now >= reset_at && last < reset_at {
                        squareoff::reset_account(tx, c.user(), cfg.starting_capital, &now_s)?;
                    }
                }
                let f = funds::ensure(tx, c.user(), cfg.starting_capital, &now_s)?;
                Ok::<_, SandboxError>(FundsReply {
                    status: SUCCESS,
                    data: FundsData {
                        availablecash: money(f.available_balance),
                        collateral: 0.0,
                        m2munrealized: money(f.unrealized_pnl),
                        m2mrealized: money(f.today_realized_pnl),
                        total_realized_pnl: money(f.realized_pnl),
                        today_realized_pnl: money(f.today_realized_pnl),
                        utiliseddebits: money(f.used_margin),
                        grossexposure: money(f.used_margin),
                        totalpnl: money(f.total_pnl),
                        last_reset: display_seconds(&f.last_reset_date),
                        reset_count: f.reset_count,
                    },
                    mode: ANALYZE,
                })
            })
        })
        .await
    }

    /// `orderstatus`.
    pub async fn order_status(&self, orderid: &str) -> SbResult<OrderStatusReply> {
        let orderid = orderid.to_string();
        blocking(self.core(), move |c| {
            let row =
                c.db.with_conn(|conn| orders::get_by_orderid(conn, c.user(), &orderid))?;
            match row {
                Some(o) => Ok(orders::order_status_reply(&o)),
                None => Err(SandboxError::not_found(format!(
                    "Order {orderid} not found"
                ))),
            }
        })
        .await
    }

    /// `openposition`: the session quantity of one position (0 when none).
    pub async fn open_position(
        &self,
        symbol: &str,
        exchange: &str,
        product: &str,
    ) -> SbResult<OpenPositionReply> {
        let (rows, _) = positions::open_positions(self.core(), true).await?;
        let quantity = rows
            .iter()
            .find(|p| p.symbol == symbol && p.exchange == exchange && p.product.as_str() == product)
            .map(|p| p.quantity)
            .unwrap_or(0);
        Ok(OpenPositionReply {
            status: SUCCESS,
            quantity,
            mode: ANALYZE,
        })
    }

    /// `pnl/symbols`: the day's P&L per symbol.
    pub async fn pnl_symbols(&self) -> SbResult<PnlSymbolsReply> {
        let book = self.positionbook().await?;
        Ok(PnlSymbolsReply {
            status: SUCCESS,
            data: book
                .data
                .iter()
                .map(|p| PnlSymbolRow {
                    symbol: p.symbol.clone(),
                    exchange: p.exchange.clone(),
                    product: p.product.clone(),
                    quantity: p.quantity,
                    pnl: p.pnl,
                    unrealized_pnl: p.unrealized_pnl,
                    today_realized_pnl: p.today_realized_pnl,
                    total_pnl_today: p.total_pnl_today,
                })
                .collect(),
            total_pnl: book.total_pnl,
            total_unrealized_pnl: book.total_unrealized_pnl,
            total_today_realized_pnl: book.total_today_realized_pnl,
            total_pnl_today: book.total_pnl_today,
            mode: ANALYZE,
        })
    }

    // -- GTT ---------------------------------------------------------------

    /// `placegttorder`.
    pub async fn place_gtt(&self, req: GttRequest) -> SbResult<GttReply> {
        gtt::place(self.core(), req).await
    }

    /// `modifygttorder`.
    pub async fn modify_gtt(&self, trigger_id: &str, req: GttRequest) -> SbResult<GttReply> {
        gtt::modify(self.core(), trigger_id, req).await
    }

    /// `cancelgttorder`.
    pub async fn cancel_gtt(&self, trigger_id: &str) -> SbResult<GttReply> {
        gtt::cancel(self.core(), trigger_id).await
    }

    /// `gttorderbook`. `status`: `Some("active")` is the web default;
    /// `None` (the request's `"all"`) returns every status.
    pub async fn gtt_orderbook(&self, status: Option<&str>) -> SbResult<GttOrderbookReply> {
        gtt::list(self.core(), status.map(str::to_string)).await
    }

    // -- settings ----------------------------------------------------------

    /// `/sandbox/api/configs` body.
    pub async fn configs(&self) -> SbResult<serde_json::Value> {
        blocking(self.core(), |c| Ok(c.db.with_conn(config::grouped)?)).await
    }

    /// Raw value of one config key.
    pub async fn config_value(&self, key: &str) -> SbResult<String> {
        let key = key.to_string();
        blocking(self.core(), move |c| {
            Ok(c.db.with_conn(|conn| config::get_raw(conn, &key))?)
        })
        .await
    }

    /// The typed config.
    pub async fn config(&self) -> SbResult<SandboxConfig> {
        blocking(self.core(), |c| c.config()).await
    }

    /// `/sandbox/update`: validate, write, apply side effects (capital
    /// rebase, schedule reload).
    pub async fn update_config(&self, key: &str, value: &str) -> SbResult<SettingsMessage> {
        if config::default_of(key).is_none() {
            return Err(SandboxError::bad_request(format!(
                "Unknown sandbox setting: {key}"
            )));
        }
        if let Some(msg) = config::validate(key, value) {
            return Err(SandboxError::bad_request(msg));
        }
        let (k, v) = (key.to_string(), value.to_string());
        blocking(self.core(), move |c| {
            c.db.with_tx(|tx| {
                let now = c.now_ts();
                config::set_raw(tx, &k, &v, &now)?;
                if k == "starting_capital" {
                    let capital = types::dec_from_db(&v);
                    let users: Vec<String> = {
                        let mut stmt =
                            tx.prepare("SELECT user_id FROM sandbox_funds ORDER BY id")?;
                        let rows = stmt.query_map([], |r| r.get(0))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    for u in users {
                        let r = funds::apply(tx, &u, capital, &now, |f| {
                            Ok(funds::rebased(f, capital))
                        })?;
                        if let Err(r) = r {
                            return Err(SandboxError::conflict(r.0));
                        }
                    }
                }
                Ok(())
            })
        })
        .await?;
        if key.ends_with("square_off_time")
            || key == "reset_day"
            || key == "reset_time"
            || key == "order_check_interval"
        {
            self.inner.invalidate_schedule();
            self.core().notify_engine(EngineCmd::ConfigChanged);
        }
        Ok(SettingsMessage {
            status: SUCCESS,
            message: format!("Configuration {key} updated successfully"),
        })
    }

    /// `/sandbox/reset`: the 14 editable settings back to their defaults and
    /// the account wiped (orders, trades, positions, holdings, P&L history,
    /// GTTs) with funds at the starting capital and `reset_count + 1`. The
    /// engine is paused for the wipe and resumed only if analyzer mode still
    /// wants it.
    pub async fn reset(&self) -> SbResult<SettingsMessage> {
        let core = self.core().clone();
        let _mode = core.mode_lock.lock().await;
        let paused = self.inner.engine.lock().await.take();
        let ticks = paused.as_ref().map(|h| h.ticks.clone());
        if let Some(h) = paused {
            h.stop().await;
        }
        let r = blocking(&core, |c| {
            c.db.with_tx(|tx| {
                let now = c.now_ts();
                for k in config::RESET_KEYS {
                    config::set_raw(tx, k, config::default_of(k).unwrap_or(""), &now)?;
                }
                let user = c.user();
                for sql in [
                    "DELETE FROM sandbox_orders WHERE user_id = ?1",
                    "DELETE FROM sandbox_trades WHERE user_id = ?1",
                    "DELETE FROM sandbox_positions WHERE user_id = ?1",
                    "DELETE FROM sandbox_holdings WHERE user_id = ?1",
                    "DELETE FROM sandbox_daily_pnl WHERE user_id = ?1",
                    "DELETE FROM sandbox_gtt_legs WHERE gtt_id IN (SELECT gtt_id FROM sandbox_gtt WHERE user_id = ?1)",
                    "DELETE FROM sandbox_gtt WHERE user_id = ?1",
                ] {
                    tx.execute(sql, rusqlite::params![user])?;
                }
                let capital = types::dec_from_db(config::default_of("starting_capital").unwrap_or("10000000"));
                funds::reset_row(tx, user, capital, &now)?;
                Ok::<_, SandboxError>(())
            })
        })
        .await;
        self.inner.invalidate_schedule();
        if let Some(t) = ticks {
            let handle = engine::spawn(Arc::downgrade(&self.inner), core.clone(), t);
            *self.inner.engine.lock().await = Some(handle);
        }
        r?;
        Ok(SettingsMessage {
            status: SUCCESS,
            message: "Configuration and data reset to defaults successfully. All orders, trades, positions, holdings, and P&L history have been cleared.".to_string(),
        })
    }

    /// `/sandbox/squareoff-status`.
    pub async fn squareoff_status(&self) -> SquareOffStatusReply {
        let running = self
            .inner
            .engine
            .lock()
            .await
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        let mut guard = self.inner.scheduler.lock();
        if guard.is_none() {
            if let Ok(cfg) = self.core().config() {
                *guard = Some(Scheduler::build(
                    &cfg,
                    self.core().opts.session_expiry,
                    self.core().now(),
                ));
            }
        }
        let data = guard
            .as_ref()
            .map(|s| s.status_reply(running))
            .unwrap_or(SquareOffStatus {
                running,
                timezone: None,
                jobs: vec![],
            });
        SquareOffStatusReply {
            status: SUCCESS,
            data,
            mode: ANALYZE,
        }
    }

    /// `/sandbox/reload-squareoff`: rebuild the schedule from config.
    pub async fn reload_squareoff(&self) -> SquareOffStatusReply {
        self.inner.invalidate_schedule();
        self.core().notify_engine(EngineCmd::ConfigChanged);
        self.squareoff_status().await
    }

    /// `/sandbox/mypnl/api/data`.
    pub async fn mypnl(&self) -> SbResult<MyPnlReply> {
        blocking(self.core(), |c| {
            c.db.with_conn(|conn| {
                let user = c.user();
                let mut positions_rows = positions::list_user(conn, user)?;
                positions_rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                let holdings_rows = holdings::list_user(conn, user)?;
                let f = funds::read(conn, user)?;
                let mut pos_unrealized = Decimal::ZERO;
                let positions_out = positions_rows
                    .iter()
                    .map(|p| {
                        let unrealized = if p.quantity != 0 { p.pnl } else { Decimal::ZERO };
                        pos_unrealized += unrealized;
                        MyPnlPosition {
                            symbol: p.symbol.clone(),
                            exchange: p.exchange.clone(),
                            product: p.product.as_str().to_string(),
                            quantity: p.quantity,
                            average_price: money(p.average_price),
                            ltp: money(p.ltp.unwrap_or(Decimal::ZERO)),
                            unrealized_pnl: money(unrealized),
                            today_realized_pnl: money(p.today_realized_pnl),
                            all_time_realized_pnl: money(p.accumulated_realized_pnl),
                            status: if p.quantity != 0 { "Open" } else { "Closed" }.to_string(),
                            updated_at: display_seconds(&p.updated_at),
                        }
                    })
                    .collect();
                let mut hold_unrealized = Decimal::ZERO;
                let holdings_out = holdings_rows
                    .iter()
                    .map(|h| {
                        hold_unrealized += h.pnl;
                        MyPnlHolding {
                            symbol: h.symbol.clone(),
                            exchange: h.exchange.clone(),
                            product: "CNC".to_string(),
                            quantity: h.quantity,
                            average_price: money(h.average_price),
                            ltp: money(h.ltp.unwrap_or(Decimal::ZERO)),
                            unrealized_pnl: money(h.pnl),
                            pnl_percent: pct(h.pnl_percent),
                            settlement_date: h.settlement_date.clone(),
                        }
                    })
                    .collect();
                let trades = {
                    let mut stmt = conn.prepare_cached(&format!(
                        "SELECT {} FROM sandbox_trades WHERE user_id = ?1 ORDER BY trade_timestamp DESC, id DESC LIMIT 50",
                        db::TradeRow::COLUMNS
                    ))?;
                    let rows = stmt.query_map(rusqlite::params![user], db::TradeRow::from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                let daily = {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM sandbox_daily_pnl WHERE user_id = ?1 ORDER BY date DESC LIMIT 30",
                    )?;
                    let rows = stmt.query_map(rusqlite::params![user], db::DailyPnlRow::from_row)?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                let today = f.as_ref().map(|f| f.today_realized_pnl).unwrap_or(Decimal::ZERO);
                let total_unrealized = pos_unrealized + hold_unrealized;
                Ok::<_, SandboxError>(MyPnlReply {
                    status: SUCCESS,
                    data: MyPnlData {
                        summary: MyPnlSummary {
                            today_realized_pnl: money(today),
                            all_time_realized_pnl: f.as_ref().map(|f| money(f.realized_pnl)).unwrap_or(0.0),
                            positions_unrealized_pnl: money(pos_unrealized),
                            holdings_unrealized_pnl: money(hold_unrealized),
                            total_unrealized_pnl: money(total_unrealized),
                            today_total_mtm: money(today + total_unrealized),
                            total_pnl: f.as_ref().map(|f| money(f.total_pnl)).unwrap_or(0.0),
                            available_balance: f.as_ref().map(|f| money(f.available_balance)).unwrap_or(0.0),
                            total_capital: f.as_ref().map(|f| money(f.total_capital)).unwrap_or(0.0),
                        },
                        daily_pnl: daily
                            .iter()
                            .map(|d| MyPnlDaily {
                                date: d.date.clone(),
                                realized_pnl: money(d.realized_pnl),
                                positions_unrealized: money(d.positions_unrealized_pnl),
                                holdings_unrealized: money(d.holdings_unrealized_pnl),
                                total_unrealized: money(d.positions_unrealized_pnl + d.holdings_unrealized_pnl),
                                total_mtm: money(d.total_mtm),
                                portfolio_value: money(d.portfolio_value),
                            })
                            .collect(),
                        positions: positions_out,
                        holdings: holdings_out,
                        trades: trades
                            .iter()
                            .map(|t| MyPnlTrade {
                                tradeid: t.tradeid.clone(),
                                symbol: t.symbol.clone(),
                                exchange: t.exchange.clone(),
                                action: t.action.clone(),
                                quantity: t.quantity,
                                price: float(t.price),
                                product: t.product.clone(),
                                timestamp: display_seconds(&t.trade_timestamp),
                            })
                            .collect(),
                    },
                })
            })
        })
        .await
    }

    // -- engine ------------------------------------------------------------

    /// Analyzer mode toggled: on starts the engine (after a catch-up), off
    /// stops it. One transition at a time.
    pub async fn set_analyzer_mode(&self, on: bool, ticks: Arc<dyn TickSource>) -> SbResult<()> {
        let _mode = self.core().mode_lock.lock().await;
        if on {
            self.start_engine_locked(ticks).await
        } else {
            self.stop_engine_locked().await;
            Ok(())
        }
    }

    /// Start the engine task (no-op when already running). Runs catch-up
    /// first.
    pub async fn start_engine(&self, ticks: Arc<dyn TickSource>) -> SbResult<()> {
        let _mode = self.core().mode_lock.lock().await;
        self.start_engine_locked(ticks).await
    }

    async fn start_engine_locked(&self, ticks: Arc<dyn TickSource>) -> SbResult<()> {
        let mut slot = self.inner.engine.lock().await;
        if slot.as_ref().map(|h| !h.is_finished()).unwrap_or(false) {
            return Ok(());
        }
        if let Err(e) = catch_up::run(self.core()).await {
            tracing::warn!("Sandbox catch-up before start failed: {}", e.message);
        }
        *slot = Some(engine::spawn(
            Arc::downgrade(&self.inner),
            self.core().clone(),
            ticks,
        ));
        Ok(())
    }

    /// Stop the engine task gracefully.
    pub async fn stop_engine(&self) {
        let _mode = self.core().mode_lock.lock().await;
        self.stop_engine_locked().await;
    }

    async fn stop_engine_locked(&self) {
        let h = self.inner.engine.lock().await.take();
        if let Some(h) = h {
            h.stop().await;
        }
    }

    /// Whether the engine task is running.
    pub async fn is_engine_running(&self) -> bool {
        self.inner
            .engine
            .lock()
            .await
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// App exit.
    pub async fn shutdown(&self) {
        self.stop_engine().await;
    }

    /// Evaluate a tick directly (as the engine does for a watched symbol).
    /// Returns `(orders filled, GTT legs fired)`.
    pub async fn on_tick(&self, tick: Tick) -> SbResult<(usize, usize)> {
        let key = tick.key();
        let filled = execution::on_price(self.core(), &key, tick.ltp).await?;
        let fired = gtt::on_price(self.core(), &key, tick.ltp).await?;
        Ok((filled, fired))
    }

    /// One polling pass over every resting order and GTT leg.
    pub async fn poll_once(&self) -> SbResult<PollStats> {
        execution::poll_once(self.core()).await
    }

    /// Run the schedule's due jobs at the clock's current time. The first
    /// call builds the schedule; returns the ids of jobs that ran.
    pub async fn run_due_jobs(&self) -> SbResult<Vec<String>> {
        self.inner.run_due_jobs_inner().await
    }

    /// Catch-up (single flight). `None` when another run is in progress.
    pub async fn catch_up(&self) -> SbResult<Option<CatchUpReport>> {
        catch_up::run(self.core()).await
    }

    /// One square-off sweep now.
    pub async fn square_off_now(&self) -> SbResult<SweepReport> {
        squareoff::sweep(self.core()).await
    }

    /// T+1 settlement now. Returns positions settled.
    pub async fn t1_settlement(&self) -> SbResult<usize> {
        holdings::process_t1(self.core()).await
    }

    /// GTT maintenance now: `(legs reclaimed, GTTs expired)`.
    pub async fn gtt_maintenance(&self) -> SbResult<(usize, usize)> {
        gtt::maintain(self.core()).await
    }

    /// Margin discrepancy (`used_margin - held`) for the user; zero when the
    /// books agree.
    pub async fn margin_discrepancy(&self) -> SbResult<Decimal> {
        blocking(self.core(), |c| {
            Ok(c.db.with_conn(|conn| -> rusqlite::Result<Decimal> {
                let Some(f) = funds::read(conn, c.user())? else {
                    return Ok(Decimal::ZERO);
                };
                Ok(f.used_margin - funds::expected_used_margin(conn, c.user())?)
            })?)
        })
        .await
    }

    /// The raw funds row (tests and diagnostics).
    pub async fn funds_row(&self) -> SbResult<Option<funds::FundsRow>> {
        blocking(self.core(), |c| {
            Ok(c.db.with_conn(|conn| funds::read(conn, c.user()))?)
        })
        .await
    }

    /// The raw position row (tests and diagnostics).
    pub async fn position_row(
        &self,
        symbol: &str,
        exchange: &str,
        product: &str,
    ) -> SbResult<Option<db::PositionRow>> {
        let (s, e, p) = (
            symbol.to_string(),
            exchange.to_string(),
            product.to_string(),
        );
        blocking(self.core(), move |c| {
            Ok(c.db.with_conn(|conn| positions::get(conn, c.user(), &s, &e, &p))?)
        })
        .await
    }

    /// The raw order row (tests and diagnostics).
    pub async fn order_row(&self, orderid: &str) -> SbResult<Option<db::OrderRow>> {
        let id = orderid.to_string();
        blocking(self.core(), move |c| {
            Ok(c.db.with_conn(|conn| orders::get_by_orderid(conn, c.user(), &id))?)
        })
        .await
    }

    /// Symbols the engine would watch right now.
    pub async fn watched_symbols(&self) -> SbResult<std::collections::HashSet<SymbolKey>> {
        blocking(self.core(), |c| {
            Ok(c.db.with_conn(engine::needed_symbols)?)
        })
        .await
    }
}

pub use types::{Action, OrderStatus, PriceType, Product};
