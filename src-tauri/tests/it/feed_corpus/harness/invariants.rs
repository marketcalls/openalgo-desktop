//! What must hold of a value a broker's synthetic feed draws at random
//! (Dhan Sandbox): `case.json` `invariants`, checked on every
//! `market_data` frame a client receives and on the web's publish it is
//! compared with, where the exact value cannot be.
//!
//! ```json
//! "invariants": [
//!   {"key": "high", "at_least": "low"},
//!   {"key": "ltp", "at_least": 1477.6, "at_most": 1492.5},
//!   {"key": "open", "below": {"key": "ltp", "times": 0.96}},
//!   {"key": "depth.buy.*.quantity", "at_least": 10, "at_most": 500, "mode": 3}
//! ]
//! ```
//!
//! `key` is a path in the frame's `data` (`*` for every item of a list); a
//! bound is a number, another key, or a key times a factor. An invariant
//! applies to a frame that has its key (and is of its `mode`, if named),
//! and must apply to at least one frame of the case.

use super::case::bad;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(crate) struct Invariant {
    /// As written, for a failure message.
    pub(crate) text: String,
    key: Vec<String>,
    mode: Option<u64>,
    bounds: Vec<(Op, Bound)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    AtLeast,
    AtMost,
    Above,
    Below,
}

#[derive(Debug, Clone)]
enum Bound {
    Number(f64),
    Key { key: Vec<String>, times: f64 },
}

const OPS: &[(&str, Op)] = &[
    ("at_least", Op::AtLeast),
    ("at_most", Op::AtMost),
    ("above", Op::Above),
    ("below", Op::Below),
];

/// `case.json` `invariants`.
pub(crate) fn parse(at: &str, v: Option<&Value>) -> Vec<Invariant> {
    let Some(v) = v else {
        return Vec::new();
    };
    v.as_array()
        .unwrap_or_else(|| bad(at, "\"invariants\" is an array"))
        .iter()
        .map(|i| invariant(at, i))
        .collect()
}

fn path(at: &str, v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| bad(at, "an invariant names its \"key\""))
        .split('.')
        .map(String::from)
        .collect()
}

fn invariant(at: &str, v: &Value) -> Invariant {
    let o = v
        .as_object()
        .unwrap_or_else(|| bad(at, "an invariant is an object"));
    let text = v.to_string();
    let at = &format!("{} invariant {}", at, text);
    for k in o.keys() {
        if k != "key" && k != "mode" && !OPS.iter().any(|(n, _)| n == k) {
            bad(at, format!("\"{}\" is not a key of an invariant", k));
        }
    }
    let bounds: Vec<(Op, Bound)> = OPS
        .iter()
        .filter_map(|(name, op)| o.get(*name).map(|b| (*op, bound(at, b))))
        .collect();
    if bounds.is_empty() {
        bad(at, "an invariant bounds its key");
    }
    Invariant {
        text,
        key: path(at, o.get("key")),
        mode: o.get("mode").map(|m| {
            m.as_u64()
                .filter(|m| (1..=3).contains(m))
                .unwrap_or_else(|| bad(at, "\"mode\" is 1, 2 or 3"))
        }),
        bounds,
    }
}

fn bound(at: &str, v: &Value) -> Bound {
    match v {
        Value::Number(n) => Bound::Number(n.as_f64().unwrap_or_else(|| bad(at, "a bound"))),
        Value::String(_) => Bound::Key {
            key: path(at, Some(v)),
            times: 1.0,
        },
        Value::Object(o) => Bound::Key {
            key: path(at, o.get("key")),
            times: o
                .get("times")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| bad(at, "a key bound gives \"times\"")),
        },
        _ => bad(at, "a bound is a number, a key, or {\"key\", \"times\"}"),
    }
}

/// The values at `path` in `data` (several where it says `*`).
fn values<'a>(data: &'a Value, path: &[String]) -> Vec<&'a Value> {
    let Some((first, rest)) = path.split_first() else {
        return vec![data];
    };
    match (first.as_str(), data) {
        ("*", Value::Array(items)) => items.iter().flat_map(|i| values(i, rest)).collect(),
        (k, Value::Object(o)) => o.get(k).map_or_else(Vec::new, |v| values(v, rest)),
        (i, Value::Array(items)) => i
            .parse::<usize>()
            .ok()
            .and_then(|i| items.get(i))
            .map_or_else(Vec::new, |v| values(v, rest)),
        _ => Vec::new(),
    }
}

