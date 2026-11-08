//! Exact comparison of a frame with the case's expected frame: the same
//! keys at every level (a field the expected frame leaves out must be
//! absent), the same values with the same JSON types (`2950.0` is not
//! `2950`), arrays of the same length in the same order.
//!
//! What a case cannot know is written as a placeholder:
//!
//! * `"<receipt_ms>"`: a time the desktop stamps when it receives a packet,
//!   an integer epoch-ms time between the latest push and the frame's
//!   arrival (so never 0, a trade time from the packet or a time in s);
//! * `"<receipt_ms:N>"`: the receipt time of the push on script line `N`
//!   (a book frame that carries an earlier ticker's time), between that
//!   push and the next one;
//! * `"<random:float>"`, `"<random:int>"`, `"<random:string>"`: a value a
//!   broker's synthetic feed draws at random (Dhan Sandbox): any value of
//!   that JSON type, checked against the case's invariants instead;
//! * `"<ist_today_ms:HH:MM:SS>"` (or `HH:MM:SS.mmm`): that time of day on
//!   the IST date of receipt, in epoch ms (Definedge's depth `ltt`, a time
//!   of day the feed dates by the day it receives it); either date counts
//!   when the receipt window spans IST midnight.

use serde_json::Value;
use std::collections::BTreeMap;

/// IST is UTC+05:30 all year.
const IST_OFFSET_MS: i64 = (5 * 60 + 30) * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The placeholder for a time stamped on receipt of the latest push.
pub(crate) const RECEIPT_MS: &str = "<receipt_ms>";

/// When a frame can have been stamped: from just before its packet was
/// pushed to just after the client read it (or the next push).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    pub(crate) from_ms: i64,
    pub(crate) to_ms: i64,
}

/// The receipt windows a frame's times may fall in.
#[derive(Debug, Clone)]
pub(crate) struct Receipts {
    /// The latest push's.
    pub(crate) latest: Window,
    /// Each push's, by its script line.
    pub(crate) by_push: BTreeMap<usize, Window>,
}

/// `Ok` when `got` is `want`; otherwise every difference, one per line,
/// and both frames.
pub(crate) fn exact(want: &Value, got: &Value, times: Option<&Receipts>) -> Result<(), String> {
    let mut diffs = Vec::new();
    walk("", want, got, times, &mut diffs);
    if diffs.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\n  expected {}\n  received {}",
        diffs.join("\n"),
        want,
        got
    ))
}

/// `got` as an expected frame would write it: an integer inside the latest
/// push's receipt window is `"<receipt_ms>"`, one inside an earlier push's
/// `"<receipt_ms:N>"` (`N` its script line). For pinning a case's expected
/// frames from what the desktop sent; random values are left as drawn.
pub(crate) fn as_expected(got: &Value, times: &Receipts) -> Value {
    match got {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), as_expected(v, times)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|v| as_expected(v, times)).collect()),
        Value::Number(n) if n.is_i64() => {
            let t = n.as_i64().unwrap_or_default();
            let inside = |w: &Window| (w.from_ms..=w.to_ms).contains(&t);
            if inside(&times.latest) {
                return Value::String(RECEIPT_MS.into());
            }
            times.by_push.iter().find(|(_, w)| inside(w)).map_or_else(
                || got.clone(),
                |(line, _)| Value::String(format!("<receipt_ms:{}>", line)),
            )
        }
        _ => got.clone(),
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

/// The receipt window a `<receipt_ms>` or `<receipt_ms:N>` placeholder
/// names, or why there is none; `None` if `p` is no such placeholder.
fn receipt_window(p: &str, times: Option<&Receipts>) -> Option<Result<Window, String>> {
    if p == RECEIPT_MS {
        return Some(
            times
                .map(|t| t.latest)
                .ok_or_else(|| "no push to time it by".into()),
        );
    }
    let line = p.strip_prefix("<receipt_ms:")?.strip_suffix('>')?;
    Some(match (line.parse::<usize>(), times) {
        (Ok(n), Some(t)) => t
            .by_push
            .get(&n)
            .copied()
            .ok_or_else(|| format!("no push on script line {}", n)),
        (Ok(_), None) => Err("no push to time it by".into()),
        (Err(_), _) => Err(format!("{} names no script line", p)),
    })
}

