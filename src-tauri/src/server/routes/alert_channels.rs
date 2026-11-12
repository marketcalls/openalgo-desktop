//! Web `blueprints/alert_channels.py`: the /alerts page's API, every alert
//! channel, its switch and its routing. Signed-in user and CSRF on writes,
//! like every other page route; external callers use
//! `/api/v1/slack/notify` and `/api/v1/discord/notify`, which can only post.
//! The work is in `messaging::channels::page`.

use super::RouteSpec;
use crate::messaging::channels::page;
use crate::server::envelope::is_json;
use crate::server::middleware::ClientIp;
use crate::server::ratelimit::{Admission, Bucket};
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{FromRequest, Path, Request, State},
    response::Response,
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn send(r: Reply) -> Response {
    crate::server::api_v1::send(r)
}

/// The body as JSON, or `None` for anything else (Flask's
/// `get_json(silent=True)`): the page answers that with its own sentence.
async fn json_body(req: Request) -> Option<Value> {
    if !is_json(&req) {
        return None;
    }
    let bytes = Bytes::from_request(req, &()).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// GET /alerts/channels
pub async fn list(State(ctx): Ctx) -> Response {
    send(page::list(&ctx).await)
}

/// POST /alerts/channels/{name}
pub async fn update(State(ctx): Ctx, Path(name): Path<String>, req: Request) -> Response {
    send(page::update(&ctx, &name, json_body(req).await).await)
}

/// PUT /alerts/channels/{name}/webhook
pub async fn save_webhook(State(ctx): Ctx, Path(name): Path<String>, req: Request) -> Response {
    send(page::save_webhook(&ctx, &name, json_body(req).await).await)
}

/// DELETE /alerts/channels/{name}/webhook
pub async fn forget_webhook(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    send(page::forget_webhook(&ctx, &name).await)
}

/// POST /alerts/channels/{name}/test, five a minute (the web's
/// `TEST_RATE_LIMIT`): a test is a real send to a real account. Over the
/// limit the page is told so in a sentence; the web redirects to its
/// rate-limited page, which a page's request cannot show.
pub async fn send_test(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    Path(name): Path<String>,
) -> Response {
    let admitted = ctx.limiter.admit(Bucket::AlertTest, ip, ctx.limiter.now());
    if !matches!(admitted, Admission::Allowed) {
        return send(Reply::error(
            429,
            "Too many test messages in a minute. Wait a minute, then send another.",
        ));
    }
    send(page::send_test(&ctx, &name).await)
}

/// The /alerts page routes, each with its access rule. The chart's alert
/// log shares the `/alerts` prefix (`watchlist`); neither serves the bare
/// path, which is the page.
pub fn table() -> Vec<RouteSpec> {
    vec![
        r!(GET, "/alerts/channels", User, list),
        r!(POST, "/alerts/channels/{name}", User, update),
        r!(PUT, "/alerts/channels/{name}/webhook", User, save_webhook),
        r!(
            DELETE,
            "/alerts/channels/{name}/webhook",
            User,
            forget_webhook
        ),
        r!(POST, "/alerts/channels/{name}/test", User, send_test),
    ]
}
