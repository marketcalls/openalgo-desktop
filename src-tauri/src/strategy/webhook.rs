//! The public `/strategy/webhook/<token>` pipeline (web
//! `services/strategy_module/webhook.py` and `webhook_bridge.py`).
//!
//! The URL token is the whole credential (TradingView cannot set a header).
//! It is stored only as a SHA-256 digest, never logged, never written to the
//! audit row (the payload is redacted first), and an unknown token answers
//! exactly like a malformed one.
//!
//! Stages, in order, each audited in `sm_webhook_event`:
//!
//! | Stage | Result | Status |
//! |---|---|---|
//! | token resolves | `rejected_token` | 404 |
//! | kill switch off | `rejected_locked` | 403 |
//! | IP in allowlist | `rejected_ip` | 403 |
//! | payload parses and fits | `rejected_payload` | 400 |
//! | action valid for this kind | `rejected_invalid_action` | 400 |
//! | start names a mode | `rejected_invalid_action` | 400 |
//! | live opt-in | `rejected_live_disabled` | 403 |
//! | not a duplicate (batch) | `rejected_dedupe` | 200, ok |
//! | not cooling off (batch) | `rejected_cooling_off` | 409 |
//! | engine acted | `rejected_engine_error` | 500 |
//!
//! Signal actions skip the dedupe and cooling-off windows: signal mode is
//! idempotent by meaning, and a 60 second window would suppress a genuine
//! long, short, long sequence.

use super::store::{hash_webhook_token, StrategyRow, RUN_MODES, WEBHOOK_TOKEN_PREFIX};
use super::StrategyModule;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const MAX_PAYLOAD_BYTES: usize = 16384;
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(60);
pub const COOLING_OFF: Duration = Duration::from_secs(30);
pub const MAX_TRACKED_KEYS: usize = 4096;
/// `WEBHOOK_RATE_LIMIT`: 100 per minute, by caller address and by token.
pub const RATE_LIMIT: usize = 100;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

const UNKNOWN_TOKEN_MESSAGE: &str = "Unknown or expired webhook token";
const REDACTED: &str = "[redacted]";
const SECRET_KEY_HINTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "auth",
    "signature",
];
const MAX_AUDIT_ITEMS: usize = 50;
const MAX_AUDIT_STRING: usize = 500;
const MAX_AUDIT_DEPTH: usize = 4;

pub fn result_status(result: &str) -> u16 {
    match result {
        "ok" | "rejected_dedupe" => 200,
        "rejected_token" => 404,
        "rejected_locked" | "rejected_ip" | "rejected_live_disabled" => 403,
        "rejected_cooling_off" => 409,
        "rejected_engine_error" => 500,
        "rate_limited" => 429,
        _ => 400,
    }
}

/// The decision, ready for the route to turn into a response.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookOutcome {
    pub ok: bool,
    pub result: String,
    pub status: u16,
    pub message: String,
    pub strategy_id: Option<i64>,
    pub run_id: Option<i64>,
    pub webhook_event_id: Option<i64>,
    pub stop_pending: Option<bool>,
    pub exits: Vec<Value>,
}

impl WebhookOutcome {
    fn new(result: &str, message: impl Into<String>, ok: bool) -> Self {
        Self {
            ok,
            result: result.into(),
            status: result_status(result),
            message: message.into(),
            strategy_id: None,
            run_id: None,
            webhook_event_id: None,
            stop_pending: None,
            exits: vec![],
        }
    }

    /// The JSON body. Carries nothing a caller has not already proved.
    pub fn body(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            "status".into(),
            json!(if self.ok { "success" } else { "error" }),
        );
        m.insert("result".into(), json!(self.result));
        m.insert("message".into(), json!(self.message));
        if let Some(s) = self.strategy_id {
            m.insert("strategy_id".into(), json!(s));
        }
        if let Some(r) = self.run_id {
            m.insert("run_id".into(), json!(r));
        }
        if let Some(p) = self.stop_pending {
            m.insert("stop_pending".into(), json!(p));
            m.insert("exits".into(), json!(self.exits));
        }
        Value::Object(m)
    }
}

/// Bounded in-memory windows: dedupe, cooling off and the two rate limits.
pub struct WebhookState {
    dedupe: Mutex<HashMap<(i64, String, Option<String>), Instant>>,
    cooling: Mutex<HashMap<i64, Instant>>,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Added to `Instant::now()`; tests drive the windows without sleeping.
    offset: Mutex<Duration>,
}

