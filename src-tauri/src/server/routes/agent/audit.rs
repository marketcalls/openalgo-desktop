//! `GET /agent/api/audit`: the Agent's audit trail, read (a desktop
//! addition, D-006; the web has `list_audit` and no route for it).
//!
//! Filters as the web's `list_audit`: `conversation_id`, `run_id`, `tool`,
//! and `limit` (200 when absent). Newest first. A conversation that is
//! another user's is not found (404, never 403, so ids cannot be probed).
//! Rows were redacted when written and are redacted again here.

use crate::agent::safety::audit::{self, store, Filter, Listing};
use crate::server::envelope::{error, json_response, unexpected};
use crate::server::middleware::AuthedUser;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
    Extension,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// The query as a filter, or the sentence for the field that is not one.
fn filter(q: &HashMap<String, String>) -> Result<Filter, &'static str> {
    let text = |k: &str| q.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let conversation_id = match text("conversation_id") {
        None => None,
        Some(v) => Some(
            v.parse::<i64>()
                .map_err(|_| "conversation_id must be a whole number")?,
        ),
    };
    let limit = match text("limit") {
        None => store::DEFAULT_LIMIT,
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| "limit must be a whole number")?,
    };
    Ok(Filter {
        conversation_id,
        run_id: text("run_id").map(str::to_string),
        tool: text("tool").map(str::to_string),
        limit,
    })
}

pub async fn list(
    State(ctx): State<Arc<AppState>>,
    Extension(user): Extension<AuthedUser>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let f = match filter(&q) {
        Ok(f) => f,
        Err(sentence) => return error(StatusCode::BAD_REQUEST, sentence),
    };
    match audit::list(&ctx, &user.username, f).await {
        Ok(Listing::Rows(rows)) => {
            json_response(StatusCode::OK, json!({"status": "success", "data": rows}))
        }
        Ok(Listing::NotYours) => error(StatusCode::NOT_FOUND, "Conversation not found"),
        Err(e) => {
            tracing::error!("The Agent audit trail could not be read: {}", e);
            unexpected()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_query_reads_as_the_webs_filters() {
        assert_eq!(
            filter(&q(&[])).unwrap(),
            Filter {
                limit: 200,
                ..Filter::default()
            }
        );
        assert_eq!(
            filter(&q(&[
                ("conversation_id", " 7 "),
                ("run_id", "r1"),
                ("tool", "place_order"),
                ("limit", "5")
            ]))
            .unwrap(),
            Filter {
                conversation_id: Some(7),
                run_id: Some("r1".into()),
                tool: Some("place_order".into()),
                limit: 5
            }
        );
        assert_eq!(
            filter(&q(&[("conversation_id", "x")])).unwrap_err(),
            "conversation_id must be a whole number"
        );
        assert_eq!(
            filter(&q(&[("limit", "1.5")])).unwrap_err(),
            "limit must be a whole number"
        );
    }
}
