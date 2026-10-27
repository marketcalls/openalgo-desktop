//! GTT endpoints (order bucket for writes).

use super::{check, load, send, Auth, Style};
use crate::events::GttKind;
use crate::server::envelope::read_json_object;
use crate::server::middleware::ClientIp;
use crate::services::gtt_service as gtt;
use crate::services::schemas;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    response::Response,
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// POST /api/v1/placegttorder
pub async fn placegttorder(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let mut body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    schemas::gtt_pre_load(&mut body);
    let style = Style::PyGtt("placegttorder", GttKind::Failed);
    match check(&ctx, ip, &body, schemas::place_gtt(), style, Auth::Broker).await {
        Ok(v) => send(gtt::place_gtt(&ctx, &v).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/modifygttorder
pub async fn modifygttorder(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let mut body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    schemas::gtt_pre_load(&mut body);
    let style = Style::PyGtt("modifygttorder", GttKind::ModifyFailed);
    match check(&ctx, ip, &body, schemas::modify_gtt(), style, Auth::Broker).await {
        Ok(v) => send(gtt::modify_gtt(&ctx, &v).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/cancelgttorder
pub async fn cancelgttorder(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let style = Style::PyGtt("cancelgttorder", GttKind::CancelFailed);
    match load(&ctx, ip, req, schemas::cancel_gtt(), style, Auth::Broker).await {
        Ok(v) => send(gtt::cancel_gtt(&ctx, &v).await),
        Err(r) => *r,
    }
}

/// POST /api/v1/gttorderbook
pub async fn gttorderbook(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(
        &ctx,
        ip,
        req,
        schemas::gtt_orderbook(),
        Style::Object,
        Auth::Broker,
    )
    .await
    {
        Ok(v) => {
            let status = v.get("status").and_then(Value::as_str).unwrap_or("active");
            send(gtt::gtt_orderbook(&ctx, status).await)
        }
        Err(r) => *r,
    }
}
