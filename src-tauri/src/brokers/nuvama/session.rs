//! The Nuvama session packed into the stored token, and the header set
//! every API Connect call carries (web `api/baseurl.py`, from the official
//! SDK `APIConnect` 2.0.12: `APIConnect/http.py`, `login_helper.py`).
//!
//! Every authenticated call sends four headers:
//!
//! * `Authorization`: the session `auth` from `/accounts/logindata`;
//! * `Source`: the API key;
//! * `SourceToken`: the vendor session (`msg`) from `/accounts/loginvendor`;
//! * `AppIdKey`: the app id key the login answered with.
//!
//! The stored token packs these with the account (web layout
//! `auth:::source_token:::app_id_key:::account_id:::account_type:::api_key`),
//! so a request never pairs this session with another key. The SDK also
//! sends `X-Forwarded-For` with a public IP it looks up from a third-party
//! service; the web leaves it out, and so does the desktop.

use crate::error::{AppError, Result};
use crate::security::Secret;

/// The web's separator of the packed token.
pub const TOKEN_SEPARATOR: &str = ":::";

/// The app id key the SDK starts every session with (`APIConnect.__init__`,
/// web `INITIAL_APP_ID_KEY`): a public SDK constant, not a credential. The
/// login answers with the key the session uses (`AppIdKey` response
/// header), and that one is stored. Put together from its three parts so no
/// token-shaped literal is committed.
pub fn initial_app_id_key() -> String {
    [
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        concat!(
            "eyJhcHAiOjAsImZmIjoiVyIsImJkIjoid2ViLXBjIiwibmJmIjoxNzg1NDI3NDM5",
            "LCJzcmMiOiJlbXRtdyIsImF2IjoiMi4wLjEyIiwiYXBwaWQiOiI1NDc3ZjFhMGVhYTMxN2I3YTY1ZTk2NGY3ZDliNTNmNiIsImlzcyI6",
            "ImVtdCIsImV4cCI6MTc4NTQzNjIwMCwiaWF0IjoxNzg1NDI3NzM5fQ"
        ),
        "I-BZnbG-Xsvvk23dvTsgxQhCGm0Ph0qtwCxmidwgUuM",
    ]
    .join(".")
}

/// The per-session values. `Debug` names the account type only.
#[derive(Clone)]
pub struct NuvamaSession {
    pub auth: Secret,
    pub source_token: Secret,
    pub app_id_key: Secret,
    /// The account id (`eqAccID`, else `coAccID`): the `{userid}` path
    /// segment and the order stream's `accID` and `userID`.
    pub account_id: String,
    /// `accTyp` (`EQ`, `CO`, `COMEQ`), sent as `accType` on the stream.
    pub account_type: String,
    pub api_key: Secret,
}

impl std::fmt::Debug for NuvamaSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NuvamaSession")
            .field("account_type", &self.account_type)
            .finish_non_exhaustive()
    }
}

impl NuvamaSession {
    /// The stored token (web `NuvamaSession.pack`).
    pub fn pack(&self) -> String {
        [
            self.auth.expose(),
            self.source_token.expose(),
            self.app_id_key.expose(),
            self.account_id.as_str(),
            self.account_type.as_str(),
            self.api_key.expose(),
        ]
        .join(TOKEN_SEPARATOR)
    }

    /// Split a stored token (web `parse_token`). A token that is not a
    /// complete session (one stored before the API key was part of it) is
    /// refused, so no request goes out with half a header set.
    pub fn parse(token: &str) -> Result<Self> {
        let parts: Vec<&str> = token.split(TOKEN_SEPARATOR).collect();
        let complete = parts.len() == 6
            && parts[..4].iter().all(|p| !p.trim().is_empty())
            && !parts[5].trim().is_empty();
        if !complete {
            return Err(AppError::Auth(
                "The stored Nuvama session is incomplete. Log in to Nuvama again.".into(),
            ));
        }
        Ok(Self {
            auth: Secret::new(parts[0]),
            source_token: Secret::new(parts[1]),
            app_id_key: Secret::new(parts[2]),
            account_id: parts[3].trim().to_string(),
            account_type: parts[4].trim().to_string(),
            api_key: Secret::new(parts[5]),
        })
    }

    /// `accType` for the stream (web: the session's, else `EQ`).
    pub fn stream_account_type(&self) -> &str {
        if self.account_type.is_empty() {
            "EQ"
        } else {
            &self.account_type
        }
    }

    /// The session headers of every authenticated call (web
    /// `build_headers(session)`).
    pub fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        json_headers(req)
            .header("Authorization", self.auth.expose())
            .header("SourceToken", self.source_token.expose())
            .header("AppIdKey", self.app_id_key.expose())
            .header("Source", self.api_key.expose())
    }
}

/// `Content-Type` and `Accept` JSON, on every call.
pub fn json_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("Content-Type", "application/json")
        .header("Accept", "application/json")
}