impl Default for WebhookState {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookState {
    pub fn new() -> Self {
        Self {
            dedupe: Mutex::new(HashMap::new()),
            cooling: Mutex::new(HashMap::new()),
            rate: Mutex::new(HashMap::new()),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn now(&self) -> Instant {
        Instant::now() + *self.offset.lock()
    }

    /// Move the windows' clock forward (tests).
    pub fn advance(&self, by: Duration) {
        *self.offset.lock() += by;
    }

    /// Drop every window.
    pub fn reset(&self) {
        self.dedupe.lock().clear();
        self.cooling.lock().clear();
        self.rate.lock().clear();
    }

    /// Arm the cooling-off window for a strategy whose run just ended (every
    /// stop, not just webhook ones).
    pub fn note_run_stopped(&self, strategy_id: i64, _at: Instant) {
        let now = self.now();
        let mut c = self.cooling.lock();
        c.retain(|_, at| now.saturating_duration_since(*at) < COOLING_OFF);
        if c.len() >= MAX_TRACKED_KEYS {
            c.clear();
        }
        c.insert(strategy_id, now);
    }

    fn cooling_remaining(&self, strategy_id: i64, now: Instant) -> u64 {
        match self.cooling.lock().get(&strategy_id) {
            Some(at) => {
                let elapsed = now.saturating_duration_since(*at);
                if elapsed >= COOLING_OFF {
                    0
                } else {
                    (COOLING_OFF - elapsed).as_secs().max(1)
                }
            }
            None => 0,
        }
    }

    /// Count one hit against `key`; false when over the limit.
    pub fn rate_check(&self, key: &str) -> bool {
        let now = self.now();
        let mut map = self.rate.lock();
        if map.len() >= MAX_TRACKED_KEYS {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.saturating_duration_since(*t) < RATE_WINDOW)
            });
            if map.len() >= MAX_TRACKED_KEYS {
                map.clear();
            }
        }
        let q = map.entry(key.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= RATE_WINDOW)
        {
            q.pop_front();
        }
        if q.len() >= RATE_LIMIT {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Keys held across every window (hygiene tests).
    pub fn tracked(&self) -> usize {
        self.dedupe.lock().len() + self.cooling.lock().len() + self.rate.lock().len()
    }
}

/// The two route-level limits: by caller address (stops token walking) and
/// by token digest (caps what one leaked token can do). The raw token is
/// never a key.
pub fn rate_limited(state: &WebhookState, ip: IpAddr, token: &str) -> bool {
    !state.rate_check(&format!("ip:{}", ip))
        || !state.rate_check(&format!("token:{}", hash_webhook_token(token)))
}

fn looks_like_token(token: &str) -> bool {
    let Some(body) = token.strip_prefix(WEBHOOK_TOKEN_PREFIX) else {
        return false;
    };
    (16..=128).contains(&body.len())
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether `ip` falls inside the allowlist. Empty allows everything; a
/// non-empty list is closed, a malformed entry is skipped.
pub fn ip_allowed(ip: Option<&str>, allowlist: &Value) -> bool {
    let entries: Vec<&str> = allowlist
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        return true;
    }
    let Some(addr) = ip.and_then(|i| i.trim().parse::<IpAddr>().ok()) else {
        return false;
    };
    let mut candidates = vec![addr];
    if let IpAddr::V6(v6) = addr {
        if let Some(v4) = v6.to_ipv4_mapped() {
            candidates.push(IpAddr::V4(v4));
        }
    }
    entries.iter().any(|entry| match parse_network(entry) {
        Some((net, prefix)) => candidates.iter().any(|c| in_network(*c, net, prefix)),
        None => {
            tracing::warn!("Skipping a malformed webhook allowlist entry");
            false
        }
    })
}

/// `addr` or `addr/prefix`, read like Python's `ip_network(strict=False)`.
pub fn parse_network(entry: &str) -> Option<(IpAddr, u8)> {
    let (a, p) = match entry.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (entry, None),
    };
    let addr: IpAddr = a.trim().parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match p {
        Some(p) => p.trim().parse::<u8>().ok().filter(|p| *p <= max)?,
        None => max,
    };
    Some((addr, prefix))
}

fn in_network(c: IpAddr, net: IpAddr, prefix: u8) -> bool {
    match (c, net) {
        (IpAddr::V4(c), IpAddr::V4(n)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            (u32::from(c) & mask) == (u32::from(n) & mask)
        }
        (IpAddr::V6(c), IpAddr::V6(n)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            (u128::from(c) & mask) == (u128::from(n) & mask)
        }
        _ => false,
    }
}

/// Read the body into an object, or say why it cannot be one.
pub fn parse_payload(body: &[u8]) -> Result<Map<String, Value>, String> {
    if body.len() > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "The request body is larger than {} bytes",
            MAX_PAYLOAD_BYTES
        ));
    }
    let text =
        std::str::from_utf8(body).map_err(|_| "The request body is not valid UTF-8".to_string())?;
    let text = text.trim();
    if text.is_empty() {
        return Err("The request body is empty".into());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("The request body must be a JSON object".into()),
        Err(_) => Err("The request body is not valid JSON".into()),
    }
}

