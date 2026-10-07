//! Broker sign-in routes (web `blueprints/brlogin.py`, `/auth/broker-config`,
//! `blueprints/broker_credentials.py`).

use crate::brokers::catalog::{self, AuthType};
use crate::db::sqlite::credentials::{self, CredentialUpdate};
use crate::security::Secret;
use crate::server::envelope::{error, json_response};
use crate::server::form::FormData;
use crate::server::middleware::{login_limited, redirect, ClientIp};
use crate::services::broker_auth_service::{BrokerAuthService, FormLogin};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn valid_broker(b: &str) -> bool {
    catalog::ALL_BROKERS.contains(&b)
}

fn broker_page_with_error(msg: &str) -> Response {
    redirect(&format!("/broker?error={}", urlencoding::encode(msg)))
}

/// GET /auth/broker-config. The broker app key is never sent to the page:
/// the authorize URL is built by `/<broker>/initiate-oauth`.
pub async fn broker_config(State(ctx): Ctx) -> Response {
    let cfg = ctx.server_config();
    match cfg.active_broker.clone() {
        Some(b) => json_response(
            StatusCode::OK,
            json!({
                "status": "success",
                "broker_name": b,
                "broker_api_key": serde_json::Value::Null,
                "redirect_url": cfg.redirect_url_for(&b),
                "auth_type": match catalog::auth_type(&b) { AuthType::OAuth => "oauth", AuthType::Form => "form" },
                "login_url": format!("/{}/initiate-oauth", b),
            }),
        ),
        None => json_response(
            StatusCode::NOT_FOUND,
            json!({
                "status": "error",
                "code": "BROKER_NOT_CONFIGURED",
                "message": "Choose your broker and add its API key in Profile, Broker Configuration.",
            }),
        ),
    }
}

/// GET /<broker>/initiate-oauth: store a fresh `state` and send the browser
/// to the broker. Form brokers go to their in-app form.
pub async fn initiate_oauth(State(ctx): Ctx, Path(broker): Path<String>) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if catalog::auth_type(&broker) == AuthType::Form {
        return redirect(&format!("/broker/{}/totp", broker));
    }
    match BrokerAuthService::start_oauth(&ctx, &broker) {
        Ok(url) => redirect(&url),
        Err(e) => {
            tracing::warn!("Could not start broker sign-in: {}", e.code());
            broker_page_with_error(&e.client_message())
        }
    }
}

/// GET /<broker>/callback: the broker's redirect. Public, but only a
/// server-issued, unexpired, unused `state` is accepted. The code is
/// exchanged here; the browser is sent to the dashboard.
pub async fn oauth_callback(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    ClientIp(ip): ClientIp,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if catalog::auth_type(&broker) == AuthType::Form && params.is_empty() {
        return redirect(&format!("/broker/{}/totp", broker));
    }
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    match BrokerAuthService::complete_oauth(&ctx, &broker, &params).await {
        Ok(_) => redirect("/dashboard"),
        Err(e) => {
            tracing::warn!("Broker sign-in for {} failed: {}", broker, e.code());
            broker_page_with_error(&e.client_message())
        }
    }
}

/// POST /<broker>/callback: form brokers (client id, PIN, TOTP).
pub async fn form_login(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    ClientIp(ip): ClientIp,
    form: FormData,
) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    let input = FormLogin::from_fields(&form.0);
    match BrokerAuthService::login_with_form(&ctx, &broker, input).await {
        Ok(_) => json_response(
            StatusCode::OK,
            json!({"status": "success", "message": "Authentication successful", "redirect": "/dashboard"}),
        ),
        Err(e) => {
            tracing::warn!("Broker form sign-in for {} failed: {}", broker, e.code());
            error(StatusCode::UNAUTHORIZED, e.client_message())
        }
    }
}

/// POST /auth/broker/oauth/manual (json: url). For when the broker's
/// redirect cannot reach this computer: the trader pastes the address the
/// broker redirected to.
pub async fn oauth_manual(State(ctx): Ctx, form: FormData) -> Response {
    let Some(raw) = form.non_empty("url") else {
        return error(
            StatusCode::BAD_REQUEST,
            "Paste the full address from the browser.",
        );
    };
    let Ok(url) = url::Url::parse(&raw) else {
        return error(
            StatusCode::BAD_REQUEST,
            "That does not look like a web address. Paste the full address from the browser.",
        );
    };
    let segs: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    let broker = match segs.as_slice() {
        [b, "callback"] if valid_broker(b) => b.to_string(),
        _ => return error(StatusCode::BAD_REQUEST, "That address is not a broker sign-in address. Paste the address shown after you signed in at the broker."),
    };
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    match BrokerAuthService::complete_oauth(&ctx, &broker, &params).await {
        Ok(_) => json_response(
            StatusCode::OK,
            json!({"status": "success", "message": "Authentication successful", "redirect": "/dashboard"}),
        ),
        Err(e) => error(StatusCode::UNAUTHORIZED, e.client_message()),
    }
}

