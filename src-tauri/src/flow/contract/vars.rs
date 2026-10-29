//! A run's variables and `{{...}}` interpolation (the web's
//! `WorkflowContext`, `flow_executor_service.py` 424-570).
//!
//! Every node reads its fields through here. The rules are the web's, and a
//! workflow exported from the web depends on them:
//!
//! - `{{name}}`, `{{a.b.c}}` and `{{a.b[0]}}` / `{{a[0][1]}}` walk the
//!   stored values; a key on a non-object, an index out of range or on a
//!   non-list, or a `null` anywhere along the path leaves the reference
//!   unresolved, and interpolation keeps its original `{{...}}` text (order
//!   nodes then refuse it: `resolve`).
//! - A resolved value is written as Python's `str()` of it (`100.0`, `True`,
//!   `{'ltp': 1.5}`: `py`).
//! - The built-ins (`timestamp`, `date`, `time`, `weekday_num`,
//!   `session_date`, ...) match only as a whole token and win over a stored
//!   variable of the same name. They are market time, Asia/Kolkata, on any
//!   machine (the web reads the host's local clock, wrong on a host outside
//!   IST; lead decision 1, parity E-13). `session_date` is the trading day,
//!   which turns at the session boundary (03:00 IST unless configured).
//! - [`VarStore::resolve_raw`] hands a node the stored object itself for a
//!   field that is exactly one reference, so a list or dict passes between
//!   nodes without a text round trip.

use super::pyfmt;
use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

/// `{{...}}` anywhere in a text.
static TOKEN: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\{\{([^}]+)\}\}").ok());
/// A text that is exactly one reference.
static WHOLE_TOKEN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^\{\{\s*([^}]+?)\s*\}\}$").ok());
/// One path segment: a dotted key or a bracketed index.
static SEGMENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"([^.\[\]]+)|\[(\d+)\]").ok());
/// What an unresolved reference looks like after interpolation (the web's
/// `_UNRESOLVED_PATTERN`).
static UNRESOLVED: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\{\{[^}]*\}\}").ok());

/// Whether interpolated text still holds an unresolved `{{...}}` reference.
pub fn has_unresolved(text: &str) -> bool {
    UNRESOLVED.as_ref().is_some_and(|re| re.is_match(text))
}

/// The variables and condition results of one run, behind a lock that is
/// never held across an `.await`.
#[derive(Default)]
pub struct VarStore {
    inner: Mutex<Inner>,
    /// The clock the built-ins read (the run's injected clock).
    now: Option<std::sync::Arc<dyn crate::clock::Clock>>,
    /// The session boundary, IST hour and minute (`None`: 03:00).
    boundary: Option<(u32, u32)>,
}

#[derive(Default)]
struct Inner {
    variables: Map<String, Value>,
    conditions: HashMap<String, bool>,
}

impl VarStore {
    /// An empty store whose built-ins read `clock`, with the trading day
    /// turning at `boundary` (IST hour, minute).
    pub fn new(clock: std::sync::Arc<dyn crate::clock::Clock>, boundary: (u32, u32)) -> Self {
        Self {
            inner: Mutex::default(),
            now: Some(clock),
            boundary: Some(boundary),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.now.as_ref().map(|c| c.now()).unwrap_or_else(Utc::now)
    }

    /// Store a variable (`set_variable`).
    pub fn set(&self, name: &str, value: Value) {
        self.inner.lock().variables.insert(name.to_string(), value);
    }

    /// A stored variable (`get_variable`), `None` when absent.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.inner.lock().variables.get(name).cloned()
    }

    /// Whether a variable of that name was stored, even as `null`.
    pub fn contains(&self, name: &str) -> bool {
        self.inner.lock().variables.contains_key(name)
    }

    /// Record the boolean a condition or gate evaluated to
    /// (`set_condition_result`).
    pub fn set_condition(&self, node_id: &str, met: bool) {
        self.inner
            .lock()
            .conditions
            .insert(node_id.to_string(), met);
    }

    /// The condition result of a node, `None` while it has not evaluated
    /// (`get_condition_result`).
    pub fn condition(&self, node_id: &str) -> Option<bool> {
        self.inner.lock().conditions.get(node_id).copied()
    }

