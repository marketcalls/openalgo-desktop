//! A workflow's webhook: its settings (the session routes under
//! `/flow/api/workflows/<id>/webhook`) and the public trigger
//! (`POST /flow/webhook/<token>` and `/flow/webhook/<token>/<symbol>`), as
//! the web's `blueprints/flow.py` 796-1201 answers them.
//!
//! The token in the path names the workflow; the secret proves the caller,
//! in the JSON field `secret` (auth type `payload`, the default) or in
//! `?secret=` (auth type `url`). Departures from the web, each decided:
//!
//! - A secret is always required (decisions.md 13, R-52): every workflow
//!   gets one when it is made, so the token alone never runs it (the web
//!   ran a workflow with an empty secret on its token alone).
//! - A secret that is not text is a wrong secret, not a 500 (R-52).
//! - Wrong secrets lock the webhook (decisions.md 13, `webhook_guard`).
//! - Settings routes answer 404 for an unknown workflow (R-41, R-42, R-45;
//!   the web answered 500 or 400), and the auth-type route reads a missing
//!   body as the default (the web answered 500).
//! - The addresses are built from the Host Server address (the tunnel's
//!   while it is on), the same source as the strategy and Chartink webhook
//!   addresses (D-16, T-110); the editor shows the live tunnel address
//!   while the tunnel is online.