/// GET /api/broker/credentials (masked, web shape).
pub async fn get_credentials(State(ctx): Ctx) -> Response {
    let cfg = ctx.server_config();
    let broker = cfg.active_broker.clone().unwrap_or_default();
    let masked = if broker.is_empty() {
        Ok(credentials::MaskedCredentials::default())
    } else {
        ctx.sqlite.conn().and_then(|c| {
            credentials::load(&c, &ctx.security, &broker)
                .map(|o| o.map(|s| s.masked()).unwrap_or_default())
        })
    };
    let masked = match masked {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let mut data = serde_json::to_value(masked).unwrap_or_default();
    let ws_host = cfg.bind_host.clone();
    if let Some(o) = data.as_object_mut() {
        o.insert(
            "redirect_url".into(),
            json!(if broker.is_empty() {
                String::new()
            } else {
                cfg.redirect_url_for(&broker)
            }),
        );
        o.insert("current_broker".into(), json!(broker));
        o.insert("valid_brokers".into(), json!(catalog::ALL_BROKERS));
        o.insert("ngrok_allow".into(), json!(cfg.ngrok_allow));
        o.insert(
            "host_server".into(),
            json!(cfg
                .host_server
                .clone()
                .unwrap_or_else(|| format!("http://127.0.0.1:{}", cfg.http_port))),
        );
        o.insert(
            "websocket_url".into(),
            json!(cfg
                .websocket_url
                .clone()
                .unwrap_or_else(|| format!("ws://127.0.0.1:{}", cfg.ws_port))),
        );
        o.insert(
            "server_status".into(),
            json!({
                "flask": {"host": cfg.bind_host, "port": cfg.http_port.to_string()},
                "websocket": {"host": ws_host, "port": cfg.ws_port.to_string()},
                "zmq": {"host": "127.0.0.1", "port": "5555"},
            }),
        );
    }
    json_response(StatusCode::OK, json!({"status": "success", "data": data}))
}

/// POST /api/broker/credentials (form or json, web field names). Empty
/// values keep what is stored. Applies immediately; no restart needed.
pub async fn update_credentials(State(ctx): Ctx, form: FormData) -> Response {
    let cfg = ctx.server_config();
    let redirect_url = form.non_empty("redirect_url");
    let mut broker = cfg.active_broker.clone();
    if let Some(r) = &redirect_url {
        let b = r
            .strip_suffix("/callback")
            .and_then(|p| p.rsplit('/').next())
            .map(|s| s.to_lowercase());
        let ok_format = (r.starts_with("http://") || r.starts_with("https://")) && b.is_some();
        if !ok_format {
            return error(
                StatusCode::BAD_REQUEST,
                "Invalid redirect URL format. Must end with /<broker>/callback",
            );
        }
        let b = b.unwrap_or_default();
        if !valid_broker(&b) {
            return error(
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid broker '{}'. Valid brokers: {}",
                    b,
                    catalog::ALL_BROKERS.join(", ")
                ),
            );
        }
        broker = Some(b);
    }
    if let Some(b) = form.non_empty("broker") {
        if !valid_broker(&b) {
            return error(StatusCode::BAD_REQUEST, "Choose a broker from the list.");
        }
        broker = Some(b);
    }
    let key = form.non_empty("broker_api_key");
    if let (Some(b), Some(k)) = (&broker, &key) {
        let parts = k.matches(":::").count();
        let bad = match b.as_str() {
            "fivepaisa" => parts != 2,
            "flattrade" | "dhan" => parts != 1,
            _ => false,
        };
        if bad {
            let fmt = match b.as_str() {
                "fivepaisa" => "5paisa API key must be in format: 'User_Key:::User_ID:::client_id'",
                "flattrade" => "Flattrade API key must be in format: 'client_id:::api_key'",
                _ => "Dhan API key must be in format: 'client_id:::api_key'",
            };
            return error(StatusCode::BAD_REQUEST, fmt);
        }
    }
    for (k, prefix) in [("host_server", "http"), ("websocket_url", "ws")] {
        if let Some(v) = form.non_empty(k) {
            if !v.starts_with(prefix) {
                return error(
                    StatusCode::BAD_REQUEST,
                    if k == "host_server" {
                        "Invalid HOST_SERVER format. Must start with http:// or https://"
                    } else {
                        "Invalid WEBSOCKET_URL format. Must start with ws:// or wss://"
                    },
                );
            }
        }
    }
    let update = CredentialUpdate {
        api_key: key.map(Secret::new),
        api_secret: form.non_empty("broker_api_secret").map(Secret::new),
        api_key_market: form.non_empty("broker_api_key_market").map(Secret::new),
        api_secret_market: form.non_empty("broker_api_secret_market").map(Secret::new),
        client_id: form.non_empty("client_id"),
    };
    let mut updated: Vec<&str> = Vec::new();
    for (f, name) in [
        (update.api_key.is_some(), "BROKER_API_KEY"),
        (update.api_secret.is_some(), "BROKER_API_SECRET"),
        (update.api_key_market.is_some(), "BROKER_API_KEY_MARKET"),
        (
            update.api_secret_market.is_some(),
            "BROKER_API_SECRET_MARKET",
        ),
        (redirect_url.is_some(), "REDIRECT_URL"),
        (form.get("ngrok_allow").is_some(), "NGROK_ALLOW"),
        (form.non_empty("host_server").is_some(), "HOST_SERVER"),
        (form.non_empty("websocket_url").is_some(), "WEBSOCKET_URL"),
    ] {
        if f {
            updated.push(name);
        }
    }
    if updated.is_empty() && form.non_empty("broker").is_none() {
        return error(StatusCode::BAD_REQUEST, "No credentials provided to update");
    }
    let has_secret_fields = update.api_key.is_some()
        || update.api_secret.is_some()
        || update.api_key_market.is_some()
        || update.api_secret_market.is_some()
        || update.client_id.is_some();
    let res = ctx.sqlite.conn().and_then(|c| {
        if has_secret_fields {
            let b = broker.clone().ok_or_else(|| {
                crate::error::AppError::Validation(
                    "Choose your broker before saving its API key.".into(),
                )
            })?;
            credentials::save(&c, &ctx.security, &b, update)?;
        }
        crate::config::save(
            &c,
            &crate::config::ServerConfigUpdate {
                active_broker: broker.clone(),
                redirect_url: redirect_url.clone(),
                host_server: form.non_empty("host_server"),
                websocket_url: form.non_empty("websocket_url"),
                ngrok_allow: form
                    .get("ngrok_allow")
                    .map(|v| v.eq_ignore_ascii_case("true")),
                ..Default::default()
            },
        )
    });
    if let Err(e) = res {
        return match e {
            crate::error::AppError::Validation(m) => error(StatusCode::BAD_REQUEST, m),
            other => other.into_response(),
        };
    }
    let _ = ctx.reload_config();
    tracing::info!("Broker settings updated: {}", updated.join(", "));
    json_response(
        StatusCode::OK,
        json!({
            "status": "success",
            "message": format!("Credentials updated successfully. Updated: {}", updated.join(", ")),
            "updated_fields": updated,
            "restart_required": false,
        }),
    )
}

