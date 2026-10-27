//! What the `/agent/api/chatgpt/*` routes answer (the ChatGPT part of the
//! web's `blueprints/agent.py`), each in the web's envelope.
//!
//! The status describes the credential and shows none of it: a fingerprint
//! over the refresh token, the account, the access token's expiry. On the
//! desktop the credential lives only in the vault, so `token_dir` is null
//! and `stored_in_database` says whether the vault holds one (decision 10).
//! Cancel answers from the snapshot alone and decrypts nothing.

use super::record::{self, AuthRecord};
use super::signin::{unix, LoginState, LoginStatus, SignIn};
use crate::agent::models::{success, truthy};
use crate::agent::reply::Refusal;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const STATUS_FAILED: &str = "Could not read the ChatGPT subscription status";
const START_FAILED: &str = "Could not start the ChatGPT sign-in";
const CANCEL_FAILED: &str = "Could not cancel the ChatGPT sign-in";
const SIGN_OUT_FAILED: &str = "Could not sign out of ChatGPT";
pub const SIGNED_OUT: &str = "ChatGPT subscription signed out";

fn sign_in(app: &AppState, failure: &str) -> Result<Arc<SignIn>, Refusal> {
    super::sign_in_of(app).ok_or_else(|| {
        tracing::error!("The ChatGPT plan's sign-in is not installed");
        Refusal::new(500, failure, None)
    })
}

/// The status's credential half, from the stored record.
fn describe(stored: Option<AuthRecord>, in_vault: bool, now: f64) -> Map<String, Value> {
    let mut m = Map::new();
    let r = stored.unwrap_or_default();
    m.insert("provider".into(), json!(super::models::PROVIDER));
    m.insert("authorised".into(), json!(r.has_credential(now)));
    m.insert("fingerprint".into(), json!(r.fingerprint()));
    m.insert("account_id".into(), json!(r.account_id));
    m.insert("access_token_expires_at".into(), json!(r.expiry()));
    // Only a token that exists can have expired.
    m.insert(
        "access_token_expired".into(),
        json!(r.access().is_some() && r.access_expired(now, 0.0)),
    );
    m.insert("stored_in_database".into(), json!(in_vault));
    m.insert("token_dir".into(), Value::Null);
    m
}

/// `GET /agent/api/chatgpt/status`.
pub async fn status(app: &Arc<AppState>) -> Result<Value, Refusal> {
    let plan = sign_in(app, STATUS_FAILED)?;
    let read = app
        .blocking(|c| {
            let conn = c.sqlite.conn()?;
            Ok((record::load(&conn, &c.security), record::is_stored(&conn)))
        })
        .await
        .map_err(|_| {
            tracing::error!("{}", STATUS_FAILED);
            Refusal::new(500, STATUS_FAILED, None)
        })?;
    let mut data = describe(read.0, read.1, unix(app));
    data.insert("login".into(), plan.snapshot().to_json());
    Ok(success(json!({"data": data})))
}

/// The optional `{force}` body: absent is no force; anything but an object
/// is refused.
pub fn force_of(body: Option<&Value>) -> Result<bool, Refusal> {
    match body {
        None => Ok(false),
        Some(Value::Object(m)) => Ok(m.get("force").is_some_and(truthy)),
        Some(_) => Err(Refusal::bad_request(
            "The request body must be a JSON object",
        )),
    }
}

/// `POST /agent/api/chatgpt/login`: the code to show, at once. A sign-in
/// already pending comes back `reused`; one whose code could not be had is
/// a 502 with the reason and the snapshot.
pub async fn login(app: &Arc<AppState>, force: bool) -> Result<Value, Refusal> {
    let plan = sign_in(app, START_FAILED)?;
    let before = plan.snapshot();
    match plan.start(app, force).await {
        Ok(after) => {
            let reused = before.state == LoginState::Pending
                && after.started_at == before.started_at
                && after.user_code == before.user_code;
            Ok(success(json!({"data": after.to_json(), "reused": reused})))
        }
        Err(failed) => {
            tracing::error!("The ChatGPT subscription sign-in could not start");
            Err(failed_start(&failed))
        }
    }
}

fn failed_start(failed: &LoginStatus) -> Refusal {
    Refusal::new(
        502,
        failed.message.clone(),
        Some(json!({"data": failed.to_json()})),
    )
}

/// `POST /agent/api/chatgpt/cancel`: idempotent; `stopped` says whether a
/// sign-in was running.
pub async fn cancel(app: &Arc<AppState>) -> Result<Value, Refusal> {
    let plan = sign_in(app, CANCEL_FAILED)?;
    let stopped = plan.cancel().await;
    Ok(success(
        json!({"data": plan.snapshot().to_json(), "stopped": stopped}),
    ))
}

/// `DELETE /agent/api/chatgpt/session`: sign the plan out (idempotent).
pub async fn sign_out(app: &Arc<AppState>) -> Result<Value, Refusal> {
    let plan = sign_in(app, SIGN_OUT_FAILED)?;
    match plan.forget(app).await {
        Ok(removed) => Ok(success(json!({"removed": removed, "message": SIGNED_OUT}))),
        Err(_) => {
            tracing::error!("Could not remove the ChatGPT subscription sign-in");
            Err(Refusal::new(500, SIGN_OUT_FAILED, None))
        }
    }
}