use super::{api_key, execution_blocked, load, run_status, workflow_not_found, Body};
use crate::flow::contract::model::Workflow;
use crate::flow::contract::node::RunTrigger;
use crate::flow::contract::pyfmt;
use crate::flow::executor::{self, RunRequest};
use crate::flow::store::workflows;
use crate::flow::triggers::runs;
use crate::services::core::Reply;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The base of the webhook addresses: the Host Server address (pinned to
/// the tunnel's while it is on), else this computer's.
fn base(app: &AppState) -> String {
    match app.server_config().host_server {
        Some(h) => h.trim_end_matches('/').to_string(),
        None => format!("http://127.0.0.1:{}", app.listening_port()),
    }
}

/// The webhook settings as the web answers them.
fn settings(app: &AppState, w: &Workflow) -> Map<String, Value> {
    let token = w.webhook_token.clone().unwrap_or_default();
    let secret = w.webhook_secret.as_ref().map(|s| s.expose().to_string());
    let url = format!("{}/flow/webhook/{}", base(app), token);
    let mut m = Map::new();
    m.insert("webhook_token".into(), json!(w.webhook_token));
    m.insert("webhook_secret".into(), json!(secret));
    m.insert("webhook_enabled".into(), json!(w.webhook_enabled));
    m.insert("webhook_auth_type".into(), json!(w.webhook_auth_type));
    m.insert("webhook_url".into(), json!(url));
    m.insert(
        "webhook_url_with_symbol".into(),
        json!(format!("{}/{{symbol}}", url)),
    );
    m.insert(
        "webhook_url_with_secret".into(),
        match (&secret, w.webhook_auth_type.as_str()) {
            (Some(s), "url") => json!(format!("{}?secret={}", url, s)),
            _ => Value::Null,
        },
    );
    m
}

fn failed(what: &str, e: crate::error::AppError) -> Reply {
    tracing::error!("Flow could not {}: {}", what, e);
    Reply::new(500, json!({"error": format!("Failed to {}", what)}))
}

/// The workflow, after making a missing token or secret.
async fn with_credentials(app: &Arc<AppState>, id: i64) -> crate::error::Result<Option<Workflow>> {
    let ensured = app
        .blocking(move |c| {
            workflows::ensure_webhook_credentials(&*c.sqlite.conn()?, &c.security, id, c.now())
        })
        .await?;
    if !ensured {
        return Ok(None);
    }
    load(app, id).await
}

/// `GET /flow/api/workflows/<id>/webhook`.
pub async fn info(app: &Arc<AppState>, id: i64) -> Reply {
    match with_credentials(app, id).await {
        Ok(Some(w)) => Reply::new(200, Value::Object(settings(app, &w))),
        Ok(None) => workflow_not_found(),
        Err(e) => failed("read the webhook settings", e),
    }
}

/// Turn the webhook on or off.
async fn switch(app: &Arc<AppState>, id: i64, on: bool) -> crate::error::Result<bool> {
    let now = app.now();
    let done = app
        .blocking(move |c| workflows::set_webhook_enabled(&*c.sqlite.conn()?, id, on, now))
        .await?;
    // Off and on again is how the trader unlocks a locked webhook.
    app.flow.webhook_guard.unlock(id);
    Ok(done)
}

/// `POST /flow/api/workflows/<id>/webhook/enable`.
pub async fn enable(app: &Arc<AppState>, id: i64) -> Reply {
    if !matches!(load(app, id).await, Ok(Some(_))) {
        return workflow_not_found();
    }
    let enabled = match with_credentials(app, id).await {
        Ok(Some(_)) => switch(app, id, true).await,
        Ok(None) => return workflow_not_found(),
        Err(e) => Err(e),
    };
    match (enabled, load(app, id).await) {
        (Ok(true), Ok(Some(w))) => {
            let mut m = settings(app, &w);
            m.insert("status".into(), json!("success"));
            m.insert("message".into(), json!("Webhook enabled"));
            m.insert("webhook_enabled".into(), json!(true));
            Reply::new(200, Value::Object(m))
        }
        _ => Reply::new(500, json!({"error": "Failed to enable webhook"})),
    }
}

/// `POST /flow/api/workflows/<id>/webhook/disable`.
pub async fn disable(app: &Arc<AppState>, id: i64) -> Reply {
    if !matches!(load(app, id).await, Ok(Some(_))) {
        return workflow_not_found();
    }
    match switch(app, id, false).await {
        Ok(true) => Reply::new(
            200,
            json!({"status": "success", "message": "Webhook disabled"}),
        ),
        _ => Reply::new(500, json!({"error": "Failed to disable webhook"})),
    }
}

/// `POST /flow/api/workflows/<id>/webhook/regenerate`: a new token and
/// secret; the old address and secret stop working at once.
pub async fn regenerate(app: &Arc<AppState>, id: i64) -> Reply {
    let now = app.now();
    let token = app
        .blocking(move |c| workflows::regenerate_webhook_token(&*c.sqlite.conn()?, id, now))
        .await;
    if !matches!(token, Ok(Some(_))) {
        return Reply::new(500, json!({"error": "Failed to regenerate token"}));
    }
    tracing::info!("Regenerated webhook token for workflow {}", id);
    if new_secret(app, id).await.is_none() {
        return Reply::new(500, json!({"error": "Failed to regenerate secret"}));
    }
    match load(app, id).await {
        Ok(Some(w)) => {
            let s = settings(app, &w);
            Reply::new(
                200,
                json!({
                    "status": "success",
                    "message": "Webhook token and secret regenerated",
                    "webhook_token": s["webhook_token"],
                    "webhook_secret": s["webhook_secret"],
                    "webhook_url": s["webhook_url"],
                    "webhook_url_with_symbol": s["webhook_url_with_symbol"],
                }),
            )
        }
        _ => Reply::new(500, json!({"error": "Failed to regenerate secret"})),
    }
}

/// A new secret for the workflow (unlocking a locked webhook).
async fn new_secret(app: &Arc<AppState>, id: i64) -> Option<String> {
    let now = app.now();
    let secret = app
        .blocking(move |c| {
            workflows::regenerate_webhook_secret(&*c.sqlite.conn()?, &c.security, id, now)
        })
        .await;
    match secret {
        Ok(Some(s)) => {
            tracing::info!("Regenerated webhook secret for workflow {}", id);
            app.flow.webhook_guard.unlock(id);
            Some(s.expose().to_string())
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(
                "Could not regenerate the webhook secret of workflow {}: {}",
                id,
                e
            );
            None
        }
    }
}

/// `POST /flow/api/workflows/<id>/webhook/regenerate-secret`.
pub async fn regenerate_secret(app: &Arc<AppState>, id: i64) -> Reply {
    match new_secret(app, id).await {
        Some(s) => Reply::new(
            200,
            json!({"status": "success", "message": "Webhook secret regenerated", "webhook_secret": s}),
        ),
        None => Reply::new(500, json!({"error": "Failed to regenerate secret"})),
    }
}

/// `POST /flow/api/workflows/<id>/webhook/auth-type` with
/// `{"auth_type": "payload" | "url"}` (default `payload`).
pub async fn set_auth_type(app: &Arc<AppState>, id: i64, body: Body) -> Reply {
    if !matches!(load(app, id).await, Ok(Some(_))) {
        return workflow_not_found();
    }
    let chosen = match &body {
        Body::Json(Value::Object(m)) => m.get("auth_type").cloned().unwrap_or(json!("payload")),
        Body::Json(_) => Value::Null,
        Body::Nothing | Body::NonFinite => json!("payload"),
    };
    let Some(auth) = chosen.as_str().map(str::to_string) else {
        return Reply::new(400, json!({"error": "Invalid auth type"}));
    };
    let now = app.now();
    let given = auth.clone();
    let set = app
        .blocking(move |c| workflows::set_webhook_auth_type(&*c.sqlite.conn()?, id, &given, now))
        .await;
    if !matches!(set, Ok(true)) {
        tracing::error!("Invalid webhook auth type: {}", auth);
        return Reply::new(400, json!({"error": "Invalid auth type"}));
    }
    match load(app, id).await {
        Ok(Some(w)) => {
            let s = settings(app, &w);
            Reply::new(
                200,
                json!({
                    "status": "success",
                    "message": format!("Webhook auth type set to '{}'", auth),
                    "webhook_auth_type": auth,
                    "webhook_url": s["webhook_url"],
                    "webhook_url_with_secret": s["webhook_url_with_secret"],
                }),
            )
        }
        _ => Reply::new(500, json!({"error": "Invalid auth type"})),
    }
}

/// `_read_webhook_payload`: whatever the sender declared, a JSON object
/// (read as Python's `json.loads` reads it) is the payload; other JSON
/// keeps its text under `message` and its value under `payload`; a body
/// the sender declared a form becomes its fields (`form`, the web's
/// `request.form`: urlencoded or multipart, first value per key); other
/// text lands under `message`; an empty body is `{}`.
pub fn read_payload(body: &[u8], form: &Map<String, Value>) -> Map<String, Value> {
    let raw = String::from_utf8_lossy(body);
    let text = raw.trim();
    if !text.is_empty() {
        // Python's json.loads: TradingView's bare NaN and Infinity parse
        // as on the web (decision 24).
        match crate::flow::nodes::pyjson::loads(text) {
            Ok(Value::Object(m)) => return m,
            Ok(Value::Null) | Err(_) => {}
            Ok(other) => {
                let mut m = Map::new();
                m.insert("message".into(), json!(text));
                m.insert("payload".into(), other);
                return m;
            }
        }
    }
    if !text.is_empty() && !form.is_empty() {
        return form.clone();
    }
    let mut m = Map::new();
    if !text.is_empty() {
        m.insert("message".into(), json!(text));
    }
    m
}

/// The fields of an urlencoded body, as text, first value per key.
pub fn urlencoded_fields(body: &[u8]) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in url::form_urlencoded::parse(body) {
        m.entry(k.into_owned()).or_insert(json!(v.into_owned()));
    }
    m
}

