//! The voice routes of the web's `blueprints/agent.py` (design 56): the
//! configuration and its write-only key, the key test, the session mint,
//! spoken approval and the spoken-line record. Each reads the request,
//! calls one service in `crate::agent::voice` and shapes the reply.
//!
//! The mint takes the SDP offer and nothing else: no model, voice, URL or
//! instructions from the request. It counts against the streaming limit (a
//! mint is a billed call, like opening a turn); the key test against the
//! provider-test limit; the rest against the ordinary one.

use super::MAX_REQUEST_BYTES;
use super::{answer, refused, AgentBody, ApiCaller, Caller, StreamCaller, TestCaller};
use crate::agent::models::{probe::MAX_TEST_ERROR_CHARS, success, text_field};
use crate::agent::reply::Refusal;
use crate::agent::voice::approval::Rules;
use crate::agent::voice::config::{self, VoiceSettings};
use crate::agent::voice::mint::{self, MintError};
use crate::agent::voice::transcript;
use crate::server::routes::{Access, RouteSpec};
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{delete, get, post, put, MethodRouter},
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

type Ctx = State<Arc<AppState>>;

/// The largest SDP offer read (the web's `MAX_SDP_BYTES`): an audio offer
/// with a data channel is a few kilobytes.
pub const MAX_SDP_BYTES: usize = 256_000;

const OFFER_TOO_LARGE: &str = "That connection offer is too large";

/// Run a settings service on the blocking pool and answer.
async fn blocking<F>(ctx: &Arc<AppState>, f: F) -> Response
where
    F: FnOnce(&AppState) -> Result<Value, Refusal> + Send + 'static,
{
    let r = ctx.blocking(move |c| Ok(f(c))).await.unwrap_or_else(|e| {
        tracing::error!("A voice settings request failed: {}", e);
        Err(Refusal::new(500, "Something went wrong. Try again.", None))
    });
    answer(r, StatusCode::OK)
}

/// `GET /agent/api/voice`.
pub async fn get_voice(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, config::voice_answer).await
}

/// `PUT /agent/api/voice`.
pub async fn put_voice(State(ctx): Ctx, Caller(_): ApiCaller, AgentBody(b): AgentBody) -> Response {
    blocking(&ctx, move |c| config::update_answer(c, &b)).await
}

/// `PUT /agent/api/voice/key` with `{api_key}`.
pub async fn put_key(State(ctx): Ctx, Caller(_): ApiCaller, AgentBody(b): AgentBody) -> Response {
    blocking(&ctx, move |c| config::set_key_answer(c, &b)).await
}

/// `DELETE /agent/api/voice/key`.
pub async fn delete_key(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    blocking(&ctx, config::clear_key_answer).await
}

/// `POST /agent/api/voice/test`: mint one throwaway session (works with
/// voice switched off).
pub async fn test_key(State(ctx): Ctx, Caller(_): TestCaller) -> Response {
    let Ok(result) = mint::probe(&ctx).await else {
        tracing::error!("The voice test could not be run");
        return refused(Refusal::new(500, "Could not run the voice test", None));
    };
    let message: String = result.message.chars().take(MAX_TEST_ERROR_CHARS).collect();
    if result.ok {
        tracing::info!("Voice provider passed its test in {}ms", result.latency_ms);
    } else {
        tracing::error!("Voice provider failed its test: {}", message);
    }
    let fields = json!({
        "ok": result.ok, "message": message,
        "latency_ms": result.latency_ms, "model": result.model,
    });
    blocking(&ctx, move |c| {
        let mut payload = fields;
        payload["data"] = config::config_json(c).unwrap_or(Value::Null);
        Ok(success(payload))
    })
    .await
}

/// `POST /agent/api/voice/transcript`: record one spoken line; never fails.
pub async fn post_transcript(
    State(ctx): Ctx,
    Caller(c): ApiCaller,
    AgentBody(b): AgentBody,
) -> Response {
    answer(
        Ok(transcript::record(&ctx, &c.user, &b).await),
        StatusCode::OK,
    )
}

/// `POST /agent/api/voice/approve` with `{run_id, transcript}`: decided
/// here; a refusal is a 200 with `approved: false` and its reason.
pub async fn post_approve(
    State(ctx): Ctx,
    Caller(_): ApiCaller,
    AgentBody(b): AgentBody,
) -> Response {
    let run_id = text_field(&b, "run_id");
    if run_id.is_empty() {
        return refused(Refusal::bad_request("A run_id is required"));
    }
    let heard = text_field(&b, "transcript");
    let read = ctx.blocking(|c| Ok(VoiceSettings::read(c))).await;
    // An unreadable setting approves nothing.
    let rules = read.map_or(
        Rules {
            trading_effective: false,
            window_seconds: 0,
        },
        |v| Rules {
            trading_effective: v.trading_effective(),
            window_seconds: v.confirm_window_seconds,
        },
    );
    let verdict = ctx
        .agent
        .voice()
        .windows
        .judge(&run_id, &heard, rules, Instant::now());
    let data = json!({"approved": verdict.approved, "reason": verdict.reason});
    answer(Ok(success(json!({ "data": data }))), StatusCode::OK)
}

/// The offer in a mint request: refused as too large by its declared
/// length before anything is read, then by what arrives; blank is refused.
async fn sdp_offer(req: Request) -> Result<String, Refusal> {
    let too_large = || Refusal::new(413, OFFER_TOO_LARGE, None);
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_SDP_BYTES) {
        return Err(too_large());
    }
    let bytes = axum::body::to_bytes(req.into_body(), MAX_SDP_BYTES)
        .await
        .map_err(|_| too_large())?;
    let offer = String::from_utf8_lossy(&bytes).into_owned();
    if offer.trim().is_empty() {
        return Err(Refusal::bad_request("A connection offer is required"));
    }
    Ok(offer)
}

/// `POST /agent/api/voice/session`: the SDP offer in, the provider's SDP
/// answer out (`application/sdp`), or the web's JSON error.
pub async fn post_session(State(ctx): Ctx, Caller(_): StreamCaller, req: Request) -> Response {
    let offer = match sdp_offer(req).await {
        Ok(o) => o,
        Err(r) => return refused(r),
    };
    match mint::mint_session(&ctx, &offer, false).await {
        Ok(answer) => {
            let mut r = Response::new(Body::from(answer));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/sdp"),
            );
            r
        }
        Err(MintError::Unavailable(m)) => refused(Refusal::bad_request(m)),
        Err(MintError::Internal) => {
            refused(Refusal::new(500, "Could not start the voice session", None))
        }
    }
}

/// The eight voice route-methods, as the web's paths.
pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    vec![
        spec("/agent/api/voice", Method::GET, || get(get_voice)),
        spec("/agent/api/voice", Method::PUT, || {
            put(put_voice).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/voice/key", Method::PUT, || {
            put(put_key).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/voice/key", Method::DELETE, || {
            delete(delete_key)
        }),
        spec("/agent/api/voice/test", Method::POST, || post(test_key)),
        spec("/agent/api/voice/transcript", Method::POST, || {
            post(post_transcript).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/voice/approve", Method::POST, || {
            post(post_approve).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        // The offer's own limit is read in `sdp_offer`, with the web's 413.
        spec("/agent/api/voice/session", Method::POST, || {
            post(post_session)
        }),
    ]
}
