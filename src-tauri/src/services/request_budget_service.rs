//! A broker's daily request counts survive a restart (Fyers caps each
//! request category per day): saved while the app runs, at most
//! [`SAVE_EVERY`] apart, and at quit, and given back to the adapters at
//! start, so a restart in the middle of the day keeps the day's count.
//! Only today's counts (IST) are given back; a row of an earlier day is
//! dropped. Desktop only: the web counts no daily requests.

use crate::brokers::common::ratelimit::{today_ist, DailyCount};
use crate::db::sqlite::request_count;
use crate::error::Result;
use crate::state::AppState;
use std::sync::Arc;
use std::time::Duration;

/// How often changed counts are saved while the app runs. A crash loses at
/// most this much of the day's count; Fyers' own throttle answers stay
/// authoritative.
pub const SAVE_EVERY: Duration = Duration::from_secs(15);

/// Each adapter's counts for today, for the brokers that keep any.
type Counts = Vec<(&'static str, Vec<DailyCount>)>;

/// Give every adapter the counts it saved earlier today (at start-up).
pub fn restore(state: &AppState) {
    let today = today_ist();
    let conn = match state.sqlite.conn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Saved broker request counts could not be read: {}", e);
            return;
        }
    };
    if let Err(e) = request_count::clear_before(&conn, today) {
        tracing::warn!("Old broker request counts could not be cleared: {}", e);
    }
    for broker in state.brokers.list() {
        match request_count::on_day(&conn, broker.id(), today) {
            Ok(saved) if !saved.is_empty() => broker.restore_daily_request_counts(&saved),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                "Saved request counts for {} could not be read: {}",
                broker.id(),
                e
            ),
        }
    }
}

/// Today's counts of every adapter that keeps daily counts.
fn current(state: &AppState) -> Counts {
    state
        .brokers
        .list()
        .into_iter()
        .map(|b| (b.id(), b.daily_request_counts()))
        .filter(|(_, counts)| !counts.is_empty())
        .collect()
}

fn write(state: &AppState, counts: &Counts) -> Result<()> {
    let conn = state.sqlite.conn()?;
    for (broker, today) in counts {
        request_count::save(&conn, broker, today)?;
    }
    if let Some(day) = counts.iter().flat_map(|(_, c)| c).map(|c| c.day).max() {
        request_count::clear_before(&conn, day)?;
    }
    Ok(())
}

/// One save at a time, each reading the counts while it holds the turn: a
/// periodic save whose write waited behind a busy database or a slow disk
/// cannot land after a later save with the older counts it was started
/// with. Counts only grow within a day, so the last write is the newest.
static SAVING: parking_lot::Mutex<()> = parking_lot::const_mutex(());

/// Save every adapter's counts as they are now; the counts written.
fn save_current(state: &AppState) -> Result<Counts> {
    let _turn = SAVING.lock();
    let now = current(state);
    write(state, &now)?;
    Ok(now)
}

/// Save every adapter's counts now (at quit).
pub fn save(state: &AppState) -> Result<()> {
    save_current(state).map(|_| ())
}