/// Whether a token names a workflow: the cheap check the route makes
/// before reading the body, to decide which rate limit applies.
pub async fn token_known(app: &Arc<AppState>, token: &str) -> bool {
    let token = token.to_string();
    app.blocking(move |c| crate::flow::triggers::store::token_exists(&*c.sqlite.conn()?, &token))
        .await
        .unwrap_or(false)
}

/// A public webhook call, as the route read it.
pub struct WebhookCall {
    pub token: String,
    pub payload: Map<String, Value>,
    pub url_secret: Option<String>,
}

/// What the route does with the outcome: the reply, and whether a known
/// token came with a wrong secret (counted against the caller too).
pub struct Answer {
    pub reply: Reply,
    pub wrong_secret: bool,
}

fn answer(status: u16, error: &str) -> Answer {
    Answer {
        reply: Reply::new(status, json!({"error": error})),
        wrong_secret: false,
    }
}

fn same(given: &str, stored: &str) -> bool {
    use subtle::ConstantTimeEq;
    given.len() == stored.len() && bool::from(given.as_bytes().ct_eq(stored.as_bytes()))
}

/// The secret check per auth type (`payload` pops `secret` so the run never
/// sees it); `Err` is the refusal.
fn check_secret(w: &Workflow, call: &mut WebhookCall) -> Result<(), Answer> {
    let Some(stored) = w.webhook_secret.as_ref() else {
        return Err(answer(401, "This webhook has no secret yet. Open the workflow's webhook settings in OpenAlgo to create one, then send it with each alert."));
    };
    let wrong = || Answer {
        reply: Reply::new(401, json!({"error": "Invalid webhook secret"})),
        wrong_secret: true,
    };
    if w.webhook_auth_type == "url" {
        let Some(given) = call.url_secret.as_deref().filter(|s| !s.is_empty()) else {
            return Err(answer(
                401,
                "Missing webhook secret in URL. Use ?secret=your_secret",
            ));
        };
        return if same(given, stored.expose()) {
            Ok(())
        } else {
            Err(wrong())
        };
    }
    match call.payload.remove("secret") {
        Some(v) if !pyfmt::truthy(&v) => Err(answer(401, MISSING_IN_PAYLOAD)),
        None => Err(answer(401, MISSING_IN_PAYLOAD)),
        Some(Value::String(given)) if same(&given, stored.expose()) => Ok(()),
        Some(_) => Err(wrong()),
    }
}

