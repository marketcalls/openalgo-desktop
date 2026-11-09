//! The case's expected frames against the web's output for the same raw
//! frames: `web.json`, written by `scripts/feed_corpus/web_expected.py`,
//! which runs the web adapter's own decoding and normalising methods on
//! the case's pushes (README, "The web's output").
//!
//! For every frame a client receives after a delivery point (a push, or
//! any step that made the web adapter publish), the frame the web's own
//! server sends that client is found (`deliveries.rs`: each web frame
//! consumed once per held mode, in order, by instrument and mode; an order
//! update by its id), and the two `data` objects (whole frames for order
//! updates) are compared key by key. Every web frame that answers a held
//! mode and that no receive consumes must be declared by a `silence` step
//! naming it (`missing-frame`), before the next delivery point.
//! Every key on which they differ must be one of the case's deviations,
//! and every deviation must be such a difference, so a deviation cannot
//! outlive its cause. A `random-values` key is compared by presence and
//! JSON type only, and the web's publish must keep the case's invariants
//! too. The live run has already shown the expected frames are the
//! desktop's.

use super::case::{bad, read_json, Action, Case, WebFrame, WebPair, MISSING_FRAME};
use super::deliveries::{self, Consumer};
use super::invariants;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The deviation key for a frame the web does not send at all.
const NO_WEB_FRAME: &str = "frame";

pub(crate) fn check(case: &Case) {
    let path = case.dir.join("web.json");
    if !path.exists() {
        return;
    }
    let at = format!("{} web.json", case.id);
    let web = read_json(&path);
    let digest = hex::encode(Sha256::digest(&case.inputs));
    if web["inputs_sha256"] != digest.as_str() {
        bad(
            &at,
            "computed from another script or other frames: run scripts/feed_corpus/web_expected.py again",
        );
    }
    if web["instruments"] != case.instruments_json {
        bad(&at, "computed with other instruments than case.json's");
    }
    let sha = web["web_sha"].as_str().unwrap_or_default();
    if sha.is_empty() || !(sha.starts_with(&case.web_sha) || case.web_sha.starts_with(sha)) {
        bad(
            &at,
            format!(
                "computed at web {}, but case.json's provenance says {}",
                sha, case.web_sha
            ),
        );
    }
    let points = deliveries::points(&at, &web);
    let push_lines: BTreeSet<usize> = case
        .steps
        .iter()
        .filter(|s| matches!(s.action, Action::Push { .. } | Action::HttpServe(_)))
        .map(|s| s.line)
        .collect();
    let step_lines: BTreeSet<usize> = case.steps.iter().map(|s| s.line).collect();
    if !push_lines.iter().all(|l| points.contains_key(l))
        || !points.keys().all(|l| step_lines.contains(l))
    {
        bad(
            &at,
            "its delivery points are not the script's: run scripts/feed_corpus/web_expected.py again",
        );
    }

    let mut cmp = Comparison {
        case,
        diffs: BTreeMap::new(),
    };
    let mut current: Option<Consumer> = None;
    let mut unconsumed = Vec::new();
    for step in &case.steps {
        let at = format!("{} script.jsonl:{}", case.id, step.line);
        if let Some(point) = points.get(&step.line) {
            if let Some(c) = current.take() {
                unconsumed.extend(c.unconsumed());
            }
            current = Some(Consumer::new(step.line, point));
            continue;
        }
        let Some(c) = current.as_mut() else {
            continue;
        };
        match &step.action {
            Action::Receive { client, frame, web } => {
                let theirs = match (web, frame["type"].as_str()) {
                    (WebPair::Nothing, _) => None,
                    (WebPair::Mode(_), Some("order_update")) => c.order(client, &frame["orderid"]),
                    (WebPair::Mode(mode), _) => c.market(
                        client,
                        frame["symbol"].as_str().unwrap_or_default(),
                        frame["exchange"].as_str().unwrap_or_default(),
                        mode.map(u64::from)
                            .or_else(|| frame["mode"].as_u64())
                            .unwrap_or(0),
                    ),
                };
                match (theirs, web) {
                    (Some(theirs), _) => cmp.frames(&at, frame, theirs),
                    (None, WebPair::Nothing) => cmp.note(
                        NO_WEB_FRAME,
                        format!("{}: the web sends nothing for this frame", at),
                    ),
                    (None, WebPair::Mode(_)) => bad(
                        &at,
                        format!(
                            "the web sends client {} nothing this frame could answer (delivery point at line {}): \
                             mark the step \"web\": false and add a deviation \"{}\"",
                            client, c.line, NO_WEB_FRAME
                        ),
                    ),
                }
            }
            Action::Silence {
                client,
                web: Some(w),
                ..
            } => {
                let Some(theirs) = c.market(client, &w.symbol, &w.exchange, u64::from(w.mode))
                else {
                    bad(
                        &at,
                        format!(
                            "the web sends client {} no {} {} frame at mode {} after line {} either: drop \"web\"",
                            client, w.exchange, w.symbol, w.mode, c.line
                        ),
                    );
                };
                cmp.web_only(&at, w, theirs);
            }
            _ => {}
        }
    }
    if let Some(c) = current.take() {
        unconsumed.extend(c.unconsumed());
    }
    if !unconsumed.is_empty() {
        bad(
            &at,
            format!(
                "web frames the case neither receives nor declares (a silence step naming each, deviation \"{}\"):\n  {}",
                MISSING_FRAME,
                unconsumed.join("\n  ")
            ),
        );
    }
    report(&at, case, &cmp.diffs);
}

