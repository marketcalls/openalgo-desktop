//! API-key authentication for feed clients.
//!
//! Production goes through `ApiKeyService::is_valid` (HMAC index, Argon2,
//! bounded cache), run on the blocking pool because Argon2 is CPU-bound.

use crate::db::sqlite::user;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Key valid and a broker is connected.
    Ok {
        user_id: String,
        broker: String,
    },
    /// Key valid but no broker session: the web marks the socket
    /// authenticated and answers `BROKER_ERROR`.
    NoBroker {
        user_id: String,
    },
    Invalid,
}

#[async_trait::async_trait]
pub trait FeedAuth: Send + Sync + 'static {
    async fn authenticate(&self, api_key: &str) -> AuthOutcome;

    /// Broker adapter status reported by `get_broker_info`.
    fn adapter_status(&self) -> &'static str {
        "connected"
    }

    /// Whether a connection from `peer` is refused before anything else is
    /// read from it: a banned address on the network, from the same ban
    /// list every HTTP surface checks.
    fn refused(&self, _peer: std::net::IpAddr) -> bool {
        false
    }

    /// Count one `authenticate` from `caller`
    /// (`feed::server::handshake_source`) against the resource guard every
    /// request passes (well above legitimate use); `false` when over it.
    fn admit(&self, _caller: std::net::IpAddr) -> bool {
        true
    }

    /// Count a key that failed for `caller` against the failure budget
    /// `/api/v1` and `/mcp` share. It only ever refuses invalid
    /// credentials; a valid key is never refused by it.
    fn failed(&self, _caller: std::net::IpAddr) {}

    /// Whether `caller` has spent its failure budget: its failed
    /// `authenticate` is then answered and the connection closed.
    fn spent(&self, _caller: std::net::IpAddr) -> bool {
        false
    }
}

/// Authentication against the app's stored API key and broker session.
pub struct AppAuth {
    ctx: Arc<AppState>,
}

impl AppAuth {
    pub fn new(ctx: Arc<AppState>) -> Self {
        Self { ctx }
    }
}

#[async_trait::async_trait]
impl FeedAuth for AppAuth {
    /// Bans apply to devices on the network only (this computer and tunnel
    /// callers are never banned), compared as canonical addresses.
    fn refused(&self, peer: std::net::IpAddr) -> bool {
        let ip = crate::server::addr::canonical(peer);
        !ip.is_loopback() && self.ctx.monitor.is_banned(&ip.to_string(), self.ctx.now())
    }

    fn admit(&self, caller: std::net::IpAddr) -> bool {
        use crate::server::ratelimit::Bucket;
        let limiter = &self.ctx.limiter;
        limiter.check(Bucket::Guard, caller, limiter.now()).is_ok()
    }

    fn failed(&self, caller: std::net::IpAddr) {
        use crate::server::ratelimit::Bucket;
        crate::server::middleware::count_failure(&self.ctx, caller, Bucket::ApiKeyFail)
    }

    fn spent(&self, caller: std::net::IpAddr) -> bool {
        use crate::server::ratelimit::Bucket;
        crate::server::middleware::failures_exhausted(&self.ctx, caller, Bucket::ApiKeyFail)
    }

    async fn authenticate(&self, api_key: &str) -> AuthOutcome {
        let ctx = self.ctx.clone();
        let key = api_key.to_string();
        let checked = tokio::task::spawn_blocking(move || {
            if !ApiKeyService::is_valid(&ctx, &key) {
                return None;
            }
            let user_id = ctx
                .sqlite
                .conn()
                .ok()
                .and_then(|c| user::find_first(&c).ok().flatten())
                .map(|u| u.username)
                .or_else(|| ctx.signed_in_user())
                .unwrap_or_default();
            Some(user_id)
        })
        .await;
        let user_id = match checked {
            Ok(Some(u)) => u,
            Ok(None) => return AuthOutcome::Invalid,
            Err(e) => {
                tracing::error!("Feed API key check did not complete: {}", e);
                return AuthOutcome::Invalid;
            }
        };
        match self.ctx.get_broker_session() {
            Some(s) => AuthOutcome::Ok {
                user_id,
                broker: s.broker_id,
            },
            None => AuthOutcome::NoBroker { user_id },
        }
    }

    fn adapter_status(&self) -> &'static str {
        if self.ctx.is_broker_connected() {
            "connected"
        } else {
            "disconnected"
        }
    }
}

/// Fixed-answer authenticator for tests and the soak harness.
pub struct StaticAuth {
    pub api_key: String,
    pub user_id: String,
    pub broker: Option<String>,
}

#[async_trait::async_trait]
impl FeedAuth for StaticAuth {
    async fn authenticate(&self, api_key: &str) -> AuthOutcome {
        use subtle::ConstantTimeEq;
        if api_key
            .as_bytes()
            .ct_eq(self.api_key.as_bytes())
            .unwrap_u8()
            != 1
        {
            return AuthOutcome::Invalid;
        }
        match &self.broker {
            Some(b) => AuthOutcome::Ok {
                user_id: self.user_id.clone(),
                broker: b.clone(),
            },
            None => AuthOutcome::NoBroker {
                user_id: self.user_id.clone(),
            },
        }
    }
}