/// A copy of the payload safe to store: credential-named keys lose their
/// value, any string carrying the token (or the token prefix) is replaced,
/// and depth, item count and length are capped.
pub fn redact(value: &Value, token: &str, depth: usize) -> Value {
    if depth > MAX_AUDIT_DEPTH {
        return json!("[truncated]");
    }
    match value {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m.iter().take(MAX_AUDIT_ITEMS) {
                let name: String = k.chars().take(MAX_AUDIT_STRING).collect();
                let lower = name.to_ascii_lowercase();
                if SECRET_KEY_HINTS.iter().any(|h| lower.contains(h)) {
                    out.insert(name, json!(REDACTED));
                } else {
                    out.insert(name, redact(v, token, depth + 1));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .take(MAX_AUDIT_ITEMS)
                .map(|v| redact(v, token, depth + 1))
                .collect(),
        ),
        Value::String(s) => {
            if (!token.is_empty() && s.contains(token)) || s.contains(WEBHOOK_TOKEN_PREFIX) {
                json!(REDACTED)
            } else {
                json!(s.chars().take(MAX_AUDIT_STRING).collect::<String>())
            }
        }
        other => other.clone(),
    }
}

struct Audit<'a> {
    module: &'a StrategyModule,
    ip: Option<&'a str>,
    ua: Option<&'a str>,
}

impl Audit<'_> {
    fn write(
        &self,
        result: &str,
        strategy_id: Option<i64>,
        action: Option<&str>,
        mode: Option<&str>,
        payload: Option<&Value>,
        error: Option<&str>,
    ) -> Option<i64> {
        match self.module.store.record_webhook_event(
            result,
            strategy_id,
            action,
            mode,
            payload,
            self.ip,
            self.ua,
            error,
        ) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!("Could not audit a webhook ({}): {}", result, e);
                None
            }
        }
    }
}

impl StrategyModule {
    /// The canonical answer to a token that resolves to nothing.
    pub fn unknown_token_outcome(&self, ip: Option<&str>, ua: Option<&str>) -> WebhookOutcome {
        let id = Audit {
            module: self,
            ip,
            ua,
        }
        .write("rejected_token", None, None, None, None, None);
        let mut o = WebhookOutcome::new("rejected_token", UNKNOWN_TOKEN_MESSAGE, false);
        o.webhook_event_id = id;
        o
    }

