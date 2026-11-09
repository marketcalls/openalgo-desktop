use super::SqliteConnectionManager;
use crate::db::sqlite::{build_pool, open_pool, Durability};
use r2d2::ManageConnection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A manager whose set-up counts itself and leaves a mark only it sets on
/// the connection (`cache_size` is per connection, never kept in the file).
fn counting(path: &Path) -> (SqliteConnectionManager, Arc<AtomicUsize>) {
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let manager = SqliteConnectionManager::file(path).with_init(move |c| {
        count.fetch_add(1, Ordering::SeqCst);
        c.execute_batch("PRAGMA cache_size = -4321;")
    });
    (manager, opened)
}

fn cache_size(c: &rusqlite::Connection) -> i64 {
    c.query_row("PRAGMA cache_size", [], |r| r.get(0)).unwrap()
}

fn pool_of(manager: SqliteConnectionManager, max: u32) -> r2d2::Pool<SqliteConnectionManager> {
    build_pool(
        manager,
        "t.db".into(),
        max,
        Duration::from_secs(10),
        Duration::from_secs(60),
    )
    .unwrap()
}

/// Every connection the pool hands out ran the store's set-up first (its
/// busy handler and pragmas are per connection, as r2d2_sqlite's
/// `with_init` ran them).
#[test]
fn every_pooled_connection_ran_the_init() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, opened) = counting(&dir.path().join("t.db"));
    let pool = pool_of(manager, 4);
    let held: Vec<_> = (0..4).map(|_| pool.get().unwrap()).collect();
    for c in &held {
        assert_eq!(cache_size(c), -4321);
    }
    assert_eq!(opened.load(Ordering::SeqCst), 4);
    assert_eq!(pool.state().connections, 4);
}

/// The real stores' set-up reaches every pooled connection: `logs.db`
/// runs at `synchronous=NORMAL`, which SQLite does not default to.
#[test]
fn every_pooled_connection_has_the_store_pragmas() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_pool(&dir.path().join("logs.db"), 3, Durability::LOGS).unwrap();
    let held: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
    for c in &held {
        let sync: i64 = c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        let fk: i64 = c
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!((sync, mode.as_str(), fk), (1, "wal", 1));
    }
}

/// A connection passes the check r2d2 runs before each checkout and is
/// never judged broken, so the pool hands the same connection back
/// instead of opening a new one each time.
#[test]
fn a_connection_survives_the_check_before_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let manager = SqliteConnectionManager::file(&path);
    let mut c = manager.connect().unwrap();
    assert!(manager.is_valid(&mut c).is_ok());
    assert!(!manager.has_broken(&mut c));

    let (manager, opened) = counting(&path);
    let pool = pool_of(manager, 1);
    let first = pool.get().unwrap();
    first.execute_batch("PRAGMA cache_size = -777;").unwrap();
    drop(first);
    for _ in 0..3 {
        assert_eq!(
            cache_size(&pool.get().unwrap()),
            -777,
            "the same connection"
        );
    }
    assert_eq!(opened.load(Ordering::SeqCst), 1);
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The store's file and its `-wal` and `-shm` are created as before, and
/// a dropped pool closes every connection it opened: the last connection
/// to a WAL database to close checkpoints it and removes both files, so
/// either one left behind is a connection still open.
#[test]
fn a_store_and_its_wal_are_created_and_closed_with_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let (wal, shm) = (sibling(&path, "-wal"), sibling(&path, "-shm"));
    let pool = open_pool(&path, 4, Durability::MAIN).unwrap();
    {
        let a = pool.get().unwrap();
        let b = pool.get().unwrap();
        a.execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        let n: i64 = b
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    assert!(path.is_file() && wal.is_file() && shm.is_file());
    drop(pool);
    // A worker thread may still be opening the pool's idle connection; it
    // closes once that open returns.
    let deadline = Instant::now() + Duration::from_secs(10);
    while (wal.exists() || shm.exists()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!wal.exists() && !shm.exists(), "a connection is still open");
    let c = rusqlite::Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "the write was checkpointed into the file");
}
