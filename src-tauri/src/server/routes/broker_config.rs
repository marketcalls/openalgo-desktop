//! Broker Configuration and Select Broker (desktop; the web reads one
//! broker's keys from `.env`): one key set per broker, for as many brokers
//! as the trader likes, and one broker logged in at a time. Every route
//! needs the signed-in user, writes are CSRF-checked by the session layer,
//! and answers carrying masked keys are never cached.

use crate::db::sqlite::credentials::CredentialUpdate;
use crate::security::Secret;
use crate::server::envelope::{error, json_response};
use crate::server::form::FormData;
use crate::server::middleware::User;
use crate::server::routes::webui::no_store;
use crate::services::broker_config_service::{
    BrokerConfigService, ForgetOutcome, KeySave, SaveOutcome,
};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

type Ctx = State<Arc<AppState>>;

/// The trader confirmed ending the logged-in broker's session.
fn confirmed(q: &HashMap<String, String>) -> bool {
    q.get("sign_out")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn failed(e: crate::error::AppError) -> Response {
    match e {
        crate::error::AppError::Validation(m) => error(StatusCode::BAD_REQUEST, m),
        crate::error::AppError::Locked => crate::error::AppError::Locked.into_response(),
        other => crate::server::routes::webui::failed(
            "Saving broker keys",
            other,
            "Your broker keys were not changed. Try again.",
        ),
    }
}

/// GET /api/broker/list: every broker with its sign-in kind, exchanges, the
/// keys it needs and a masked view of what is saved, and the broker that is
/// logged in.
pub async fn list(State(ctx): Ctx) -> Response {
    match BrokerConfigService::list(&ctx) {
        Ok(data) => no_store(json_response(
            StatusCode::OK,
            json!({"status": "success", "data": data}),
        )),
        Err(e) => no_store(e.into_response()),
    }
}

/// PUT /api/broker/credentials/{broker}[?sign_out=true] (json or form:
/// api_key, api_secret, client_id, api_key_market, api_secret_market,
/// redirect_url). A key left empty or out keeps its saved value;
/// `redirect_url` empty means the automatic address. New keys for the
/// broker that is logged in end its session, only with `sign_out=true`
/// (the page asks the trader); without it the answer is 409 and nothing is
/// saved.
pub async fn save(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    form: FormData,
) -> Response {
    let secret = |k: &str| form.non_empty(k).map(Secret::new);
    let req = KeySave {
        update: CredentialUpdate {
            api_key: secret("api_key"),
            api_secret: secret("api_secret"),
            api_key_market: secret("api_key_market"),
            api_secret_market: secret("api_secret_market"),
            client_id: form.non_empty("client_id"),
        },
        redirect_url: form.get("redirect_url").map(str::to_string),
    };
    let name = crate::brokers::setup_guide::display_name(&broker);
    match BrokerConfigService::save(&ctx, &broker, req, confirmed(&q)).await {
        Ok(SaveOutcome::Saved {
            message,
            signed_out,
        }) => no_store(json_response(
            StatusCode::OK,
            json!({"status": "success", "message": message, "signed_out": signed_out}),
        )),
        Ok(SaveOutcome::Refused(why)) => no_store(error(StatusCode::CONFLICT, why)),
        Ok(SaveOutcome::LoggedIn) => no_store(error(
            StatusCode::CONFLICT,
            format!(
                "You are logged in to {} now. New keys end that session; confirm to save them.",
                name
            ),
        )),
        Err(e) => no_store(failed(e)),
    }
}

/// DELETE /api/broker/credentials/{broker}[?sign_out=true]: forget a
/// broker's keys. The broker that is logged in is signed out first, only
/// with `sign_out=true` (the page asks the trader); without it the answer
/// is 409 and nothing changes.
pub async fn forget(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let name = crate::brokers::setup_guide::display_name(&broker);
    match BrokerConfigService::forget(&ctx, &broker, confirmed(&q)).await {
        Ok(ForgetOutcome::Forgotten { signed_out }) => no_store(json_response(
            StatusCode::OK,
            json!({
                "status": "success",
                "message": if signed_out {
                    format!("Signed out of {} and forgot its keys.", name)
                } else {
                    format!("Forgot your {} keys.", name)
                },
                "signed_out": signed_out,
            }),
        )),
        Ok(ForgetOutcome::Refused(why)) => no_store(error(StatusCode::CONFLICT, why)),
        Ok(ForgetOutcome::LoggedIn) => no_store(error(
            StatusCode::CONFLICT,
            format!(
                "You are logged in to {} now. Confirm to sign out of {} and forget its keys.",
                name, name
            ),
        )),
        Err(e) => no_store(failed(e)),
    }
}

/// The API plan route (BRL-01) and the reason a sign-in sent Select Broker
/// (its address carries only an id). The other Broker Configuration routes
/// are listed in the main table, which is over its size limit; they move
/// here in the code-health pass.
pub fn table() -> Vec<super::RouteSpec> {
    vec![
        super::RouteSpec {
            path: "/api/broker/rate-plan/{broker}",
            method: axum::http::Method::PUT,
            access: super::Access::User,
            make: || axum::routing::put(set_rate_plan),
        },
        super::RouteSpec {
            path: "/api/broker/sign-in-failure/{id}",
            method: axum::http::Method::GET,
            access: super::Access::User,
            make: || axum::routing::get(sign_in_failure),
        },
    ]
}

/// What Select Broker shows when the reason behind its address is unknown,
/// already read, expired or another session's.
const SIGN_IN_FAILED: &str = "The broker login did not complete. Try again.";

/// GET /api/broker/sign-in-failure/{id}: why the sign-in that sent this
/// browser back to Select Broker failed (`session::sign_in_failures`), read
/// once, by the session it belongs to.
pub async fn sign_in_failure(
    State(ctx): Ctx,
    User(user): User,
    Path(id): Path<String>,
) -> Response {
    let found = ctx
        .sessions
        .sign_in_failures
        .take(&id, &user.session_id, Instant::now());
    no_store(match found {
        Some(f) => json_response(
            StatusCode::OK,
            json!({"status": "success", "data": {"broker": f.broker, "message": f.message}}),
        ),
        None => error(StatusCode::NOT_FOUND, SIGN_IN_FAILED),
    })
}

/// PUT /api/broker/rate-plan/{broker} (json or form: plan): the API plan
/// the broker's requests are paced for (Fyers Standard or Prime). Applies
/// at once; nothing about the session changes.
pub async fn set_rate_plan(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    form: FormData,
) -> Response {
    let plan = form.get("plan").unwrap_or_default().to_string();
    let name = crate::brokers::setup_guide::display_name(&broker);
    match BrokerConfigService::set_rate_plan(&ctx, &broker, &plan) {
        Ok(label) => no_store(json_response(
            StatusCode::OK,
            json!({
                "status": "success",
                "message": format!("{} requests are now paced for the {} plan.", name, label),
                "rate_plan": plan.trim(),
            }),
        )),
        Err(crate::error::AppError::Validation(m)) => no_store(error(StatusCode::BAD_REQUEST, m)),
        Err(e) => no_store(crate::server::routes::webui::failed(
            "Saving a broker API plan",
            e,
            "The API plan was not changed. Try again.",
        )),
    }
}
