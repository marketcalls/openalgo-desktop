//! `/api/v1/chart` (web `restx_api/chart_api.py`): chart preferences, read
//! with the key in the query and merged with the key in the body. A valid
//! key is enough; no broker session is needed. What happens next is
//! `services::chart_prefs_service`.

use super::{authorize, query_map, send, Auth};
use crate::server::envelope::{error, json_response, read_json_object};
use crate::server::middleware::ClientIp;
use crate::services::chart_prefs_service as prefs;
use crate::services::core::{Reply, INVALID_API_KEY};
use crate::services::schemas;
use crate::state::AppState;
use axum::{
    extract::{RawQuery, Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// GET /api/v1/chart?apikey=
pub async fn get_preferences(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    RawQuery(q): RawQuery,
) -> Response {
    let key = query_map(q)
        .get("apikey")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if key.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Missing apikey parameter");
    }
    if !authorize(&ctx, &key, ip, Auth::KeyOnly).await {
        return send(Reply::error(403, INVALID_API_KEY));
    }
    send(prefs::get_reply(&ctx, &key))
}

/// POST /api/v1/chart with `apikey` and the preferences in the body. The
/// web's order: no body, the key's schema, the limits, then the key itself.
pub async fn update_preferences(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "No data provided");
    }
    let key_only: Map<String, Value> = body
        .get("apikey")
        .map(|k| [("apikey".to_string(), k.clone())].into_iter().collect())
        .unwrap_or_default();
    let key = match schemas::apikey_only().load(&key_only) {
        Ok(loaded) => loaded
            .get("apikey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": e.to_json()}),
            )
        }
    };
    let preferences = match prefs::preferences_of(&body) {
        Ok(p) => p,
        Err(refused) => return send(refused),
    };
    if !authorize(&ctx, &key, ip, Auth::KeyOnly).await {
        return send(Reply::error(403, INVALID_API_KEY));
    }
    send(prefs::update_reply(&ctx, &key, preferences))
}
