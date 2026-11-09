//! The SQLite the stores run is new enough for the advisories it has had.
//!
//! RUSTSEC-2026-0459 (CVE-2026-119375): SQLite before 3.54.0 can hand back
//! uninitialized page-cache memory when a database is opened beside a
//! `-wal` of another page size. The stores link the SQLite that rusqlite
//! bundles (its `bundled` feature), so a dependency change that brings an
//! older one back fails here, before a release.

use openalgo_desktop_lib::db::sqlite::{open_pool, Durability};

/// The first SQLite with the fix for RUSTSEC-2026-0459, as SQLite numbers
/// versions (`3_054_000` is 3.54.0).
const WAL_PAGE_SIZE_FIX: i32 = 3_054_000;

#[test]
fn the_bundled_sqlite_has_the_wal_page_size_fix() {
    assert!(
        rusqlite::version_number() >= WAL_PAGE_SIZE_FIX,
        "SQLite {} is older than 3.54.0 (RUSTSEC-2026-0459)",
        rusqlite::version()
    );
    let dir = tempfile::tempdir().unwrap();
    let pool = open_pool(&dir.path().join("v.db"), 1, Durability::MAIN).unwrap();
    let running: String = pool
        .get()
        .unwrap()
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        running,
        rusqlite::version(),
        "a store's connection runs the bundled SQLite"
    );
}
