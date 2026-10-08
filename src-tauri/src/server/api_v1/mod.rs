//! `/api/v1`: the external API (Python SDK, TradingView, Amibroker, Excel,
//! MCP), at web parity with OpenAlgo's `restx_api`.
//!
//! Every handler does the same three things: read the JSON body with the
//! web's framework errors ([`read_json_object`]), validate it with the web's
//! Marshmallow schema and answer schema failures in that endpoint's error
//! shape ([`Style`]), check the API key, then call one service and send its
//! reply unchanged. Business logic lives in `crate::services`.
//!
//! | File | Endpoints |
//! |---|---|
//! | `orders.rs` | placeorder, placesmartorder, modifyorder, cancelorder, cancelallorder, closeposition, basketorder, splitorder, optionsorder, optionsmultiorder |
//! | `account.rs` | ping, analyzer, analyzer/toggle, funds, orderbook, tradebook, positionbook, holdings, orderstatus, openposition, pnl/symbols |
//! | `data.rs` | quotes, multiquotes, depth, history, intervals, ticker, symbol, search, expiry, instruments, margin |
//! | `options.rs` | optionsymbol, optionchain, syntheticfuture, optiongreeks, multioptiongreeks |
//! | `gtt.rs` | placegttorder, modifygttorder, cancelgttorder, gttorderbook |
//!
//! `market/holidays` and `market/timings` are served by
//! `crate::server::routes::market_calendar` and mounted here; Telegram, WhatsApp, strategy, chart, portfolio and SIP endpoints
//! arrive with their own waves.

mod account;
mod data;
mod gtt;
mod notify;
mod options;
mod orders;
mod strategy;

use crate::events::{Event, GttKind, Mode};
use crate::server::envelope::{json_response, not_found, read_json_object};
use crate::server::ratelimit::Bucket;
use crate::services::apikey_service::ApiKeyService;
use crate::services::core::{is_analyze, meta, safe_request, Reply, INVALID_API_KEY};
use crate::services::schema::{FieldErrors, Schema};
use crate::state::AppState;
use axum::{
    extract::Request,
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Router,
};
use serde_json::{json, Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

pub use crate::services::account_service::funds_payload;

/// How an endpoint reports schema failures (the web has four shapes).
#[derive(Debug, Clone, Copy)]
pub enum Style {
    /// `{"status":"error","message":{field:[msgs]}}` (read endpoints).
    Object,
    /// `{"status":"error","message":"{'field': [...]}"}` (placeorder,
    /// analyzer, margin).
    Py,
    /// As `Py`, but in analyzer mode the body carries `"mode":"analyze"` and
    /// `analyzer.error` is published; live mode publishes `order.failed`.
    PyOrder(&'static str),
    /// As `PyOrder`, with the GTT failure topics in live mode.
    PyGtt(&'static str, GttKind),
    /// `{"status":"error","message":<text>,"errors":{...}}` (options).
    Envelope(&'static str),
}

/// What the API key must prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// A valid key and a live broker session (web `get_auth_token_broker`).
    Broker,
    /// A valid key only (web `verify_api_key`: search, expiry, instruments).
    KeyOnly,
    /// As `Broker`, but refused with 401 (the Greeks endpoints).
    Broker401,
}

/// Send a service reply as is.
pub fn send(r: Reply) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

fn schema_error(
    ctx: &AppState,
    style: Style,
    body: &Map<String, Value>,
    e: &FieldErrors,
) -> Response {
    match style {
        Style::Object => json_response(
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "message": e.to_json()}),
        ),
        Style::Py => json_response(
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "message": e.to_python()}),
        ),
        Style::Envelope(msg) => json_response(
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "message": msg, "errors": e.to_json()}),
        ),
        Style::PyOrder(api_type) | Style::PyGtt(api_type, _) => {
            let raw = Value::Object(body.clone());
            let msg = e.to_python();
            if is_analyze(ctx) {
                return send(crate::services::core::analyzer_error(
                    ctx, api_type, &raw, &msg, 400,
                ));
            }
            let reply = Reply::error(400, msg);
            let s = |k: &str| {
                body.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let event = match style {
                Style::PyGtt(_, kind) => Event::Gtt {
                    kind,
                    meta: meta(Mode::Live, api_type, safe_request(&raw), &reply.body),
                    symbol: s("symbol"),
                    exchange: s("exchange"),
                    trigger_id: s("trigger_id"),
                    triggered_order_id: String::new(),
                },
                _ => Event::OrderFailed {
                    meta: meta(Mode::Live, api_type, safe_request(&raw), &reply.body),
                    symbol: s("symbol"),
                    exchange: s("exchange"),
                    error_message: reply.message(),
                },
            };
            ctx.bus.publish(event);
            send(reply)
        }
    }
}

/// Web rule (`get_auth_token_broker`): a valid key with no live broker
/// session is reported as an invalid key. An address that keeps sending bad
/// keys stops being checked at all for a minute (same answer, no Argon2).
pub fn authorize(ctx: &AppState, key: &str, ip: IpAddr, auth: Auth) -> bool {
    let now = ctx.limiter.now();
    if ctx.limiter.is_exhausted(Bucket::ApiKeyFail, ip, now) {
        return false;
    }
    let ok =
        ApiKeyService::is_valid(ctx, key) && (auth == Auth::KeyOnly || ctx.is_broker_connected());
    if !ok {
        let _ = ctx.limiter.check(Bucket::ApiKeyFail, ip, now);
    }
    ok
}

