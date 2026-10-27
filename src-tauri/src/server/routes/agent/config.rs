//! The config part of the web's `blueprints/agent.py`: the setup gate
//! (`/status`), the model catalogue, the model registry and its credential
//! test, the Agent's settings, and the desktop's kill switch route behind
//! the Trading safety panel. Each handler reads the request, calls one
//! service in `crate::agent` and shapes the reply; the services read the
//! database off the async runtime.

use super::{answer, path_id, AgentBody, ApiCaller, Caller, TestCaller, MAX_REQUEST_BYTES};
use crate::agent::reply::Refusal;
use crate::agent::{catalog, models, settings, setup_gate};
use crate::server::routes::{Access, RouteSpec};
use crate::state::AppState;
use axum::{
    extract::{DefaultBodyLimit, OriginalUri, Path, Query, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{delete, get, patch, post, put, MethodRouter},
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// Run a service that reads the database on the blocking pool and answer.
async fn blocking<F>(ctx: &Arc<AppState>, ok: StatusCode, f: F) -> Response
where
    F: FnOnce(&AppState) -> Result<Value, Refusal> + Send + 'static,
{
    let r = ctx.blocking(move |c| Ok(f(c))).await.unwrap_or_else(|e| {
        tracing::error!("An Agent settings request failed: {}", e);
        Err(Refusal::new(500, "Something went wrong. Try again.", None))
    });
    answer(r, ok)
}

/// `GET /agent/api/status`: the setup gate.
pub async fn status(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, StatusCode::OK, |c| Ok(setup_gate::status(c))).await
}

/// `GET /agent/api/catalog/providers`.
pub async fn catalog_providers(Caller(_): ApiCaller) -> Response {
    answer(Ok(catalog::providers_answer()), StatusCode::OK)
}

/// `GET /agent/api/catalog/models?provider=&chat_only=`.
pub async fn catalog_models(
    Caller(_): ApiCaller,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let r = catalog::models_answer(
        q.get("provider").map(String::as_str),
        q.get("chat_only").map(String::as_str),
    );
    answer(r, StatusCode::OK)
}

/// `GET /agent/api/models`.
pub async fn list_models(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, StatusCode::OK, models::service::list).await
}

/// `POST /agent/api/models` (201).
pub async fn create_model(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    AgentBody(body): AgentBody,
) -> Response {
    blocking(&ctx, StatusCode::CREATED, move |c| {
        models::service::create(c, &body)
    })
    .await
}

/// `PATCH /agent/api/models/<id>`.
pub async fn update_model(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    AgentBody(body): AgentBody,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => {
            blocking(&ctx, StatusCode::OK, move |c| {
                models::service::update(c, id, &body)
            })
            .await
        }
        Err(r) => *r,
    }
}

/// `DELETE /agent/api/models/<id>`.
pub async fn delete_model(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => {
            blocking(&ctx, StatusCode::OK, move |c| {
                models::service::delete(c, id)
            })
            .await
        }
        Err(r) => *r,
    }
}

/// `POST /agent/api/models/<id>/default`.
pub async fn set_default_model(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => {
            blocking(&ctx, StatusCode::OK, move |c| {
                models::service::set_default(c, id)
            })
            .await
        }
        Err(r) => *r,
    }
}

/// `POST /agent/api/models/<id>/test`: one capped request to the provider
/// (12 a minute, the web's `agent_test` limit).
pub async fn test_model(
    State(ctx): Ctx,
    Caller(_): TestCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => answer(models::probe::test(&ctx, id).await, StatusCode::OK),
        Err(r) => *r,
    }
}

/// `GET /agent/api/settings`: every setting and its default.
pub async fn get_settings(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, StatusCode::OK, |c| Ok(settings::settings_answer(c))).await
}

/// `PUT /agent/api/settings`: a partial update, all or nothing.
pub async fn put_settings(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    AgentBody(body): AgentBody,
) -> Response {
    blocking(&ctx, StatusCode::OK, move |c| {
        settings::update_answer(c, &body)
    })
    .await
}

/// `GET /agent/api/kill-switch` (desktop): both halves of the switch.
pub async fn get_kill_switch(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, StatusCode::OK, |c| {
        Ok(settings::kill_switch_answer(c))
    })
    .await
}

/// `PUT /agent/api/kill-switch` (desktop) with `{"engaged": bool}`.
pub async fn put_kill_switch(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    AgentBody(body): AgentBody,
) -> Response {
    blocking(&ctx, StatusCode::OK, move |c| {
        settings::set_kill_switch_answer(c, &body)
    })
    .await
}

/// The configuration routes, as the web's paths and methods (plus the
/// desktop's kill switch route).
pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    vec![
        spec("/agent/api/status", Method::GET, || get(status)),
        spec("/agent/api/catalog/providers", Method::GET, || {
            get(catalog_providers)
        }),
        spec("/agent/api/catalog/models", Method::GET, || {
            get(catalog_models)
        }),
        spec("/agent/api/models", Method::GET, || get(list_models)),
        spec("/agent/api/models", Method::POST, || {
            post(create_model).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/models/{id}", Method::PATCH, || {
            patch(update_model).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/models/{id}", Method::DELETE, || {
            delete(delete_model)
        }),
        spec("/agent/api/models/{id}/test", Method::POST, || {
            post(test_model)
        }),
        spec("/agent/api/models/{id}/default", Method::POST, || {
            post(set_default_model)
        }),
        spec("/agent/api/settings", Method::GET, || get(get_settings)),
        spec("/agent/api/settings", Method::PUT, || {
            put(put_settings).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/kill-switch", Method::GET, || {
            get(get_kill_switch)
        }),
        spec("/agent/api/kill-switch", Method::PUT, || {
            put(put_kill_switch).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
    ]
}
