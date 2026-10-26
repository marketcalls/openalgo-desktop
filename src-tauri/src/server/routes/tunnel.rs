//! The built-in tunnel's settings (Profile, Broker tab; `services::tunnel`):
//! `GET /api/tunnel` answers the switch, whether an authtoken is saved, the
//! reserved domain and the tunnel's status; `POST /api/tunnel` changes them
//! and applies the change at once. The authtoken is never sent back.

use super::{Access, RouteSpec};
use crate::error::AppError;
use crate::security::Secret;
use crate::server::envelope::error;
use crate::server::form::FormData;
use crate::server::routes::webui::{failed, ok};
use crate::services::tunnel::{self, TunnelChange, TunnelView};
use crate::state::AppState;
use axum::extract::State;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::Response;
use axum::routing::{get as get_route, post, MethodRouter};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The tunnel's routes, for the signed-in user.
pub fn table() -> Vec<RouteSpec> {
    let user = |path, method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
        path,
        method,
        access: Access::User,
        make,
    };
    vec![
        user("/api/tunnel", Method::GET, || get_route(get)),
        user("/api/tunnel", Method::POST, || post(save)),
    ]
}

/// GET /api/tunnel
pub async fn get(State(ctx): Ctx) -> Response {
    match tunnel::settings::view(&ctx).await {
        Ok(v) => reply(&v),
        Err(e) => failed(
            "Reading the tunnel settings",
            e,
            "OpenAlgo could not read the tunnel settings. Try again.",
        ),
    }
}

/// POST /api/tunnel (JSON or form): `enabled` ("true"/"false"),
/// `authtoken`, `forget_authtoken` ("true"), `domain` (empty clears it).
/// A field left out keeps what is saved.
pub async fn save(State(ctx): Ctx, form: FormData) -> Response {
    let yes = |k: &str| form.get(k).map(|v| v.trim().eq_ignore_ascii_case("true"));
    let change = TunnelChange {
        enabled: yes("enabled"),
        authtoken: form.non_empty("authtoken").map(Secret::new),
        forget_authtoken: yes("forget_authtoken").unwrap_or(false),
        domain: form.get("domain").map(str::to_string),
    };
    match tunnel::save(&ctx, change).await {
        Ok(v) => reply(&v),
        Err(AppError::Validation(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(e) => failed(
            "Saving the tunnel settings",
            e,
            "Your tunnel settings were not saved. Try again.",
        ),
    }
}

fn reply(view: &TunnelView) -> Response {
    let mut resp = ok(json!({"status": "success", "data": view}));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}
