use super::super::testing::db;
use super::*;
use serde_json::json;

fn now() -> DateTime<Utc> {
    "2026-10-30T03:45:00Z".parse().unwrap()
}

fn new_workflow(conn: &Connection, sec: &SecurityManager, name: &str) -> Workflow {
    let nodes = json!([{"id": "s", "type": "start", "position": {"x": 0, "y": 0}, "data": {}}]);
    create(
        conn,
        sec,
        &NewWorkflow {
            name,
            description: None,
            nodes: &nodes,
            edges: &json!([]),
        },
        now(),
    )
    .unwrap()
}

#[test]
fn a_new_workflow_has_webhook_credentials_and_keeps_its_graph() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "Gap fade");
    assert_eq!(w.name, "Gap fade");
    assert!(!w.is_active && !w.webhook_enabled);
    assert_eq!(w.webhook_auth_type, "payload");
    assert_eq!(w.webhook_token.as_deref().map(str::len), Some(43));
    assert_eq!(
        w.webhook_secret.as_ref().map(|s| s.expose().len()),
        Some(64)
    );
    // S-07, R-46: `token_hex(32)` is lower-case hex; `token_urlsafe(32)`
    // is base64url without padding.
    let secret = w.webhook_secret.as_ref().unwrap().expose().to_string();
    assert!(
        secret.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "{}",
        secret
    );
    let token = w.webhook_token.clone().unwrap();
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{}",
        token
    );
    assert_eq!(w.nodes[0]["position"], json!({"x": 0, "y": 0}));
    assert_eq!(w.created_at.as_deref(), Some("2026-10-30T03:45:00"));
    // The secret is not on disk in usable form.
    let stored: String = conn
        .query_row(
            "SELECT webhook_secret FROM flow_workflows WHERE id = ?1",
            params![w.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(
        Some(stored.as_str()),
        w.webhook_secret.as_ref().map(|s| s.expose())
    );
}

#[test]
fn an_edit_changes_only_what_it_names() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    let later = now() + chrono::Duration::seconds(5);
    let e = Edit {
        name: Some("B".into()),
        description: Some(Some("d".into())),
        ..Default::default()
    };
    let u = update(&conn, &sec, w.id, &e, later).unwrap().unwrap();
    assert_eq!(
        (u.name.as_str(), u.description.as_deref()),
        ("B", Some("d"))
    );
    assert_eq!(u.nodes, w.nodes);
    assert_eq!(u.updated_at.as_deref(), Some("2026-10-30T03:45:05"));
    let cleared = update(
        &conn,
        &sec,
        w.id,
        &Edit {
            description: Some(None),
            ..Default::default()
        },
        later,
    )
    .unwrap()
    .unwrap();
    assert_eq!(cleared.description, None);
    assert!(update(&conn, &sec, 999, &e, later).unwrap().is_none());
}

#[test]
fn the_list_is_newest_first_with_the_last_run() {
    let (conn, sec) = db();
    let a = new_workflow(&conn, &sec, "A");
    let b = new_workflow(&conn, &sec, "B");
    set_active(&conn, a.id, true, now() + chrono::Duration::seconds(1)).unwrap();
    super::super::executions::create(&conn, b.id, now()).unwrap();
    let l = list(&conn).unwrap();
    assert_eq!(
        l.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
        vec!["A", "B"]
    );
    assert_eq!(l[1].last_execution_status.as_deref(), Some("running"));
    assert_eq!(l[0].last_execution_status, None);
    assert_eq!(active_ids(&conn).unwrap(), vec![a.id]);
}

#[test]
fn only_workflows_that_wait_on_a_trigger_are_named() {
    let (conn, sec) = db();
    let mk = |name: &str, nodes: Value| {
        let w = create(
            &conn,
            &sec,
            &NewWorkflow {
                name,
                description: None,
                nodes: &nodes,
                edges: &json!([]),
            },
            now(),
        )
        .unwrap();
        set_active(&conn, w.id, true, now()).unwrap();
        w.id
    };
    mk(
        "Daily open",
        json!([{"id": "s", "type": "start", "data": {"scheduleType": "daily", "time": "09:15"}}]),
    );
    mk(
        "Alert",
        json!([{"id": "p", "type": "priceAlert", "data": {}}]),
    );
    let manual = mk(
        "Manual",
        json!([{"id": "s", "type": "start", "data": {"scheduleType": "manual"}}]),
    );
    mk(
        "Manual hooked",
        json!([{"id": "s", "type": "start", "data": {}}]),
    );
    let hooked = get(&conn, &sec, manual + 1).unwrap().unwrap();
    set_webhook_enabled(&conn, hooked.id, true, now()).unwrap();
    let inactive = create(
        &conn,
        &sec,
        &NewWorkflow {
            name: "Off",
            description: None,
            nodes: &json!([{"id": "w", "type": "webhookTrigger", "data": {}}]),
            edges: &json!([]),
        },
        now(),
    )
    .unwrap();
    assert!(!inactive.is_active);
    assert_eq!(
        active_waiting_names(&conn).unwrap(),
        vec!["Alert", "Daily open", "Manual hooked"]
    );
}