impl Invariant {
    /// Whether it applies to a frame of `mode` with `data`; if so, every
    /// way `data` breaks it.
    pub(crate) fn check(&self, data: &Value, mode: Option<u64>) -> Option<Vec<String>> {
        if self.mode.is_some_and(|m| Some(m) != mode) {
            return None;
        }
        let found = values(data, &self.key);
        if found.is_empty() {
            return None;
        }
        let mut broken = Vec::new();
        for v in found {
            let Some(x) = v.as_f64() else {
                broken.push(format!("{} is not a number", v));
                continue;
            };
            for (op, b) in &self.bounds {
                match self.limit(data, b) {
                    Ok(limit) if holds(*op, x, limit) => {}
                    Ok(limit) => broken.push(format!("{} is not {} {}", v, name(*op), limit)),
                    Err(e) => broken.push(e),
                }
            }
        }
        Some(broken)
    }

    fn limit(&self, data: &Value, b: &Bound) -> Result<f64, String> {
        match b {
            Bound::Number(n) => Ok(*n),
            Bound::Key { key, times } => match values(data, key).as_slice() {
                [one] => one
                    .as_f64()
                    .map(|x| x * times)
                    .ok_or_else(|| format!("{} is not a number", key.join("."))),
                _ => Err(format!("{} is not one value in the frame", key.join("."))),
            },
        }
    }
}

fn holds(op: Op, x: f64, limit: f64) -> bool {
    match op {
        Op::AtLeast => x >= limit,
        Op::AtMost => x <= limit,
        Op::Above => x > limit,
        Op::Below => x < limit,
    }
}

fn name(op: Op) -> &'static str {
    OPS.iter().find(|(_, o)| *o == op).map_or("", |(n, _)| n)
}

/// Every invariant `data` breaks, one per line; `applied[i]` counts the
/// frames invariant `i` applied to.
pub(crate) fn broken(
    invariants: &[Invariant],
    frame: &Map<String, Value>,
    applied: &mut [usize],
) -> Vec<String> {
    let data = frame.get("data").unwrap_or(&Value::Null);
    let mode = frame.get("mode").and_then(Value::as_u64);
    let mut out = Vec::new();
    for (i, inv) in invariants.iter().enumerate() {
        if let Some(found) = inv.check(data, mode) {
            applied[i] += 1;
            out.extend(found.into_iter().map(|e| format!("{}: {}", inv.text, e)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(v: Value) -> Invariant {
        parse("t", Some(&json!([v]))).remove(0)
    }

    #[test]
    fn a_bound_is_a_number_a_key_or_a_scaled_key() {
        let data = json!({"ltp": 100.0, "high": 105.0, "low": 90.0});
        assert_eq!(
            one(json!({"key": "high", "at_least": "low"})).check(&data, Some(2)),
            Some(vec![])
        );
        let e = one(json!({"key": "low", "at_least": "high"}))
            .check(&data, Some(2))
            .unwrap();
        assert_eq!(e, vec!["90.0 is not at_least 105".to_string()]);
        let scaled = one(json!({"key": "high", "at_most": {"key": "ltp", "times": 1.05}}));
        assert_eq!(scaled.check(&data, Some(2)), Some(vec![]));
        let band = one(json!({"key": "ltp", "above": 99.5, "below": 100.5}));
        assert_eq!(band.check(&data, Some(2)), Some(vec![]));
    }

    #[test]
    fn it_applies_only_where_its_key_and_mode_are() {
        let inv = one(json!({"key": "depth.buy.*.quantity", "at_least": 10, "mode": 3}));
        let book = json!({"depth": {"buy": [{"quantity": 12}, {"quantity": 9}]}});
        assert_eq!(inv.check(&book, Some(2)), None);
        assert_eq!(inv.check(&json!({"ltp": 1.0}), Some(3)), None);
        assert_eq!(inv.check(&book, Some(3)).map(|e| e.len()), Some(1));
    }
}
