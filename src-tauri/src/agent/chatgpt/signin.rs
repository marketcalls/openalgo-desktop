//! The one sign-in this app runs at a time (the web's `start_login`,
//! `cancel_login`, `login_status`, `forget` and the poll thread).
//!
//! A start makes one bounded request for a code and returns it at once; the
//! fifteen-minute poll runs as a task the app owns, so no request handler
//! ever waits on it. A second start while one is pending returns the one
//! in flight (its code may already be half typed); `force` replaces it.
//! A start that is still asking for its code holds a claim, so a double
//! click asks once. A replaced poll can never overwrite the newer sign-in's
//! snapshot: every write names the flight it belongs to.
//!
//! The poll stops when the trader cancels or signs the plan out, and when
//! the app signs out, locks, resets or quits ([`SignIn::stop`], through
//! the Agent's `stop_all`). The code is shown on the trader's screen and
//! written to no log line.

use super::flow::{self, Device, Ended, Flow};
use super::record::{self, AuthRecord};
use crate::state::AppState;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long a cancel waits for the poll to finish (the web's
/// `_CANCEL_JOIN_SECONDS`).
const CANCEL_WAIT: Duration = Duration::from_secs(20);

pub const AUTHORISED: &str = "ChatGPT subscription authorised.";
pub const NOT_SAVED: &str =
    "The sign-in was approved but could not be saved in OpenAlgo's data. Try again.";
pub const UNEXPECTED: &str = "The sign-in failed unexpectedly. Try again.";
pub const CANCELLED: &str = "Sign-in cancelled.";

/// Where a sign-in is. `Pending` is the only state that is not final.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    Pending,
    Authorised,
    Expired,
    Failed,
    Cancelled,
}

impl LoginState {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginState::Idle => "idle",
            LoginState::Pending => "pending",
            LoginState::Authorised => "authorised",
            LoginState::Expired => "expired",
            LoginState::Failed => "failed",
            LoginState::Cancelled => "cancelled",
        }
    }
}

/// A snapshot of the sign-in (the web's `LoginStatus`). The code is blank
/// in every state but `Pending`.
#[derive(Clone, PartialEq)]
pub struct LoginStatus {
    pub state: LoginState,
    pub user_code: String,
    pub verification_url: String,
    /// Unix seconds.
    pub started_at: Option<f64>,
    pub expires_at: Option<f64>,
    pub message: String,
}

impl std::fmt::Debug for LoginStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginStatus")
            .field("state", &self.state)
            .field("started_at", &self.started_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl LoginStatus {
    pub fn idle() -> LoginStatus {
        LoginStatus {
            state: LoginState::Idle,
            user_code: String::new(),
            verification_url: String::new(),
            started_at: None,
            expires_at: None,
            message: String::new(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "user_code": self.user_code,
            "verification_url": self.verification_url,
            "started_at": self.started_at,
            "expires_at": self.expires_at,
            "message": self.message,
        })
    }

    fn end(&mut self, state: LoginState, message: &str) {
        self.state = state;
        self.user_code.clear();
        self.message = message.to_string();
    }
}

/// One running poll.
#[derive(Clone)]
struct Flight {
    id: u64,
    /// Set to stop the poll.
    cancel: CancellationToken,
    /// Set by the poll when it has finished, however it finished.
    done: CancellationToken,
}

struct Inner {
    login: LoginStatus,
    /// A start is asking for its code.
    claimed: bool,
    flight: Option<Flight>,
    next: u64,
}

/// The plan's sign-in.
pub struct SignIn {
    pub(super) flow: Flow,
    inner: Mutex<Inner>,
    /// One renewal at a time: a rotated refresh token must not be spent twice.
    pub(super) renewal: tokio::sync::Mutex<()>,
}

/// Seconds since the epoch on the app's clock.
pub fn unix(app: &AppState) -> f64 {
    app.now().timestamp_millis() as f64 / 1000.0
}

/// Releases a start's claim on every path out.
struct Claim<'a>(&'a SignIn);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.inner.lock().claimed = false;
    }
}

impl SignIn {
    pub fn new(flow: Flow) -> SignIn {
        SignIn {
            flow,
            inner: Mutex::new(Inner {
                login: LoginStatus::idle(),
                claimed: false,
                flight: None,
                next: 0,
            }),
            renewal: tokio::sync::Mutex::new(()),
        }
    }

