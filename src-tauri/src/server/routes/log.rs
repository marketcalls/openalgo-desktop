//! Web `blueprints/log.py`: the API order log (live `/api/v1` calls, from
//! `logs.db` `order_logs`), paged as JSON and exported as CSV. Request
//! bodies were stored without the API key; it is removed again here.

use crate::db::sqlite::monitor::OrderLogRow;
use crate::server::envelope::error;
use crate::server::routes::webui::{csv_row, download, failed, json_text, ok_text};
use crate::services::order_log_service::{self, PER_PAGE};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

fn utc_iso_of_ist_midnight(d: NaiveDate) -> String {
    Kolkata
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
        .single()
        .map(|t| {
            t.with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default()
}

/// The answer to a date that is not `YYYY-MM-DD`.
fn bad_date() -> Response {
    error(StatusCode::BAD_REQUEST, "Choose dates as YYYY-MM-DD.")
}

/// UTC bounds for an IST date range; today in IST when neither is given.
/// `None`: a date is not `YYYY-MM-DD`.
fn range(ctx: &AppState, q: &HashMap<String, String>) -> Option<(String, String)> {
    let parse = |k: &str| -> Option<Option<NaiveDate>> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Some(None),
            Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(Some),
        }
    };
    let start = parse("start_date")?;
    let end = parse("end_date")?;
    let (from, to) = match (start, end) {
        (None, None) => {
            let today = ctx.now().with_timezone(&Kolkata).date_naive();
            (Some(today), Some(today))
        }
        other => other,
    };
    let from = from
        .map(utc_iso_of_ist_midnight)
        .unwrap_or_else(|| "0000".into());
    let to = to
        .map(|d| utc_iso_of_ist_midnight(d + Duration::days(1)))
        .unwrap_or_else(|| "9999".into());
    Some((from, to))
}

fn sanitize(v: Value) -> Value {
    crate::db::sqlite::logs::redact(v)
}

fn entry(row: &OrderLogRow) -> Value {
    let (id, api_type, req, resp, created) = row;
    let request = serde_json::from_str::<Value>(req)
        .map(sanitize)
        .unwrap_or_else(|_| json!({}));
    let response = serde_json::from_str::<Value>(resp).unwrap_or_else(|_| json!({}));
    let strategy = request
        .get("strategy")
        .and_then(|s| s.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let created_at = chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| {
            t.with_timezone(&Kolkata)
                .format("%Y-%m-%d %I:%M:%S %p")
                .to_string()
        })
        .unwrap_or_else(|_| created.clone());
    json!({
        "id": id,
        "api_type": api_type,
        "request_data": request,
        "response_data": response,
        "strategy": strategy,
        "created_at": created_at,
    })
}

/// GET /logs (JSON request): `{logs, total_pages, current_page}`, read and
/// shaped on the blocking pool (a search reads every entry in the range).
pub async fn view(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some((from, to)) = range(&ctx, &q) else {
        return bad_date();
    };
    let page = q
        .get("page")
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let search = q
        .get("search")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let read = ctx.report(move |c| {
        let (rows, total) = order_log_service::page(c, &from, &to, search.as_deref(), page)?;
        json_text(json!({
            "logs": rows.iter().map(entry).collect::<Vec<_>>(),
            "total_pages": ((total + PER_PAGE - 1) / PER_PAGE).max(1),
            "current_page": page,
        }))
    });
    match read.await {
        Ok(body) => ok_text(body),
        Err(e) => failed(
            "Reading order logs",
            e,
            "Could not load the logs. Try again.",
        ),
    }
}

/// GET /logs/export: every matching entry as CSV (web column set), read
/// and formatted on the blocking pool.
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some((from, to)) = range(&ctx, &q) else {
        return bad_date();
    };
    let search = q
        .get("search")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d_%H%M%S");
    match order_log_service::export(&ctx, from, to, search, export_csv).await {
        Ok(out) => download(out, "text/csv", &format!("openalgo_logs_{}.csv", stamp)),
        Err(e) => failed(
            "Exporting order logs",
            e,
            "An error occurred while exporting logs",
        ),
    }
}

/// The web's CSV columns for the order log, one line per entry; run on the
/// blocking pool (`order_log_service::export`).
fn export_csv(rows: &[OrderLogRow]) -> String {
    let mut out = csv_row(
        &[
            "ID",
            "Timestamp",
            "API Type",
            "Strategy",
            "Exchange",
            "Symbol",
            "Action",
            "Product",
            "Price Type",
            "Quantity",
            "Position Size",
            "Price",
            "Trigger Price",
            "Disclosed Quantity",
            "Order ID",
            "Response",
        ]
        .map(String::from),
    );
    for row in rows {
        let e = entry(row);
        let rq = &e["request_data"];
        let f = |k: &str| match rq.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        };
        out.push_str(&csv_row(&[
            e["id"].to_string(),
            e["created_at"].as_str().unwrap_or_default().to_string(),
            e["api_type"].as_str().unwrap_or_default().to_string(),
            e["strategy"].as_str().unwrap_or_default().to_string(),
            f("exchange"),
            f("symbol"),
            f("action"),
            f("product"),
            f("pricetype"),
            f("quantity"),
            f("position_size"),
            f("price"),
            f("trigger_price"),
            f("disclosed_quantity"),
            f("orderid"),
            e["response_data"].to_string(),
        ]));
    }
    out
}