    /// Run one inbound alert through the pipeline and act on it, or refuse
    /// it. Never fails: an unexpected error is `rejected_engine_error`.
    pub async fn handle_webhook(
        &self,
        token: &str,
        body: &[u8],
        ip: Option<&str>,
        ua: Option<&str>,
    ) -> WebhookOutcome {
        let audit = Audit {
            module: self,
            ip,
            ua,
        };
        // 1. Token: shape first (no lookup for a non-token), then the digest.
        if !looks_like_token(token) {
            return self.unknown_token_outcome(ip, ua);
        }
        let strategy = match self.store.get_strategy_by_webhook_token(token) {
            Ok(Some(s)) => s,
            Ok(None) => return self.unknown_token_outcome(ip, ua),
            Err(e) => {
                tracing::error!("Could not resolve a webhook token: {}", e);
                return self.unknown_token_outcome(ip, ua);
            }
        };
        let sid = strategy.id;
        let with = |mut o: WebhookOutcome, id: Option<i64>| {
            o.strategy_id = Some(sid);
            o.webhook_event_id = id;
            o
        };

        // 2. Kill switch, ahead of everything a caller controls.
        if strategy.webhook_locked {
            let id = audit.write(
                "rejected_locked",
                Some(sid),
                None,
                None,
                None,
                Some("The webhook kill switch is engaged"),
            );
            return with(
                WebhookOutcome::new(
                    "rejected_locked",
                    "This strategy's webhook is locked",
                    false,
                ),
                id,
            );
        }
        // 3. Address, before the body is parsed.
        if !ip_allowed(ip, &strategy.webhook_ip_allowlist) {
            let id = audit.write(
                "rejected_ip",
                Some(sid),
                None,
                None,
                None,
                Some("The caller address is outside the allowlist"),
            );
            return with(
                WebhookOutcome::new(
                    "rejected_ip",
                    "This address is not allowed to trigger this strategy",
                    false,
                ),
                id,
            );
        }
        // 4. Payload.
        let payload = match parse_payload(body) {
            Ok(p) => p,
            Err(e) => {
                let id = audit.write("rejected_payload", Some(sid), None, None, None, Some(&e));
                return with(WebhookOutcome::new("rejected_payload", e, false), id);
            }
        };
        let safe = redact(&Value::Object(payload.clone()), token, 0);

        // 5. Action, by kind.
        let allowed = super::signals::actions_for(&strategy.strategy_kind);
        let raw_action = payload.get("action");
        let action = raw_action
            .and_then(Value::as_str)
            .map(|a| a.trim().to_ascii_lowercase());
        let Some(action) = action.filter(|a| allowed.contains(&a.as_str())) else {
            let shown = match raw_action {
                Some(Value::String(s)) => Some(s.trim().to_ascii_lowercase()),
                Some(Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            let id = audit.write(
                "rejected_invalid_action",
                Some(sid),
                shown.as_deref(),
                None,
                Some(&safe),
                Some("Unrecognised action"),
            );
            return with(
                WebhookOutcome::new(
                    "rejected_invalid_action",
                    format!("'action' must be one of {}", allowed.join(", ")),
                    false,
                ),
                id,
            );
        };

        // 5b. Signal mode branches out before the batch-only stages.
        if super::signals::SIGNAL_ACTIONS.contains(&action.as_str()) {
            let leg_id = payload.get("leg_id");
            let symbol = payload.get("symbol").and_then(Value::as_str);
            let exchange = payload.get("exchange").and_then(Value::as_str);
            let r = self
                .handle_signal(&strategy, &action, leg_id, symbol, exchange)
                .await;
            if !r.ok {
                let err = r
                    .error
                    .clone()
                    .unwrap_or_else(|| "The signal was refused".into());
                let id = audit.write(
                    "rejected_invalid_action",
                    Some(sid),
                    Some(&action),
                    None,
                    Some(&safe),
                    Some(&err),
                );
                return with(
                    WebhookOutcome::new("rejected_invalid_action", err, false),
                    id,
                );
            }
            let id = audit.write("ok", Some(sid), Some(&action), None, Some(&safe), None);
            let message = match &r.note {
                Some(n) => format!("Signal accepted ({})", n),
                None => "Signal accepted".into(),
            };
            let mut o = with(WebhookOutcome::new("ok", message, true), id);
            o.run_id = r.run_id;
            return o;
        }

        // 6. Mode, required by start, ignored by stop.
        let mut mode: Option<String> = None;
        if action == "start" {
            let raw = payload.get("mode");
            let m = raw
                .and_then(Value::as_str)
                .map(|m| m.trim().to_ascii_lowercase());
            match m.filter(|m| RUN_MODES.contains(&m.as_str())) {
                Some(m) => mode = Some(m),
                None => {
                    let shown = match raw {
                        Some(Value::String(s)) => Some(s.trim().to_ascii_lowercase()),
                        Some(Value::Null) | None => None,
                        Some(o) => Some(o.to_string()),
                    };
                    let id = audit.write(
                        "rejected_invalid_action",
                        Some(sid),
                        Some(&action),
                        shown.as_deref(),
                        Some(&safe),
                        Some("Unrecognised mode"),
                    );
                    return with(
                        WebhookOutcome::new(
                            "rejected_invalid_action",
                            format!(
                                "'mode' must be one of {} when starting",
                                RUN_MODES.join(", ")
                            ),
                            false,
                        ),
                        id,
                    );
                }
            }
            // 7. The live gate.
            if mode.as_deref() == Some("live") && !strategy.live_enabled {
                let id = audit.write(
                    "rejected_live_disabled",
                    Some(sid),
                    Some(&action),
                    mode.as_deref(),
                    Some(&safe),
                    Some("Live trading is not enabled for this strategy"),
                );
                return with(
                    WebhookOutcome::new(
                        "rejected_live_disabled",
                        "Live trading is not enabled for this strategy",
                        false,
                    ),
                    id,
                );
            }
        }

        // 8 and 9: both windows read and the claim written under one lock.
        let key = (sid, action.clone(), mode.clone());
        let now = self.webhook.now();
        let refusal = {
            let mut dedupe = self.webhook.dedupe.lock();
            dedupe.retain(|_, at| now.saturating_duration_since(*at) < DEDUPE_WINDOW);
            if dedupe.len() >= MAX_TRACKED_KEYS {
                dedupe.clear();
            }
            let cooling = if action == "start" {
                self.webhook.cooling_remaining(sid, now)
            } else {
                0
            };
            if dedupe.contains_key(&key) {
                Some(("rejected_dedupe", 0))
            } else if cooling > 0 {
                Some(("rejected_cooling_off", cooling))
            } else {
                dedupe.insert(key.clone(), now);
                None
            }
        };
        match refusal {
            Some(("rejected_dedupe", _)) => {
                let id = audit.write(
                    "rejected_dedupe",
                    Some(sid),
                    Some(&action),
                    mode.as_deref(),
                    Some(&safe),
                    Some(&format!(
                        "Duplicate signal inside {}s",
                        DEDUPE_WINDOW.as_secs()
                    )),
                );
                return with(
                    WebhookOutcome::new(
                        "rejected_dedupe",
                        format!(
                            "Duplicate signal ignored, already handled within {}s",
                            DEDUPE_WINDOW.as_secs()
                        ),
                        true,
                    ),
                    id,
                );
            }
            Some((_, remaining)) => {
                let id = audit.write(
                    "rejected_cooling_off",
                    Some(sid),
                    Some(&action),
                    mode.as_deref(),
                    Some(&safe),
                    Some(&format!(
                        "Stopped within the last {}s",
                        COOLING_OFF.as_secs()
                    )),
                );
                return with(
                    WebhookOutcome::new(
                        "rejected_cooling_off",
                        format!(
                            "This strategy stopped recently, try again in {}s",
                            remaining
                        ),
                        false,
                    ),
                    id,
                );
            }
            None => {}
        }

        // 10. Dispatch. The accepted row first, so the run can point at it.
        let event_id = audit.write(
            "ok",
            Some(sid),
            Some(&action),
            mode.as_deref(),
            Some(&safe),
            None,
        );
        let was_live = strategy.status != "stopped";
        let (ok, run_id, error, stop_pending, exits) = if action == "start" {
            let r = self
                .start_run(
                    sid,
                    &strategy.user_id,
                    mode.as_deref().unwrap_or("sandbox"),
                    "webhook",
                    event_id,
                )
                .await;
            (r.ok, r.run_id, r.error, None, vec![])
        } else {
            self.bridge_stop(&strategy).await
        };
        if !ok {
            // Release the claim so the sender's retry is not swallowed.
            self.webhook.dedupe.lock().remove(&key);
            let err = error.unwrap_or_else(|| "The engine could not act on the signal".into());
            let id = audit.write(
                "rejected_engine_error",
                Some(sid),
                Some(&action),
                mode.as_deref(),
                Some(&safe),
                Some(&err),
            );
            let mut o = with(WebhookOutcome::new("rejected_engine_error", err, false), id);
            o.run_id = run_id;
            o.stop_pending = stop_pending;
            o.exits = exits;
            return o;
        }
        if action == "stop" && was_live && stop_pending != Some(true) {
            self.webhook.note_run_stopped(sid, now);
        }
        let mut o = with(
            WebhookOutcome::new("ok", format!("Strategy {} accepted", action), true),
            event_id,
        );
        o.run_id = run_id;
        o.stop_pending = stop_pending;
        o.exits = exits;
        o
    }

    /// Web `webhook_bridge.stop_run`: a stop applies to the current run, and
    /// stopping an already-flat strategy is a success.
    async fn bridge_stop(
        &self,
        strategy: &StrategyRow,
    ) -> (bool, Option<i64>, Option<String>, Option<bool>, Vec<Value>) {
        let current = self
            .store
            .get_strategy_unscoped(strategy.id)
            .ok()
            .flatten()
            .and_then(|s| s.current_run_id);
        let Some(run_id) = current else {
            return (true, None, None, None, vec![]);
        };
        let r = self.stop_run(run_id, &strategy.user_id, "manual").await;
        (r.ok, Some(run_id), r.error, Some(r.stop_pending), r.exits)
    }
}
