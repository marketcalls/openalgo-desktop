//! The REST calls behind XTS subscriptions: one call's answer
//! (`subscription_call`) and one command's run of calls (`run_command`), at
//! most `SUBSCRIBE_BATCH` instruments per call, `SubscriptionTarget::gap`
//! apart.

use super::{mapping, release, SUBSCRIBE_BATCH};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;

/// A subscription request queued for the worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub subscribe: bool,
    pub code: u16,
    pub instruments: Vec<Value>,
    /// A release or an admission the manager waits on: answered; a release
    /// is never retried behind its caller's back (audit C01, R107-03).
    pub waited: Option<release::Waited>,
}

/// Whether the market-data token may be sent to `url`.
///
/// Every XTS member is served over TLS (web `broker/*/baseurl.py`: all
/// `https://`), so the `Authorization` token only ever travels encrypted.
/// Plain `http://` is accepted for loopback hosts only, which is what the
/// local test doubles use; any other URL is refused before a byte is sent.
pub fn token_transport_allowed(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

/// What one subscription call achieved.
#[derive(Debug, PartialEq, Eq)]
pub enum CallOutcome {
    /// Accepted; the snapshot strings of `listQuotes`.
    Done(Vec<String>),
    /// Refused because an instrument is already subscribed (non-fatal).
    AlreadySubscribed,
}

/// Why a subscription call failed (BF-03).
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// No answer, or a server error or rate limit: the batch is retried.
    Transient(String),
    /// The broker refused the request (a validation error): not retried.
    Refused(String),
    /// The market-data token is no longer accepted: the feed reconnects
    /// (a new token, then every subscription again) instead of retrying.
    InvalidToken,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transient(m) | CallError::Refused(m) => f.write_str(m),
            CallError::InvalidToken => f.write_str("the market data token was refused"),
        }
    }
}

/// XTS's refusal of an expired or unknown market-data token.
pub fn is_invalid_token(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("invalid token") || b.contains("token expired") || b.contains("invalid session")
}

/// XTS's "Instrument Already Subscribed" refusal (`e-session-0002`), in a
/// JSON description or a plain-text body.
pub fn is_already_subscribed(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("already subscribed") || b.contains("e-session-0002")
}

