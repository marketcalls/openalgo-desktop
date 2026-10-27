//! Broker sign-in routes (web `blueprints/brlogin.py`, `/auth/broker-config`,
//! `blueprints/broker_credentials.py`).

use crate::brokers::catalog::{self, AuthType, SignIn};
use crate::config::url_with_scheme;
use crate::db::sqlite::credentials::CredentialUpdate;
use crate::error::AppError;
use crate::security::Secret;
use crate::server::envelope::{error, json_response};
use crate::server::form::FormData;
use crate::server::middleware::{login_limited, redirect, ClientIp, Sess, User};
use crate::services::broker_auth_service::{BrokerAuthService, CallbackOrigin, FormLogin};
use crate::services::broker_config_service::BrokerConfigService;
use crate::services::broker_settings_service::{
    self, SettingsError, SettingsOutcome, SettingsSave,
};
use crate::session::web::WebSession;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn valid_broker(b: &str) -> bool {
    catalog::ALL_BROKERS.contains(&b)
}

/// Back to Select Broker with the reason, shown on that broker's card.
fn broker_page_with_error(broker: &str, msg: &str) -> Response {
    redirect(&format!(
        "/broker?error={}&broker={}",
        urlencoding::encode(msg),
        urlencoding::encode(broker)
    ))
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
                "redirect_url": BrokerConfigService::redirect_url(&ctx, &b)
                    .unwrap_or_else(|_| cfg.redirect_url_for(&b)),
                "auth_type": match catalog::auth_type(&b) { AuthType::OAuth => "oauth", AuthType::Form => "form" },
                // Desktop: how the broker page starts the sign-in.
                "sign_in": catalog::sign_in(&b),
                "login_url": format!("/{}/initiate-oauth", b),
            }),
        ),
        None => json_response(
            StatusCode::NOT_FOUND,
            json!({
                "status": "error",
                "code": "BROKER_NOT_CONFIGURED",
                "message": "Choose your broker and add its API key on the Broker Configuration page.",
            }),
        ),
    }
}

/// GET /<broker>/initiate-oauth: store a fresh `state` and send the browser
/// to the broker. Form brokers go to their in-app form, saved-keys brokers
/// back to the broker page (which signs them in with one action).
///
/// Recording a pending sign-in is a change of state that a GET makes, and a
/// cross-site navigation carries the `SameSite=Lax` session cookie. A
/// pending sign-in is what a state-less callback completes (see
/// `docs/security/known-residuals.md`), so one is started only on a
/// navigation made inside OpenAlgo (the broker page's Connect) or typed by
/// the trader; any other site's navigation goes to the broker page and
/// starts nothing.
pub async fn initiate_oauth(
    State(ctx): Ctx,
    User(user): User,
    Path(broker): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if catalog::sign_in(&broker) == SignIn::SavedKeys {
        return redirect("/broker");
    }
    if catalog::auth_type(&broker) == AuthType::Form {
        return redirect(&format!("/broker/{}/totp", broker));
    }
    if !crate::server::middleware::same_origin_navigation(&ctx, &headers) {
        tracing::info!(
            "Not starting the {} sign-in: the page was not opened from OpenAlgo",
            broker
        );
        return redirect("/broker");
    }
    match BrokerAuthService::start_oauth(&ctx, &broker, Some(&user.session_id)).await {
        Ok(url) => redirect(&url),
        Err(e) => {
            tracing::warn!("Could not start broker sign-in: {}", e.code());
            broker_page_with_error(&broker, &e.client_message())
        }
    }
}

