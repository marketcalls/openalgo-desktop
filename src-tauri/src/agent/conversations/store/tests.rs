use super::*;
use chrono::{Duration, TimeZone};

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    migrate(&c).unwrap();
    c
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 27, 9, 0, 0).unwrap()
}

#[test]
fn the_migration_runs_twice_and_keeps_rows() {
    let c = db();
    let conv = create_conversation(&c, "trader", Some("q"), "chat", t0()).unwrap();
    add_message(&c, conv.id, "user", "hi", None, None, t0()).unwrap();
    migrate(&c).unwrap();
    assert_eq!(list_messages(&c, conv.id, 500).unwrap().len(), 1);
}

#[test]
fn rows_read_back_as_the_web_writes_them() {
    let c = db();
    let conv = create_conversation(&c, "trader", None, "chart", t0()).unwrap();
    let v = conv.to_json();
    assert_eq!(v["title"], Value::Null);
    assert_eq!(v["agno_session_id"], Value::Null);
    assert_eq!(v["created_at"], "2026-10-27T09:00:00+00:00");
    let later = t0() + Duration::microseconds(1500);
    let m = add_message(
        &c,
        conv.id,
        "assistant",
        "ok",
        None,
        Some(&json!([{"type": "usage"}])),
        later,
    )
    .unwrap();
    let back = list_messages(&c, conv.id, 500).unwrap();
    assert_eq!(back[0].to_json(), m.to_json());
    assert_eq!(back[0].to_json()["tools"], json!([]));
    assert_eq!(
        back[0].to_json()["created_at"],
        "2026-10-27T09:00:00.001500+00:00"
    );
    // Adding a message moves the conversation up.
    let listed = list_conversations(&c, "trader", &[], 100).unwrap();
    assert_eq!(listed[0].updated_at, stamp(later));
}

#[test]
fn another_users_conversation_reads_as_absent() {
    let c = db();
    let conv = create_conversation(&c, "trader", None, "chat", t0()).unwrap();
    assert!(get_conversation(&c, conv.id, "someone").unwrap().is_none());
    assert!(!delete_conversation(&c, conv.id, "someone").unwrap());
    assert!(
        update_conversation(&c, conv.id, "someone", Some("x"), None, t0())
            .unwrap()
            .is_none()
    );
}

#[test]
fn listing_filters_surfaces_and_orders_newest_first() {
    let c = db();
    let a = create_conversation(&c, "trader", None, "chat", t0()).unwrap();
    let b = create_conversation(&c, "trader", None, "voice", t0() + Duration::seconds(1)).unwrap();
    create_conversation(&c, "trader", None, "chart", t0() + Duration::seconds(2)).unwrap();
    let both = list_conversations(&c, "trader", &["chat".into(), "voice".into()], 100).unwrap();
    assert_eq!(
        both.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![b.id, a.id]
    );
    assert_eq!(list_conversations(&c, "trader", &[], 2).unwrap().len(), 2);
}

#[test]
fn truncation_removes_the_message_and_everything_after() {
    let c = db();
    let conv = create_conversation(&c, "trader", None, "chat", t0()).unwrap();
    let ids: Vec<i64> = (0..4)
        .map(|i| {
            add_message(&c, conv.id, "user", &i.to_string(), None, None, t0())
                .unwrap()
                .id
        })
        .collect();
    let removed = truncate_from(&c, conv.id, ids[1]).unwrap();
    assert_eq!(
        removed.iter().map(|m| m.id).collect::<Vec<_>>(),
        ids[1..].to_vec()
    );
    assert_eq!(list_messages(&c, conv.id, 500).unwrap().len(), 1);
}

#[test]
fn history_is_the_last_completed_runs_oldest_first() {
    let c = db();
    let conv = create_conversation(&c, "trader", None, "chat", t0()).unwrap();
    for (i, status) in ["completed", "cancelled", "completed", "error", "completed"]
        .iter()
        .enumerate()
    {
        let id = format!("r{}", i);
        let at = t0() + Duration::seconds(i as i64);
        insert_run(&c, &id, conv.id, "s", at).unwrap();
        save_run(&c, &id, status, &format!("[\"{}\"]", id), at).unwrap();
    }
    assert_eq!(history(&c, "s", 2).unwrap(), vec!["[\"r2\"]", "[\"r4\"]"]);
    assert_eq!(delete_runs(&c, &["r2".into(), "nope".into()]).unwrap(), 1);
    assert_eq!(history(&c, "s", 8).unwrap(), vec!["[\"r0\"]", "[\"r4\"]"]);
}

fn paused(c: &Connection, run: &str, at: DateTime<Utc>) {
    let conv = create_conversation(c, "trader", None, "chat", at).unwrap();
    save_paused(
        c,
        &PausedRow {
            run_id: run.into(),
            conversation_id: conv.id,
            session_id: "s".into(),
            user_id: "trader".into(),
            pending: "{}".into(),
            paused_at: stamp(at),
            expires_at: stamp(at + Duration::minutes(10)),
        },
    )
    .unwrap();
}

/// D-021 on the web: approving from two tabs resumed the run twice. The
/// claim is one statement, so the second approval finds it taken.
#[test]
fn a_paused_run_is_claimed_once() {
    let c = db();
    paused(&c, "r1", t0());
    let now = t0() + Duration::minutes(1);
    assert!(matches!(
        claim_paused(&c, "r1", "trader", now).unwrap(),
        Claim::Claimed(_)
    ));
    assert_eq!(
        claim_paused(&c, "r1", "trader", now).unwrap(),
        Claim::AlreadyAnswered
    );
    release_paused(&c, "r1").unwrap();
    assert_eq!(
        claim_paused(&c, "r1", "trader", now).unwrap(),
        Claim::AlreadyAnswered
    );
    assert_eq!(
        claim_paused(&c, "r1", "other", now).unwrap(),
        Claim::NotFound
    );
}

#[test]
fn a_paused_run_expires_after_ten_minutes() {
    let c = db();
    paused(&c, "r1", t0());
    let late = t0() + Duration::minutes(10);
    assert_eq!(
        claim_paused(&c, "r1", "trader", late).unwrap(),
        Claim::Expired
    );
    assert_eq!(
        claim_paused(&c, "r1", "trader", t0()).unwrap(),
        Claim::Expired
    );
    paused(&c, "r2", t0());
    assert_eq!(expire_paused(&c, late).unwrap(), vec!["r2".to_string()]);
}

#[test]
fn deleting_a_conversation_takes_its_runs_and_paused_runs() {
    let c = db();
    paused(&c, "r1", t0());
    let conv = list_conversations(&c, "trader", &[], 1).unwrap()[0].clone();
    insert_run(&c, "r1", conv.id, "s", t0()).unwrap();
    assert!(delete_conversation(&c, conv.id, "trader").unwrap());
    assert_eq!(
        claim_paused(&c, "r1", "trader", t0()).unwrap(),
        Claim::NotFound
    );
    assert!(run_messages(&c, "r1").unwrap().is_none());
}

#[test]
fn a_run_left_running_by_a_quit_is_closed_and_never_replayed() {
    let c = db();
    let conv = create_conversation(&c, "trader", None, "chat", t0()).unwrap();
    insert_run(&c, "r1", conv.id, "s", t0()).unwrap();
    assert_eq!(close_interrupted_runs(&c, t0()).unwrap(), 1);
    assert!(history(&c, "s", 8).unwrap().is_empty());
}
