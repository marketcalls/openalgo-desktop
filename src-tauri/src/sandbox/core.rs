//! The shared engine state every sandbox operation runs against.

use super::clock::{self, Clock};
use super::config::SandboxConfig;
use super::db::SandboxDb;
use super::events::Outbox;
use super::locks::PositionLocks;
use super::quotes::{Quote, QuoteSource};
use super::session;
use super::types::{SandboxError, SbResult, SymbolKey, SymbolMeta, SymbolSource};
use crate::events::EventBus;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Is this date a trading day? Used to skip the 23:59 P&L snapshot on
/// weekends and holidays.
pub type TradingDayFn = Arc<dyn Fn(NaiveDate) -> bool + Send + Sync>;

/// Engine settings that are not trader-editable sandbox config.
#[derive(Clone)]
pub struct SandboxOptions {
    /// The signed-in user (single-user desktop; the column is kept for web
    /// schema parity).
    pub user_id: String,
    /// Session boundary (web `SESSION_EXPIRY_TIME`, default 03:00 IST).
    pub session_expiry: NaiveTime,
    /// 24x7 markets (crypto): no session boundary square-off catch-up
    /// (web `DISABLE_SESSION_EXPIRY`).
    pub session_expiry_disabled: bool,
    /// Waits between quote attempts when pricing a MARKET order (web: 0.3 s
    /// then 0.6 s, three attempts in all).
    pub quote_retry_delays: Vec<Duration>,
    /// Trading-day calendar (weekdays by default; the app passes the market
    /// calendar, `market_calendar_service::sandbox_trading_day`).
    pub trading_day: TradingDayFn,
    /// A symbol whose feed has been silent this long counts as stale and
    /// the polling fallback quotes it.
    pub stale_feed_after: Duration,
    /// A tick or quote received longer ago than this is not filled at; the
    /// order waits for a newer price.
    pub stale_quote_after: Duration,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            user_id: "sandbox".to_string(),
            session_expiry: session::default_session_expiry(),
            session_expiry_disabled: false,
            quote_retry_delays: vec![Duration::from_millis(300), Duration::from_millis(600)],
            trading_day: Arc::new(|d: NaiveDate| {
                !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
            }),
            stale_feed_after: Duration::from_secs(30),
            stale_quote_after: Duration::from_secs(30),
        }
    }
}

impl std::fmt::Debug for SandboxOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxOptions")
            .field("user_id", &self.user_id)
            .field("session_expiry", &self.session_expiry)
            .field("session_expiry_disabled", &self.session_expiry_disabled)
            .finish()
    }
}

/// Messages from the order path to the running engine task.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCmd {
    /// Something on this symbol now needs ticks (pending order, GTT leg,
    /// open position).
    Watch(SymbolKey),
    /// Config changed: rebuild the schedule.
    ConfigChanged,
    /// Re-read everything from the database.
    Rebuild,
}

/// Bound of the engine command channel. A full channel drops the command;
/// the engine's periodic rebuild recovers anything missed.
pub const ENGINE_CMD_CAPACITY: usize = 1024;

pub(crate) struct Core {
    pub db: Arc<SandboxDb>,
    pub symbols: Arc<dyn SymbolSource>,
    pub quotes: Arc<dyn QuoteSource>,
    pub clock: Arc<dyn Clock>,
    pub bus: Option<Arc<EventBus>>,
    pub locks: PositionLocks,
    pub opts: SandboxOptions,
    pub engine_tx: RwLock<Option<mpsc::Sender<EngineCmd>>>,
    /// One square-off sweep at a time.
    pub sweep_lock: tokio::sync::Mutex<()>,
    /// One T+1 settlement at a time.
    pub t1_lock: tokio::sync::Mutex<()>,
    /// One catch-up at a time (single flight: a second trigger skips).
    pub catch_up_lock: tokio::sync::Mutex<()>,
    /// Mode transitions and resets.
    pub mode_lock: tokio::sync::Mutex<()>,
}

impl Core {
    pub fn now(&self) -> NaiveDateTime {
        clock::now_ist(self.clock.as_ref())
    }

    pub fn now_ts(&self) -> String {
        clock::ts(self.now())
    }

    pub fn user(&self) -> &str {
        &self.opts.user_id
    }

    pub fn symbol(&self, symbol: &str, exchange: &str) -> Option<SymbolMeta> {
        self.symbols.lookup(symbol, exchange)
    }

    /// Contract value multiplier (1 when unknown).
    pub fn contract_value(&self, symbol: &str, exchange: &str) -> Decimal {
        self.symbol(symbol, exchange)
            .map(|m| m.contract_value)
            .filter(|cv| *cv > Decimal::ZERO)
            .unwrap_or(Decimal::ONE)
    }

    pub fn config(&self) -> SbResult<SandboxConfig> {
        Ok(self.db.with_conn(SandboxConfig::load)?)
    }

    pub fn session_start(&self) -> NaiveDateTime {
        session::session_start(self.now(), self.opts.session_expiry)
    }

    pub fn publish(&self, outbox: Outbox) {
        outbox.publish(self.bus.as_deref());
    }

    pub fn notify_engine(&self, cmd: EngineCmd) {
        if let Some(tx) = self.engine_tx.read().as_ref() {
            if tx.try_send(cmd).is_err() {
                tracing::debug!("Sandbox engine command queue full; the next rebuild catches up");
            }
        }
    }

    /// One quote with the web's retry schedule.
    pub async fn quote_with_retry(&self, symbol: &str, exchange: &str) -> Option<Quote> {
        let attempts = self.opts.quote_retry_delays.len() + 1;
        for attempt in 0..attempts {
            if let Some(q) = self.quotes.quote(symbol, exchange).await {
                if q.ltp > Decimal::ZERO {
                    return Some(q);
                }
            }
            if let Some(d) = self.opts.quote_retry_delays.get(attempt) {
                if !d.is_zero() {
                    tokio::time::sleep(*d).await;
                }
            }
        }
        None
    }
}

/// Run a synchronous database closure off the async runtime.
pub(crate) async fn blocking<R, F>(core: &Arc<Core>, f: F) -> SbResult<R>
where
    R: Send + 'static,
    F: FnOnce(&Core) -> SbResult<R> + Send + 'static,
{
    let core = core.clone();
    match tokio::task::spawn_blocking(move || f(&core)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Sandbox task failed: {}", e);
            Err(SandboxError::internal(
                "The sandbox could not complete this request. Try again; if it keeps failing, restart OpenAlgo.",
            ))
        }
    }
}
