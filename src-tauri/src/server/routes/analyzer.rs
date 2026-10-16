//! Web `blueprints/analyzer.py`: the sandbox mode request log (stats and
//! requests for a date range) and its CSV export.

use crate::server::envelope::error;
use crate::server::routes::webui::{download, failed, ok};
use crate::services::analyzer_log_service as svc;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::NaiveDate;
use chrono_tz::Asia::Kolkata;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

/// The asked IST date range as stored UTC bounds (today when none is
/// given), or the error reply (boxed: a reply is large).
fn asked_range(
    ctx: &AppState,
    q: &HashMap<String, String>,
) -> Result<(String, String), Box<Response>> {
    let parse = |k: &str| -> Result<Option<NaiveDate>, Box<Response>> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .map_err(|_| {
                    Box::new(error(
                        StatusCode::BAD_REQUEST,
                        "Choose dates as YYYY-MM-DD.",
                    ))
                }),
        }
    };
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    Ok(svc::range(today, parse("start_date")?, parse("end_date")?))
}

fn read_failed(e: crate::error::AppError) -> Response {
    failed(
        "Reading analyzer logs",
        e,
        "Could not load the analyzer logs. Try again.",
    )
}

/// GET /analyzer/api/data
pub async fn data(State(ctx): Ctx, Query(q): Q) -> Response {
    let (from, to) = match asked_range(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return *r,
    };
    match svc::page(&ctx, from, to).await {
        Ok((reqs, stats)) => {
            ok(json!({"status": "success", "data": {"stats": stats, "requests": reqs}}))
        }
        Err(e) => read_failed(e),
    }
}

/// GET /analyzer/export
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let (from, to) = match asked_range(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let reqs = match svc::requests(&ctx, from, to).await {
        Ok(r) => r,
        Err(e) => return read_failed(e),
    };
    let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d_%H%M%S");
    download(
        svc::csv(&reqs),
        "text/csv",
        &format!("analyzer_logs_{}.csv", stamp),
    )
}