const MISSING_IN_PAYLOAD: &str = "Missing webhook secret in payload. Send JSON with a 'secret' field. Plain text cannot carry one: switch the webhook to URL auth to authenticate with ?secret=... instead.";

/// The workflow a token names, enabled and active, its secret checked.
async fn admitted(app: &Arc<AppState>, call: &mut WebhookCall) -> Result<Workflow, Answer> {
    let token = call.token.clone();
    let found = app
        .blocking(move |c| workflows::get_by_token(&*c.sqlite.conn()?, &c.security, &token))
        .await;
    let w = match found {
        Ok(Some(w)) => w,
        Ok(None) => return Err(answer(404, "Invalid webhook token")),
        Err(e) => {
            tracing::error!("Flow webhook lookup failed: {}", e);
            return Err(answer(500, "The webhook could not be checked. Try again."));
        }
    };
    if !w.webhook_enabled {
        return Err(answer(403, "Webhook is disabled"));
    }
    if !w.is_active {
        return Err(answer(403, "Workflow is not active"));
    }
    if app.flow.webhook_guard.is_locked(w.id) {
        return Err(answer(403, super::webhook_guard::LOCKED));
    }
    if let Err(refused) = check_secret(&w, call) {
        if refused.wrong_secret {
            app.flow.webhook_guard.record_failure(w.id);
        }
        return Err(refused);
    }
    Ok(w)
}

/// `_execute_webhook`: the token, the switch, the active flag, the secret,
/// the key, the strict validator, then the run (awaited, so the sender gets
/// the broker's answer; a run that waits long answers 202).
pub async fn trigger(app: &Arc<AppState>, mut call: WebhookCall) -> Answer {
    let w = match admitted(app, &mut call).await {
        Ok(w) => w,
        Err(refused) => return refused,
    };
    let Some(key) = api_key(app).await else {
        tracing::error!("Webhook: No API key for workflow {}", w.id);
        return answer(
            500,
            "No API key configured for workflow execution. Please re-activate the workflow.",
        );
    };
    if let Some(blocked) = execution_blocked(&w) {
        tracing::error!(
            "Webhook for workflow {} rejected: {}",
            w.id,
            pyfmt::str(&blocked["message"])
        );
        return Answer {
            reply: Reply::new(400, blocked),
            wrong_secret: false,
        };
    }
    if let Some(reason) = runs::paused(app, RunTrigger::Webhook) {
        runs::record_skip(app, w.id, reason.clone()).await;
        return answer(503, &reason);
    }
    tracing::info!("Webhook triggered for workflow {}: {}", w.id, w.name);
    let result = executor::execute(
        app,
        w.id,
        RunRequest {
            webhook_data: Some(Value::Object(call.payload)),
            api_key: Some(key),
            trigger: RunTrigger::Webhook,
        },
    )
    .await;
    let status = result["status"].as_str().unwrap_or("success").to_string();
    let triggered = format!("Workflow '{}' triggered", w.name);
    let message = if status == "success" {
        triggered
    } else {
        result
            .get("message")
            .filter(|m| pyfmt::truthy(m))
            .map(pyfmt::str)
            .unwrap_or(triggered)
    };
    Answer {
        reply: Reply::new(
            run_status(&result),
            json!({
                "status": status,
                "message": message,
                "errors": result.get("errors").cloned().unwrap_or(Value::Null),
                "execution_id": result.get("execution_id").cloned().unwrap_or(Value::Null),
                "workflow_id": w.id,
            }),
        ),
        wrong_secret: false,
    }
}

#[cfg(test)]
mod tests;
