//! Account endpoints: ping, analyzer, funds, books, orderstatus,
//! openposition, pnl/symbols.

use super::{load, send, Auth, Style};
use crate::server::envelope::{json_response, read_json_object};
use crate::server::middleware::ClientIp;
use crate::services::account_service as account;
use crate::services::core::{analyze_mode, Reply};
use crate::services::schemas;
use crate::services::AnalyzerService;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// POST /api/v1/ping
pub async fn ping(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    if let Err(r) = load(
        &ctx,
        ip,
        req,
        schemas::apikey_only(),
        Style::Object,
        Auth::Broker,
    )
    .await
    {
        return r;
    }
    let broker = ctx
        .get_broker_session()
        .map(|b| b.broker_id)
        .unwrap_or_default();
    send(Reply::ok(
        json!({"status": "success", "data": {"message": "pong", "broker": broker}}),
    ))
}

/// POST /api/v1/analyzer
pub async fn analyzer(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    if let Err(r) = load(
        &ctx,
        ip,
        req,
        schemas::apikey_only(),
        Style::Py,
        Auth::Broker,
    )
    .await
    {
        return r;
    }
    send(AnalyzerService::status_reply(&ctx))
}

/// POST /api/v1/analyzer/toggle
pub async fn analyzer_toggle(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let v = match load(
        &ctx,
        ip,
        req,
        schemas::analyzer_toggle(),
        Style::Py,
        Auth::Broker,
    )
    .await
    {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mode = v.get("mode").and_then(Value::as_bool).unwrap_or(false);
    send(AnalyzerService::toggle_reply(&ctx, mode).await)
}

macro_rules! book {
    ($name:ident, $svc:path) => {
        pub async fn $name(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
            if let Err(r) = load(
                &ctx,
                ip,
                req,
                schemas::apikey_only(),
                Style::Object,
                Auth::Broker,
            )
            .await
            {
                return r;
            }
            send($svc(&ctx).await)
        }
    };
}

book!(funds, account::funds);
book!(orderbook, account::orderbook);
book!(tradebook, account::tradebook);
book!(positionbook, account::positionbook);
book!(holdings, account::holdings);

/// POST /api/v1/orderstatus
pub async fn orderstatus(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let style = Style::PyOrder("orderstatus");
    match load(&ctx, ip, req, schemas::order_status(), style, Auth::Broker).await {
        Ok(v) => send(account::order_status(&ctx, &v).await),
        Err(r) => r,
    }
}

/// POST /api/v1/openposition
pub async fn openposition(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let style = Style::PyOrder("openposition");
    match load(&ctx, ip, req, schemas::open_position(), style, Auth::Broker).await {
        Ok(v) => send(account::open_position(&ctx, &v).await),
        Err(r) => r,
    }
}

/// POST /api/v1/pnl/symbols: sandbox only. The mode is checked before the
/// body, as on the web; the key is checked without a broker session.
pub async fn pnl_symbols(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match analyze_mode(&ctx) {
        Ok(true) => {}
        Ok(false) => return send(Reply::error(400, account::PNL_LIVE_MESSAGE)),
        Err(r) => return send(r),
    }
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let loaded = match schemas::apikey_only().load(&body) {
        Ok(l) => l,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": e.to_json()}),
            )
        }
    };
    let key = loaded
        .get("apikey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !super::authorize(&ctx, key, ip, Auth::KeyOnly) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({"status": "error", "message": "Invalid API key", "mode": "analyze"}),
        );
    }
    send(account::pnl_symbols(&ctx).await)
}