#[test]
fn deleting_a_workflow_deletes_its_runs() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    super::super::executions::create(&conn, w.id, now()).unwrap();
    assert!(delete(&conn, w.id).unwrap());
    assert!(!delete(&conn, w.id).unwrap());
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM flow_workflow_executions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn credentials_rotate_and_are_remade_when_unreadable() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    let token = regenerate_webhook_token(&conn, w.id, now())
        .unwrap()
        .unwrap();
    assert_ne!(Some(token.clone()), w.webhook_token);
    assert!(get_by_token(&conn, &sec, &token).unwrap().is_some());
    assert!(
        get_by_token(&conn, &sec, w.webhook_token.as_deref().unwrap())
            .unwrap()
            .is_none()
    );
    let secret = regenerate_webhook_secret(&conn, &sec, w.id, now())
        .unwrap()
        .unwrap();
    assert_eq!(
        get(&conn, &sec, w.id)
            .unwrap()
            .unwrap()
            .webhook_secret
            .map(|s| s.expose().to_string()),
        Some(secret.expose().to_string())
    );
    // A fresh start drops what the old keys protected; ensure remakes it.
    super::super::drop_secrets(&conn).unwrap();
    assert!(get(&conn, &sec, w.id)
        .unwrap()
        .unwrap()
        .webhook_secret
        .is_none());
    assert!(ensure_webhook_credentials(&conn, &sec, w.id, now()));
    assert!(get(&conn, &sec, w.id)
        .unwrap()
        .unwrap()
        .webhook_secret
        .is_some());
    assert!(!ensure_webhook_credentials(&conn, &sec, 999, now()));
}

#[test]
fn an_account_reset_stops_every_workflow_and_webhook() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    set_active(&conn, w.id, true, now()).unwrap();
    set_webhook_enabled(&conn, w.id, true, now()).unwrap();
    super::super::reset_credentials(&conn).unwrap();
    let r = get(&conn, &sec, w.id).unwrap().unwrap();
    assert!(!r.is_active && !r.webhook_enabled && r.webhook_secret.is_none());
    assert_ne!(r.webhook_token, w.webhook_token);
}

#[test]
fn only_the_two_auth_types_are_accepted() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    assert!(set_webhook_auth_type(&conn, w.id, "url", now()).unwrap());
    assert!(!set_webhook_auth_type(&conn, w.id, "header", now()).unwrap());
    assert_eq!(
        get(&conn, &sec, w.id).unwrap().unwrap().webhook_auth_type,
        "url"
    );
}

/// S-02: the writers the web's row `onupdate` covers move updated_at,
/// making missing webhook credentials and recording the scheduler job
/// included; ensuring credentials that are all there changes nothing.
#[test]
fn every_write_moves_updated_at() {
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    let stamp_of = |conn: &Connection| get(conn, &sec, w.id).unwrap().unwrap().updated_at;
    let later = now() + chrono::Duration::hours(1);
    assert!(ensure_webhook_credentials(&conn, &sec, w.id, later));
    assert_eq!(stamp_of(&conn), w.updated_at, "nothing was missing");
    super::super::drop_secrets(&conn).unwrap();
    assert!(ensure_webhook_credentials(&conn, &sec, w.id, later));
    assert_eq!(stamp_of(&conn).as_deref(), Some("2026-10-30T04:45:00"));
    let latest = later + chrono::Duration::hours(1);
    assert!(crate::flow::triggers::store::set_schedule_job_id(
        &conn,
        w.id,
        Some("flow_workflow_1"),
        latest
    )
    .unwrap());
    assert_eq!(stamp_of(&conn).as_deref(), Some("2026-10-30T05:45:00"));
}

/// S-10, S-13, S-14 and the webhook writers: the web's `flow_db` lines.
#[test]
fn writes_are_logged_as_the_web_logs_them() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "Gap fade");
    let id = w.id;
    update(&conn, &sec, id, &Edit::default(), now()).unwrap();
    set_active(&conn, id, true, now()).unwrap();
    set_webhook_enabled(&conn, id, true, now()).unwrap();
    set_webhook_auth_type(&conn, id, "url", now()).unwrap();
    set_webhook_auth_type(&conn, id, "header", now()).unwrap();
    regenerate_webhook_token(&conn, id, now()).unwrap();
    regenerate_webhook_secret(&conn, &sec, id, now()).unwrap();
    super::super::drop_secrets(&conn).unwrap();
    ensure_webhook_credentials(&conn, &sec, id, now());
    delete(&conn, id).unwrap();
    let text = logs.text();
    let updated = format!("Updated workflow {}", id);
    assert_eq!(text.matches(&updated).count(), 4, "{}", text);
    for line in [
        format!("Created workflow: Gap fade (id={})", id),
        "Invalid webhook auth type: header".to_string(),
        format!("Regenerated webhook token for workflow {}", id),
        format!("Regenerated webhook secret for workflow {}", id),
        format!("Generated webhook credentials for workflow {}", id),
        format!("Deleted workflow {}", id),
    ] {
        assert!(text.contains(&line), "{}\n{}", line, text);
    }
    conn.execute_batch("ALTER TABLE flow_workflows RENAME TO gone")
        .unwrap();
    let nodes = json!([]);
    let made = create(
        &conn,
        &sec,
        &NewWorkflow {
            name: "B",
            description: None,
            nodes: &nodes,
            edges: &nodes,
        },
        now(),
    );
    assert!(made.is_err());
    assert!(
        logs.text().contains("Error creating workflow: "),
        "{}",
        logs.text()
    );
}