/// GET /api/broker/capabilities
pub async fn capabilities(State(ctx): Ctx) -> Response {
    let Some(b) = ctx.get_broker_session() else {
        return error(StatusCode::BAD_REQUEST, "No broker in session");
    };
    // Web `plugin.json`: the crypto venue (Delta Exchange) declares its own
    // type, exchanges and `leverage_config` (which shows the Leverage page).
    let adapter = ctx.brokers.get(&b.broker_id);
    let crypto = adapter.as_ref().filter(|a| a.broker_type() == "crypto");
    let (broker_type, exchanges): (&str, Vec<&str>) = match crypto {
        Some(a) => (
            a.broker_type(),
            a.supported_exchanges().iter().map(|e| e.as_str()).collect(),
        ),
        None => (
            "IN_stock",
            vec![
                "NSE",
                "BSE",
                "NFO",
                "BFO",
                "CDS",
                "BCD",
                "MCX",
                "NSE_INDEX",
                "BSE_INDEX",
            ],
        ),
    };
    let leverage = adapter.as_ref().is_some_and(|a| a.leverage_config());
    json_response(
        StatusCode::OK,
        json!({"status": "success", "data": {
            "broker_name": b.broker_id,
            "broker_type": broker_type,
            "supported_exchanges": exchanges,
            "leverage_config": leverage,
        }}),
    )
}
