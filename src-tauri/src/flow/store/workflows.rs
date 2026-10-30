//! `flow_workflows`: create, read, edit, delete, the active flag, the
//! scheduler job id and the webhook credentials (the web's `flow_db.py`
//! workflow functions).

use super::{iso, stamp};
use crate::error::Result;
use crate::flow::contract::model::{Workflow, WorkflowSummary};
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

const COLUMNS: &str = "id, name, description, nodes, edges, is_active, schedule_job_id, \
     webhook_token, webhook_secret, webhook_secret_nonce, webhook_enabled, webhook_auth_type, \
     created_at, updated_at";

/// The web's `secrets.token_urlsafe(32)`: 32 random bytes, base64url.
pub fn generate_webhook_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The web's `secrets.token_hex(32)`: 32 random bytes, hex.
pub fn generate_webhook_secret() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

pub(crate) fn secret_aad(id: i64) -> Aad {
    Aad::new("flow_workflows", "webhook_secret", &id.to_string())
}

/// Stored JSON text as a value; a column that does not parse reads as an
/// empty list (it can only be one written before validation existed).
fn json_col(text: Option<String>) -> Value {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

struct Raw {
    w: Workflow,
    secret: Option<(String, String)>,
}

fn from_row(r: &Row) -> rusqlite::Result<Raw> {
    let secret: Option<String> = r.get(8)?;
    let nonce: Option<String> = r.get(9)?;
    Ok(Raw {
        w: Workflow {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            nodes: json_col(r.get(3)?),
            edges: json_col(r.get(4)?),
            is_active: r.get(5)?,
            schedule_job_id: r.get(6)?,
            webhook_token: r.get(7)?,
            webhook_secret: None,
            webhook_enabled: r.get(10)?,
            webhook_auth_type: r
                .get::<_, Option<String>>(11)?
                .unwrap_or_else(|| "payload".into()),
            created_at: iso(r.get(12)?),
            updated_at: iso(r.get(13)?),
        },
        secret: secret.zip(nonce),
    })
}

/// Decrypt the secret into the row; one that cannot be read (keys locked,
/// or changed since) is left out and logged once here.
fn with_secret(sec: &SecurityManager, raw: Raw) -> Workflow {
    let mut w = raw.w;
    if let Some((ct, nonce)) = raw.secret {
        match sec.decrypt(&ct, &nonce, &secret_aad(w.id)) {
            Ok(s) => w.webhook_secret = Some(s),
            Err(e) => tracing::warn!(
                "The webhook secret of workflow {} could not be read: {}",
                w.id,
                e
            ),
        }
    }
    w
}

/// A workflow with its webhook secret (`get_workflow`).
pub fn get(conn: &Connection, sec: &SecurityManager, id: i64) -> Result<Option<Workflow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {} FROM flow_workflows WHERE id = ?1", COLUMNS),
            params![id],
            from_row,
        )
        .optional()?;
    Ok(raw.map(|r| with_secret(sec, r)))
}

/// The workflow a webhook token names (`get_workflow_by_webhook_token`).
pub fn get_by_token(
    conn: &Connection,
    sec: &SecurityManager,
    token: &str,
) -> Result<Option<Workflow>> {
    let raw = conn
        .query_row(
            &format!(
                "SELECT {} FROM flow_workflows WHERE webhook_token = ?1",
                COLUMNS
            ),
            params![token],
            from_row,
        )
        .optional()?;
    Ok(raw.map(|r| with_secret(sec, r)))
}

/// Every workflow, most recently changed first, with its newest run's
/// status (`get_all_workflows` plus one execution per workflow).
pub fn list(conn: &Connection) -> Result<Vec<WorkflowSummary>> {
    let mut st = conn.prepare(
        "SELECT w.id, w.name, w.description, w.is_active, w.webhook_enabled, w.created_at,
                w.updated_at,
                (SELECT e.status FROM flow_workflow_executions e
                  WHERE e.workflow_id = w.id ORDER BY e.id DESC LIMIT 1)
           FROM flow_workflows w
          ORDER BY w.updated_at DESC, w.id DESC",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(WorkflowSummary {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                is_active: r.get(3)?,
                webhook_enabled: r.get(4)?,
                created_at: iso(r.get(5)?),
                updated_at: iso(r.get(6)?),
                last_execution_status: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The ids of the active workflows (`get_active_workflows`).
pub fn active_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut st = conn.prepare("SELECT id FROM flow_workflows WHERE is_active = 1 ORDER BY id")?;
    let v = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(v)
}

/// The names of the active workflows that wait on something while the app
/// runs: a schedule, a price alert, an order watch or a webhook (a manual
/// start with its webhook off waits on nothing). For the quit warning.
pub fn active_waiting_names(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT name, nodes, webhook_enabled FROM flow_workflows WHERE is_active = 1 ORDER BY name, id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                json_col(r.get(1)?),
                r.get::<_, bool>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, nodes, webhook)| *webhook || waits_on_a_trigger(nodes))
        .map(|(name, _, _)| name)
        .collect())
}

