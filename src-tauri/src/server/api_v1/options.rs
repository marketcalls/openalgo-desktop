//! Options analytics endpoints (flat responses, no `data` wrapper).

use super::{load, send, Auth, Style};
use crate::server::middleware::ClientIp;
use crate::services::options_service as options;
use crate::services::schemas;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const VALIDATION: Style = Style::Envelope("Validation error");
const GREEKS_VALIDATION: Style = Style::Envelope("Validation failed");

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// POST /api/v1/optionsymbol
pub async fn optionsymbol(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::option_symbol(),
        VALIDATION,
        Auth::Broker,
    )
    .await
    {
        Ok(v) => send(options::option_symbol(&ctx, &v).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/optionchain
pub async fn optionchain(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::option_chain(),
        VALIDATION,
        Auth::Broker,
    )
    .await
    {
        Ok(v) => send(
            options::option_chain(
                &ctx,
                &s(&v, "underlying"),
                &s(&v, "exchange"),
                &s(&v, "expiry_date"),
                v.get("strike_count").and_then(Value::as_i64),
                v.get("with_greeks")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                v.get("interest_rate").and_then(Value::as_f64),
            )
            .await,
        ),
        Err(r) => *r,
    }
}

/// POST /api/v1/syntheticfuture
pub async fn syntheticfuture(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::synthetic_future(),
        VALIDATION,
        Auth::Broker,
    )
    .await
    {
        Ok(v) => send(
            options::synthetic_future(
                &ctx,
                &s(&v, "underlying"),
                &s(&v, "exchange"),
                &s(&v, "expiry_date"),
            )
            .await,
        ),
        Err(r) => *r,
    }
}

/// POST /api/v1/optiongreeks
pub async fn optiongreeks(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::option_greeks(),
        GREEKS_VALIDATION,
        Auth::Broker401,
    )
    .await
    {
        Ok(v) => send(options::option_greeks(&ctx, &v).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/multioptiongreeks
pub async fn multioptiongreeks(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::multi_option_greeks(),
        GREEKS_VALIDATION,
        Auth::Broker401,
    )
    .await
    {
        Ok(v) => send(options::multi_option_greeks(&ctx, &v).await),
        Err(r) => *r,
    }
}
