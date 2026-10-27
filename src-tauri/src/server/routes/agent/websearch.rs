//! The web search routes (the web search part of the web's
//! `blueprints/agent.py`): the configuration, each provider's key (write
//! only), and a provider's live test (12 a minute: it reaches the
//! provider). A route naming a provider resolves it before anything else.

use super::chatgpt::optional_json;
use super::{answer, refused, AgentBody, ApiCaller, Caller, TestCaller, MAX_REQUEST_BYTES};
use crate::agent::reply::Refusal;
use crate::agent::websearch::service;
use crate::server::routes::{Access, RouteSpec};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{delete, get, post, put, MethodRouter},
};
use serde_json::{Map, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// Run a service that reads the database on the blocking pool.
async fn blocking<F>(ctx: &Arc<AppState>, f: F) -> Response
where
    F: FnOnce(&AppState) -> Result<Value, Refusal> + Send + 'static,
{
    let r = ctx.blocking(move |c| Ok(f(c))).await.unwrap_or_else(|e| {
        tracing::error!("A web search settings request failed: {}", e);
        Err(Refusal::new(500, "Something went wrong. Try again.", None))
    });
    answer(r, StatusCode::OK)
}

/// The body a key write needs, refused as the web refuses it.
fn required_json(bytes: &Bytes) -> Result<Map<String, Value>, Refusal> {
    match optional_json(bytes) {
        None => Err(Refusal::bad_request("A JSON body is required")),
        Some(Value::Object(m)) => Ok(m),
        Some(_) => Err(Refusal::bad_request(
            "The request body must be a JSON object",
        )),
    }
}

/// `GET /agent/api/websearch`.
pub async fn get_config(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, service::get).await
}

/// `PUT /agent/api/websearch`.
pub async fn put_config(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    AgentBody(body): AgentBody,
) -> Response {
    blocking(&ctx, move |c| service::put(c, &body)).await
}

/// `PUT /agent/api/websearch/providers/<provider>/key` with `{api_key}`.
pub async fn put_key(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    Path(provider): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(r) = service::checked_provider(&provider) {
        return refused(r);
    }
    match required_json(&body) {
        Ok(m) => blocking(&ctx, move |c| service::put_key(c, &provider, &m)).await,
        Err(r) => refused(r),
    }
}

/// `DELETE /agent/api/websearch/providers/<provider>/key`.
pub async fn delete_key(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    Path(provider): Path<String>,
) -> Response {
    blocking(&ctx, move |c| service::delete_key(c, &provider)).await
}

/// `POST /agent/api/websearch/providers/<provider>/test`, optionally with
/// a typed key to test before it is saved.
pub async fn test(
    State(ctx): Ctx,
    Caller(_): TestCaller,
    Path(provider): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(r) = service::checked_provider(&provider) {
        return refused(r);
    }
    match service::typed_key(optional_json(&body).as_ref()) {
        Ok(typed) => answer(service::test(&ctx, &provider, typed).await, StatusCode::OK),
        Err(r) => refused(r),
    }
}

pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    const KEY: &str = "/agent/api/websearch/providers/{provider}/key";
    vec![
        spec("/agent/api/websearch", Method::GET, || get(get_config)),
        spec("/agent/api/websearch", Method::PUT, || {
            put(put_config).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec(KEY, Method::PUT, || {
            put(put_key).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec(KEY, Method::DELETE, || delete(delete_key)),
        spec(
            "/agent/api/websearch/providers/{provider}/test",
            Method::POST,
            || post(test).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES)),
        ),
    ]
}