/// S-13 and T-086: a write that fails is logged with the web's words,
/// and the failure reaches the caller.
#[test]
fn failed_writes_are_logged_as_the_web_logs_them() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    conn.execute_batch(
        "CREATE TRIGGER frozen BEFORE UPDATE ON flow_workflows
         BEGIN SELECT RAISE(ABORT, 'held'); END;
         CREATE TRIGGER kept BEFORE DELETE ON flow_workflows
         BEGIN SELECT RAISE(ABORT, 'held'); END;",
    )
    .unwrap();
    let edit = Edit {
        name: Some("B".into()),
        ..Default::default()
    };
    assert!(update(&conn, &sec, w.id, &edit, now()).is_err());
    assert!(set_active(&conn, w.id, true, now()).is_err());
    assert!(delete(&conn, w.id).is_err());
    let text = logs.text();
    let failed = format!("Error updating workflow {}: held", w.id);
    assert_eq!(text.matches(&failed).count(), 2, "{}", text);
    assert!(
        text.contains(&format!("Error deleting workflow {}: ", w.id)),
        "{}",
        text
    );
    assert!(!text.contains("Updated workflow"), "{}", text);
}

/// Refuse every update of `column`, as a database that cannot be
/// written answers.
fn refuse(conn: &Connection, column: &str) {
    conn.execute_batch(&format!(
        "CREATE TRIGGER refuse_{column} BEFORE UPDATE OF {column} ON flow_workflows
         BEGIN SELECT RAISE(ABORT, 'refused'); END;"
    ))
    .unwrap();
}

fn credentials(conn: &Connection, id: i64) -> (Option<String>, Option<String>) {
    conn.query_row(
        "SELECT webhook_token, webhook_secret FROM flow_workflows WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// S-20 (decision 36): minting the missing credentials is one
/// transaction; a write that fails rolls back the token written before
/// it, is logged with the web's words and answers false.
#[test]
fn minting_credentials_is_one_transaction_and_false_on_failure() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    super::super::drop_secrets(&conn).unwrap();
    conn.execute(
        "UPDATE flow_workflows SET webhook_token = NULL WHERE id = ?1",
        params![w.id],
    )
    .unwrap();
    refuse(&conn, "webhook_secret");
    assert!(!ensure_webhook_credentials(&conn, &sec, w.id, now()));
    assert_eq!(credentials(&conn, w.id), (None, None), "rolled back");
    let text = logs.text();
    assert!(
        text.contains(&format!(
            "Error ensuring webhook credentials for workflow {}: ",
            w.id
        )),
        "{}",
        text
    );
    assert!(!text.contains("Generated webhook credentials"), "{}", text);
    conn.execute_batch("DROP TRIGGER refuse_webhook_secret")
        .unwrap();
    assert!(ensure_webhook_credentials(&conn, &sec, w.id, now()));
    let (token, secret) = credentials(&conn, w.id);
    assert!(token.is_some() && secret.is_some());
}

/// S-17, S-18 (decision 36): each rotation writes the web's line once,
/// here where the web writes it; a rotation whose write fails writes
/// the web's error line and changes nothing.
#[test]
fn a_rotation_is_logged_once_and_its_failure_with_the_webs_words() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let (conn, sec) = db();
    let w = new_workflow(&conn, &sec, "A");
    regenerate_webhook_token(&conn, w.id, now()).unwrap();
    regenerate_webhook_secret(&conn, &sec, w.id, now()).unwrap();
    let text = logs.text();
    for line in [
        format!("Regenerated webhook token for workflow {}", w.id),
        format!("Regenerated webhook secret for workflow {}", w.id),
    ] {
        assert_eq!(text.matches(&line).count(), 1, "{}\n{}", line, text);
    }
    let before = credentials(&conn, w.id);
    refuse(&conn, "webhook_token");
    refuse(&conn, "webhook_secret");
    assert!(regenerate_webhook_token(&conn, w.id, now()).is_err());
    assert!(regenerate_webhook_secret(&conn, &sec, w.id, now()).is_err());
    assert_eq!(credentials(&conn, w.id), before);
    let text = logs.text();
    for line in [
        format!("Error regenerating webhook token for workflow {}: ", w.id),
        format!("Error regenerating webhook secret for workflow {}: ", w.id),
    ] {
        assert!(text.contains(&line), "{}\n{}", line, text);
    }
}
