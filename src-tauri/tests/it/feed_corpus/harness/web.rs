//! The case's expected frames against the web's output for the same raw
//! frames: `web.json`, written by `scripts/feed_corpus/web_expected.py`,
//! which runs the web adapter's own decoding and normalising methods on
//! the case's pushes (README, "The web's output").
//!
//! For every frame a client receives after a push, the web's publish for
//! the same instrument and mode for that push is found (or, when the web
//! publishes none of that mode, its lowest richer one, which is what the
//! web proxy delivers to a client at that mode), and the two `data`
//! objects (whole frames for order updates) are compared key by key.
//! Every key on which they differ must be one of the case's deviations,
//! and every deviation must be such a difference, so a deviation cannot
//! outlive its cause. A `random-values` key is compared by presence and
//! JSON type only, and the web's publish must keep the case's invariants
//! too. The live run has already shown the expected frames are the
//! desktop's.

use super::case::{bad, read_json, Action, Case, WebFrame, WebPair};
use super::invariants;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The deviation key for a frame the web does not send at all.
const NO_WEB_FRAME: &str = "frame";
/// The deviation key for a web frame the desktop does not send.
const NO_DESKTOP_FRAME: &str = "missing-frame";

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
    let pushes = published_by_line(&at, &web);
    let push_lines: BTreeSet<usize> = case
        .steps
        .iter()
        .filter(|s| matches!(s.action, Action::Push { .. } | Action::HttpServe(_)))
        .map(|s| s.line)
        .collect();
    if push_lines != pushes.keys().copied().collect::<BTreeSet<usize>>() {
        bad(
            &at,
            "its pushes are not the script's: run scripts/feed_corpus/web_expected.py again",
        );
    }

    let mut cmp = Comparison {
        case,
        diffs: BTreeMap::new(),
    };
    let mut last: Option<(usize, usize)> = None;
    for step in &case.steps {
        let at = format!("{} script.jsonl:{}", case.id, step.line);
        match &step.action {
            Action::Push { .. } | Action::HttpServe(_) => last = Some((step.line, 0)),
            Action::Receive { frame, web, .. } => {
                let Some((line, orders_seen)) = last.as_mut() else {
                    continue;
                };
                let line = *line;
                let found = counterpart(&pushes[&line], frame, *web, orders_seen);
                match (found, web) {
                    (Some((mine, theirs)), _) => cmp.frames(&at, mine, theirs),
                    (None, WebPair::Nothing) => cmp.note(
                        NO_WEB_FRAME,
                        format!("{}: the web sends nothing for this frame", at),
                    ),
                    (None, WebPair::Mode(_)) => bad(
                        &at,
                        format!(
                            "the web publishes nothing this frame could come from (push at line {}): \
                             mark the step \"web\": false and add a deviation \"{}\"",
                            line, NO_WEB_FRAME
                        ),
                    ),
                }
            }
            Action::Silence { web: Some(w), .. } => {
                let Some((line, _)) = last else {
                    bad(&at, "\"web\" names a frame of a push, and none came yet");
                };
                let Some(theirs) = market_publish(&pushes[&line], &w.symbol, &w.exchange, w.mode)
                else {
                    bad(
                        &at,
                        format!(
                            "the web publishes no {} {} frame at mode {} for the push at line {} either: drop \"web\"",
                            w.exchange, w.symbol, w.mode, line
                        ),
                    );
                };
                cmp.web_only(&at, w, theirs);
            }
            _ => {}
        }
    }
    report(&at, case, &cmp.diffs);
}

/// Each push's publishes, by its script line.
fn published_by_line(at: &str, web: &Value) -> BTreeMap<usize, Vec<Value>> {
    web["pushes"]
        .as_array()
        .unwrap_or_else(|| bad(at, "no \"pushes\""))
        .iter()
        .map(|p| {
            let line = p["line"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or_else(|| bad(at, "a push without its script line"));
            let published = p["published"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| bad(at, "a push without \"published\""));
            (line, published)
        })
        .collect()
}

/// The web's publish for an instrument (its topic's exchange and symbol,
/// so one push may publish several) that a client at `mode` gets: that
/// mode's, else the lowest richer one's.
fn market_publish<'a>(
    published: &'a [Value],
    symbol: &str,
    exchange: &str,
    mode: u8,
) -> Option<&'a Value> {
    let market: Vec<&Value> = published
        .iter()
        .filter(|p| p["kind"] == "market")
        .filter(|p| p.get("symbol").is_none_or(|v| v == symbol))
        .filter(|p| p.get("exchange").is_none_or(|v| v == exchange))
        .collect();
    let mode = u64::from(mode);
    market
        .iter()
        .find(|p| mode_of(p) == Some(mode))
        .or_else(|| {
            market
                .iter()
                .filter(|p| mode_of(p).is_some_and(|m| m > mode))
                .min_by_key(|p| mode_of(p))
        })
        .copied()
}

/// The desktop's frame and the web output it is compared with (both
/// whole, for a market frame the publish whose `data` the web forwards).
fn counterpart<'a>(
    published: &'a [Value],
    frame: &'a Value,
    pair: WebPair,
    orders_seen: &mut usize,
) -> Option<(&'a Value, &'a Value)> {
    let WebPair::Mode(mode) = pair else {
        return None;
    };
    match frame["type"].as_str() {
        Some("market_data") => {
            let mode = mode
                .or_else(|| frame["mode"].as_u64().and_then(|m| u8::try_from(m).ok()))
                .unwrap_or(0);
            let symbol = frame["symbol"].as_str().unwrap_or_default();
            let exchange = frame["exchange"].as_str().unwrap_or_default();
            Some((frame, market_publish(published, symbol, exchange, mode)?))
        }
        Some("order_update") => {
            let theirs = published
                .iter()
                .filter(|p| p["kind"] == "order")
                .nth(*orders_seen)?;
            *orders_seen += 1;
            Some((frame, theirs))
        }
        _ => None,
    }
}

/// The mode of a web publish (1 LTP, 2 Quote, 3 Depth).
fn mode_of(p: &Value) -> Option<u64> {
    p["mode"].as_u64()
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
            NO_DESKTOP_FRAME,
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
