//! The device flow's requests (LiteLLM's `chatgpt` provider endpoints, as
//! the web's `chatgpt_oauth.py` drives them): ask for a code, poll until
//! the trader approves it, exchange the approval for tokens, and renew an
//! access token.
//!
//! Every request carries a 15 s timeout. A failure reaches the trader as a
//! sentence without a status code or an error class (D-023); the status or
//! the failure's kind goes to the log, and nothing else: a transport
//! error's text can quote the body, which for a renewal holds the refresh
//! token.

use crate::security::Secret;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// LiteLLM's `CHATGPT_CLIENT_ID`.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// The web's `HTTP_TIMEOUT_SECONDS`, on every request.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// LiteLLM's `DEVICE_CODE_TIMEOUT_SECONDS`: a code is good for 15 minutes.
pub const DEVICE_CODE_LIFETIME: Duration = Duration::from_secs(900);

/// LiteLLM's `DEVICE_CODE_POLL_SLEEP_SECONDS`.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The floor under every wait between polls (the web's `_MIN_POLL_SECONDS`).
const MIN_POLL: Duration = Duration::from_millis(10);

/// Where the sign-in happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The origin the redirect URI is built from (`CHATGPT_AUTH_BASE`).
    pub auth_base: String,
    /// Where a sign-in code is asked for.
    pub device_code_url: String,
    /// Where the approved code is polled for.
    pub device_poll_url: String,
    /// OpenAI's token endpoint (an address, not a credential).
    pub token_url: String,
    /// The page the trader opens to enter the code.
    pub verify: String,
}

impl Endpoints {
    /// LiteLLM's addresses under one origin.
    pub fn at(auth_base: &str) -> Endpoints {
        let base = auth_base.trim_end_matches('/');
        Endpoints {
            auth_base: base.to_string(),
            device_code_url: format!("{}/api/accounts/deviceauth/usercode", base),
            device_poll_url: format!("{}/api/accounts/deviceauth/token", base),
            token_url: format!("{}/oauth/token", base),
            verify: format!("{}/codex/device", base),
        }
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints::at("https://auth.openai.com")
    }
}

/// The flow's addresses and timings (a test's are local and quick).
#[derive(Debug, Clone)]
pub struct Flow {
    pub endpoints: Endpoints,
    /// How long a code is polled for before it reads as expired.
    pub lifetime: Duration,
    /// The wait between polls (the endpoint's own interval is a floor).
    pub interval: Duration,
}

impl Default for Flow {
    fn default() -> Self {
        Flow {
            endpoints: Endpoints::default(),
            lifetime: DEVICE_CODE_LIFETIME,
            interval: POLL_INTERVAL,
        }
    }
}

// The sentences the trader reads (D-023: the web's meaning, no status code
// and no error class; those stay in the log).
pub const REQUESTING: &str = "Requesting a sign-in code from ChatGPT...";
pub const CODE_REFUSED: &str =
    "ChatGPT did not issue a sign-in code. Wait a minute, then try connecting again.";
pub const CODE_INCOMPLETE: &str =
    "ChatGPT sent back an incomplete sign-in code. Try connecting again.";
pub const POLL_REFUSED: &str = "ChatGPT stopped this sign-in before it was approved. Start again.";
pub const EXPIRED: &str = "The sign-in code expired before it was approved. Start again.";
pub const EXCHANGE_REFUSED: &str =
    "ChatGPT approved the sign-in but did not hand over the credential. Try connecting again.";
pub const EXCHANGE_INCOMPLETE: &str =
    "ChatGPT approved the sign-in but sent back an incomplete credential. Try connecting again.";
pub const UNREACHABLE: &str =
    "Could not reach ChatGPT's sign-in service from this computer. Check the internet connection and try again.";

/// One answer: the status and the JSON object it carried (empty when the
/// body was not one).
struct Reply {
    status: u16,
    body: Map<String, Value>,
}

/// What a request sends.
enum Payload {
    Json(Value),
    Form(Secret),
}

/// One POST with the flow's timeout. A request that could not be made is
/// logged by kind and answered with [`UNREACHABLE`].
async fn post(url: &str, payload: Payload) -> Result<Reply, &'static str> {
    let client = crate::agent::outbound::client().map_err(|e| {
        tracing::error!("The ChatGPT sign-in could not build its HTTP client: {}", e);
        UNREACHABLE
    })?;
    let request = client.post(url).timeout(HTTP_TIMEOUT);
    let request = match payload {
        Payload::Json(v) => request.json(&v),
        Payload::Form(body) => request
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.expose().to_string()),
    };
    let response = request.send().await.map_err(|e| {
        tracing::error!(
            "A ChatGPT sign-in request failed: {}",
            crate::agent::outbound::error_kind(&e)
        );
        UNREACHABLE
    })?;
    let status = response.status().as_u16();
    let body = match response.json::<Value>().await {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    Ok(Reply { status, body })
}

