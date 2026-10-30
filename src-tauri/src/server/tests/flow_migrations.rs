//! Parity IE-11 and V-115: legacy node payloads are upgraded before the
//! workflow is validated at import and at replace, so the upgrade decides
//! the verdict, and nowhere else (create, update and a run take the graph as
//! it is stored), as the web's `blueprints/flow.py` does.

use super::flow::F;
use super::*;

fn at(y: i64) -> Value {
    json!({"x": 0, "y": y})
}

/// A positionCheck saved before it had `condition` (an `operator` instead)
/// and a fundCheck saved before `minAvailable` (a `threshold`).
fn legacy(position_operator: &str) -> Value {
    json!({
        "name": "Legacy guard",
        "nodes": [
            {"id": "s", "type": "start", "position": at(0), "data": {"scheduleType": "manual"}},
            {"id": "p", "type": "positionCheck", "position": at(100),
             "data": {"symbol": "SBIN", "exchange": "NSE", "operator": position_operator, "threshold": 5}},
            {"id": "f", "type": "fundCheck", "position": at(200),
             "data": {"operator": "gt", "threshold": 10000}}
        ],
        "edges": [
            {"id": "e1", "source": "s", "target": "p"},
            {"id": "e2", "source": "p", "target": "f", "sourceHandle": "true"}
        ]
    })
}

const NOTES: [&str; 2] = [
    "positionCheck 'p': operator 'gt' -> condition 'quantity_above' (threshold kept)",
    "fundCheck 'f': threshold -> minAvailable (10000)",
];

/// Each node's data, by node id.
fn data_by_id(graph: &Value) -> Value {
    let mut by_id = serde_json::Map::new();
    for node in graph["nodes"].as_array().unwrap() {
        by_id.insert(
            node["id"].as_str().unwrap().to_string(),
            node["data"].clone(),
        );
    }
    Value::Object(by_id)
}

/// The stored node data, by node id.
async fn stored(f: &F, id: i64) -> Value {
    let (s, v) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    data_by_id(&v)
}

fn upgraded() -> Value {
    json!({
        "s": {"scheduleType": "manual"},
        "p": {"symbol": "SBIN", "exchange": "NSE", "condition": "quantity_above", "threshold": 5},
        "f": {"minAvailable": 10000}
    })
}

/// IE-11: import upgrades first. As stored, the positionCheck lacks the
/// `condition` strict validation requires; the upgrade supplies it, so the
/// import is accepted, answers the notes and stores the upgraded nodes.
#[tokio::test]
async fn import_upgrades_legacy_nodes_before_validating() {
    let f = F::new();
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows/import",
            Some(&legacy("gt").to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    assert_eq!(v["migrations"], json!(NOTES));
    let id = v["workflow_id"].as_i64().unwrap();
    assert_eq!(stored(&f, id).await, upgraded());
}

/// V-115: when the upgrade has no equivalent the node is left as it was, so
/// the validation that follows refuses it.
#[tokio::test]
async fn an_import_the_upgrade_cannot_fix_is_refused() {
    let f = F::new();
    let (s, v) = f
        .call(
            Method::POST,
            "/flow/api/workflows/import",
            Some(&legacy("weird").to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["error"], "Invalid workflow format");
    assert_eq!(
        v["message"],
        "A positionCheck node needs condition. Without it the node fails at run time, not at import."
    );
}

/// V-115: replace upgrades first too, answers the notes and stores the
/// upgraded graph in place.
#[tokio::test]
async fn replace_upgrades_legacy_nodes_before_validating() {
    let f = F::new();
    let id = f.create(json!({"name": "To replace"})).await["id"]
        .as_i64()
        .unwrap();
    let (s, v) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/replace", id),
            Some(&legacy("gt").to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["migrations"], json!(NOTES));
    assert_eq!(stored(&f, id).await, upgraded());
}

/// V-115: create and update store the graph as sent (a draft may still be
/// legacy), and a run validates the stored graph without upgrading it, so a
/// legacy positionCheck saved that way cannot run until it is restated.
#[tokio::test]
async fn create_update_and_run_never_upgrade() {
    let f = F::new();
    let created = f.create(legacy("gt")).await;
    let id = created["id"].as_i64().unwrap();
    assert_eq!(stored(&f, id).await, data_by_id(&legacy("gt")));

    let mut changed = legacy("lt");
    changed.as_object_mut().unwrap().remove("name");
    let (s, v) = f
        .call(
            Method::PUT,
            &format!("/flow/api/workflows/{}", id),
            Some(&changed.to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(stored(&f, id).await, data_by_id(&legacy("lt")));

    let (s, v) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/execute", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(v["error"], "Workflow cannot be executed");
    assert_eq!(
        v["message"],
        "A positionCheck node needs condition. Without it the node fails at run time, not at import."
    );
}