fn auth_failure(auth: Auth) -> Response {
    let status = if auth == Auth::Broker401 {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::FORBIDDEN
    };
    json_response(
        status,
        json!({"status": "error", "message": INVALID_API_KEY}),
    )
}

/// Validate a body already read and check its key.
#[allow(clippy::result_large_err)]
pub fn check(
    ctx: &AppState,
    ip: IpAddr,
    body: &Map<String, Value>,
    schema: &Schema,
    style: Style,
    auth: Auth,
) -> Result<Value, Response> {
    let loaded = schema
        .load(body)
        .map_err(|e| schema_error(ctx, style, body, &e))?;
    let key = loaded
        .get("apikey")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !authorize(ctx, &key, ip, auth) {
        return Err(auth_failure(auth));
    }
    Ok(Value::Object(loaded))
}

/// Read the JSON body, validate it and check the key.
pub async fn load(
    ctx: &AppState,
    ip: IpAddr,
    req: Request,
    schema: &Schema,
    style: Style,
    auth: Auth,
) -> Result<Value, Response> {
    let body = read_json_object(req, &()).await?;
    check(ctx, ip, &body, schema, style, auth)
}

/// 404 JSON for unknown `/api/v1` paths and wrong methods (web contract:
/// wrong method is 404, not 405).
pub async fn api_not_found(req: Request) -> Response {
    not_found(req.uri().path())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // account
        .route("/api/v1/ping", post(account::ping))
        .route("/api/v1/analyzer", post(account::analyzer))
        .route("/api/v1/analyzer/toggle", post(account::analyzer_toggle))
        // market calendar (routes owned by the market-calendar module)
        .route(
            "/api/v1/market/holidays",
            post(crate::server::routes::market_calendar::holidays),
        )
        .route(
            "/api/v1/market/timings",
            post(crate::server::routes::market_calendar::timings),
        )
        .route("/api/v1/funds", post(account::funds))
        .route("/api/v1/orderbook", post(account::orderbook))
        .route("/api/v1/tradebook", post(account::tradebook))
        .route("/api/v1/positionbook", post(account::positionbook))
        .route("/api/v1/holdings", post(account::holdings))
        .route("/api/v1/orderstatus", post(account::orderstatus))
        .route("/api/v1/openposition", post(account::openposition))
        .route("/api/v1/pnl/symbols", post(account::pnl_symbols))
        // orders
        .route("/api/v1/placeorder", post(orders::placeorder))
        .route("/api/v1/placesmartorder", post(orders::placesmartorder))
        .route("/api/v1/modifyorder", post(orders::modifyorder))
        .route("/api/v1/cancelorder", post(orders::cancelorder))
        .route("/api/v1/cancelallorder", post(orders::cancelallorder))
        .route("/api/v1/closeposition", post(orders::closeposition))
        .route("/api/v1/basketorder", post(orders::basketorder))
        .route("/api/v1/splitorder", post(orders::splitorder))
        .route("/api/v1/optionsorder", post(orders::optionsorder))
        .route("/api/v1/optionsmultiorder", post(orders::optionsmultiorder))
        // GTT
        .route("/api/v1/placegttorder", post(gtt::placegttorder))
        .route("/api/v1/modifygttorder", post(gtt::modifygttorder))
        .route("/api/v1/cancelgttorder", post(gtt::cancelgttorder))
        .route("/api/v1/gttorderbook", post(gtt::gttorderbook))
        // market data and symbols
        .route("/api/v1/quotes", post(data::quotes))
        .route("/api/v1/multiquotes", post(data::multiquotes))
        .route("/api/v1/depth", post(data::depth))
        .route("/api/v1/history", post(data::history))
        .route("/api/v1/intervals", post(data::intervals))
        .route("/api/v1/ticker/{symbol}", get(data::ticker))
        .route("/api/v1/symbol", post(data::symbol))
        .route("/api/v1/search", post(data::search))
        .route("/api/v1/expiry", post(data::expiry))
        .route("/api/v1/instruments", get(data::instruments))
        .route("/api/v1/margin", post(data::margin))
        // options analytics
        .route("/api/v1/optionsymbol", post(options::optionsymbol))
        .route("/api/v1/optionchain", post(options::optionchain))
        .route("/api/v1/syntheticfuture", post(options::syntheticfuture))
        .route("/api/v1/optiongreeks", post(options::optiongreeks))
        .route(
            "/api/v1/multioptiongreeks",
            post(options::multioptiongreeks),
        )
        // Telegram and WhatsApp
        .route("/api/v1/telegram/notify", post(notify::telegram_notify))
        .route("/api/v1/whatsapp/notify", post(notify::whatsapp_notify))
        // strategy module (web restx_api/strategy.py)
        .route("/api/v1/strategy/list", post(strategy::list))
        .route("/api/v1/strategy/status", post(strategy::status))
        .route("/api/v1/strategy/start", post(strategy::start))
        .route("/api/v1/strategy/stop", post(strategy::stop))
        .route("/api/v1/strategy/close_all", post(strategy::close_all))
        .route("/api/v1/strategy/close_leg", post(strategy::close_leg))
        .route("/api/v1/strategy/runs", post(strategy::runs))
        .route("/api/v1/strategy/orders", post(strategy::orders))
        .route("/api/v1/strategy/events", post(strategy::events))
}