    /// The built-in variable `name`, as the web's `_get_builtin_variable`.
    pub fn builtin(&self, name: &str) -> Option<String> {
        let utc = self.now();
        let now = utc.with_timezone(&Kolkata);
        let text = match name {
            "timestamp" => now.format("%Y-%m-%d %H:%M:%S").to_string(),
            "date" => now.format("%Y-%m-%d").to_string(),
            "time" => now.format("%H:%M:%S").to_string(),
            "year" => now.format("%Y").to_string(),
            "month" => now.format("%m").to_string(),
            "day" => now.format("%d").to_string(),
            "hour" => now.format("%H").to_string(),
            "minute" => now.format("%M").to_string(),
            "second" => now.format("%S").to_string(),
            "weekday" => now.format("%A").to_string(),
            "iso_timestamp" => iso_ist(utc),
            "weekday_num" => now.weekday().number_from_monday().to_string(),
            "quarter" => ((now.month() - 1) / 3 + 1).to_string(),
            "week_of_year" => now.iso_week().week().to_string(),
            "day_of_year" => now.ordinal().to_string(),
            "session_date" => {
                let (h, m) = self.boundary.unwrap_or(DEFAULT_BOUNDARY);
                crate::strategy::session::session_day(utc, h, m)
                    .format("%Y-%m-%d")
                    .to_string()
            }
            _ => return None,
        };
        Some(text)
    }

    /// Walk a dotted / bracketed path over the stored variables
    /// (`_walk_path`); `None` when any segment does not resolve or meets a
    /// `null`.
    pub fn walk(&self, path: &str) -> Option<Value> {
        let re = SEGMENT.as_ref()?;
        let inner = self.inner.lock();
        let mut current: Option<&Value> = None;
        let mut first = true;
        for seg in re.captures_iter(path) {
            let next = match (seg.get(1), seg.get(2)) {
                (Some(key), _) => {
                    if first {
                        inner.variables.get(key.as_str())
                    } else {
                        current.and_then(Value::as_object)?.get(key.as_str())
                    }
                }
                (None, Some(idx)) => {
                    if first {
                        // An index on the variable map itself: not a list.
                        return None;
                    }
                    // The pattern takes digits only, so an index is never
                    // negative here.
                    let list = current.and_then(Value::as_array)?;
                    list.get(idx.as_str().parse::<usize>().ok()?)
                }
                _ => return None,
            };
            first = false;
            match next {
                None | Some(Value::Null) => return None,
                Some(v) => current = Some(v),
            }
        }
        if first {
            // An empty path walks to the variable map itself.
            return Some(Value::Object(inner.variables.clone()));
        }
        current.cloned()
    }

    /// Replace every `{{path}}` in `text` with its value (`interpolate`):
    /// a built-in first, else the stored value as Python's `str()`, else
    /// the reference left as written.
    pub fn interpolate(&self, text: &str) -> String {
        let Some(re) = TOKEN.as_ref() else {
            return text.to_string();
        };
        if !text.contains("{{") {
            return text.to_string();
        }
        re.replace_all(text, |caps: &regex::Captures| {
            let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
            let path = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
            if let Some(b) = self.builtin(path) {
                return b;
            }
            match self.walk(path) {
                Some(v) => pyfmt::str(&v),
                None => whole.to_string(),
            }
        })
        .into_owned()
    }

    /// [`Self::interpolate`] over a JSON value: text is interpolated, any
    /// other value is returned as it is (the web returns non-strings
    /// untouched).
    pub fn interpolate_value(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.interpolate(s)),
            other => other.clone(),
        }
    }

    /// The stored object for a text that is exactly one `{{path}}`
    /// (`resolve_raw`); otherwise, or when the path does not resolve, the
    /// interpolated text. A built-in name is never read as a stored path.
    pub fn resolve_raw(&self, value: &Value) -> Value {
        let Value::String(text) = value else {
            return value.clone();
        };
        if let Some(caps) = WHOLE_TOKEN.as_ref().and_then(|re| re.captures(text)) {
            let path = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
            if self.builtin(path).is_none() {
                if let Some(v) = self.walk(path) {
                    return v;
                }
            }
        }
        Value::String(self.interpolate(text))
    }
}

/// The session boundary when none is configured: 03:00 IST.
pub const DEFAULT_BOUNDARY: (u32, u32) = (3, 0);

