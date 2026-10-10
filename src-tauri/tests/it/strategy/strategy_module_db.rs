//! web: test/test_strategy_module_db.py, test_strategy_module_broadcast.py

use super::*;
use openalgo_desktop_lib::strategy::checkpoint::{write_once, CHECKPOINT_KEEP};
use openalgo_desktop_lib::strategy::store::{
    hash_webhook_token, ClaimOutcome, CHANGED_WHILE_EDITING, WEBHOOK_TOKEN_PREFIX,
};

#[test]
fn a_token_is_shown_once_and_stored_only_as_a_digest() {
    let t = t();
    let (sid, token) = t.make_with_token(config("Tok", json!([short_call_leg()]), json!({})));
    assert!(token.starts_with(WEBHOOK_TOKEN_PREFIX));
    let row = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    assert_eq!(row.webhook_token_hash, hash_webhook_token(&token));
    assert!(!row.to_dict(true).to_string().contains(&token));
    assert!(row.to_dict(true).get("webhook_token_hash").is_none());
    let rotated = t.m.store.rotate_webhook_token(sid, USER).unwrap();
    assert!(t
        .m
        .store
        .get_strategy_by_webhook_token(&token)
        .unwrap()
        .is_none());
    assert_eq!(
        t.m.store
            .get_strategy_by_webhook_token(&rotated)
            .unwrap()
            .unwrap()
            .id,
        sid
    );
}

#[test]
fn a_duplicate_name_is_refused_for_the_same_user() {
    let t = t();
    t.make(config("Dup", json!([short_call_leg()]), json!({})));
    assert!(t
        .m
        .store
        .create_strategy(USER, &config("Dup", json!([]), json!({})))
        .is_err());
    assert!(t
        .m
        .store
        .create_strategy("someone", &config("Dup", json!([]), json!({})))
        .is_ok());
}

#[test]
fn a_strategy_that_is_not_yours_reads_as_absent() {
    let t = t();
    let sid = t.default_strategy();
    assert!(t.m.store.get_strategy(sid, "intruder").unwrap().is_none());
}

#[tokio::test]
async fn deleting_a_strategy_removes_every_child_row() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    write_once(&t.m, Some(false));
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 100.0).await;
    t.m.store.delete_strategy(sid, USER).unwrap();
    assert!(t.m.store.list_runs(sid, 10).unwrap().is_empty());
    assert!(t.m.store.list_orders(run).unwrap().is_empty());
    assert_eq!(t.m.store.count_checkpoints(run).unwrap(), 0);
    assert!(t.events(sid).is_empty());
}

#[test]
fn a_running_strategy_cannot_be_edited_or_deleted() {
    let t = t();
    let sid = t.default_strategy();
    t.m.store.set_strategy_status(sid, "running", None).unwrap();
    assert!(t.m.store.delete_strategy(sid, USER).is_err());
    let mut ch = serde_json::Map::new();
    ch.insert("name".into(), json!("x"));
    assert!(t.m.store.update_strategy(sid, USER, 0, &ch).is_err());
}

#[test]
fn the_kind_cannot_change_by_update() {
    let t = t();
    let sid = t.default_strategy();
    let mut ch = serde_json::Map::new();
    ch.insert("strategy_kind".into(), json!("signal"));
    let e = t.m.store.update_strategy(sid, USER, 0, &ch).unwrap_err();
    assert!(e
        .to_string()
        .contains("cannot change between batch and signal"));
}

#[tokio::test]
async fn checkpoints_are_pruned_to_the_newest() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    for _ in 0..(CHECKPOINT_KEEP + 30) {
        write_once(&t.m, Some(false));
    }
    assert_eq!(
        t.m.store.count_checkpoints(run).unwrap(),
        CHECKPOINT_KEEP + 30
    );
    write_once(&t.m, Some(true));
    assert_eq!(t.m.store.count_checkpoints(run).unwrap(), CHECKPOINT_KEEP);
}

#[tokio::test]
async fn the_list_carries_the_last_finalised_run() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 90.0).await;
    let list = t.m.store.list_strategies(USER, None, None).unwrap();
    assert_eq!(list[0]["last_finalized_run"]["id"], run);
    assert_eq!(list[0]["last_finalized_run"]["pnl_realized"], 650.0);
    assert!(list[0].get("legs").is_none());
    let filtered =
        t.m.store
            .list_strategies(USER, None, Some("engine"))
            .unwrap();
    assert_eq!(filtered.len(), 1);
}

#[test]
fn the_claim_is_one_conditional_update() {
    let t = t();
    let sid = t.default_strategy();
    assert_eq!(t.claim(sid), ClaimOutcome::Claimed);
    assert_eq!(t.claim(sid), ClaimOutcome::Running);
}

// ------------------------------------------------- atomic edits (SM-02)

