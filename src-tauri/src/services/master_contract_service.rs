//! Master contract download and the symbol cache (web `utils/auth_utils.py`
//! `should_download_master_contract`, `async_master_contract_download`,
//! `load_existing_master_contract`, and `database/master_contract_cache_hook.py`).
//!
//! * Smart rule: download when the broker never downloaded, when another
//!   broker downloaded more recently (the `symtoken` table holds one master),
//!   when the last download was on an earlier day, or before the cutoff
//!   (08:00 IST; crypto brokers 00:00 UTC). Otherwise the stored master is
//!   loaded into memory.
//! * One download per broker at a time (claimed under one lock).
//! * Socket.IO, as the web: `master_contract_download` `{status, message}`
//!   when a download ends, then `cache_loaded` with the symbol counts.
//! * The downloaded list is written to SQLite on a blocking thread and
//!   handed to the resolver; nothing else keeps it.

use crate::brokers::types::AuthToken;
use crate::brokers::Broker;
use crate::db::sqlite::{master_contract_status as mcs, symbol};
use crate::error::{AppError, Result};
use crate::events::Event;
use crate::state::AppState;
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

/// Trader-facing refusal while a download runs (web
/// `MASTER_CONTRACT_BUSY_MESSAGE`).
pub const BUSY_MESSAGE: &str =
    "A master contract download is already running. Wait for it to finish, then try again.";

/// Brokers whose master follows the UTC day (web `CRYPTO_BROKERS`).
const CRYPTO_BROKERS: &[&str] = &["deltaexchange"];

/// Download claims, one per broker.
#[derive(Default)]
pub struct DownloadClaims {
    running: Mutex<HashSet<String>>,
}

impl DownloadClaims {
    /// Claim `broker`; false when a download for it is already running.
    pub fn claim(&self, broker: &str) -> bool {
        self.running.lock().insert(broker.to_string())
    }

    pub fn release(&self, broker: &str) {
        self.running.lock().remove(broker);
    }

    pub fn is_running(&self, broker: &str) -> bool {
        self.running.lock().contains(broker)
    }
}

/// Cutoff hour, minute and reference zone (web `get_master_contract_cutoff`).
pub fn cutoff(broker: &str) -> (u32, u32, Tz) {
    if CRYPTO_BROKERS.contains(&broker) {
        (0, 0, chrono_tz::UTC)
    } else {
        (8, 0, chrono_tz::Asia::Kolkata)
    }
}

fn zone_label(tz: Tz) -> &'static str {
    if tz == chrono_tz::UTC {
        "UTC"
    } else {
        "IST"
    }
}

/// The web's smart-download decision from the stored history.
pub fn should_download_at(
    broker: &str,
    last_download: Option<DateTime<Utc>>,
    last_broker: Option<&str>,
    now: DateTime<Utc>,
) -> (bool, String) {
    let Some(last) = last_download else {
        return (true, "No previous download found".into());
    };
    if let Some(lb) = last_broker.filter(|b| *b != broker) {
        return (
            true,
            format!(
                "Broker changed from {} to {}, symtoken needs refresh",
                lb, broker
            ),
        );
    }
    let (h, m, tz) = cutoff(broker);
    let label = zone_label(tz);
    let today = now.with_timezone(&tz).date_naive();
    let last_local = last.with_timezone(&tz);
    if last_local.date_naive() != today {
        return (
            true,
            format!(
                "Last download was on {} {}, today is {}",
                last_local.date_naive(),
                label,
                today
            ),
        );
    }
    if last_local.hour() * 60 + last_local.minute() >= h * 60 + m {
        (
            false,
            format!(
                "Already downloaded today at {} {} (after {:02}:{:02} cutoff)",
                last_local.format("%H:%M"),
                label,
                h,
                m
            ),
        )
    } else {
        (
            true,
            format!("Download was before {:02}:{:02} {} cutoff", h, m, label),
        )
    }
}

/// The smart-download decision for `broker` now.
pub fn should_download(ctx: &AppState, broker: &str) -> Result<(bool, String)> {
    let conn = ctx.sqlite.conn()?;
    let last = mcs::last_download_time(&conn, broker)?;
    let lb = mcs::last_downloaded_broker(&conn)?;
    Ok(should_download_at(broker, last, lb.as_deref(), ctx.now()))
}

fn emit_download(ctx: &AppState, broker: &str, status: &str, message: &str) {
    ctx.bus.publish(Event::MasterContractDownload {
        broker: broker.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    });
}

