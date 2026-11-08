//! The case's expected frames against the web's output for the same raw
//! frames: `web.json`, written by `scripts/feed_corpus/web_expected.py`,
//! which runs the web adapter's own decoding and normalising methods on
//! the case's pushes (README, "The web's output").
//!
//! For every frame a client receives after a push, the web's publish of
//! the same mode for that push is found (or, when the web publishes none
//! of that mode, its lowest richer one, which is what the web proxy
//! delivers to a client at that mode), and the two `data` objects (whole
//! frames for order updates) are compared key by key. Every key on which
//! they differ must be one of the case's deviations, and every deviation
//! must be such a difference, so a deviation cannot outlive its cause.
//! The live run has already shown the expected frames are the desktop's.

use super::case::{bad, read_json, Action, Case, WebPair};
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

    let mut diffs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut last: Option<(usize, usize)> = None;
    for step in &case.steps {
        match &step.action {
            Action::Push { .. } | Action::HttpServe(_) => last = Some((step.line, 0)),
            Action::Receive { frame, web, .. } => {
                let Some((line, orders_seen)) = last.as_mut() else {
                    continue;
                };
                let line = *line;
                let published = &pushes[&line];
                let found = counterpart(published, frame, *web, orders_seen);
                let at = format!("{} script.jsonl:{}", case.id, step.line);
                match (found, web) {
                    (Some((mine, theirs)), _) => differences(&at, mine, theirs, &mut diffs),
                    (None, WebPair::Nothing) => diffs
                        .entry(NO_WEB_FRAME.into())
                        .or_default()
                        .push(format!("{}: the web sends nothing for this frame", at)),
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
            _ => {}
        }
    }
    report(&at, case, &diffs);
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

/// The desktop's frame (its `data`, or the whole frame for an order
/// update) and the web output it is compared with.
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
                .map(u64::from)
                .or_else(|| frame["mode"].as_u64())
                .unwrap_or(0);
            // The web's publishes for this frame's instrument (its topic's
            // exchange and symbol), so one push may publish several.
            let market: Vec<&Value> = published
                .iter()
                .filter(|p| p["kind"] == "market")
                .filter(|p| {
                    ["symbol", "exchange"]
                        .iter()
                        .all(|k| p.get(*k).is_none_or(|v| *v == frame[*k]))
                })
                .collect();
            let theirs: &Value = market
                .iter()
                .find(|p| mode_of(p) == Some(mode))
                .or_else(|| {
                    market
                        .iter()
                        .filter(|p| mode_of(p).is_some_and(|m| m > mode))
                        .min_by_key(|p| mode_of(p))
                })
                .copied()?;
            Some((&frame["data"], &theirs["data"]))
        }
        Some("order_update") => {
            let theirs = published
                .iter()
                .filter(|p| p["kind"] == "order")
                .nth(*orders_seen)?;
            *orders_seen += 1;
            Some((frame, &theirs["frame"]))
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

fn differences(at: &str, mine: &Value, theirs: &Value, diffs: &mut BTreeMap<String, Vec<String>>) {
    let (Some(m), Some(t)) = (mine.as_object(), theirs.as_object()) else {
        bad(at, "the desktop's or the web's frame is not an object");
    };
    let keys: BTreeSet<&String> = m.keys().chain(t.keys()).collect();
    for k in keys {
        let (d, w) = (m.get(k), t.get(k));
        if d != w {
            diffs.entry(k.clone()).or_default().push(format!(
                "{}: web {}, desktop {}",
                at,
                shown(w),
                shown(d)
            ));
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