fn text(m: &Map<String, Value>, key: &str) -> Option<String> {
    match m.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// An issued code.
pub struct Device {
    pub device_auth_id: Secret,
    /// What the trader types. Shown on their screen, never logged.
    pub user_code: Secret,
    /// The endpoint's own interval, unparsed.
    pub interval: Option<String>,
}

/// Ask for a code (the web's `_request_device_code`).
pub async fn request_code(e: &Endpoints) -> Result<Device, &'static str> {
    let reply = post(
        &e.device_code_url,
        Payload::Json(json!({"client_id": CLIENT_ID})),
    )
    .await?;
    if reply.status != 200 {
        tracing::error!(
            "The ChatGPT sign-in service refused a code request (HTTP {})",
            reply.status
        );
        return Err(CODE_REFUSED);
    }
    let id = text(&reply.body, "device_auth_id");
    let code = text(&reply.body, "user_code").or_else(|| text(&reply.body, "usercode"));
    match (id, code) {
        (Some(id), Some(code)) => Ok(Device {
            device_auth_id: Secret::new(id),
            user_code: Secret::new(code),
            interval: text(&reply.body, "interval"),
        }),
        _ => {
            tracing::error!("The ChatGPT sign-in service returned an incomplete device code");
            Err(CODE_INCOMPLETE)
        }
    }
}

/// The wait between polls (the web's `_poll_wait`): the larger of the
/// flow's interval and the endpoint's, never under the floor. An endpoint
/// value that is not a number is ignored.
pub fn poll_wait(interval: Duration, endpoint: Option<&str>) -> Duration {
    let asked = endpoint
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|f| f.is_finite() && *f > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::ZERO);
    interval.max(asked).max(MIN_POLL)
}

/// Why a sign-in ended without tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The code ran out unapproved: nothing is broken.
    Expired,
    /// The sentence to show.
    Failed(&'static str),
}

/// The approved sign-in's tokens.
pub struct Tokens {
    pub access: Secret,
    pub refresh: Option<Secret>,
    pub id: Secret,
}

/// Poll until the code is approved or expires, then exchange it (the web's
/// `_poll_for_code` and `_exchange_code`). 403 and 404 mean "not yet";
/// any other refusal ends the sign-in rather than retrying it for fifteen
/// minutes. A cancel is the caller's: it drops this future.
pub async fn await_approval(flow: &Flow, device: &Device) -> Result<Tokens, Ended> {
    let wait = poll_wait(flow.interval, device.interval.as_deref());
    let deadline = tokio::time::Instant::now() + flow.lifetime;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(Ended::Expired);
        }
        let reply = post(
            &flow.endpoints.device_poll_url,
            Payload::Json(json!({
                "device_auth_id": device.device_auth_id.expose(),
                "user_code": device.user_code.expose(),
            })),
        )
        .await
        .map_err(Ended::Failed)?;
        if reply.status == 200 {
            let found = ["authorization_code", "code_challenge", "code_verifier"]
                .iter()
                .all(|k| reply.body.contains_key(*k));
            if found {
                return exchange(&flow.endpoints, &reply.body).await;
            }
        } else if reply.status != 403 && reply.status != 404 {
            tracing::error!(
                "The ChatGPT sign-in service refused a poll (HTTP {})",
                reply.status
            );
            return Err(Ended::Failed(POLL_REFUSED));
        }
        tokio::time::sleep(wait).await;
    }
}

/// Exchange an approval for tokens: the form body LiteLLM sends, values
/// unencoded as it writes them.
async fn exchange(e: &Endpoints, approval: &Map<String, Value>) -> Result<Tokens, Ended> {
    let code = text(approval, "authorization_code").unwrap_or_default();
    let verifier = text(approval, "code_verifier").unwrap_or_default();
    let body = Secret::new(format!(
        "grant_type=authorization_code&code={}&redirect_uri={}/deviceauth/callback&client_id={}&code_verifier={}",
        code, e.auth_base, CLIENT_ID, verifier
    ));
    let reply = post(&e.token_url, Payload::Form(body))
        .await
        .map_err(Ended::Failed)?;
    if reply.status != 200 {
        tracing::error!(
            "The ChatGPT token exchange was refused (HTTP {})",
            reply.status
        );
        return Err(Ended::Failed(EXCHANGE_REFUSED));
    }
    let get = |k: &str| text(&reply.body, k).map(Secret::new);
    match (get("access_token"), get("refresh_token"), get("id_token")) {
        (Some(access), Some(refresh), Some(id)) => Ok(Tokens {
            access,
            refresh: Some(refresh),
            id,
        }),
        _ => {
            tracing::error!("The ChatGPT token exchange returned an incomplete response");
            Err(Ended::Failed(EXCHANGE_INCOMPLETE))
        }
    }
}

/// Why a renewal did not produce a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewFailure {
    /// The service refused the refresh token: the trader signs in again.
    Refused,
    /// The answer lacked a token.
    Incomplete,
    /// Nothing could be asked.
    Unreachable,
}

/// Trade the refresh token for a new access token (the web's
/// `refresh_access_token`, LiteLLM's `_refresh_tokens`): one bounded
/// request, never a loop. The new refresh token, when one is sent, comes
/// back for the caller to keep.
pub async fn renew(e: &Endpoints, refresh_token: &str) -> Result<Tokens, RenewFailure> {
    let reply = post(
        &e.token_url,
        Payload::Json(json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "scope": "openid profile email",
        })),
    )
    .await
    .map_err(|_| RenewFailure::Unreachable)?;
    if reply.status != 200 {
        tracing::error!("The ChatGPT renewal was refused (HTTP {})", reply.status);
        return Err(RenewFailure::Refused);
    }
    let get = |k: &str| text(&reply.body, k).map(Secret::new);
    match (get("access_token"), get("id_token")) {
        (Some(access), Some(id)) => Ok(Tokens {
            access,
            refresh: get("refresh_token"),
            id,
        }),
        _ => {
            tracing::error!("The ChatGPT renewal returned an incomplete response");
            Err(RenewFailure::Incomplete)
        }
    }
}