fn emit_cache_loaded(ctx: &AppState, broker: &str, started: std::time::Instant) {
    let total = ctx.symbol_count();
    // Rough in-memory size: the resolver keeps each row once.
    let mb = (total as f64 * 400.0) / (1024.0 * 1024.0);
    ctx.bus.publish(Event::CacheLoaded {
        payload: json!({
            "status": "success",
            "broker": broker,
            "total_symbols": total,
            "memory_usage_mb": format!("{:.2}", mb),
            "load_time": format!("{:.2}", started.elapsed().as_secs_f64()),
        }),
    });
}

/// Load the stored master into memory (start-up, resume, cache reload).
/// Returns the number of instruments loaded.
pub async fn load_cached(ctx: &Arc<AppState>, broker: &str) -> Result<usize> {
    let started = std::time::Instant::now();
    let db = ctx.sqlite.clone();
    // With the contract multipliers (crypto `contract_value`).
    let master = tokio::task::spawn_blocking(move || {
        let conn = db.conn()?;
        symbol::load_master(&conn)
    })
    .await
    .map_err(|e| AppError::Internal(format!("symbol load task failed: {}", e)))??;
    let n = ctx.load_master_cache(master);
    emit_cache_loaded(ctx, broker, started);
    tracing::info!(
        "Loaded {} instruments for {} from the stored master",
        n,
        broker
    );
    Ok(n)
}

/// Download, store and load the master for a claimed broker.
async fn download_claimed(
    ctx: &Arc<AppState>,
    broker: &Arc<dyn Broker>,
    auth: &AuthToken,
) -> Result<usize> {
    let id = broker.id();
    let started = std::time::Instant::now();
    {
        let conn = ctx.sqlite.conn()?;
        mcs::update(
            &conn,
            id,
            "downloading",
            "Master contract download in progress",
            None,
            ctx.now(),
        )?;
    }
    // `download_master` carries per-row extras (Delta's contract_value),
    // stored and loaded with the rows.
    let master = match broker.download_master(auth).await {
        Ok(m) if !m.rows.is_empty() => m,
        Ok(_) => {
            return Err(AppError::Broker(
                "The broker sent an empty instrument list. Try the download again shortly.".into(),
            ))
        }
        Err(e) => return Err(e),
    };
    let db = ctx.sqlite.clone();
    let (master, stats) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut conn = db.conn()?;
        symbol::store_master(&mut conn, &master)?;
        let stats = symbol::exchange_counts(&conn)?;
        Ok((master, stats))
    })
    .await
    .map_err(|e| AppError::Internal(format!("symbol store task failed: {}", e)))??;
    let total = ctx.load_master_cache(master);
    {
        let conn = ctx.sqlite.conn()?;
        let now = ctx.now();
        mcs::update(
            &conn,
            id,
            "success",
            "Master contract download completed successfully",
            Some(total as i64),
            now,
        )?;
        mcs::record_download(&conn, id, started.elapsed().as_secs() as i64, &stats, now)?;
    }
    emit_download(ctx, id, "success", "Successfully Downloaded");
    emit_cache_loaded(ctx, id, started);
    tracing::info!(
        "Master contract for {}: {} instruments in {:.1}s",
        id,
        total,
        started.elapsed().as_secs_f64()
    );
    Ok(total)
}

/// Download now (the claim must not be held). `Err` with `BUSY_MESSAGE`
/// when one is already running.
pub async fn download(
    ctx: &Arc<AppState>,
    broker: &Arc<dyn Broker>,
    auth: &AuthToken,
) -> Result<usize> {
    let Some(claim) = try_claim(ctx, broker.id()) else {
        return Err(AppError::Validation(BUSY_MESSAGE.into()));
    };
    run_claimed(ctx, broker, auth, claim).await
}

/// A held download claim. Dropping it releases the claim, so a download
/// that finishes, fails, or whose task is aborted (logout, shutdown) never
/// leaves the broker marked as downloading.
pub struct DownloadClaim {
    ctx: std::sync::Weak<AppState>,
    broker: String,
}

impl Drop for DownloadClaim {
    fn drop(&mut self) {
        if let Some(c) = self.ctx.upgrade() {
            c.runtime.claims.release(&self.broker);
        }
    }
}

fn try_claim(ctx: &Arc<AppState>, broker: &str) -> Option<DownloadClaim> {
    ctx.runtime.claims.claim(broker).then(|| DownloadClaim {
        ctx: Arc::downgrade(ctx),
        broker: broker.to_string(),
    })
}

