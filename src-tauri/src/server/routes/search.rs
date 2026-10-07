//! Web `blueprints/search.py`: symbol search, expiries and underlyings for
//! the pages, over the shared symbol master.

use crate::server::routes::webui::ok;
use crate::services::options_service::today_ist;
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
    ))
}

/// GET /search/api/expiries
pub async fn api_expiries(State(ctx): Ctx, Query(q): Q) -> Response {
    let snap = ctx.symbols.snapshot();
    let expiries = svc::expiries(
        snap.rows(),
        arg(&q, "exchange"),
        arg(&q, "underlying"),
        arg(&q, "instrumenttype"),
        today_ist(&ctx),
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
    let list = svc::underlyings(
        snap.rows(),
        arg(&q, "exchange"),
        include_futures,
        today_ist(&ctx),
    );
    ok(json!({"status": "success", "underlyings": list}))
}
