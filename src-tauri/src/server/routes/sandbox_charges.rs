//! Web `blueprints/sandbox.py` charge routes (marketcalls/openalgo#2496):
//! the brokerage plan and the transaction cost report behind
//! `/sandbox/commissions`, and the calculator behind `/sandbox/calculator`.
//! Codes, envelopes and sentences are the web's; the market each page is
//! shown comes from the logged-in broker (`charges_service::market`).

use crate::sandbox::SandboxError;
use crate::server::envelope::{error, json_response};
use crate::server::routes::webui::{download, ok};
use crate::services::charges_service::market;
use crate::services::sandbox_export_service::cost_report_csv;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The charge routes, each for the signed-in trader.
pub fn table() -> Vec<super::RouteSpec> {
    vec![
        r!(GET, "/sandbox/commissions/api/settings", User, settings),
        r!(POST, "/sandbox/commissions/api/settings", User, save),
        r!(POST, "/sandbox/commissions/api/reset", User, reset),
        r!(GET, "/sandbox/commissions/api/report", User, report),
        r!(GET, "/sandbox/commissions/export", User, export),
        r!(POST, "/sandbox/calculator/api/estimate", User, estimate),
    ]
}

type Q = Query<HashMap<String, String>>;

/// A refusal with the sandbox's own sentence; a failure with the web's
/// fixed one for this route.
fn refused(e: SandboxError, failed: &str) -> Response {
    if e.http_status >= 500 {
        tracing::error!("Sandbox charges: {}", e.message);
        return error(StatusCode::INTERNAL_SERVER_ERROR, failed);
    }
    error(
        StatusCode::from_u16(e.http_status).unwrap_or(StatusCode::BAD_REQUEST),
        e.message,
    )
}

/// The body as Flask's `get_json(silent=True)` reads it: anything that is
/// not JSON is `null`, which every charge route refuses in its own words.
fn json_body(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

/// GET /sandbox/commissions/api/settings
pub async fn settings(State(ctx): Ctx) -> Response {
    match ctx.sandbox.charge_settings(market(&ctx)).await {
        Ok(s) => ok(json!({"status": "success", "data": s})),
        Err(e) => refused(e, "The charge settings could not be loaded."),
    }
}

/// POST /sandbox/commissions/api/settings: the switch and the plan, saved
/// in one commit.
pub async fn save(State(ctx): Ctx, body: Bytes) -> Response {
    match ctx
        .sandbox
        .save_charge_settings(json_body(&body), market(&ctx))
        .await
    {
        Ok((message, s)) => {
            tracing::info!("Sandbox charge settings updated");
            ok(json!({"status": "success", "message": message, "data": s}))
        }
        Err(e) => refused(e, "The settings could not be saved. Please try again."),
    }
}

/// POST /sandbox/commissions/api/reset: the plan back to its defaults;
/// trades and funds untouched.
pub async fn reset(State(ctx): Ctx) -> Response {
    match ctx.sandbox.reset_charge_settings(market(&ctx)).await {
        Ok(s) => {
            tracing::info!("Sandbox charge settings reset to defaults");
            ok(json!({
                "status": "success",
                "message": "Charges are back to the defaults. They apply from the next trade.",
                "data": s,
            }))
        }
        Err(e) => refused(e, "The defaults could not be restored. Please try again."),
    }
}

/// POST /sandbox/calculator/api/estimate: a buy and a sell under a plan.
pub async fn estimate(State(ctx): Ctx, body: Bytes) -> Response {
    match ctx
        .sandbox
        .estimate_charges(json_body(&body), market(&ctx))
        .await
    {
        Ok(r) => ok(json!({"status": "success", "data": r})),
        Err(e) => refused(e, "The charges could not be worked out. Please try again."),
    }
}

/// GET /sandbox/commissions/api/report?from=&to=: charges paid in a range
/// of IST days.
pub async fn report(State(ctx): Ctx, Query(q): Q) -> Response {
    const FAILED: &str = "The report could not be loaded. Please try again.";
    let range = match ctx.sandbox.report_range(
        q.get("from").map(String::as_str),
        q.get("to").map(String::as_str),
    ) {
        Ok(r) => r,
        Err(e) => return refused(e, FAILED),
    };
    match ctx.sandbox.cost_report(range).await {
        Ok(r) => ok(json!({"status": "success", "data": r})),
        Err(e) => refused(e, FAILED),
    }
}

/// GET /sandbox/commissions/export?from=&to=: every trade in the range with
/// its charges, as CSV.
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    const FAILED: &str = "The report could not be exported. Please try again.";
    let range = match ctx.sandbox.report_range(
        q.get("from").map(String::as_str),
        q.get("to").map(String::as_str),
    ) {
        Ok(r) => r,
        Err(e) => return refused(e, FAILED),
    };
    match ctx.sandbox.cost_report_trades(range).await {
        Ok(rows) if rows.is_empty() => json_response(
            StatusCode::NOT_FOUND,
            json!({"status": "error", "message": "There are no trades between those dates."}),
        ),
        Ok(rows) => download(
            cost_report_csv(&rows),
            "text/csv",
            &format!(
                "sandbox_transaction_costs_{}_to_{}.csv",
                range.from.format("%Y-%m-%d"),
                range.to.format("%Y-%m-%d")
            ),
        ),
        Err(e) => refused(e, FAILED),
    }
}
