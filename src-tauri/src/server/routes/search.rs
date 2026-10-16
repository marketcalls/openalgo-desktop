//! Web `blueprints/search.py`: symbol search, expiries and underlyings for
//! the pages, over the shared symbol master.

use crate::server::routes::webui::ok;
use crate::services::search_ui_service::{self as svc, FnoFilter};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    response::Response,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

fn arg<'a>(q: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty())
}

/// GET /search/api/search
pub async fn api_search(State(ctx): Ctx, Query(q): Q) -> Response {
    let strike = |k: &str| arg(&q, k).and_then(|s| s.parse::<f64>().ok());
    let filter = FnoFilter {
        query: arg(&q, "q"),
        expiry: arg(&q, "expiry"),
        underlying: arg(&q, "underlying"),
        strike_min: strike("strike_min"),
        strike_max: strike("strike_max"),
        ..Default::default()
    };
    let exchanges = svc::parse_multi(q.get("exchange").map(String::as_str));
    let insts = svc::parse_multi(q.get("instrumenttype").map(String::as_str));
    let snap = ctx.symbols.snapshot();
    ok(svc::api_search_with(
        snap.rows(),
        &filter,
        &exchanges,
        &insts,
        &|r| snap.contract_value(&r.exchange, &r.token),
        &ctx.freeze,
    ))
}

/// The answer while no master contract is loaded (none downloaded yet, a
/// download still running, or no broker connected): an expiry list would
/// look empty for every underlying, which is not what is wrong.
pub const MASTER_NOT_READY: &str = "Expiry dates are not available until the master contract is loaded. Open Master Contract to download it, then retry.";

/// GET /search/api/expiries: the live expiries (web `api_expiries`). With
/// no master loaded the answer says so (`master_ready: false`), so the page
/// can tell that apart from an underlying with no live expiry.
pub async fn api_expiries(State(ctx): Ctx, Query(q): Q) -> Response {
    let snap = ctx.symbols.snapshot();
    if snap.is_empty() {
        return ok(json!({
            "status": "error",
            "message": MASTER_NOT_READY,
            "master_ready": false,
            "expiries": [],
        }));
    }
    let expiries = svc::expiries(
        snap.rows(),
        arg(&q, "exchange"),
        arg(&q, "underlying"),
        arg(&q, "instrumenttype"),
        ctx.now(),
    );
    ok(json!({"status": "success", "expiries": expiries}))
}

/// GET /search/api/underlyings
pub async fn api_underlyings(State(ctx): Ctx, Query(q): Q) -> Response {
    let include_futures = matches!(
        arg(&q, "include_futures").map(str::to_lowercase).as_deref(),
        Some("1" | "true" | "yes")
    );
    let snap = ctx.symbols.snapshot();
    let list = svc::underlyings(snap.rows(), arg(&q, "exchange"), include_futures, ctx.now());
    ok(json!({"status": "success", "underlyings": list}))
}