/// A form broker's callback opened with no parameters: the start of its
/// in-app login, never its end.
async fn form_login_open(
    ctx: &AppState,
    broker: &str,
    ip: std::net::IpAddr,
    sess: Option<&WebSession>,
    headers: &axum::http::HeaderMap,
) -> Response {
    // Definedge and Nubra send their login OTP as the page opens (web
    // brlogin), only for a browser session signed in to OpenAlgo, only
    // when the app itself opened the page (a cross-site navigation
    // carries the Lax session cookie too, and must not text the trader,
    // replace a pending OTP or use up the login limit), and within the
    // login limits (each open sends the trader a message). Otherwise
    // the page opens with a Send OTP action, a CSRF-checked POST.
    let signed_in = sess.is_some_and(|s| s.user.is_some());
    if signed_in && catalog::sends_login_otp(broker) {
        if !crate::server::middleware::same_origin_navigation(ctx, headers) {
            tracing::info!(
                "Not sending the {} login OTP: the page was not opened from OpenAlgo",
                broker
            );
            return redirect(&format!("/broker/{}/totp?otp=send", broker));
        }
        if let Some(r) = login_limited(ctx, ip) {
            return r;
        }
        if let Err(e) = BrokerAuthService::prepare_form_login(ctx, broker).await {
            // Back to the broker page with the reason (web
            // `handle_auth_failure` on a page navigation).
            tracing::warn!("Preparing the {} sign-in failed: {}", broker, e.code());
            return broker_page_with_error(broker, &e.client_message());
        }
    }
    // The saved keys sign in from the broker page in one action (a
    // CSRF-checked POST), never from this GET, which any site can make
    // the browser send. Web brlogin signs these in here.
    if catalog::sign_in(broker) == SignIn::SavedKeys {
        return redirect("/broker");
    }
    // Samco's connect page (key exchange plus the static IP check).
    if broker == "samco" {
        return redirect("/broker/samco/auth");
    }
    redirect(&format!("/broker/{}/totp", broker))
}

/// GET /<broker>/callback: the broker's redirect. Public, but only a
/// server-issued, unexpired, unused `state` is accepted. The code is
/// exchanged here; the browser is sent to the dashboard.
pub async fn oauth_callback(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    ClientIp(ip): ClientIp,
    Sess(sess): Sess,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if catalog::auth_type(&broker) == AuthType::Form && params.is_empty() {
        return form_login_open(&ctx, &broker, ip, sess.as_ref(), &headers).await;
    }
    // The broker's redirect is a top-level page load; an image or frame
    // from another site must not use up the sign-in limit (S-02).
    if crate::server::middleware::cross_site_subresource(&headers) {
        return error(StatusCode::FORBIDDEN, "Request blocked.");
    }
    // A redirect broker's callback opened without any answer from the
    // broker is the start of a sign-in, not its end (web brlogin sends that
    // first visit to the broker's login page). It never completes or uses up
    // a pending sign-in. Like the login OTP above, it starts one only for
    // the signed-in trader on a navigation made inside OpenAlgo (or typed);
    // any other open, such as another site's, goes to the broker page,
    // whose Connect starts it, and changes nothing.
    if params.is_empty() {
        let signed_in = sess.as_ref().is_some_and(|s| s.user.is_some());
        if signed_in && crate::server::middleware::same_origin_navigation(&ctx, &headers) {
            return redirect(&format!("/{}/initiate-oauth", broker));
        }
        return redirect("/broker");
    }
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    let origin = CallbackOrigin::Redirect {
        session_id: sess.as_ref().map(|s| s.id.as_str()),
    };
    match BrokerAuthService::complete_oauth(&ctx, &broker, &params, origin).await {
        Ok(_) => redirect("/dashboard"),
        Err(e) => {
            tracing::warn!("Broker sign-in for {} failed: {}", broker, e.code());
            broker_page_with_error(&broker, &e.client_message())
        }
    }
}