/// `datetime.now().isoformat()` as the web writes it, in IST: microseconds
/// only when non-zero, no offset.
pub fn iso_ist(now: DateTime<Utc>) -> String {
    let t = now.with_timezone(&Kolkata).naive_local();
    if t.nanosecond() / 1000 == 0 {
        t.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> VarStore {
        let s = VarStore::default();
        s.set(
            "quote",
            json!({"data": {"ltp": 1250.5, "bids": [{"p": 1.0}, {"p": 2.0}]}}),
        );
        s.set("qty", json!(10));
        s.set("flag", json!(true));
        s.set("empty", json!(null));
        s.set("webhook", json!({"symbol": "SBIN"}));
        s
    }

    #[test]
    fn paths_walk_keys_and_indexes() {
        let s = store();
        assert_eq!(s.interpolate("LTP {{quote.data.ltp}}"), "LTP 1250.5");
        assert_eq!(s.interpolate("{{quote.data.bids[1].p}}"), "2.0");
        assert_eq!(s.interpolate("{{ qty }} lots"), "10 lots");
        assert_eq!(s.interpolate("{{flag}}"), "True");
        assert_eq!(s.interpolate("{{webhook.symbol}}"), "SBIN");
        assert_eq!(
            s.interpolate("{{quote.data.bids}}"),
            "[{'p': 1.0}, {'p': 2.0}]"
        );
    }

    #[test]
    fn an_unresolved_reference_keeps_its_text() {
        let s = store();
        assert_eq!(s.interpolate("{{missing}}"), "{{missing}}");
        assert_eq!(
            s.interpolate("{{quote.data.bids[5]}}"),
            "{{quote.data.bids[5]}}"
        );
        assert_eq!(s.interpolate("{{qty.x}}"), "{{qty.x}}");
        // A null anywhere along the path does not resolve (web `_walk_path`).
        assert_eq!(s.interpolate("{{empty}}"), "{{empty}}");
        assert!(has_unresolved(&s.interpolate("buy {{webhook.qty}}")));
        assert!(!has_unresolved("buy 10"));
    }

    #[test]
    fn whole_references_keep_their_object() {
        let s = store();
        assert_eq!(
            s.resolve_raw(&json!("{{quote.data.bids}}")),
            json!([{"p": 1.0}, {"p": 2.0}])
        );
        assert_eq!(s.resolve_raw(&json!("{{ qty }}")), json!(10));
        assert_eq!(s.resolve_raw(&json!("x {{qty}}")), json!("x 10"));
        assert_eq!(s.resolve_raw(&json!("{{nope}}")), json!("{{nope}}"));
        assert_eq!(s.resolve_raw(&json!(5)), json!(5));
    }

    #[test]
    fn builtins_win_over_variables_and_only_as_whole_tokens() {
        let s = store();
        s.set("date", json!("stored"));
        let date = s.interpolate("{{date}}");
        assert_ne!(date, "stored");
        assert_eq!(date.len(), 10);
        // resolve_raw never reads a built-in name as a stored path.
        assert_eq!(s.resolve_raw(&json!("{{date}}")), json!(date));
        assert!(s.builtin("weekday_num").is_some());
        assert!(s.builtin("date.x").is_none());
    }

    fn at(utc: &str) -> VarStore {
        let t: DateTime<Utc> = utc.parse().unwrap();
        VarStore::new(crate::clock::ManualClock::new(t), DEFAULT_BOUNDARY)
    }

    /// Market time on any machine: 21:00 UTC is 02:30 IST on the next day,
    /// which still belongs to the previous trading session.
    #[test]
    fn builtins_are_ist_and_the_session_turns_at_the_boundary() {
        let s = at("2026-10-29T21:00:00Z");
        assert_eq!(s.builtin("date").as_deref(), Some("2026-10-30"));
        assert_eq!(s.builtin("time").as_deref(), Some("02:30:00"));
        assert_eq!(s.builtin("weekday").as_deref(), Some("Friday"));
        assert_eq!(s.builtin("weekday_num").as_deref(), Some("5"));
        assert_eq!(
            s.builtin("iso_timestamp").as_deref(),
            Some("2026-10-30T02:30:00")
        );
        assert_eq!(s.builtin("session_date").as_deref(), Some("2026-10-29"));
        assert_eq!(
            at("2026-10-29T21:31:00Z")
                .builtin("session_date")
                .as_deref(),
            Some("2026-10-30")
        );
        let custom = VarStore::new(
            crate::clock::ManualClock::new("2026-10-29T21:31:00Z".parse().unwrap()),
            (4, 0),
        );
        assert_eq!(
            custom.builtin("session_date").as_deref(),
            Some("2026-10-29")
        );
        assert_eq!(
            iso_ist("2026-10-29T21:00:00.000250Z".parse().unwrap()),
            "2026-10-30T02:30:00.000250"
        );
    }

    #[test]
    fn conditions_are_unset_until_recorded() {
        let s = store();
        assert_eq!(s.condition("c1"), None);
        s.set_condition("c1", false);
        assert_eq!(s.condition("c1"), Some(false));
    }
}
