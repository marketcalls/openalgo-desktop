use super::*;
use crate::state::testing::{build, TestCtx};
use chrono::Utc;

fn app() -> TestCtx {
    build(crate::brokers::BrokerRegistry::new(), Utc::now())
}

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

async fn all(t: &TestCtx) -> Vec<Value> {
    match list(
        &t.ctx,
        "trader",
        Filter {
            limit: 1000,
            ..Filter::default()
        },
    )
    .await
    .unwrap()
    {
        Listing::Rows(r) => r,
        Listing::NotYours => panic!("not yours"),
    }
}

/// D-009: the web redacted only the arguments, by twelve exact names; the
/// broker's reply and the error text went into the row as they came. Every
/// part of every row is redacted here.
#[tokio::test]
async fn no_secret_reaches_a_row_whatever_part_it_arrives_in() {
    let t = app();
    let mut attempt = Entry::new(Phase::Attempt, "place_order");
    attempt.args = Some(json!({
        "symbol": "SBIN", "apikey": KEY, "nested": {"Token": "t-1", "feed_token": "f"},
        "legs": [{"password": "p"}], "ciphertext": "c", "note": format!("key {}", KEY)
    }));
    let attempt_id = append(&t.ctx, attempt).await.unwrap();
    let mut result = Entry::new(Phase::Result, "place_order");
    result.attempt_id = Some(attempt_id);
    result.ok = Some(false);
    result.response = Some(json!({
        "status": "error", "access_token": "a", "message": "Bearer abcdefghijklmnopqrstuvwxyz rejected",
        "exception": format!("Uncertain: key={}", KEY), "broker_secret": "s", "symboltoken": "3045"
    }));
    append(&t.ctx, result).await.unwrap();
    let rows = all(&t).await;
    let text = serde_json::to_string(&rows).unwrap();
    for secret in [
        KEY,
        "t-1",
        "\"f\"",
        "\"p\"",
        "\"c\"",
        "abcdefghijklmnopqrstuvwxyz",
        "\"s\"",
        "\"a\"",
    ] {
        assert!(!text.contains(secret), "{} reached a row: {}", secret, text);
    }
    assert!(text.contains("3045"), "an instrument token is not a secret");
    let (res, att) = (&rows[0], &rows[1]);
    assert_eq!(res["attempt_id"], att["id"], "the result names its attempt");
    assert_eq!(att["args"]["symbol"], "SBIN");
    assert_eq!(att["args"]["apikey"], REDACTED);
    assert_eq!(res["response"]["status"], "error");
}

/// The fail-before check for the stored row itself, not the read.
#[tokio::test]
async fn the_stored_text_is_already_redacted() {
    let t = app();
    let mut result = Entry::new(Phase::Result, "cancel_order");
    result.response = Some(json!({"status": "error", "message": format!("bad key {}", KEY)}));
    append(&t.ctx, result).await.unwrap();
    let stored: String = t
        .ctx
        .logs
        .conn()
        .unwrap()
        .query_row("SELECT response FROM ag_audit", [], |r| r.get(0))
        .unwrap();
    assert!(!stored.contains(KEY), "{}", stored);
}

#[test]
fn one_extractor_finds_every_order_id_shape() {
    let reply = json!({
        "status": "success", "orderid": "A1", "order_id": 42,
        "results": [{"orderid": "B2"}, {"order_ids": ["C3", "A1", null, {"x": 1}]}],
        "data": {"NorenOrdNo": "D4", "nestOrderNumber": "E5", "broker_orderid": " F6 ", "trigger_id": "T7"},
        "pending_order_id": 9
    });
    // Sorted: the walk follows the reply's own key order, which depends on
    // how serde_json keeps maps (insertion order or sorted).
    let mut ids = extract_order_ids(&reply);
    ids.sort();
    assert_eq!(ids, ["42", "A1", "B2", "C3", "D4", "E5", "F6", "T7"]);
    let many: Vec<Value> = (0..300).map(|i| json!({"orderid": i})).collect();
    assert_eq!(
        extract_order_ids(&json!({"results": many})).len(),
        MAX_ORDER_IDS
    );
    assert!(extract_order_ids(
        &json!({"status": "success", "mode": "semi_auto", "pending_order_id": 3})
    )
    .is_empty());
}

/// D-041: the vocabulary has four phases and nothing else parses.
#[tokio::test]
async fn the_phases_include_transcript_and_nothing_else() {
    assert_eq!(
        Phase::ALL.map(Phase::as_str),
        ["attempt", "result", "decision", "transcript"]
    );
    assert_eq!(Phase::parse("transcript"), Some(Phase::Transcript));
    assert_eq!(Phase::parse("note"), None);
    let t = app();
    record_transcript(
        &t.ctx,
        "agent",
        "Buying ten SBIN at market.",
        Some(7),
        Some("run-1"),
    )
    .await
    .unwrap();
    record_transcript(&t.ctx, "someone", "Yes, place it.", None, Some(""))
        .await
        .unwrap();
    let rows = all(&t).await;
    assert_eq!(rows[0]["tool"], "voice:trader");
    assert_eq!(rows[0]["run_id"], Value::Null);
    assert_eq!(rows[1]["phase"], "transcript");
    assert_eq!(rows[1]["tool"], "voice:agent");
    assert_eq!(
        rows[1]["args"],
        json!({"text": "Buying ten SBIN at market."})
    );
    assert_eq!(rows[1]["ok"], true);
    assert_eq!(rows[1]["conversation_id"], 7);
}