/// POST /<broker>/callback. For the XTS third-party login (compositedge,
/// rmoney) this is the broker's redirect, a form POST carrying `session`
/// with `state` on the query: public and state-verified, like the GET
/// callback. For everyone else it is the in-app login form (with no fields
/// for a saved-keys broker), which needs the signed-in user.
pub async fn callback_post(
    State(ctx): Ctx,
    Path(broker): Path<String>,
    ClientIp(ip): ClientIp,
    Sess(sess): Sess,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    form: FormData,
) -> Response {
    if !valid_broker(&broker) {
        return error(StatusCode::NOT_FOUND, "Unknown broker.");
    }
    if catalog::posts_callback(&broker) {
        // The broker's form post is a top-level page load (S-02).
        if crate::server::middleware::cross_site_subresource(&headers) {
            return error(StatusCode::FORBIDDEN, "Request blocked.");
        }
        if let Some(r) = login_limited(&ctx, ip) {
            return r;
        }
        let mut params = form.0;
        params.extend(query);
        let origin = CallbackOrigin::Redirect {
            session_id: sess.as_ref().map(|s| s.id.as_str()),
        };
        return match BrokerAuthService::complete_oauth(&ctx, &broker, &params, origin).await {
            Ok(_) => redirect("/dashboard"),
            Err(e) => {
                tracing::warn!("Broker sign-in for {} failed: {}", broker, e.code());
                broker_page_with_error(&broker, &e.client_message())
            }
        };
    }
    // Every other broker: the in-app login form, which needs the signed-in
    // user and, checked here as well as in the session layer, the CSRF
    // token and a same-origin request.
    let Some(s) = sess.as_ref().filter(|s| s.user.is_some()) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "Sign in to OpenAlgo first, then log in to your broker.",
        );
    };
    if !crate::server::middleware::write_allowed(
        &ctx,
        &headers,
        &s.csrf_token,
        form.get("csrf_token"),
    ) {
        return error(
            StatusCode::FORBIDDEN,
            "This sign-in did not come from OpenAlgo. Reload the page and try again.",
        );
    }
    form_login(ctx, broker, ip, form).await
}