/// One subscription call.
pub async fn subscription_call(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    subscribe: bool,
    code: u16,
    instruments: &[Value],
) -> std::result::Result<CallOutcome, CallError> {
    if !token_transport_allowed(url) {
        tracing::error!("XTS subscription refused: market-data host is not served over TLS");
        return Err(CallError::Refused(
            "Live market data could not be started because the broker connection is not secure. Check the broker address in Broker Configuration.".into(),
        ));
    }
    let req = if subscribe {
        http.post(url)
    } else {
        http.put(url)
    };
    let transient = |e: reqwest::Error| {
        CallError::Transient(crate::brokers::common::redact::url_safe_error(&e))
    };
    let resp = req
        .header("Authorization", token)
        .header("Content-Type", "application/json")
        .json(&json!({"instruments": instruments, "xtsMessageCode": code}))
        .send()
        .await
        .map_err(transient)?;
    let status = resp.status();
    let body = resp.text().await.map_err(transient)?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if v.get("type").and_then(Value::as_str) != Some("success") {
        if !subscribe && release::is_not_subscribed(&body) {
            return Ok(CallOutcome::Done(Vec::new()));
        }
        if is_already_subscribed(&body) {
            return Ok(CallOutcome::AlreadySubscribed);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || is_invalid_token(&body) {
            return Err(CallError::InvalidToken);
        }
        let why = mapping::error_text(&v);
        let why = if why.is_empty() {
            "The broker refused the market data subscription.".to_string()
        } else {
            why
        };
        return Err(
            if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                CallError::Transient(why)
            } else {
                CallError::Refused(why)
            },
        );
    }
    Ok(CallOutcome::Done(
        v.get("result")
            .and_then(|r| r.get("listQuotes"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|q| match q {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    ))
}

/// Where one command's calls go, and how far apart.
pub struct SubscriptionTarget<'a> {
    pub broker: &'static str,
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    pub token: &'a str,
    pub gap: Duration,
    /// `XtsHooks::split_duplicate_batch` of the member.
    pub split_duplicates: bool,
}

/// What running one command left to do.
#[derive(Debug, Default, PartialEq)]
pub struct CommandReport {
    /// Calls made.
    pub calls: usize,
    /// Instruments of the batches that failed for a passing reason (no
    /// answer, a server error, a rate limit), to send again.
    pub retry: Vec<Value>,
    /// The token was refused: the feed must reconnect.
    pub reconnect: bool,
    /// Instruments of the batches the broker refused, with why.
    pub refused: Vec<(Value, String)>,
}

/// Run one command: at most `SUBSCRIBE_BATCH` instruments per call, `gap`
/// between calls. With `split_duplicates`, a subscribe batch refused as
/// already subscribed is retried one instrument at a time; without it the
/// refusal is non-fatal. A batch that failed for a passing reason is
/// reported for a retry (BF-03; the web retries rmoney's on a timer).
pub async fn run_command(
    t: &SubscriptionTarget<'_>,
    cmd: &Command,
    snapshots: &mpsc::Sender<String>,
    first: &mut bool,
) -> CommandReport {
    let mut calls = 0;
    let mut call = |batch: Vec<Value>| {
        let wait = !*first;
        *first = false;
        calls += 1;
        async move {
            if wait {
                tokio::time::sleep(t.gap).await;
            }
            let r =
                subscription_call(t.http, t.url, t.token, cmd.subscribe, cmd.code, &batch).await;
            (batch, r)
        }
    };
    let mut outcomes = Vec::new();
    for batch in cmd.instruments.chunks(SUBSCRIBE_BATCH) {
        let (batch, r) = call(batch.to_vec()).await;
        match r {
            // One refused instrument must not refuse its batch when each
            // has an owner waiting for its own answer (R107-03).
            Err(CallError::Refused(_))
                if cmd.subscribe && cmd.waited.is_some() && batch.len() > 1 =>
            {
                for inst in batch {
                    outcomes.push(call(vec![inst]).await);
                }
            }
            Ok(CallOutcome::AlreadySubscribed)
                if t.split_duplicates && cmd.subscribe && batch.len() > 1 =>
            {
                tracing::info!(
                    broker = t.broker,
                    code = cmd.code,
                    "Batch holds an instrument already subscribed; subscribing one by one"
                );
                for inst in batch {
                    let (one, r) = call(vec![inst]).await;
                    outcomes.push((one, r));
                }
            }
            r => outcomes.push((batch, r)),
        }
    }
    let mut report = CommandReport::default();
    for (batch, r) in outcomes {
        match r {
            Ok(CallOutcome::Done(list)) => {
                if cmd.subscribe {
                    for s in list {
                        let _ = snapshots.try_send(s);
                    }
                }
            }
            Ok(CallOutcome::AlreadySubscribed) => {
                tracing::debug!(broker = t.broker, "Instrument already subscribed")
            }
            Err(CallError::Transient(e)) => {
                tracing::warn!(
                    broker = t.broker,
                    code = cmd.code,
                    subscribe = cmd.subscribe,
                    "Subscription call failed, will retry: {}",
                    e
                );
                report.retry.extend(batch);
            }
            Err(CallError::InvalidToken) => {
                tracing::warn!(
                    broker = t.broker,
                    "Subscription refused the market data token; reconnecting"
                );
                report.reconnect = true;
                let why = release::TOKEN_REFUSED.to_string();
                report
                    .refused
                    .extend(batch.into_iter().map(|i| (i, why.clone())));
            }
            Err(CallError::Refused(e)) => {
                tracing::warn!(
                    broker = t.broker,
                    code = cmd.code,
                    subscribe = cmd.subscribe,
                    "Subscription call refused: {}",
                    e
                );
                report
                    .refused
                    .extend(batch.into_iter().map(|i| (i, e.clone())));
            }
        }
    }
    report.calls = calls;
    report
}