/// Save the counts whenever they changed, at most [`SAVE_EVERY`] apart, in
/// a task the context owns (aborted at shutdown, which saves once more).
pub fn start(ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    ctx.spawn(async move {
        let mut tick = tokio::time::interval(SAVE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut saved = Counts::new();
        loop {
            tick.tick().await;
            let Some(ctx) = weak.upgrade() else { return };
            if current(&ctx) == saved {
                continue;
            }
            match ctx.blocking(save_current).await {
                Ok(written) => saved = written,
                Err(e) => tracing::warn!("Broker request counts could not be saved: {}", e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::brokers::common::symbols::SymbolResolver;
    use crate::brokers::fyers::{FyersBroker, FyersUrls};
    use crate::brokers::types::{AuthToken, QuoteKey};
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::security::keystore::{KeyStore, MemoryKeyStore};
    use crate::state::{AppState, OpenOptions};
    use std::sync::Arc;

    fn fyers(base: &str) -> Arc<FyersBroker> {
        let symbols = SymbolResolver::new();
        symbols.load(vec![crate::brokers::common::symbols::tests::row(
            "SBIN",
            "NSE:SBIN-EQ",
            "NSE",
            "3045",
        )]);
        Arc::new(FyersBroker::with_urls(
            symbols,
            FyersUrls {
                api: base.to_string(),
                ..FyersUrls::default()
            },
        ))
    }

    /// A local server that refuses every request: each one still counts
    /// against Fyers' day.
    async fn refusing_server() -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new().fallback(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                r#"{"s":"error","code":-16,"message":"Could not authenticate the user"}"#,
            )
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        (base, task)
    }

    fn open(
        dir: &std::path::Path,
        store: &Arc<MemoryKeyStore>,
        b: Arc<FyersBroker>,
    ) -> Arc<AppState> {
        AppState::open(
            dir,
            OpenOptions {
                keystore: store.clone() as Arc<dyn KeyStore>,
                clock: Arc::new(crate::clock::SystemClock),
                brokers: Arc::new(BrokerRegistry::with(vec![b as Arc<dyn Broker>])),
            },
        )
        .unwrap()
    }

    /// Quit as a restart does: DuckDB holds its file exclusively, so the
    /// state is gone before the folder is opened again.
    async fn close(ctx: Arc<AppState>) {
        ctx.shutdown().await;
        for _ in 0..1000 {
            if Arc::strong_count(&ctx) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(Arc::strong_count(&ctx), 1, "app state outlives shutdown");
    }

    fn data_sent(b: &FyersBroker) -> u32 {
        b.daily_request_counts()
            .iter()
            .find(|c| c.category == "data")
            .map_or(0, |c| c.sent)
    }

    /// Fyers' count of today's market data requests survives a restart:
    /// the quit saves it, the next start gives it back, and counting goes
    /// on from there.
    #[tokio::test]
    async fn a_restart_keeps_the_days_request_count() {
        let (base, server) = refusing_server().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryKeyStore::new());
        let auth = AuthToken::new("APPID-100:tok");
        let key = QuoteKey::new("NSE", "SBIN");

        let first = fyers(&base);
        let ctx = open(dir.path(), &store, first.clone());
        for _ in 0..3 {
            let _ = first.get_quote(&auth, &key).await;
        }
        assert_eq!(data_sent(&first), 3);
        close(ctx).await;

        let second = fyers(&base);
        let ctx = open(dir.path(), &store, second.clone());
        assert_eq!(data_sent(&second), 3, "the restart forgot the day's count");
        let _ = second.get_quote(&auth, &key).await;
        assert_eq!(data_sent(&second), 4);
        close(ctx).await;
        server.abort();
    }

    /// A save that waits behind a busy database never writes older counts
    /// after a later save (Windows CI: the quit saved 3, then a periodic
    /// write started earlier saved 1, and the restart gave back 1). Each
    /// round holds the database's write lock while a save starts, counts
    /// one more request, starts a second save, then lets both through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delayed_save_never_overwrites_newer_counts() {
        let (base, server) = refusing_server().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryKeyStore::new());
        let auth = AuthToken::new("APPID-100:tok");
        let key = QuoteKey::new("NSE", "SBIN");
        let b = fyers(&base);
        let ctx = open(dir.path(), &store, b.clone());
        let _ = b.get_quote(&auth, &key).await;
        let pause = std::time::Duration::from_millis(50);
        for round in 0..20 {
            let holder = ctx.sqlite.conn().unwrap();
            holder.execute_batch("BEGIN IMMEDIATE").unwrap();
            let c = ctx.clone();
            let early = std::thread::spawn(move || super::save(&c));
            std::thread::sleep(pause);
            let _ = b.get_quote(&auth, &key).await;
            let c = ctx.clone();
            let late = std::thread::spawn(move || super::save(&c));
            std::thread::sleep(pause);
            holder.execute_batch("COMMIT").unwrap();
            early.join().unwrap().unwrap();
            late.join().unwrap().unwrap();
            let conn = ctx.sqlite.conn().unwrap();
            let stored = crate::db::sqlite::request_count::on_day(
                &conn,
                b.id(),
                crate::brokers::common::ratelimit::today_ist(),
            )
            .unwrap()
            .iter()
            .find(|c| c.category == "data")
            .map_or(0, |c| c.sent);
            assert_eq!(
                stored,
                data_sent(&b),
                "round {}: a save wrote older counts after a newer one",
                round
            );
        }
        close(ctx).await;
        server.abort();
    }
}