fn shown(v: Option<&Value>) -> String {
    v.map_or_else(|| "absent".to_string(), Value::to_string)
}

/// A value's shape: its JSON type at every level, with the keys of every
/// object and the length of every list; a `<random:..>` placeholder is
/// the type it names.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        Value::String(s) => match s.strip_prefix("<random:").and_then(|t| t.strip_suffix('>')) {
            Some(t) => Value::String(t.into()),
            None => Value::String("string".into()),
        },
        Value::Number(n) if n.is_f64() => Value::String("float".into()),
        Value::Number(_) => Value::String("int".into()),
        Value::Bool(_) => Value::String("bool".into()),
        Value::Null => Value::Null,
    }
}

/// The differences found, by deviation key.
struct Comparison<'a> {
    case: &'a Case,
    diffs: BTreeMap<String, Vec<String>>,
}

impl Comparison<'_> {
    fn note(&mut self, key: &str, seen: String) {
        self.diffs.entry(key.to_string()).or_default().push(seen);
    }

    /// The desktop's frame against the web's output: the `data` of a
    /// market frame, the whole of an order update.
    fn frames(&mut self, at: &str, mine: &Value, theirs: &Value) {
        if mine["type"] == "order_update" {
            self.differences(at, mine, &theirs["frame"]);
        } else {
            self.keeps_invariants(at, theirs);
            self.differences(at, &mine["data"], &theirs["data"]);
        }
    }

    /// A web frame the desktop does not send.
    fn web_only(&mut self, at: &str, w: &WebFrame, theirs: &Value) {
        self.keeps_invariants(at, theirs);
        self.note(
            &w.deviation,
            format!(
                "{}: the web sends {} {} at mode {} ({}), the desktop nothing",
                at, w.exchange, w.symbol, w.mode, theirs["data"]
            ),
        );
    }

    /// An invariant the web's own publish breaks is not the web's
    /// behaviour, so it cannot stand for it.
    fn keeps_invariants(&self, at: &str, theirs: &Value) {
        let Some(publish) = theirs.as_object() else {
            return;
        };
        let mut applied = vec![0; self.case.invariants.len()];
        let broken = invariants::broken(&self.case.invariants, publish, &mut applied);
        if !broken.is_empty() {
            bad(
                at,
                format!(
                    "the web's publish breaks the case's invariants, so they are not the web's:\n  {}",
                    broken.join("\n  ")
                ),
            );
        }
    }

    fn differences(&mut self, at: &str, mine: &Value, theirs: &Value) {
        let (Some(m), Some(t)) = (mine.as_object(), theirs.as_object()) else {
            bad(at, "the desktop's or the web's frame is not an object");
        };
        let keys: BTreeSet<&String> = m.keys().chain(t.keys()).collect();
        for k in keys {
            let (d, w) = (m.get(k), t.get(k));
            if self.case.random.contains(k) {
                // Drawn at random on both sides: the same key, of the same
                // type, is all that can match.
                let same = d.zip(w).is_some_and(|(d, w)| shape(d) == shape(w));
                let key = if same {
                    k.clone()
                } else {
                    format!("{} (type)", k)
                };
                self.note(
                    &key,
                    format!("{}: web {}, desktop {}", at, shown(w), shown(d)),
                );
            } else if d != w {
                self.note(k, format!("{}: web {}, desktop {}", at, shown(w), shown(d)));
            }
        }
    }
}

/// Fail on a difference no deviation names, or a deviation nothing shows.
fn report(at: &str, case: &Case, diffs: &BTreeMap<String, Vec<String>>) {
    let undeclared: Vec<String> = diffs
        .iter()
        .filter(|(k, _)| !case.deviations.contains(k))
        .flat_map(|(k, seen)| seen.iter().map(move |s| format!("\n  {} ({})", k, s)))
        .collect();
    let unused: Vec<&String> = case
        .deviations
        .iter()
        .filter(|k| !diffs.contains_key(*k))
        .collect();
    if !undeclared.is_empty() {
        bad(
            at,
            format!(
                "the desktop differs from the web where case.json declares no deviation:{}",
                undeclared.join("")
            ),
        );
    }
    if !unused.is_empty() {
        bad(
            at,
            format!(
                "deviations the frames no longer show (remove them): {:?}",
                unused
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_random_placeholder_has_the_shape_of_its_type() {
        assert_eq!(shape(&json!("<random:float>")), shape(&json!(101.25)));
        assert_eq!(shape(&json!("<random:int>")), shape(&json!(900)));
        assert_ne!(shape(&json!("<random:float>")), shape(&json!(900)));
        let book = json!({"buy": [{"price": 1.0, "quantity": "<random:int>"}]});
        assert_eq!(
            shape(&book),
            shape(&json!({"buy": [{"price": 2.5, "quantity": 12}]}))
        );
        assert_ne!(shape(&book), shape(&json!({"buy": []})));
    }
}
