//! `json.loads` of the node's params text, keeping what the Python dict it
//! builds keeps: the keys in written order (a repeated key: its first
//! place, its last value) and a non-finite float (`NaN`, `Infinity`,
//! `-Infinity`, `1e999`) as a float, not as text. What parses, and every
//! value, is the shared decoder's (`flow::nodes::pyjson`, Python's
//! acceptance); this only splits the outermost object into its members.

use serde_json::Value;

use crate::flow::contract::pyfmt;
use crate::flow::nodes::pyjson;

/// What the params text decodes to.
pub(super) enum Loaded {
    /// An object's members in written order: the key, the value, and
    /// whether the value is a non-finite float (its Python text is the
    /// value then).
    Object(Vec<(String, Value, bool)>),
    /// Anything else, by its Python type name.
    Other(&'static str),
}

/// `json.loads(raw)`, or `None` where it raises.
pub(super) fn loads(raw: &str) -> Option<Loaded> {
    let whole = pyjson::loads(raw).ok()?;
    if !whole.is_object() {
        let text = raw.trim_matches(is_space);
        return Some(Loaded::Other(if non_finite(text, &whole) {
            "float"
        } else {
            pyfmt::type_name(&whole)
        }));
    }
    let mut members = Vec::new();
    for (key, value) in split(raw)? {
        let Ok(Value::String(key)) = pyjson::loads(&key) else {
            return None;
        };
        let decoded = pyjson::loads(&value).ok()?;
        let float = non_finite(&value, &decoded);
        members.push((key, decoded, float));
    }
    Some(Loaded::Object(members))
}

/// The decoder keeps a non-finite float as its Python text: a value that
/// decodes to text without being written as a string is one.
fn non_finite(text: &str, decoded: &Value) -> bool {
    decoded.is_string() && !text.starts_with('"')
}

/// JSON's whitespace.
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// The outermost object's members as written: `(key, value)` text, from
/// text the decoder has accepted.
fn split(raw: &str) -> Option<Vec<(String, String)>> {
    let s: Vec<char> = raw.chars().collect();
    let skip = |mut i: usize| {
        while s.get(i).is_some_and(|c| is_space(*c)) {
            i += 1;
        }
        i
    };
    let mut i = skip(0);
    if s.get(i) != Some(&'{') {
        return None;
    }
    i = skip(i + 1);
    let mut members = Vec::new();
    if s.get(i) == Some(&'}') {
        return Some(members);
    }
    loop {
        let key_end = string_end(&s, i)?;
        let key: String = s[i..key_end].iter().collect();
        i = skip(key_end);
        if s.get(i) != Some(&':') {
            return None;
        }
        let start = skip(i + 1);
        let end = value_end(&s, start)?;
        let value: String = s[start..end].iter().collect();
        members.push((key, value.trim_end_matches(is_space).to_string()));
        i = skip(end);
        match s.get(i) {
            Some(',') => i = skip(i + 1),
            Some('}') => return Some(members),
            _ => return None,
        }
    }
}

/// Just past the string that opens at `start`.
fn string_end(s: &[char], start: usize) -> Option<usize> {
    if s.get(start) != Some(&'"') {
        return None;
    }
    let mut i = start + 1;
    loop {
        match s.get(i)? {
            '\\' => i += 2,
            '"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// Where the value that starts at `start` ends: the `,` or `}` after it.
fn value_end(s: &[char], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    loop {
        match s.get(i)? {
            '"' => {
                i = string_end(s, i)?;
                continue;
            }
            '[' | '{' => depth += 1,
            ']' | '}' if depth == 0 => return Some(i),
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
}