/// Claim `broker`'s download and only then reset its status row to pending
/// (web `try_start_master_contract_download(reset_status=True)`). `None`,
/// with the row untouched, when a download already holds the claim: that
/// download owns the row, and a reset made before losing the claim would
/// leave it pending after it had reported success.
pub fn claim_and_reset(ctx: &Arc<AppState>, broker: &str) -> Option<DownloadClaim> {
    let Some(claim) = try_claim(ctx, broker) else {
        tracing::info!(
            "Master contract download for {} already running; not starting another",
            broker
        );
        return None;
    };
    match ctx.sqlite.conn() {
        Ok(c) => {
            if let Err(e) = mcs::init_pending(&c, broker, ctx.now()) {
                tracing::warn!("Master contract status for {} not reset: {}", broker, e);
            }
        }
        Err(e) => tracing::warn!("Master contract status for {} not reset: {}", broker, e),
    }
    Some(claim)
}

/// Run a download under a held claim, release it, and record a failure.
pub async fn run_claimed(
    ctx: &Arc<AppState>,
    broker: &Arc<dyn Broker>,
    auth: &AuthToken,
    claim: DownloadClaim,
) -> Result<usize> {
    let id = broker.id();
    let r = download_claimed(ctx, broker, auth).await;
    drop(claim);
    if let Err(e) = &r {
        tracing::error!("Master contract download for {} failed: {}", id, e);
        let msg = e.client_message();
        if let Ok(conn) = ctx.sqlite.conn() {
            let _ = mcs::update(
                &conn,
                id,
                "error",
                &format!("Master contract download error: {}", msg),
                None,
                ctx.now(),
            );
        }
        emit_download(ctx, id, "error", &msg);
    }
    r
}

/// After a sign-in or resume: download when the smart rule says so,
/// otherwise load the stored master (web `handle_auth_success`).
pub async fn ensure(
    ctx: &Arc<AppState>,
    broker: &Arc<dyn Broker>,
    auth: &AuthToken,
) -> Result<usize> {
    let id = broker.id();
    let (needed, reason) = should_download(ctx, id)?;
    tracing::info!(
        "Smart download check for {}: should_download={}, reason={}",
        id,
        needed,
        reason
    );
    if !needed {
        {
            let conn = ctx.sqlite.conn()?;
            mcs::init_pending(&conn, id, ctx.now())?;
            mcs::mark_ready_cached(&conn, id, ctx.now())?;
        }
        match load_cached(ctx, id).await {
            Ok(n) if n > 0 => return Ok(n),
            Ok(_) => tracing::warn!("Stored master for {} is empty; downloading", id),
            Err(e) => tracing::warn!("Stored master for {} could not be read: {}", id, e),
        }
        return download(ctx, broker, auth).await;
    }
    // Claim first, reset the row second (web `handle_auth_success`).
    let Some(claim) = claim_and_reset(ctx, id) else {
        return Err(AppError::Validation(BUSY_MESSAGE.into()));
    };
    run_claimed(ctx, broker, auth, claim).await
}