fn changes(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// An intraday strategy trading 09:30 to 10:00.
fn intraday(t: &T, name: &str) -> i64 {
    t.make(config(
        name,
        json!([short_call_leg()]),
        json!({"strategy_type": "intraday", "entry_time": "09:30", "exit_time": "10:00"}),
    ))
}

#[test]
fn a_rename_to_a_taken_name_changes_nothing() {
    let t = t();
    intraday(&t, "Alpha");
    let sid = intraday(&t, "Beta");
    let before = t.row(sid);
    // Fields are written in name order: the times come before the name.
    let e =
        t.m.store
            .update_strategy(
                sid,
                USER,
                before.revision,
                &changes(json!({"name": "Alpha", "entry_time": "11:00", "exit_time": "12:00"})),
            )
            .unwrap_err();
    assert!(e.to_string().contains("already exists"), "{}", e);
    assert_eq!(t.row(sid), before, "nothing of the refused edit was saved");
}

#[test]
fn a_write_that_fails_part_way_saves_no_field() {
    let t = t();
    let sid = t.default_strategy();
    let before = t.row(sid);
    t.db.conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_scheduler BEFORE UPDATE OF scheduler ON sm_strategy \
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();
    let mut legs = short_call_leg();
    legs["lots"] = json!(3);
    // `legs` is written before `scheduler`, which then fails.
    let r = t.m.store.update_strategy(
        sid,
        USER,
        before.revision,
        &changes(json!({"legs": [legs], "scheduler": {"enabled": false}})),
    );
    assert!(r.is_err());
    assert_eq!(t.row(sid), before, "the legs were not saved on their own");
}

#[test]
fn an_edit_against_a_stale_revision_is_refused_and_a_saved_one_advances_it() {
    let t = t();
    let sid = t.default_strategy();
    let read = t.row(sid);
    // Someone else saves first.
    let saved =
        t.m.store
            .update_strategy(
                sid,
                USER,
                read.revision,
                &changes(json!({"overall_sl_mtm": 2500})),
            )
            .unwrap();
    assert_eq!(saved.revision, read.revision + 1);
    assert_eq!(saved.overall_sl_mtm, Some(2500.0));
    // The edit validated against the older read is refused whole.
    let e =
        t.m.store
            .update_strategy(
                sid,
                USER,
                read.revision,
                &changes(json!({"overall_sl_mtm": 900, "overall_target_mtm": 4000})),
            )
            .unwrap_err();
    assert!(e.to_string().contains(CHANGED_WHILE_EDITING), "{}", e);
    assert_eq!(t.row(sid), saved);
}

#[test]
fn every_management_write_advances_the_revision() {
    let t = t();
    let sid = t.default_strategy();
    let r0 = t.row(sid).revision;
    t.m.store.set_live_enabled(sid, USER, true).unwrap();
    assert_eq!(t.row(sid).revision, r0 + 1);
    t.m.store.set_webhook_locked(sid, USER, true).unwrap();
    assert_eq!(t.row(sid).revision, r0 + 2);
    t.m.store.rotate_webhook_token(sid, USER).unwrap();
    assert_eq!(t.row(sid).revision, r0 + 3);
    // A claim and its release are runtime state, not a change of config.
    assert_eq!(t.claim(sid), ClaimOutcome::Claimed);
    t.m.store.release_strategy(sid).unwrap();
    assert_eq!(t.row(sid).revision, r0 + 3);
    // Not yours: refused, nothing moves.
    assert!(t.m.store.set_live_enabled(sid, "intruder", false).is_err());
    assert!(t
        .m
        .store
        .set_webhook_locked(sid, "intruder", false)
        .is_err());
    assert!(t.m.store.rotate_webhook_token(sid, "intruder").is_err());
    assert_eq!(t.row(sid).revision, r0 + 3);
}

#[test]
fn the_revision_migration_keeps_existing_strategies_and_runs_once() {
    use openalgo_desktop_lib::clock::ManualClock;
    use openalgo_desktop_lib::db::sqlite::SqliteDb;
    use openalgo_desktop_lib::strategy::store::{migrate_revision, Store};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openalgo.db");
    let clock = ManualClock::new(ist(2026, 10, 7, 10, 0));
    let sid = {
        let db = std::sync::Arc::new(SqliteDb::new(&path).unwrap());
        let store = Store::new(db.clone(), clock.clone());
        let (row, _) = store
            .create_strategy(USER, &config("Kept", json!([short_call_leg()]), json!({})))
            .unwrap();
        // Back to the shape builds before migration 078 left it.
        db.conn()
            .unwrap()
            .execute_batch(
                "ALTER TABLE sm_strategy DROP COLUMN revision; \
                 DELETE FROM migrations WHERE name = '078_strategy_revision';",
            )
            .unwrap();
        row.id
    };
    let db = std::sync::Arc::new(SqliteDb::new(&path).unwrap());
    assert!(db.migration_applied("078_strategy_revision").unwrap());
    let row = Store::new(db.clone(), clock.clone())
        .get_strategy(sid, USER)
        .unwrap()
        .unwrap();
    assert_eq!((row.name.as_str(), row.revision), ("Kept", 0));
    assert_eq!(row.legs, json!([short_call_leg()]));
    // Idempotent: run again on the migrated table, and on a reopen.
    migrate_revision(&db.conn().unwrap()).unwrap();
    drop(db);
    let db = SqliteDb::new(&path).unwrap();
    assert!(db.migration_applied("078_strategy_revision").unwrap());
}

#[test]
fn timestamps_render_with_an_explicit_utc_offset() {
    let t = t();
    let sid = t.default_strategy();
    let d =
        t.m.store
            .get_strategy(sid, USER)
            .unwrap()
            .unwrap()
            .to_dict(true);
    assert!(d["created_at"].as_str().unwrap().ends_with("+00:00"));
}