#[tokio::test]
async fn reads_filter_limit_and_come_newest_first() {
    let t = app();
    for (i, (tool, conversation, run)) in [
        ("place_order", 1, "r1"),
        ("cancel_order", 1, "r1"),
        ("place_order", 2, "r2"),
    ]
    .iter()
    .enumerate()
    {
        t.clock.advance(chrono::Duration::seconds(1));
        let mut e = Entry::new(Phase::Attempt, tool);
        e.conversation_id = Some(*conversation);
        e.run_id = Some(run.to_string());
        e.args = Some(json!({"n": i}));
        append(&t.ctx, e).await.unwrap();
    }
    let read = |f: Filter| {
        let ctx = t.ctx.clone();
        async move {
            match list(&ctx, "trader", f).await.unwrap() {
                Listing::Rows(r) => r
                    .iter()
                    .map(|r| r["args"]["n"].as_i64().unwrap())
                    .collect::<Vec<_>>(),
                Listing::NotYours => panic!(),
            }
        }
    };
    assert_eq!(
        read(Filter {
            limit: 200,
            ..Default::default()
        })
        .await,
        [2, 1, 0]
    );
    assert_eq!(
        read(Filter {
            limit: 2,
            ..Default::default()
        })
        .await,
        [2, 1]
    );
    assert_eq!(
        read(Filter {
            conversation_id: Some(1),
            limit: 200,
            ..Default::default()
        })
        .await,
        [1, 0]
    );
    assert_eq!(
        read(Filter {
            run_id: Some("r2".into()),
            limit: 200,
            ..Default::default()
        })
        .await,
        [2]
    );
    assert_eq!(
        read(Filter {
            tool: Some("place_order".into()),
            limit: 200,
            ..Default::default()
        })
        .await,
        [2, 0]
    );
    let rows = all(&t).await;
    assert_eq!(
        rows[0]["order_ids"],
        json!([]),
        "no ids reads as an empty list"
    );
    assert!(rows[0]["ts"].as_str().unwrap().ends_with("+00:00"));
}

/// Another user's conversation is not found; a deleted one still reads,
/// because the trade record outlives the conversation.
#[tokio::test]
async fn a_conversation_filter_reads_only_the_traders_own() {
    let t = app();
    let theirs = {
        let c = t.ctx.sqlite.conn().unwrap();
        crate::agent::conversations::store::create_conversation(
            &c,
            "someone-else",
            None,
            "chat",
            Utc::now(),
        )
        .unwrap()
        .id
    };
    let filter = Filter {
        conversation_id: Some(theirs),
        limit: 200,
        ..Default::default()
    };
    assert_eq!(
        list(&t.ctx, "trader", filter.clone()).await.unwrap(),
        Listing::NotYours
    );
    let mut e = Entry::new(Phase::Attempt, "place_order");
    e.conversation_id = Some(999);
    append(&t.ctx, e).await.unwrap();
    let deleted = Filter {
        conversation_id: Some(999),
        limit: 200,
        ..Default::default()
    };
    match list(&t.ctx, "trader", deleted).await.unwrap() {
        Listing::Rows(r) => assert_eq!(r.len(), 1),
        Listing::NotYours => panic!("a deleted conversation's rows are still the trader's"),
    }
}

/// A row that cannot be written is logged and the call goes on.
#[tokio::test]
async fn a_failed_write_never_fails_the_caller() {
    let t = app();
    t.ctx
        .logs
        .conn()
        .unwrap()
        .execute("DROP TABLE ag_audit", [])
        .unwrap();
    assert_eq!(
        append(&t.ctx, Entry::new(Phase::Attempt, "place_order")).await,
        None
    );
}

#[test]
fn columns_are_bounded() {
    let mut e = Entry::new(Phase::Decision, &"x".repeat(300));
    e.run_id = Some("r".repeat(300));
    e.risk_verdict = Some("v".repeat(300));
    let row = e.into_row("t".into());
    assert_eq!(row.tool.chars().count(), 120);
    assert_eq!(row.run_id.unwrap().chars().count(), 120);
    assert_eq!(row.risk_verdict.unwrap().chars().count(), 60);
}

/// The logs migration on a populated `logs.db`: rows written before it ran
/// again are kept and it is recorded once.
#[tokio::test]
async fn the_logs_migration_keeps_a_populated_trail() {
    let t = app();
    append(&t.ctx, Entry::new(Phase::Attempt, "place_order"))
        .await
        .unwrap();
    let conn = t.ctx.logs.conn().unwrap();
    conn.execute("DELETE FROM migrations WHERE name = ?1", [store::MIGRATION])
        .unwrap();
    store::migrate(&conn).unwrap();
    store::migrate(&conn).unwrap();
    let (rows, marks): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM ag_audit), (SELECT COUNT(*) FROM migrations WHERE name = ?1)",
            [store::MIGRATION],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, marks), (1, 1));
}