    /// The snapshot now: a copy, no database, no network (what the panel
    /// polls).
    pub fn snapshot(&self) -> LoginStatus {
        self.inner.lock().login.clone()
    }

    /// Claim a start, or answer the snapshot a start must hand back: one
    /// already asking for a code, or (without `force`) a pending sign-in.
    /// `Err(None)` asks the caller to stop the running poll first.
    fn claim(&self, force: bool) -> Result<(), Option<LoginStatus>> {
        let mut g = self.inner.lock();
        if g.claimed {
            return Err(Some(g.login.clone()));
        }
        let running = g.flight.is_some() && g.login.state == LoginState::Pending;
        if running {
            return Err((!force).then(|| g.login.clone()));
        }
        g.claimed = true;
        g.login.state = LoginState::Pending;
        g.login.user_code.clear();
        g.login.message = flow::REQUESTING.to_string();
        Ok(())
    }

    /// Start a sign-in and return the code to show, or the failed snapshot
    /// when no code could be had (the route's 502).
    pub async fn start(
        self: &Arc<Self>,
        app: &Arc<AppState>,
        force: bool,
    ) -> Result<LoginStatus, LoginStatus> {
        match self.claim(force) {
            Ok(()) => {}
            Err(Some(snapshot)) => return Ok(snapshot),
            Err(None) => {
                self.cancel().await;
                if let Err(Some(snapshot)) = self.claim(false) {
                    return Ok(snapshot);
                }
            }
        }
        let _claim = Claim(self);
        let started = unix(app);
        let device = match flow::request_code(&self.flow.endpoints).await {
            Ok(d) => d,
            Err(message) => {
                let mut g = self.inner.lock();
                g.login = LoginStatus {
                    state: LoginState::Failed,
                    user_code: String::new(),
                    verification_url: self.flow.endpoints.verify.clone(),
                    started_at: Some(started),
                    expires_at: None,
                    message: message.to_string(),
                };
                return Err(g.login.clone());
            }
        };
        let (snapshot, flight) = self.publish(started, &device);
        let task = self.clone();
        let weak = Arc::downgrade(app);
        app.spawn(async move { task.poll(weak, flight, device).await });
        tracing::info!(
            "ChatGPT subscription sign-in started; waiting up to {} s for approval",
            self.flow.lifetime.as_secs()
        );
        Ok(snapshot)
    }

    /// The code is out: show it and register its poll.
    fn publish(&self, started: f64, device: &Device) -> (LoginStatus, Flight) {
        let mut g = self.inner.lock();
        g.next += 1;
        let flight = Flight {
            id: g.next,
            cancel: CancellationToken::new(),
            done: CancellationToken::new(),
        };
        g.login = LoginStatus {
            state: LoginState::Pending,
            user_code: device.user_code.expose().to_string(),
            verification_url: self.flow.endpoints.verify.clone(),
            started_at: Some(started),
            expires_at: Some(started + self.flow.lifetime.as_secs_f64()),
            message: String::new(),
        };
        g.flight = Some(flight.clone());
        (g.login.clone(), flight)
    }

    /// The poll: wait for approval, keep the credential, say how it ended.
    async fn poll(self: Arc<Self>, app: Weak<AppState>, flight: Flight, device: Device) {
        let _done = flight.done.clone().drop_guard();
        let outcome = tokio::select! {
            biased;
            _ = flight.cancel.cancelled() => None,
            r = flow::await_approval(&self.flow, &device) => Some(r),
        };
        let ending = match outcome {
            None => None,
            Some(Err(Ended::Expired)) => {
                tracing::info!("The ChatGPT subscription sign-in code expired unapproved");
                Some((LoginState::Expired, flow::EXPIRED))
            }
            Some(Err(Ended::Failed(message))) => Some((LoginState::Failed, message)),
            // Approved after a cancel: nothing is kept.
            Some(Ok(_)) if flight.cancel.is_cancelled() => None,
            Some(Ok(tokens)) => Some(self.keep(&app, tokens).await),
        };
        if let Some((state, message)) = ending {
            self.end_if_current(flight.id, state, message);
        }
        self.retire(flight.id);
    }