/// Whether a `<random:..>` placeholder's type is `got`'s; `None` if `p` is
/// no such placeholder.
/// The times a `<ist_today_ms:HH:MM:SS>` placeholder can be (that time on
/// the IST date of each end of the latest receipt window), or why there
/// are none; `None` if `p` is no such placeholder.
fn ist_today(p: &str, times: Option<&Receipts>) -> Option<Result<Vec<i64>, String>> {
    let tod = p.strip_prefix("<ist_today_ms:")?.strip_suffix('>')?;
    let Some(ms) = time_of_day_ms(tod) else {
        return Some(Err(format!("\"{}\" is not HH:MM:SS or HH:MM:SS.mmm", tod)));
    };
    let Some(w) = times.map(|t| t.latest) else {
        return Some(Err("no push to date it by".into()));
    };
    let midnight = |t: i64| (t + IST_OFFSET_MS).div_euclid(DAY_MS) * DAY_MS - IST_OFFSET_MS;
    let mut days = vec![midnight(w.from_ms) + ms, midnight(w.to_ms) + ms];
    days.dedup();
    Some(Ok(days))
}

/// `HH:MM:SS` or `HH:MM:SS.mmm` as ms after midnight.
fn time_of_day_ms(tod: &str) -> Option<i64> {
    let digits = |p: &str, n: usize| -> Option<i64> {
        Some(p)
            .filter(|p| p.len() == n && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse().ok())
    };
    let (hms, frac) = tod.split_once('.').unwrap_or((tod, "000"));
    let parts: Vec<i64> = hms
        .split(':')
        .map(|p| digits(p, 2))
        .collect::<Option<_>>()?;
    let ms = digits(frac, 3)?;
    match parts.as_slice() {
        [h, m, s] if *h < 24 && *m < 60 && *s < 60 => Some(((h * 60 + m) * 60 + s) * 1000 + ms),
        _ => None,
    }
}

fn random_type(p: &str, got: &Value) -> Option<bool> {
    Some(match p.strip_prefix("<random:")?.strip_suffix('>')? {
        "float" => got.is_f64(),
        "int" => got.is_i64() || got.is_u64(),
        "string" => got.is_string(),
        _ => return None,
    })
}