/// Whether a graph's trigger fires on its own: any trigger but a manual
/// start. A graph that cannot be read counts as waiting.
fn waits_on_a_trigger(nodes: &Value) -> bool {
    let Ok(graph) = crate::flow::contract::model::Graph::parse(nodes, &Value::Array(Vec::new()))
    else {
        return true;
    };
    match graph.trigger() {
        None => false,
        Some(t) if t.node_type == "start" => !matches!(
            t.data.get("scheduleType").and_then(Value::as_str),
            None | Some("manual") | Some("")
        ),
        Some(_) => true,
    }
}

/// What a new workflow is made of.
pub struct NewWorkflow<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub nodes: &'a Value,
    pub edges: &'a Value,
}

/// Create a workflow with a fresh webhook token and secret
/// (`create_workflow`; the web's column defaults make both).
pub fn create(
    conn: &Connection,
    sec: &SecurityManager,
    w: &NewWorkflow,
    now: DateTime<Utc>,
) -> Result<Workflow> {
    let made = insert(conn, sec, w, now);
    match &made {
        Ok(saved) => tracing::info!("Created workflow: {} (id={})", saved.name, saved.id),
        Err(e) => tracing::error!("Error creating workflow: {}", e),
    }
    made
}

fn insert(
    conn: &Connection,
    sec: &SecurityManager,
    w: &NewWorkflow,
    now: DateTime<Utc>,
) -> Result<Workflow> {
    let tx = crate::db::sqlite::write_transaction(conn)?;
    let at = stamp(now);
    tx.execute(
        "INSERT INTO flow_workflows (name, description, nodes, edges, webhook_token, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![w.name, w.description, w.nodes.to_string(), w.edges.to_string(), generate_webhook_token(), at],
    )?;
    let id = tx.last_insert_rowid();
    put_secret(&tx, sec, id, &generate_webhook_secret())?;
    tx.commit()?;
    get(conn, sec, id)?.ok_or_else(|| {
        crate::error::AppError::Internal("The new workflow could not be read back".into())
    })
}

fn put_secret(conn: &Connection, sec: &SecurityManager, id: i64, secret: &str) -> Result<()> {
    let (ct, nonce) = sec.encrypt(secret, &secret_aad(id))?;
    conn.execute(
        "UPDATE flow_workflows SET webhook_secret = ?1, webhook_secret_nonce = ?2 WHERE id = ?3",
        params![ct, nonce, id],
    )?;
    Ok(())
}

/// The fields the editor may change. `None` leaves a field as it is;
/// `description: Some(None)` clears it.
#[derive(Debug, Default, Clone)]
pub struct Edit {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub nodes: Option<Value>,
    pub edges: Option<Value>,
}

/// Apply an edit (`update_workflow`); `None` when the workflow is gone.
pub fn update(
    conn: &Connection,
    sec: &SecurityManager,
    id: i64,
    e: &Edit,
    now: DateTime<Utc>,
) -> Result<Option<Workflow>> {
    let written = conn.execute(
        "UPDATE flow_workflows SET
             name = COALESCE(?1, name),
             description = CASE WHEN ?2 THEN ?3 ELSE description END,
             nodes = COALESCE(?4, nodes),
             edges = COALESCE(?5, edges),
             updated_at = ?6
         WHERE id = ?7",
        params![
            e.name,
            e.description.is_some(),
            e.description.clone().flatten(),
            e.nodes.as_ref().map(Value::to_string),
            e.edges.as_ref().map(Value::to_string),
            stamp(now),
            id
        ],
    );
    if !changed(written, id)? {
        return Ok(None);
    }
    get(conn, sec, id)
}

/// What one of the writers the web's `update_workflow` backs did, logged
/// as the web logs it: `Updated workflow {id}` when a row changed, `Error
/// updating workflow {id}: {e}` when the write failed.
fn changed(written: rusqlite::Result<usize>, id: i64) -> Result<bool> {
    match written {
        Ok(n) => {
            if n > 0 {
                tracing::info!("Updated workflow {}", id);
            }
            Ok(n > 0)
        }
        Err(e) => {
            tracing::error!("Error updating workflow {}: {}", id, e);
            Err(e.into())
        }
    }
}

/// Delete a workflow and its run history (`delete_workflow`; the
/// executions go by the foreign key's cascade). `false` when it was gone.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    let deleted = (|| -> Result<bool> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        tx.execute(
            "DELETE FROM flow_workflow_executions WHERE workflow_id = ?1",
            params![id],
        )?;
        let n = tx.execute("DELETE FROM flow_workflows WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    })();
    match &deleted {
        Ok(true) => tracing::info!("Deleted workflow {}", id),
        Ok(false) => {}
        Err(e) => tracing::error!("Error deleting workflow {}: {}", id, e),
    }
    deleted
}

