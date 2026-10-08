//! Web `blueprints/custom_indicators.py`: the trader's own chart indicator
//! modules, served from the `indicators/` folder of the app data directory to
//! the /trading chart, which `import()`s each one at runtime.
//!
//! **Never bundled.** The interface is built from the repository, so a
//! bundled indicator would have to be committed and an upgrade would erase
//! it; served at runtime it needs no rebuild and survives upgrades.
//!
//! **These modules run with the app's privileges.** A module is JavaScript
//! the page imports, so it executes on the app's own origin with the signed-in
//! session and can call every page route and `/api/v1`, exactly as on the web.
//! An indicator from an untrusted source is as dangerous as any program the
//! trader runs. What is enforced here: the signed-in user, plain `.js` names
//! only (no separator, no dot segment), regular files inside the folder only,
//! a size bound, and no listing beyond the index of those names.

use super::{Access, RouteSpec};
use crate::server::envelope::json_response;
use crate::state::AppState;
use crate::trading::indicators::{self, Fetch};
use crate::trading::names::is_indicator_name;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

pub fn table() -> Vec<RouteSpec> {
    vec![
        RouteSpec {
            path: "/custom-indicators/index.json",
            method: Method::GET,
            access: Access::User,
            make: || get(index),
        },
        RouteSpec {
            path: "/custom-indicators/{file}",
            method: Method::GET,
            access: Access::User,
            make: || get(module),
        },
    ]
}

fn no_cache(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// The trader's modules, by name, with each one's modification time so the
/// chart can version its import URL.
pub async fn index(State(ctx): Ctx) -> Response {
    let dir = ctx.trading.indicators_dir.clone();
    let list = tokio::task::spawn_blocking(move || indicators::list(&dir))
        .await
        .unwrap_or_default();
    no_cache(json_response(StatusCode::OK, json!(list)))
}

/// One module as an ES module. A versioned request (`?v=<mtime>`) is cached
/// for good, since an edit changes the version; an unversioned one is not.
pub async fn module(
    State(ctx): Ctx,
    Path(file): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !is_indicator_name(&file) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid indicator filename"}),
        );
    }
    let dir = ctx.trading.indicators_dir.clone();
    let name = file.clone();
    let fetched = tokio::task::spawn_blocking(move || indicators::read(&dir, &name))
        .await
        .unwrap_or(Fetch::Missing);
    match fetched {
        Fetch::Module(bytes) => {
            let mut r = Body::from(bytes).into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            );
            let cache = if q.get("v").is_some_and(|v| !v.is_empty()) {
                "private, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            r
        }
        Fetch::NoFolder => json_response(
            StatusCode::NOT_FOUND,
            json!({"error": "No indicators directory"}),
        ),
        Fetch::Missing => json_response(StatusCode::NOT_FOUND, json!({"error": "Not found"})),
        Fetch::TooLarge => json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"error": "This indicator file is too large to load"}),
        ),
    }
}
