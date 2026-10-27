//! `GET /agent/api/orders/preview`: what a paused smart order will send,
//! for the approval card (a desktop addition, security review
//! 2026-10-27). The query carries the call's arguments (`symbol`,
//! `exchange`, `product`, `position_size`, `quantity`, `action`); the
//! answer is `{held, target, action, quantity}`, `held` null when the
//! position cannot be read. Read only.

use crate::agent::tools::orders::smart_order_preview;
use crate::server::envelope::json_response;
use crate::server::middleware::AuthedUser;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
    Extension,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// The arguments a smart order's preview reads.
const FIELDS: [&str; 6] = [
    "symbol",
    "exchange",
    "product",
    "position_size",
    "quantity",
    "action",
];

pub async fn preview(
    State(ctx): State<Arc<AppState>>,
    Extension(user): Extension<AuthedUser>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let args: Map<String, Value> = FIELDS
        .iter()
        .filter_map(|k| q.get(*k).map(|v| (k.to_string(), json!(v))))
        .collect();
    let data = smart_order_preview(&ctx, &user.username, args).await;
    json_response(StatusCode::OK, json!({"status": "success", "data": data}))
}
