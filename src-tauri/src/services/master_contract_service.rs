//! Master contract download and the symbol cache (web `utils/auth_utils.py`
//! `should_download_master_contract`, `async_master_contract_download`,
//! `load_existing_master_contract`, and `database/master_contract_cache_hook.py`).
//!
//! * Smart rule: download when the broker never downloaded, when the stored
//!   master belongs to another broker or to no recorded broker (the
//!   `symtoken` table holds one master; its owner is written with the rows,
//!   `symbol::owner`), when the last download was on an earlier day, or
//!   before the cutoff (08:00 IST; crypto brokers 00:00 UTC). Otherwise the
//!   stored master is loaded into memory.
//! * A stored master is only ever loaded for the broker that owns it.
//! * One download per broker at a time (claimed under one lock).
//! * Socket.IO, as the web: `master_contract_download` `{status, message}`
//!   when a download ends, then `cache_loaded` with the symbol counts.
//! * The downloaded list is written to SQLite on a blocking thread and
//!   handed to the resolver; nothing else keeps it.

use crate::brokers::types::{AuthToken, SymbolData};
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

/// The web's smart-download decision from the stored history and the
/// stored master's owner (`None`: no recorded owner).
pub fn should_download_at(
    broker: &str,
    last_download: Option<DateTime<Utc>>,
    owner: Option<&str>,
    now: DateTime<Utc>,
) -> (bool, String) {
    let Some(last) = last_download else {
        return (true, "No previous download found".into());
    };
    match owner {
        None => {
            return (
                true,
                "The stored master contract has no recorded broker, symtoken needs refresh".into(),
            )
        }
        Some(o) if o != broker => {
            return (
                true,
                format!(
                    "Broker changed from {} to {}, symtoken needs refresh",
                    o, broker
                ),
            )
        }
        Some(_) => {}
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
    let owner = symbol::owner(&conn)?;
    Ok(should_download_at(
        broker,
        last,
        owner.as_ref().map(|o| o.broker.as_str()),
        ctx.now(),
    ))
}

/// The reason to skip an unforced download (`/api/master-contract/download`
/// without `force`), or `None` when it must run: the smart rule says skip
/// *and* the stored master is this broker's, holds rows, and is loaded.
pub fn skip_reason(ctx: &AppState, broker: &str) -> Option<String> {
    let (needed, reason) = should_download(ctx, broker).ok()?;
    if needed {
        return None;
    }
    let owner = ctx
        .sqlite
        .conn()
        .ok()
        .and_then(|c| symbol::owner(&c).ok())??;
    (owner.broker == broker && owner.rows > 0 && ctx.symbol_count() > 0).then_some(reason)
}

fn display_name(ctx: &AppState, broker: &str) -> String {
    ctx.brokers
        .get(broker)
        .map(|b| b.name().to_string())
        .unwrap_or_else(|| broker.to_string())
}

/// Trader-facing refusal to load a stored master that is not `broker`'s.
fn not_owner_message(ctx: &AppState, owner: Option<&str>, broker: &str) -> String {
    let want = display_name(ctx, broker);
    match owner {
        Some(o) => format!(
            "The stored master contract is for {}. Download the master contract for {}.",
            display_name(ctx, o),
            want
        ),
        None => format!(
            "The stored master contract does not say which broker it belongs to. Download the master contract for {}.",
            want
        ),
    }
}

/// Health alert (Health Monitor page) metric for a session whose master
/// contract could not be loaded; resolved once a master is loaded.
pub const MASTER_ALERT_METRIC: &str = "master_contract";

/// Tell the trader, on the Health Monitor page, that `broker`'s symbols
/// are not available, so orders, quotes and live prices cannot resolve
/// them until the master contract downloads.
pub fn report_unavailable(ctx: &AppState, broker: &str) {
    let now = ctx.now();
    let message = format!(
        "The {} master contract could not be loaded, so symbols cannot be used for orders or live prices. Download it from the Master Contract page.",
        display_name(ctx, broker)
    );
    let res = ctx.logs.conn().and_then(|c| {
        crate::db::sqlite::monitor::raise_alert(
            &c,
            &crate::db::sqlite::monitor::AlertRow {
                alert_type: "master_contract_unavailable".into(),
                severity: "fail".into(),
                metric_name: MASTER_ALERT_METRIC.into(),
                message,
                ..Default::default()
            },
            now,
        )
    });
    if let Err(e) = res {
        tracing::warn!("Could not record the master contract health alert: {}", e);
    }
}

fn clear_unavailable(ctx: &AppState) {
    let res = ctx
        .logs
        .conn()
        .and_then(|c| crate::db::sqlite::monitor::auto_resolve(&c, MASTER_ALERT_METRIC, ctx.now()));
    if let Err(e) = res {
        tracing::warn!("Could not resolve the master contract health alert: {}", e);
    }
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
/// Returns the number of instruments loaded. Refused with
/// `AppError::Validation` (the trader-facing reason) when the stored master
/// is not `broker`'s: its tokens would resolve to other instruments.
pub async fn load_cached(ctx: &Arc<AppState>, broker: &str) -> Result<usize> {
    let started = std::time::Instant::now();
    let db = ctx.sqlite.clone();
    let want = broker.to_string();
    // With the contract multipliers (crypto `contract_value`); the owner
    // and the rows are read in one transaction, so a download committing
    // meanwhile cannot slip another broker's rows under this owner.
    let read = tokio::task::spawn_blocking(move || -> Result<_> {
        let conn = db.conn()?;
        let tx = conn.unchecked_transaction()?;
        let owner = symbol::owner(&tx)?;
        if owner.as_ref().map(|o| o.broker.as_str()) != Some(want.as_str()) {
            return Ok(Err(owner.map(|o| o.broker)));
        }
        let master = symbol::load_master(&tx)?;
        tx.finish()?;
        Ok(Ok(master))
    })
    .await
    .map_err(|e| AppError::Internal(format!("symbol load task failed: {}", e)))??;
    let master = match read {
        Ok(m) => m,
        Err(owner) => {
            tracing::warn!(
                "Stored master belongs to {}, not {}; not loaded",
                owner.as_deref().unwrap_or("no recorded broker"),
                broker
            );
            return Err(AppError::Validation(not_owner_message(
                ctx,
                owner.as_deref(),
                broker,
            )));
        }
    };
    let n = ctx.load_master_cache(master);
    if n > 0 {
        clear_unavailable(ctx);
    }
    emit_cache_loaded(ctx, broker, started);
    tracing::info!(
        "Loaded {} instruments for {} from the stored master",
        n,
        broker
    );
    Ok(n)
}

/// The stored rows of `itype` a broker carries into its download
/// (`Broker::carries_stored`), when the stored table holds this broker's
/// master; empty otherwise, or when it cannot be read (the download then
/// goes ahead without them).
pub async fn stored_rows(ctx: &AppState, broker: &str, itype: &'static str) -> Vec<SymbolData> {
    let db = ctx.sqlite.clone();
    let broker = broker.to_string();
    let read = tokio::task::spawn_blocking(move || -> Result<Vec<SymbolData>> {
        let conn = db.conn()?;
        let tx = conn.unchecked_transaction()?;
        if symbol::owner(&tx)?.map(|o| o.broker).as_deref() != Some(broker.as_str()) {
            return Ok(Vec::new());
        }
        let rows = symbol::load_by_instrument_type(&tx, itype)?;
        tx.finish()?;
        Ok(rows)
    })
    .await;
    match read {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => {
            tracing::warn!("Stored {} rows could not be read: {}", itype, e);
            Vec::new()
        }
        Err(e) => {
            tracing::warn!("Stored {} rows could not be read: {}", itype, e);
            Vec::new()
        }
    }
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
    let downloaded = match broker.carries_stored() {
        Some(itype) => {
            let stored = stored_rows(ctx, id, itype).await;
            broker.download_master_carrying(auth, stored).await
        }
        None => broker.download_master(auth).await,
    };
    let master = match downloaded {
        Ok(m) if !m.rows.is_empty() => m,
        Ok(_) => {
            return Err(AppError::Broker(
                "The broker sent an empty instrument list. Try the download again shortly.".into(),
            ))
        }
        Err(e) => return Err(e),
    };
    let db = ctx.sqlite.clone();
    let stored_at = ctx.now();
    // The rows and their owner commit together on the blocking thread, which
    // finishes even when this task is aborted (logout): the owner then still
    // says whose rows they are.
    let (master, stats) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut conn = db.conn()?;
        symbol::store_master(&mut conn, id, stored_at, &master)?;
        let stats = symbol::exchange_counts(&conn)?;
        Ok((master, stats))
    })
    .await
    .map_err(|e| AppError::Internal(format!("symbol store task failed: {}", e)))??;
    // Install it only for the broker whose session this still is.
    let session = ctx
        .broker_session
        .read()
        .as_ref()
        .map(|s| s.broker_id.clone());
    if let Some(other) = session.filter(|b| b != id) {
        tracing::warn!(
            "Master contract for {} downloaded after the session moved to {}; not loaded",
            id,
            other
        );
        return Err(AppError::Broker(
            "The broker changed while its master contract was downloading, so it was not loaded."
                .into(),
        ));
    }
    let total = ctx.load_master_cache(master);
    clear_unavailable(ctx);
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

/// `/api/cache/health` (web `get_cache_health`, same keys and scores: 100
/// healthy, 50 degraded, 0 unhealthy). The cache is valid only when the
/// stored master belongs to the connected broker; `owner` and `stored_at`
/// say whose it is and when it was stored. The resolver keeps no hit or
/// miss counters, so the hit rate reads "N/A" rather than a figure.
pub fn cache_health(ctx: &AppState) -> Value {
    let total = ctx.symbol_count();
    let loaded = total > 0;
    let owner = ctx
        .sqlite
        .conn()
        .ok()
        .and_then(|c| symbol::owner(&c).ok())
        .flatten();
    let active = ctx.get_broker_session().map(|s| s.broker_id);
    let owned = match (&owner, &active) {
        (Some(o), Some(a)) => o.broker == *a,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let valid = loaded && owned;
    let score = match (loaded, valid) {
        (false, _) => 0,
        (true, false) => 50,
        (true, true) => 100,
    };
    let recommendation = if !loaded {
        "Cache is not loaded. Run master contract download.".to_string()
    } else if !valid {
        match active.as_deref() {
            Some(a) => not_owner_message(ctx, owner.as_ref().map(|o| o.broker.as_str()), a),
            None => "The stored master contract does not say which broker it belongs to. Sign in to your broker to reload it.".to_string(),
        }
    } else {
        "Cache is operating optimally.".to_string()
    };
    json!({
        "health_score": score,
        "status": match score { 100 => "healthy", 50 => "degraded", _ => "unhealthy" },
        "cache_loaded": loaded,
        "cache_valid": valid,
        "hit_rate": "N/A",
        "total_symbols": total,
        "memory_usage_mb": format!("{:.2}", (total as f64 * 400.0) / (1024.0 * 1024.0)),
        "db_queries": 0,
        "owner": owner.as_ref().map(|o| o.broker.clone()),
        "stored_at": owner.as_ref().map(|o| mcs::iso_ist(o.stored_at)),
        "recommendations": vec![recommendation],
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
        // MC-01: a stored master with no recorded owner (stored before the
        // owner table) is downloaded once, however recent the history.
        let (d, r) = should_download_at("zerodha", Some(ist(3, 9, 0)), None, now);
        assert!(d);
        assert_eq!(
            r,
            "The stored master contract has no recorded broker, symtoken needs refresh"
        );
        // Crypto follows the UTC day.
        let (d, _) = should_download_at(
            "deltaexchange",
            Some(ist(3, 6, 0)),
            Some("deltaexchange"),
            now,
        );
        assert!(!d);
        assert_eq!(cutoff("deltaexchange").0, 0);
    }

    fn two_brokers(
        now: DateTime<Utc>,
    ) -> (
        crate::state::testing::TestCtx,
        Arc<crate::brokers::mock::MockBroker>,
        Arc<crate::brokers::mock::MockBroker>,
    ) {
        use crate::brokers::common::symbols::tests::row;
        use crate::brokers::mock::MockBroker;
        use crate::brokers::BrokerRegistry;
        let zerodha = Arc::new(MockBroker::new("zerodha"));
        let angel = Arc::new(MockBroker::new("angel"));
        *zerodha.master.lock() = Some(Ok(vec![row("SBIN", "SBIN", "NSE", "779521")]));
        *angel.master.lock() = Some(Ok(vec![
            row("SBIN", "SBIN-EQ", "NSE", "3045"),
            row("INFY", "INFY-EQ", "NSE", "1594"),
        ]));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![
                zerodha.clone() as Arc<dyn Broker>,
                angel.clone() as Arc<dyn Broker>,
            ]),
            now,
        );
        (t, zerodha, angel)
    }

    fn session_for(ctx: &AppState, broker: &str) {
        ctx.set_broker_session(Some(crate::state::BrokerSession {
            broker_id: broker.into(),
            auth_token: crate::security::Secret::new("t"),
            feed_token: None,
            user_id: "AB1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        }));
    }

    /// MC-01: the stored master is loaded only for the broker that owns it;
    /// another broker's reload is refused by name and leaves memory empty.
    #[tokio::test]
    async fn a_stored_master_loads_only_for_its_owner() {
        let (t, zerodha, _angel) = two_brokers(ist(3, 10, 0));
        let ctx = &t.ctx;
        let zd: Arc<dyn Broker> = zerodha.clone();
        download(ctx, &zd, &AuthToken::new("t")).await.unwrap();
        ctx.clear_symbol_cache();
        let e = load_cached(ctx, "angel").await.unwrap_err();
        assert!(matches!(e, AppError::Validation(_)), "{:?}", e);
        // The mocks are both named "Mock".
        assert_eq!(
            e.client_message(),
            "The stored master contract is for Mock. Download the master contract for Mock."
        );
        assert_eq!(ctx.symbol_count(), 0);
        assert_eq!(load_cached(ctx, "zerodha").await.unwrap(), 1);
    }

    /// MC-01, the abort window: a download aborted at logout still commits
    /// its rows (the blocking store runs on), with the download history
    /// still naming the previous broker. Signing back in to that broker the
    /// same day must download, not load the other broker's rows.
    #[tokio::test]
    async fn rows_committed_without_their_history_are_not_loaded_as_anothers() {
        let (t, zerodha, angel) = two_brokers(ist(3, 10, 0));
        let ctx = &t.ctx;
        let zd: Arc<dyn Broker> = zerodha.clone();
        download(ctx, &zd, &AuthToken::new("t")).await.unwrap();
        // Angel's rows land; its history update never runs.
        let rows = angel.master.lock().clone().unwrap().unwrap();
        {
            let mut c = ctx.sqlite.conn().unwrap();
            symbol::store_master(
                &mut c,
                "angel",
                ctx.now(),
                &crate::brokers::types::MasterContract::new(rows),
            )
            .unwrap();
        }
        ctx.clear_symbol_cache();
        let (needed, reason) = should_download(ctx, "zerodha").unwrap();
        assert!(needed, "{}", reason);
        zerodha.calls.lock().clear();
        session_for(ctx, "zerodha");
        assert_eq!(ensure(ctx, &zd, &AuthToken::new("t")).await.unwrap(), 1);
        assert!(zerodha
            .calls
            .lock()
            .contains(&crate::brokers::mock::MockCall::MasterContract));
        assert_eq!(ctx.symbols.token("SBIN", "NSE").as_deref(), Some("779521"));
    }

    /// MC-01: a download that completes after the session moved to another
    /// broker is stored with its owner but never installed in memory.
    #[tokio::test]
    async fn a_download_is_installed_only_for_the_current_session() {
        let (t, zerodha, _angel) = two_brokers(ist(3, 10, 0));
        let ctx = &t.ctx;
        session_for(ctx, "angel");
        let zd: Arc<dyn Broker> = zerodha.clone();
        assert!(download(ctx, &zd, &AuthToken::new("t")).await.is_err());
        assert_eq!(ctx.symbol_count(), 0);
        let c = ctx.sqlite.conn().unwrap();
        assert_eq!(symbol::owner(&c).unwrap().unwrap().broker, "zerodha");
    }

    /// MC-03: an unforced download skips only when this broker's stored
    /// master is there and loaded; cache health is degraded when the stored
    /// master belongs to another broker.
    #[tokio::test]
    async fn skip_and_health_follow_the_owner() {
        let (t, zerodha, _angel) = two_brokers(ist(3, 10, 0));
        let ctx = &t.ctx;
        session_for(ctx, "zerodha");
        let zd: Arc<dyn Broker> = zerodha.clone();
        download(ctx, &zd, &AuthToken::new("t")).await.unwrap();
        assert!(skip_reason(ctx, "zerodha").is_some());
        assert_eq!(cache_health(ctx)["status"], "healthy");
        assert_eq!(cache_health(ctx)["owner"], "zerodha");
        // Today's success history, but the table was emptied.
        {
            let mut c = ctx.sqlite.conn().unwrap();
            symbol::store_symbols(&mut c, "zerodha", ctx.now(), &[]).unwrap();
        }
        ctx.clear_symbol_cache();
        assert!(!should_download(ctx, "zerodha").unwrap().0);
        assert_eq!(skip_reason(ctx, "zerodha"), None);
        // Loaded, but the stored master is another broker's.
        download(ctx, &zd, &AuthToken::new("t")).await.unwrap();
        {
            let mut c = ctx.sqlite.conn().unwrap();
            let rows = vec![crate::brokers::common::symbols::tests::row(
                "SBIN", "SBIN-EQ", "NSE", "3045",
            )];
            symbol::store_symbols(&mut c, "angel", ctx.now(), &rows).unwrap();
        }
        assert_eq!(skip_reason(ctx, "zerodha"), None);
        let h = cache_health(ctx);
        assert_eq!(h["status"], "degraded", "{}", h);
        assert_eq!(h["health_score"], 50);
        assert_eq!(h["cache_valid"], false);
        assert_eq!(h["owner"], "angel");
        assert_eq!(
            h["recommendations"][0],
            "The stored master contract is for Mock. Download the master contract for Mock."
        );
    }

    /// 14-N1: switching brokers drops the previous broker's master from
    /// memory; when the new broker's master cannot be loaded the trader is
    /// told on the Health Monitor page, and a later download clears it.
    #[tokio::test]
    async fn a_session_without_its_master_holds_no_other_brokers_symbols() {
        let (t, _zerodha, angel) = two_brokers(ist(3, 10, 0));
        let ctx = &t.ctx;
        let alerts = || {
            let c = ctx.logs.conn().unwrap();
            crate::db::sqlite::monitor::active_alerts(&c)
                .unwrap()
                .into_iter()
                .filter(|a| a.metric_name == MASTER_ALERT_METRIC)
                .count()
        };
        let session = |b: &str| crate::state::BrokerSession {
            broker_id: b.into(),
            auth_token: crate::security::Secret::new("t"),
            feed_token: None,
            user_id: "AB1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        };
        async fn until(cond: impl Fn() -> bool) {
            for _ in 0..500 {
                if cond() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("the session did not settle");
        }
        ctx.set_broker_session(Some(session("zerodha")));
        ctx.runtime.activate(ctx, &session("zerodha")).await;
        until(|| ctx.symbol_count() > 0).await;
        assert_eq!(ctx.symbol_count(), 1);
        *angel.master.lock() = Some(Err("down".into()));
        ctx.set_broker_session(Some(session("angel")));
        ctx.runtime.activate(ctx, &session("angel")).await;
        until(|| alerts() == 1).await;
        assert_eq!(ctx.symbol_count(), 0, "zerodha's symbols stayed in memory");
        *angel.master.lock() = Some(Ok(vec![crate::brokers::common::symbols::tests::row(
            "SBIN", "SBIN-EQ", "NSE", "3045",
        )]));
        let ag: Arc<dyn Broker> = angel.clone();
        download(ctx, &ag, &AuthToken::new("t")).await.unwrap();
        assert_eq!(alerts(), 0);
        ctx.runtime.teardown(ctx).await;
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

    /// Web #2198 `get_existing_index_rows`: a broker that carries stored rows
    /// (Firstock's index rows) gets them from the stored table, so the first
    /// download after a sign-in, with nothing in memory, keeps them too; the
    /// stored master of another broker is never carried.
    #[tokio::test]
    async fn carried_rows_come_from_the_stored_master() {
        use crate::brokers::common::symbols::tests::row;
        use crate::brokers::mock::MockBroker;
        use crate::brokers::BrokerRegistry;
        let firstock = Arc::new(MockBroker::new("firstock"));
        let other = Arc::new(MockBroker::new("zerodha"));
        *firstock.carry.lock() = Some("INDEX");
        let mut nifty = row("NIFTY", "NIFTY 50", "NSE_INDEX", "26000");
        nifty.instrument_type = "INDEX".into();
        let sbin = row("SBIN", "SBIN-EQ", "NSE", "3045");
        *firstock.master.lock() = Some(Ok(vec![sbin.clone(), nifty.clone()]));
        *other.master.lock() = Some(Ok(vec![sbin.clone(), nifty.clone()]));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![
                firstock.clone() as Arc<dyn Broker>,
                other.clone() as Arc<dyn Broker>,
            ]),
            ist(3, 10, 0),
        );
        let ctx = &t.ctx;
        let auth = AuthToken::new("t");
        let fs: Arc<dyn Broker> = firstock.clone();
        let zd: Arc<dyn Broker> = other.clone();
        // Nothing stored yet.
        download(ctx, &fs, &auth).await.unwrap();
        assert_eq!(firstock.carried.lock().clone(), Some(vec![]));
        // A sign-in starts with an empty master in memory; the stored one
        // still holds the index row.
        ctx.clear_symbol_cache();
        download(ctx, &fs, &auth).await.unwrap();
        assert_eq!(firstock.carried.lock().clone(), Some(vec![nifty.clone()]));
        // After another broker's (later) download the stored master is not
        // Firstock's.
        t.clock.advance(chrono::Duration::minutes(1));
        download(ctx, &zd, &auth).await.unwrap();
        assert!(other.carried.lock().is_none(), "zerodha carries nothing");
        download(ctx, &fs, &auth).await.unwrap();
        assert_eq!(firstock.carried.lock().clone(), Some(vec![]));
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
