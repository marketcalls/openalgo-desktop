//! Chart preferences over `/api/v1/chart` (web `services/chart_service.py`
//! and the limits in `restx_api/chart_api.py`): a key/value store per
//! account, read with GET and merged with POST.

use super::apikey_service::{ApiKeyService, KeyCheck};
use super::core::{Reply, INVALID_API_KEY};
use crate::db::sqlite::chart_prefs;
use crate::state::AppState;
use serde_json::{json, Map, Value};

/// Web limits on one POST.
const MAX_KEYS: usize = 50;
const MAX_KEY_CHARS: usize = 50;
const MAX_VALUE_BYTES: usize = 1_048_576;

/// The account an already authorized key belongs to.
fn account(ctx: &AppState, apikey: &str) -> Result<String, Reply> {
    match ApiKeyService::check(ctx, apikey) {
        KeyCheck::Valid(user) => Ok(user),
        _ => Err(Reply::error(403, INVALID_API_KEY)),
    }
}

/// `GET /api/v1/chart`: every saved preference.
pub fn get_reply(ctx: &AppState, apikey: &str) -> Reply {
    let user = match account(ctx, apikey) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let read = ctx
        .sqlite
        .conn()
        .and_then(|conn| chart_prefs::read_all(&conn, &user));
    match read {
        Ok(prefs) => Reply::ok(json!({"status": "success", "data": prefs})),
        Err(e) => {
            tracing::error!("Chart preferences could not be read: {}", e);
            Reply::error(500, "Server Error")
        }
    }
}

/// The preferences in a POST body (every key but `apikey`), or the web's
/// refusal when they break a limit.
pub fn preferences_of(body: &Map<String, Value>) -> Result<Vec<(String, Value)>, Reply> {
    let prefs: Vec<(String, Value)> = body
        .iter()
        .filter(|(k, _)| k.as_str() != "apikey")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if prefs.len() > MAX_KEYS {
        return Err(Reply::error(400, "Too many preference keys (max 50)"));
    }
    for (key, value) in &prefs {
        if key.chars().count() > MAX_KEY_CHARS {
            let head: String = key.chars().take(20).collect();
            return Err(Reply::error(
                400,
                format!("Preference key too long: {head}... (max 50 chars)"),
            ));
        }
        if value.to_string().len() > MAX_VALUE_BYTES {
            return Err(Reply::error(
                400,
                format!("Preference value too large for key: {key} (max 1MB)"),
            ));
        }
    }
    if prefs.is_empty() {
        return Err(Reply::error(400, "No preferences provided to update"));
    }
    Ok(prefs)
}

/// The text a value is saved as. The web's column is text: a string is
/// kept as sent and a number or a boolean as its text (`true` as "1", as
/// SQLite stores Python's `True`). The web fails on an object or a list;
/// here it is kept as its JSON text.
fn stored(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s),
        Value::Bool(b) => Some(if b { "1" } else { "0" }.to_string()),
        other => Some(other.to_string()),
    }
}

/// `POST /api/v1/chart`: merge `prefs` into the saved preferences.
pub fn update_reply(ctx: &AppState, apikey: &str, prefs: Vec<(String, Value)>) -> Reply {
    let user = match account(ctx, apikey) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let rows: Vec<(String, Option<String>)> =
        prefs.into_iter().map(|(k, v)| (k, stored(v))).collect();
    let saved = ctx
        .sqlite
        .conn()
        .and_then(|mut conn| chart_prefs::save(&mut conn, &user, &rows));
    match saved {
        Ok(()) => Reply::ok(json!({
            "status": "success",
            "message": "Preferences updated successfully",
        })),
        Err(e) => {
            tracing::error!("Chart preferences could not be saved: {}", e);
            Reply::error(500, "Failed to update preferences")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value is measured as the JSON it was sent as; the request limit
    /// keeps a single body under 1 MiB, so this is the only place a value
    /// over the web's 1 MB cap can be checked.
    #[test]
    fn a_value_over_a_megabyte_is_refused_by_key() {
        let mut body = Map::new();
        body.insert("apikey".into(), json!("k"));
        body.insert("layout".into(), json!("x".repeat(MAX_VALUE_BYTES)));
        let refused = preferences_of(&body).unwrap_err();
        assert_eq!(refused.status, 400);
        assert_eq!(
            refused.message(),
            "Preference value too large for key: layout (max 1MB)"
        );
        body.insert("layout".into(), json!("x".repeat(MAX_VALUE_BYTES - 2)));
        assert_eq!(preferences_of(&body).unwrap().len(), 1);
    }
}