    /// Store an approved sign-in (and read it back).
    async fn keep(&self, app: &Weak<AppState>, tokens: flow::Tokens) -> (LoginState, &'static str) {
        let Some(app) = app.upgrade() else {
            return (LoginState::Failed, NOT_SAVED);
        };
        let record = AuthRecord::from_tokens(tokens.access, tokens.refresh, tokens.id);
        let stored = app
            .blocking(move |c| {
                let conn = c.sqlite.conn()?;
                Ok(record::store(&conn, &c.security, &record).is_ok())
            })
            .await;
        match stored {
            Ok(true) => {
                tracing::info!("ChatGPT subscription authorised");
                (LoginState::Authorised, AUTHORISED)
            }
            Ok(false) => (LoginState::Failed, NOT_SAVED),
            Err(_) => {
                tracing::error!("The ChatGPT subscription sign-in could not be stored");
                (LoginState::Failed, UNEXPECTED)
            }
        }
    }

    /// End the sign-in, but only if it is still this flight's.
    fn end_if_current(&self, id: u64, state: LoginState, message: &str) {
        let mut g = self.inner.lock();
        if g.flight.as_ref().map(|f| f.id) == Some(id) {
            g.login.end(state, message);
        }
    }

    fn retire(&self, id: u64) {
        let mut g = self.inner.lock();
        if g.flight.as_ref().map(|f| f.id) == Some(id) {
            g.flight = None;
        }
    }

    /// Stop a sign-in in flight and wait (up to 20 s) for its poll to end;
    /// true when a running one stopped. A sign-in that was approved in the
    /// meantime stays authorised.
    pub async fn cancel(&self) -> bool {
        let flight = self.inner.lock().flight.clone();
        let Some(f) = flight.filter(|f| !f.done.is_cancelled()) else {
            return false;
        };
        f.cancel.cancel();
        let stopped = tokio::time::timeout(CANCEL_WAIT, f.done.cancelled())
            .await
            .is_ok();
        if !stopped {
            tracing::warn!("The ChatGPT sign-in did not stop within the cancel wait");
        }
        let mut g = self.inner.lock();
        if g.flight.as_ref().map(|x| x.id) == Some(f.id) {
            g.flight = None;
        }
        if g.login.state == LoginState::Pending {
            g.login.end(LoginState::Cancelled, CANCELLED);
            tracing::info!("ChatGPT subscription sign-in cancelled");
        }
        stopped
    }

    /// The app signs out, locks, resets or quits: the poll stops now,
    /// without waiting, and keeps nothing it receives afterwards.
    pub fn stop(&self) {
        let mut g = self.inner.lock();
        if let Some(f) = g.flight.take() {
            f.cancel.cancel();
        }
        if g.login.state == LoginState::Pending && !g.claimed {
            g.login.end(LoginState::Cancelled, CANCELLED);
        }
    }

    /// Sign the plan out: stop a sign-in in flight, delete the stored
    /// credential, and describe no sign-in at all. True when one was
    /// removed. Registered `chatgpt/` models are left alone.
    pub async fn forget(&self, app: &Arc<AppState>) -> crate::error::Result<bool> {
        self.cancel().await;
        let removed = app.blocking(|c| record::delete(&*c.sqlite.conn()?)).await?;
        self.inner.lock().login = LoginStatus::idle();
        if removed {
            tracing::info!("ChatGPT subscription signed out");
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::Secret;

    fn device(code: &str) -> Device {
        Device {
            device_auth_id: Secret::new("dev"),
            user_code: Secret::new(code),
            interval: None,
        }
    }

    /// A poll that was replaced (a forced restart) cannot end the newer
    /// sign-in, nor drop its handle.
    #[test]
    fn a_replaced_poll_cannot_overwrite_the_newer_sign_in() {
        let s = SignIn::new(Flow::default());
        let (_, old) = s.publish(1.0, &device("OLD-1"));
        let (_, new) = s.publish(2.0, &device("NEW-2"));
        s.end_if_current(old.id, LoginState::Expired, flow::EXPIRED);
        s.retire(old.id);
        let now = s.snapshot();
        assert_eq!(
            (now.state, now.user_code.as_str()),
            (LoginState::Pending, "NEW-2")
        );
        assert!(s.inner.lock().flight.is_some());
        s.end_if_current(new.id, LoginState::Authorised, AUTHORISED);
        assert_eq!(s.snapshot().state, LoginState::Authorised);
    }
}