fn walk(path: &str, want: &Value, got: &Value, times: Option<&Receipts>, out: &mut Vec<String>) {
    let at = if path.is_empty() { "frame" } else { path };
    if let Value::String(p) = want {
        if let Some(window) = receipt_window(p, times) {
            match (window, got.as_i64()) {
                (Ok(w), Some(t)) if (w.from_ms..=w.to_ms).contains(&t) => {}
                (Ok(w), Some(t)) => out.push(format!(
                    "{}: {} is not a receipt time {} names ({}..={})",
                    at, t, p, w.from_ms, w.to_ms
                )),
                (Ok(_), None) => out.push(format!(
                    "{}: expected a receipt time in epoch ms, received {}",
                    at, got
                )),
                (Err(e), _) => out.push(format!("{}: {}: {}", at, p, e)),
            }
            return;
        }
        if let Some(days) = ist_today(p, times) {
            match (days, got.as_i64()) {
                (Ok(d), Some(t)) if d.contains(&t) => {}
                (Ok(d), Some(t)) => out.push(format!(
                    "{}: {} is not {} on the IST date of receipt ({:?})",
                    at, t, p, d
                )),
                (Ok(_), None) => out.push(format!(
                    "{}: expected an epoch-ms time of day, received {}",
                    at, got
                )),
                (Err(e), _) => out.push(format!("{}: {}: {}", at, p, e)),
            }
            return;
        }
        if let Some(ok) = random_type(p, got) {
            if !ok {
                out.push(format!("{}: expected {}, received {}", at, p, got));
            }
            return;
        }
    }
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => walk(&join(path, k), wv, gv, times, out),
                    None => out.push(format!("{}: missing (expected {})", join(path, k), wv)),
                }
            }
            for (k, gv) in g {
                if !w.contains_key(k) {
                    out.push(format!("{}: not expected (received {})", join(path, k), gv));
                }
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                out.push(format!(
                    "{}: {} items expected, {} received",
                    at,
                    w.len(),
                    g.len()
                ));
            }
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                walk(&format!("{}[{}]", path, i), wv, gv, times, out);
            }
        }
        _ if want == got => {}
        _ => out.push(format!("{}: expected {}, received {}", at, want, got)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The latest push from 1000 ms, and the push on line 5 from 500 to
    /// 900 ms.
    fn times() -> Receipts {
        Receipts {
            latest: Window {
                from_ms: 1_000,
                to_ms: 2_000,
            },
            by_push: BTreeMap::from([(
                5,
                Window {
                    from_ms: 500,
                    to_ms: 900,
                },
            )]),
        }
    }

    fn check(want: &Value, got: &Value) -> Result<(), String> {
        exact(want, got, Some(&times()))
    }

    #[test]
    fn numbers_compare_by_value_and_type() {
        assert!(check(&json!({"ltp": 2950.0}), &json!({"ltp": 2950.0})).is_ok());
        let e = check(&json!({"ltp": 2950.0}), &json!({"ltp": 2950})).unwrap_err();
        assert!(e.contains("ltp: expected 2950.0, received 2950"), "{}", e);
        assert!(check(&json!({"q": 7}), &json!({"q": 7.0})).is_err());
    }

    /// A price one ulp away is another price: the web's 148510 * (1/100)
    /// against the desktop's 1485.1, read as the harness reads every frame.
    #[test]
    fn a_price_one_ulp_away_is_a_difference() {
        use crate::feed_corpus::harness::json_text::parse;
        let want = parse(r#"{"ltp": 1485.1}"#).unwrap();
        let e = check(&want, &parse(r#"{"ltp": 1485.1000000000001}"#).unwrap()).unwrap_err();
        assert!(
            e.contains("ltp: expected 1485.1, received 1485.1000000000001"),
            "{}",
            e
        );
        assert!(check(&want, &parse(r#"{"ltp": 1485.10}"#).unwrap()).is_ok());
    }

    #[test]
    fn an_absent_field_must_be_absent_and_a_present_one_present() {
        let e = check(&json!({"a": 1}), &json!({"a": 1, "close": 0.0})).unwrap_err();
        assert!(e.contains("close: not expected (received 0.0)"), "{}", e);
        let e = check(&json!({"a": 1, "oi": 5}), &json!({"a": 1})).unwrap_err();
        assert!(e.contains("oi: missing (expected 5)"), "{}", e);
    }

    #[test]
    fn a_receipt_time_is_an_integer_inside_the_window() {
        let want = json!({"timestamp": RECEIPT_MS});
        assert!(check(&want, &json!({"timestamp": 1_500})).is_ok());
        assert!(check(&want, &json!({"timestamp": 0})).is_err());
        assert!(check(&want, &json!({"timestamp": 2_001})).is_err());
        assert!(check(&want, &json!({"timestamp": 1_500.0})).is_err());
        assert!(exact(&want, &json!({"timestamp": 1_500}), None).is_err());
    }

    /// A book after a ticker carries the ticker's receipt time: an earlier
    /// push's window, not the latest's.
    #[test]
    fn an_earlier_push_receipt_time_is_its_window() {
        let want = json!({"ltt": "<receipt_ms:5>"});
        assert!(check(&want, &json!({"ltt": 700})).is_ok());
        assert!(check(&want, &json!({"ltt": 1_500})).is_err());
        let e = check(&json!({"ltt": "<receipt_ms:6>"}), &json!({"ltt": 700})).unwrap_err();
        assert!(e.contains("no push on script line 6"), "{}", e);
    }

    /// What a case is pinned from: receipt times as their placeholders,
    /// everything else as sent.
    #[test]
    fn a_frame_as_expected_writes_receipt_times_as_placeholders() {
        let got = json!({"ltt": 700, "timestamp": 1_500, "volume": 1_500.0, "oi": 42});
        let want = json!({
            "ltt": "<receipt_ms:5>",
            "timestamp": RECEIPT_MS,
            "volume": 1_500.0,
            "oi": 42
        });
        assert_eq!(as_expected(&got, &times()), want);
        assert!(check(&want, &got).is_ok());
    }

    fn received_between(from_ms: i64, to_ms: i64) -> Receipts {
        Receipts {
            latest: Window { from_ms, to_ms },
            by_push: BTreeMap::new(),
        }
    }

    /// 2026-11-09 09:30:00 IST, and that day's 09:15:07 IST.
    const AT_0930_IST: i64 = 1_794_196_800_000;
    const AT_091507_IST: i64 = 1_794_195_907_000;

    #[test]
    fn a_time_of_day_is_dated_by_the_ist_day_of_receipt() {
        let times = received_between(AT_0930_IST, AT_0930_IST + 1_000);
        let want = json!({"ltt": "<ist_today_ms:09:15:07>"});
        assert!(exact(&want, &json!({"ltt": AT_091507_IST}), Some(&times)).is_ok());
        let e = exact(&want, &json!({"ltt": AT_091507_IST + 1}), Some(&times)).unwrap_err();
        assert!(e.contains("is not <ist_today_ms:09:15:07>"), "{}", e);
        assert!(exact(&want, &json!({"ltt": AT_091507_IST + DAY_MS}), Some(&times)).is_err());
        let with_ms = json!({"ltt": "<ist_today_ms:09:15:07.250>"});
        assert!(exact(&with_ms, &json!({"ltt": AT_091507_IST + 250}), Some(&times)).is_ok());
        let bad = exact(
            &json!({"ltt": "<ist_today_ms:9:15>"}),
            &json!({"ltt": 1}),
            Some(&times),
        );
        assert!(bad.unwrap_err().contains("is not HH:MM:SS"));
    }

    /// Received across IST midnight: the time of day on either date.
    #[test]
    fn across_ist_midnight_either_date_counts() {
        let midnight = AT_0930_IST + (14 * 60 + 30) * 60 * 1000;
        let times = received_between(midnight - 100, midnight + 100);
        let want = json!({"ltt": "<ist_today_ms:09:15:07>"});
        for t in [AT_091507_IST, AT_091507_IST + DAY_MS] {
            assert!(
                exact(&want, &json!({"ltt": t}), Some(&times)).is_ok(),
                "{}",
                t
            );
        }
    }

    #[test]
    fn a_random_value_is_any_value_of_its_type() {
        let want = json!({"ltp": "<random:float>", "volume": "<random:int>"});
        assert!(check(&want, &json!({"ltp": 101.25, "volume": 900})).is_ok());
        let e = check(&want, &json!({"ltp": 101, "volume": 900})).unwrap_err();
        assert!(
            e.contains("ltp: expected <random:float>, received 101"),
            "{}",
            e
        );
        assert!(check(&want, &json!({"ltp": 101.25})).is_err());
    }

    #[test]
    fn books_compare_level_by_level() {
        let want = json!({"depth": {"buy": [{"price": 1.0}, {"price": 2.0}]}});
        let e = check(&want, &json!({"depth": {"buy": [{"price": 1.0}]}})).unwrap_err();
        assert!(
            e.contains("depth.buy: 2 items expected, 1 received"),
            "{}",
            e
        );
        let e = check(
            &want,
            &json!({"depth": {"buy": [{"price": 1.0}, {"price": 2.5}]}}),
        )
        .unwrap_err();
        assert!(
            e.contains("depth.buy[1].price: expected 2.0, received 2.5"),
            "{}",
            e
        );
    }
}