/// The in-app broker login form (client id, PIN, TOTP, or the broker's own
/// fields).
async fn form_login(
    ctx: Arc<AppState>,
    broker: String,
    ip: std::net::IpAddr,
    form: FormData,
) -> Response {
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    // Definedge "resend OTP" (web answers 200 either way).
    if form.get("action") == Some("resend") {
        return match BrokerAuthService::prepare_form_login(&ctx, &broker).await {
            Ok(Some(_)) => json_response(
                StatusCode::OK,
                json!({"status": "success", "message": "OTP has been resent successfully"}),
            ),
            Ok(None) => error(
                StatusCode::BAD_REQUEST,
                "This broker does not send a login OTP.",
            ),
            Err(e) => {
                tracing::warn!("OTP resend for {} failed: {}", broker, e.code());
                json_response(
                    StatusCode::OK,
                    json!({"status": "error", "message": e.client_message()}),
                )
            }
        };
    }
    let input = match FormLogin::for_broker(&broker, &form.0) {
        Ok(i) => i,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.client_message()),
    };
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
pub async fn oauth_manual(State(ctx): Ctx, User(user): User, form: FormData) -> Response {
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
    let origin = CallbackOrigin::Manual {
        session_id: &user.session_id,
    };
    match BrokerAuthService::complete_oauth(&ctx, &broker, &params, origin).await {
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
    let (masked, client_id) = match broker_settings_service::masked(&ctx, &broker) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let redirect_url = if broker.is_empty() {
        String::new()
    } else {
        BrokerConfigService::redirect_url(&ctx, &broker)
            .unwrap_or_else(|_| cfg.redirect_url_for(&broker))
    };
    let mut data = serde_json::to_value(masked).unwrap_or_default();
    if let Some(o) = data.as_object_mut() {
        o.insert("redirect_url".into(), json!(redirect_url));
        o.insert("current_broker".into(), json!(broker));
        o.insert("valid_brokers".into(), json!(catalog::ALL_BROKERS));
        // Desktop: brokers whose sign-in needs a separate client id, and the
        // saved one (an account id, not a secret), for the Profile form.
        o.insert(
            "client_id_brokers".into(),
            json!(catalog::CLIENT_ID_BROKERS),
        );
        o.insert("client_id".into(), json!(client_id));
        o.extend(server_settings(&cfg));
    }
    json_response(StatusCode::OK, json!({"status": "success", "data": data}))
}

/// The server addresses `GET /api/broker/credentials` reports with the keys.
fn server_settings(cfg: &crate::config::ServerConfig) -> serde_json::Map<String, Value> {
    let mut o = serde_json::Map::new();
    o.insert("ngrok_allow".into(), json!(cfg.ngrok_allow));
    // Desktop: whether each address is the automatic one (nothing saved), so
    // the Profile page can offer to go back to it.
    o.insert(
        "host_server_is_default".into(),
        json!(cfg.host_server.is_none()),
    );
    o.insert(
        "websocket_url_is_default".into(),
        json!(cfg.websocket_url.is_none()),
    );
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
    // Desktop: no web `server_status` (Flask, WebSocket and ZeroMQ, always
    // "Running"); the Profile page reads the real parts from Server
    // Settings (`components`, docs-35).
    o
}

/// POST /api/broker/credentials (form or json, web field names). Empty
/// values keep what is stored. Applies immediately; no restart needed.
pub async fn update_credentials(State(ctx): Ctx, form: FormData) -> Response {
    let (save, updated) = match settings_form(&form) {
        Ok(v) => v,
        Err(m) => return error(StatusCode::BAD_REQUEST, m),
    };
    let (switched, signed_out) = match broker_settings_service::save(&ctx, save).await {
        Ok(SettingsOutcome::Saved {
            switched,
            signed_out_of,
        }) => (switched, signed_out_of),
        Ok(SettingsOutcome::Refused(why)) => return error(StatusCode::CONFLICT, why),
        Err(SettingsError::Read(e)) => return e.into_response(),
        Err(SettingsError::Save(e)) => return settings_not_saved(e),
    };
    tracing::info!("Broker settings updated: {}", updated.join(", "));
    json_response(
        StatusCode::OK,
        json!({
            "status": "success",
            "message": format!("Credentials updated successfully. Updated: {}", updated.join(", ")),
            "updated_fields": updated,
            "restart_required": false,
            // Desktop: the active broker changed; the page sends the trader
            // to the broker login. `signed_out_of` names an ended session.
            "broker_switched": switched,
            "signed_out_of": signed_out,
        }),
    )
}

/// The settings form, checked, and the web's names of the fields it
/// changes (for the reply). `Err` is the trader's message.
fn settings_form(form: &FormData) -> Result<(SettingsSave, Vec<&'static str>), String> {
    let redirect_url = form.non_empty("redirect_url");
    // The broker this save names, if it names one (the web derives it from
    // the redirect URL).
    let mut broker = match &redirect_url {
        Some(r) => Some(broker_of_redirect_url(r)?),
        None => None,
    };
    if let Some(b) = form.non_empty("broker") {
        if !valid_broker(&b) {
            return Err("Choose a broker from the list.".into());
        }
        broker = Some(b);
    }
    // Desktop: an address sent empty means "use the automatic address" (it is
    // stored empty, which reads back as unset), so a saved tunnel address can
    // be cleared; the web keeps the old value.
    let host_server = form.get("host_server").map(|v| v.trim().to_string());
    let websocket_url = form.get("websocket_url").map(|v| v.trim().to_string());
    check_addresses(host_server.as_deref(), websocket_url.as_deref())?;
    let save = SettingsSave {
        broker,
        update: CredentialUpdate {
            api_key: form.non_empty("broker_api_key").map(Secret::new),
            api_secret: form.non_empty("broker_api_secret").map(Secret::new),
            api_key_market: form.non_empty("broker_api_key_market").map(Secret::new),
            api_secret_market: form.non_empty("broker_api_secret_market").map(Secret::new),
            client_id: form.non_empty("client_id"),
        },
        redirect_url,
        host_server,
        websocket_url,
        ngrok_allow: form
            .get("ngrok_allow")
            .map(|v| v.eq_ignore_ascii_case("true")),
    };
    let updated = updated_fields(&save);
    if updated.is_empty() && form.non_empty("broker").is_none() {
        return Err("No credentials provided to update".into());
    }
    Ok((save, updated))
}

/// The broker a redirect URL names (`.../<broker>/callback`), parsed like
/// the other addresses, since the broker reads this one.
fn broker_of_redirect_url(url: &str) -> Result<String, String> {
    let b = url
        .strip_suffix("/callback")
        .and_then(|p| p.rsplit('/').next())
        .map(|s| s.to_lowercase());
    let Some(b) = b.filter(|_| url_with_scheme(url, &["http", "https"], true)) else {
        return Err("Invalid redirect URL format. Must end with /<broker>/callback".into());
    };
    if !valid_broker(&b) {
        return Err(format!(
            "Invalid broker '{}'. Valid brokers: {}",
            b,
            catalog::ALL_BROKERS.join(", ")
        ));
    }
    Ok(b)
}

/// A server address must be a full URL with the right scheme and a host
/// (CFG-01, CFG-02); empty means the automatic one.
fn check_addresses(host_server: Option<&str>, websocket_url: Option<&str>) -> Result<(), String> {
    for (value, schemes, any_path, message) in [
        (
            host_server,
            ["http", "https"],
            false,
            "Invalid HOST_SERVER format. Must start with http:// or https://",
        ),
        (
            websocket_url,
            ["ws", "wss"],
            true,
            "Invalid WEBSOCKET_URL format. Must start with ws:// or wss://",
        ),
    ] {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            if !url_with_scheme(v, &schemes, any_path) {
                return Err(message.into());
            }
        }
    }
    Ok(())
}