/// `/api/cache/health` (web `get_cache_health`): the resolver has no
/// hit/miss counters, so the rate reads 100% once loaded.
pub fn cache_health(ctx: &AppState) -> Value {
    let total = ctx.symbol_count();
    let loaded = total > 0;
    let score = if loaded { 100 } else { 0 };
    json!({
        "health_score": score,
        "status": if loaded { "healthy" } else { "unhealthy" },
        "cache_loaded": loaded,
        "cache_valid": loaded,
        "hit_rate": if loaded { "100.00%" } else { "0.00%" },
        "total_symbols": total,
        "memory_usage_mb": format!("{:.2}", (total as f64 * 400.0) / (1024.0 * 1024.0)),
        "db_queries": 0,
        "recommendations": if loaded {
            vec!["Cache is operating optimally."]
        } else {
            vec!["Cache is not loaded. Run master contract download."]
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        chrono_tz::Asia::Kolkata
            .with_ymd_and_hms(2026, 10, d, h, m, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn smart_rule_follows_the_web() {
        let now = ist(3, 10, 0);
        assert_eq!(
            should_download_at("zerodha", None, None, now),
            (true, "No previous download found".into())
        );
        let (d, r) = should_download_at("zerodha", Some(ist(3, 8, 30)), Some("zerodha"), now);
        assert!(!d);
        assert_eq!(
            r,
            "Already downloaded today at 08:30 IST (after 08:00 cutoff)"
        );
        let (d, r) = should_download_at("zerodha", Some(ist(3, 7, 59)), Some("zerodha"), now);
        assert!(d);
        assert_eq!(r, "Download was before 08:00 IST cutoff");
        let (d, r) = should_download_at("zerodha", Some(ist(2, 9, 0)), Some("zerodha"), now);
        assert!(d);
        assert_eq!(
            r,
            "Last download was on 2026-10-02 IST, today is 2026-10-03"
        );
        let (d, r) = should_download_at("zerodha", Some(ist(3, 9, 0)), Some("angel"), now);
        assert!(d);
        assert_eq!(
            r,
            "Broker changed from angel to zerodha, symtoken needs refresh"
        );
        // Crypto follows the UTC day.
        let (d, _) = should_download_at("deltaexchange", Some(ist(3, 6, 0)), None, now);
        assert!(!d);
        assert_eq!(cutoff("deltaexchange").0, 0);
    }

    /// Delta's `contract_value` is filled only through `download_master`:
    /// the download stores it with the rows and loads it into memory, and a
    /// cache reload brings it back from the database.
    #[tokio::test]
    async fn contract_values_travel_with_the_master() {
        use crate::brokers::common::symbols::SymToken;
        use crate::brokers::mock::MockBroker;
        use crate::brokers::BrokerRegistry;
        let mock = Arc::new(MockBroker::new("deltaexchange"));
        let row = SymToken {
            symbol: "BTCUSD".into(),
            brsymbol: "BTCUSD".into(),
            name: "BTCUSD".into(),
            exchange: "CRYPTO".into(),
            brexchange: "CRYPTO".into(),
            token: "27".into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "PERPFUT".into(),
            tick_size: 0.5,
        };
        *mock.master.lock() = Some(Ok(vec![row]));
        mock.contract_values.lock().insert("27".into(), 0.001);
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn Broker>]),
            ist(3, 10, 0),
        );
        let ctx = &t.ctx;
        let broker: Arc<dyn Broker> = mock.clone();
        let n = download(ctx, &broker, &AuthToken::new("t")).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(ctx.symbols.contract_value("BTCUSD", "CRYPTO"), Some(0.001));
        let stored = ctx.sqlite.load_master().unwrap();
        assert_eq!(stored.contract_values.get("27"), Some(&0.001));
        ctx.clear_symbol_cache();
        assert_eq!(ctx.symbols.contract_value("BTCUSD", "CRYPTO"), None);
        assert_eq!(load_cached(ctx, "deltaexchange").await.unwrap(), 1);
        assert_eq!(ctx.symbols.contract_value("BTCUSD", "CRYPTO"), Some(0.001));
    }

    /// Web #2117 (`try_start_master_contract_download(reset_status=True)`):
    /// a start that loses the claim leaves the status row of the download
    /// that holds it alone, and a dropped claim (an aborted task) is
    /// released.
    #[tokio::test]
    async fn a_refused_start_leaves_the_running_downloads_status() {
        use crate::brokers::mock::MockBroker;
        use crate::brokers::BrokerRegistry;
        let mock = Arc::new(MockBroker::new("zerodha"));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![mock as Arc<dyn Broker>]),
            ist(3, 10, 0),
        );
        let ctx = &t.ctx;
        let status = || {
            let c = ctx.sqlite.conn().unwrap();
            mcs::get(&c, "zerodha", ctx.now())
                .unwrap()
                .map(|r| r.status)
        };
        let held = claim_and_reset(ctx, "zerodha").expect("first claim");
        assert_eq!(status().as_deref(), Some("pending"));
        {
            let c = ctx.sqlite.conn().unwrap();
            mcs::update(&c, "zerodha", "success", "done", Some(1), ctx.now()).unwrap();
        }
        assert!(claim_and_reset(ctx, "zerodha").is_none());
        assert_eq!(status().as_deref(), Some("success"));
        drop(held);
        assert!(!ctx.runtime.claims.is_running("zerodha"));
        assert!(claim_and_reset(ctx, "zerodha").is_some());
    }

    #[test]
    fn claims_are_exclusive() {
        let c = DownloadClaims::default();
        assert!(c.claim("angel"));
        assert!(!c.claim("angel"));
        assert!(c.is_running("angel"));
        c.release("angel");
        assert!(c.claim("angel"));
    }
}