/// Set the active flag (`activate_workflow` / `deactivate_workflow`).
pub fn set_active(conn: &Connection, id: i64, active: bool, now: DateTime<Utc>) -> Result<bool> {
    let written = conn.execute(
        "UPDATE flow_workflows SET is_active = ?1, updated_at = ?2 WHERE id = ?3",
        params![active, stamp(now), id],
    );
    changed(written, id)
}

/// Turn the webhook on or off (`enable_webhook` / `disable_webhook`).
pub fn set_webhook_enabled(
    conn: &Connection,
    id: i64,
    enabled: bool,
    now: DateTime<Utc>,
) -> Result<bool> {
    let written = conn.execute(
        "UPDATE flow_workflows SET webhook_enabled = ?1, updated_at = ?2 WHERE id = ?3",
        params![enabled, stamp(now), id],
    );
    changed(written, id)
}

/// Set how a webhook proves itself, `payload` or `url`
/// (`set_webhook_auth_type`); `false` for any other value or no workflow.
pub fn set_webhook_auth_type(
    conn: &Connection,
    id: i64,
    auth_type: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    if auth_type != "payload" && auth_type != "url" {
        tracing::error!("Invalid webhook auth type: {}", auth_type);
        return Ok(false);
    }
    let written = conn.execute(
        "UPDATE flow_workflows SET webhook_auth_type = ?1, updated_at = ?2 WHERE id = ?3",
        params![auth_type, stamp(now), id],
    );
    changed(written, id)
}

/// Make the webhook token and secret if either is missing or the secret
/// can no longer be read (`ensure_webhook_credentials`). `false` when the
/// workflow is gone.
pub fn ensure_webhook_credentials(
    conn: &Connection,
    sec: &SecurityManager,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool> {
    let Some(w) = get(conn, sec, id)? else {
        return Ok(false);
    };
    if w.webhook_token.is_none() {
        conn.execute(
            "UPDATE flow_workflows SET webhook_token = ?1 WHERE id = ?2",
            params![generate_webhook_token(), id],
        )?;
    }
    if w.webhook_secret.is_none() {
        put_secret(conn, sec, id, &generate_webhook_secret())?;
    }
    if w.webhook_token.is_none() || w.webhook_secret.is_none() {
        // The web's row `onupdate` moves updated_at with any change.
        conn.execute(
            "UPDATE flow_workflows SET updated_at = ?1 WHERE id = ?2",
            params![stamp(now), id],
        )?;
        tracing::info!("Generated webhook credentials for workflow {}", id);
    }
    Ok(true)
}

/// A new webhook token; the old address stops working at once
/// (`regenerate_webhook_token`).
pub fn regenerate_webhook_token(
    conn: &Connection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Option<String>> {
    let token = generate_webhook_token();
    let n = conn.execute(
        "UPDATE flow_workflows SET webhook_token = ?1, updated_at = ?2 WHERE id = ?3",
        params![token, stamp(now), id],
    )?;
    if n > 0 {
        tracing::info!("Regenerated webhook token for workflow {}", id);
    }
    Ok((n > 0).then_some(token))
}

/// A new webhook secret; the old one stops working at once
/// (`regenerate_webhook_secret`).
pub fn regenerate_webhook_secret(
    conn: &Connection,
    sec: &SecurityManager,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Option<Secret>> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM flow_workflows WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }
    let secret = generate_webhook_secret();
    put_secret(conn, sec, id, &secret)?;
    conn.execute(
        "UPDATE flow_workflows SET updated_at = ?1 WHERE id = ?2",
        params![stamp(now), id],
    )?;
    tracing::info!("Regenerated webhook secret for workflow {}", id);
    Ok(Some(Secret::new(secret)))
}

#[cfg(test)]
mod tests {
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
        assert!(ensure_webhook_credentials(&conn, &sec, w.id, now()).unwrap());
        assert!(get(&conn, &sec, w.id)
            .unwrap()
            .unwrap()
            .webhook_secret
            .is_some());
        assert!(!ensure_webhook_credentials(&conn, &sec, 999, now()).unwrap());
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
        assert!(ensure_webhook_credentials(&conn, &sec, w.id, later).unwrap());
        assert_eq!(stamp_of(&conn), w.updated_at, "nothing was missing");
        super::super::drop_secrets(&conn).unwrap();
        assert!(ensure_webhook_credentials(&conn, &sec, w.id, later).unwrap());
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
        ensure_webhook_credentials(&conn, &sec, id, now()).unwrap();
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
}