/// The web's names of the fields a save changes.
fn updated_fields(save: &SettingsSave) -> Vec<&'static str> {
    let u = &save.update;
    [
        (u.api_key.is_some(), "BROKER_API_KEY"),
        (u.api_secret.is_some(), "BROKER_API_SECRET"),
        (u.api_key_market.is_some(), "BROKER_API_KEY_MARKET"),
        (u.api_secret_market.is_some(), "BROKER_API_SECRET_MARKET"),
        // Desktop: the client id some brokers' sign-in is bound to.
        (u.client_id.is_some(), "CLIENT_ID"),
        (save.redirect_url.is_some(), "REDIRECT_URL"),
        (save.ngrok_allow.is_some(), "NGROK_ALLOW"),
        (save.host_server.is_some(), "HOST_SERVER"),
        (save.websocket_url.is_some(), "WEBSOCKET_URL"),
    ]
    .into_iter()
    .filter_map(|(changed, name)| changed.then_some(name))
    .collect()
}

/// The reply when the save itself failed: nothing was changed.
fn settings_not_saved(e: AppError) -> Response {
    match e {
        AppError::Validation(m) => error(StatusCode::BAD_REQUEST, m),
        AppError::Locked => AppError::Locked.into_response(),
        other => crate::server::routes::webui::failed(
            "Saving broker settings",
            other,
            "Your broker settings were not saved, and nothing was changed. Try again.",
        ),
    }
}

/// GET /api/broker/configured: every broker with saved keys and which one is
/// active (desktop; the broker page lists them so a trader can switch).
pub async fn configured(State(ctx): Ctx) -> Response {
    let active = ctx.server_config().active_broker.clone();
    match broker_settings_service::configured(&ctx) {
        Ok(list) => {
            let brokers: Vec<_> = list
                .iter()
                .map(|b| {
                    json!({
                        "name": b,
                        "active": active.as_deref() == Some(b.as_str()),
                        // How the broker page starts this broker's sign-in.
                        "sign_in": catalog::sign_in(b),
                    })
                })
                .collect();
            json_response(
                StatusCode::OK,
                json!({"status": "success", "data": {"brokers": brokers, "active": active}}),
            )
        }
        Err(e) => e.into_response(),
    }
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

/// GET /samco/ip-status: the source IP Samco sees against the registered
/// static IPs (web `samco_ip_status`, same body and status codes).
pub async fn samco_ip_status(State(ctx): Ctx) -> Response {
    let (code, body) = BrokerAuthService::samco_ip_status(&ctx).await;
    json_response(
        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
        body,
    )
}
