//! web: test/test_strategy_module_db.py, test_strategy_module_broadcast.py

use super::*;
use openalgo_desktop_lib::strategy::checkpoint::{write_once, CHECKPOINT_KEEP};
use openalgo_desktop_lib::strategy::store::{hash_webhook_token, WEBHOOK_TOKEN_PREFIX};

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
    assert!(t.m.store.update_strategy(sid, USER, &ch).is_err());
}

#[test]
fn the_kind_cannot_change_by_update() {
    let t = t();
    let sid = t.default_strategy();
    let mut ch = serde_json::Map::new();
    ch.insert("strategy_kind".into(), json!("signal"));
    let e = t.m.store.update_strategy(sid, USER, &ch).unwrap_err();
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
    assert!(t.m.store.claim_strategy_for_run(sid).unwrap());
    assert!(!t.m.store.claim_strategy_for_run(sid).unwrap());
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
