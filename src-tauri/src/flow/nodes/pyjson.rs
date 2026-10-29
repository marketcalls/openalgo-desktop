//! Python's `json.loads` (CPython 3.12, `json.decoder` with the C
//! scanner): what it accepts beyond strict JSON (`NaN`, `Infinity`,
//! `-Infinity`, numbers past any float) and its error sentences
//! (`Expecting value: line 1 column 1 (char 0)`), which the Variable and
//! HTTP Request nodes put in front of the trader.
//!
//! Two values a JSON value cannot hold are kept as their Python text: a
//! non-finite float (`"nan"`, `"inf"`, `"-inf"`), so `{{x}}` reads as on
//! the web, and an integer past 64 bits, which becomes the nearest float.

use crate::flow::contract::pyfmt;
use serde_json::{Map, Number, Value};

/// Deepest nesting decoded (Python's recursion limit stops it about here).
const MAX_DEPTH: usize = 900;

/// A float as a run stores it: a number when finite, else its Python text.
pub fn float_value(f: f64) -> Value {
    match Number::from_f64(f) {
        Some(n) => Value::Number(n),
        None => Value::String(pyfmt::float(f)),
    }
}

/// `json.loads(text)`: the value, or the `JSONDecodeError` text.
pub fn loads(text: &str) -> Result<Value, String> {
    let s: Vec<char> = text.chars().collect();
    let d = Decoder { s: &s, depth: 0 };
    if s.first() == Some(&'\u{feff}') {
        return Err(d.message("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let start = d.skip_ws(0);
    let (value, end) = match d.clone().scan(start) {
        Ok(v) => v,
        Err(Stop::At(i)) => return Err(d.message("Expecting value", i)),
        Err(Stop::Fail(m)) => return Err(m),
    };
    let end = d.skip_ws(end);
    if end != s.len() {
        return Err(d.message("Extra data", end));
    }
    Ok(value)
}

enum Stop {
    /// No value starts here (`StopIteration`).
    At(usize),
    /// A decode error, already worded.
    Fail(String),
}

#[derive(Clone)]
struct Decoder<'a> {
    s: &'a [char],
    depth: usize,
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Decoder<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(is_ws) {
            i += 1;
        }
        i
    }

    /// `JSONDecodeError`'s text: the message and where, in characters.
    fn message(&self, msg: &str, pos: usize) -> String {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = match before.iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        format!("{msg}: line {lineno} column {colno} (char {pos})")
    }

    fn fail(&self, msg: &str, pos: usize) -> Stop {
        Stop::Fail(self.message(msg, pos))
    }

    fn word(&self, i: usize, w: &str) -> bool {
        w.chars()
            .enumerate()
            .all(|(k, c)| self.at(i + k) == Some(c))
    }

    fn scan(&mut self, i: usize) -> Result<(Value, usize), Stop> {
        let Some(c) = self.at(i) else {
            return Err(Stop::At(i));
        };
        match c {
            '"' => {
                let (text, end) = self.string(i + 1)?;
                return Ok((Value::String(text), end));
            }
            '{' | '[' => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    let what = if c == '{' { "object" } else { "array" };
                    return Err(Stop::Fail(format!(
                        "maximum recursion depth exceeded while decoding a JSON {} from a unicode string",
                        what
                    )));
                }
                let out = if c == '{' {
                    self.object(i + 1)
                } else {
                    self.array(i + 1)
                };
                self.depth -= 1;
                return out;
            }
            _ => {}
        }
        for (w, v) in [
            ("null", Value::Null),
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("NaN", float_value(f64::NAN)),
            ("Infinity", float_value(f64::INFINITY)),
            ("-Infinity", float_value(f64::NEG_INFINITY)),
        ] {
            if self.word(i, w) {
                return Ok((v, i + w.chars().count()));
            }
        }
        self.number(i)
    }

    fn object(&mut self, mut i: usize) -> Result<(Value, usize), Stop> {
        let mut map = Map::new();
        i = self.skip_ws(i);
        if self.at(i) == Some('}') {
            return Ok((Value::Object(map), i + 1));
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(self.fail("Expecting property name enclosed in double quotes", i));
            }
            let (key, next) = self.string(i + 1)?;
            i = self.skip_ws(next);
            if self.at(i) != Some(':') {
                return Err(self.fail("Expecting ':' delimiter", i));
            }
            i = self.skip_ws(i + 1);
            let (value, next) = self.nested(i)?;
            map.insert(key, value);
            i = self.skip_ws(next);
            if self.at(i) == Some('}') {
                return Ok((Value::Object(map), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(self.fail("Expecting ',' delimiter", i));
            }
            i = self.skip_ws(i + 1);
        }
    }

    fn array(&mut self, mut i: usize) -> Result<(Value, usize), Stop> {
        let mut list = Vec::new();
        i = self.skip_ws(i);
        if self.at(i) == Some(']') {
            return Ok((Value::Array(list), i + 1));
        }
        loop {
            let (value, next) = self.nested(i)?;
            list.push(value);
            i = self.skip_ws(next);
            if self.at(i) == Some(']') {
                return Ok((Value::Array(list), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(self.fail("Expecting ',' delimiter", i));
            }
            i = self.skip_ws(i + 1);
        }
    }

    /// A value inside an object or array: no value here is "Expecting
    /// value" at this position.
    fn nested(&mut self, i: usize) -> Result<(Value, usize), Stop> {
        match self.scan(i) {
            Err(Stop::At(at)) => Err(self.fail("Expecting value", at)),
            other => other,
        }
    }

    /// A string body from just after its opening quote (`scanstring`).
    fn string(&self, start: usize) -> Result<(String, usize), Stop> {
        let begin = start - 1;
        let mut out = String::new();
        let mut end = start;
        loop {
            let mut next = end;
            let mut c = None;
            while let Some(d) = self.at(next) {
                if d == '"' || d == '\\' {
                    c = Some(d);
                    break;
                }
                if (d as u32) <= 0x1f {
                    return Err(self.fail("Invalid control character at", next));
                }
                out.push(d);
                next += 1;
            }
            let Some(c) = c else {
                return Err(self.fail("Unterminated string starting at", begin));
            };
            next += 1;
            if c == '"' {
                return Ok((out, next));
            }
            let Some(e) = self.at(next) else {
                return Err(self.fail("Unterminated string starting at", begin));
            };
            if e != 'u' {
                end = next + 1;
                let ch = match e {
                    '"' | '\\' | '/' => e,
                    'b' => '\x08',
                    'f' => '\x0c',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(self.fail("Invalid \\escape", end - 2)),
                };
                out.push(ch);
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= self.s.len() {
                return Err(self.fail("Invalid \\uXXXX escape", next - 1));
            }
            let mut cp = self
                .hex4(next)
                .ok_or_else(|| self.fail("Invalid \\uXXXX escape", end - 5))?;
            if (0xd800..0xdc00).contains(&cp)
                && end + 6 < self.s.len()
                && self.at(end) == Some('\\')
                && self.at(end + 1) == Some('u')
            {
                let low = self
                    .hex4(end + 2)
                    .ok_or_else(|| self.fail("Invalid \\uXXXX escape", end + 1))?;
                if (0xdc00..0xe000).contains(&low) {
                    cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                    end += 6;
                }
            }
            // A lone surrogate cannot be held in a Rust string.
            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        }
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        let text: String = (i..i + 4).filter_map(|k| self.at(k)).collect();
        if text.chars().count() != 4 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(&text, 16).ok()
    }

    /// `_match_number_unicode`.
    fn number(&self, start: usize) -> Result<(Value, usize), Stop> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let len = self.s.len();
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
            if i >= len {
                return Err(Stop::At(start));
            }
        }
        match self.at(i) {
            Some('1'..='9') => {
                i += 1;
                while digit(i) {
                    i += 1;
                }
            }
            Some('0') => i += 1,
            _ => return Err(Stop::At(start)),
        }
        let mut is_float = false;
        if i + 1 < len && self.at(i) == Some('.') && digit(i + 1) {
            is_float = true;
            i += 2;
            while digit(i) {
                i += 1;
            }
        }
        if i + 1 < len && matches!(self.at(i), Some('e') | Some('E')) {
            let e_start = i;
            i += 1;
            if i + 1 < len && matches!(self.at(i), Some('-') | Some('+')) {
                i += 1;
            }
            while digit(i) {
                i += 1;
            }
            if digit(i - 1) {
                is_float = true;
            } else {
                i = e_start;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        let value = if is_float {
            float_value(text.parse::<f64>().unwrap_or(f64::NAN))
        } else if let Ok(n) = text.parse::<i64>() {
            Value::from(n)
        } else if let Ok(n) = text.parse::<u64>() {
            Value::from(n)
        } else {
            float_value(text.parse::<f64>().unwrap_or(f64::NAN))
        };
        Ok((value, i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_read_as_python_writes_them() {
        assert_eq!(
            loads("nope"),
            Err("Expecting value: line 1 column 1 (char 0)".into())
        );
        assert_eq!(
            loads("{bad"),
            Err(
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
                    .into()
            )
        );
        assert_eq!(
            loads("[\n1,\n2\n,]"),
            Err("Expecting value: line 4 column 2 (char 8)".into())
        );
    }

    #[test]
    fn values_parse_as_python_parses_them() {
        assert_eq!(
            loads("{\"k\": [true, null]}"),
            Ok(json!({"k": [true, null]}))
        );
        assert_eq!(loads(" 1E5 "), Ok(json!(100000.0)));
        assert_eq!(loads("NaN"), Ok(json!("nan")));
        assert_eq!(loads("\"\\ud83d\\ude00\""), Ok(json!("\u{1F600}")));
        assert_eq!(loads("{\"a\":1,\"a\":2}"), Ok(json!({"a": 2})));
    }

    /// Nesting far past the limit is refused with Python's sentence and
    /// never overflows a tokio worker's 2 MiB stack (debug build frames).
    #[test]
    fn very_deep_nesting_is_refused_on_a_worker_sized_stack() {
        let decoded = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let arrays = "[".repeat(100_000);
                let objects = "{\"a\": ".repeat(100_000);
                let mixed = "[{\"a\": ".repeat(50_000);
                (loads(&arrays), loads(&objects), loads(&mixed))
            })
            .unwrap()
            .join()
            .unwrap();
        let sentence = |kind: &str| {
            Err(format!(
                "maximum recursion depth exceeded while decoding a JSON {} from a unicode string",
                kind
            ))
        };
        assert_eq!(decoded.0, sentence("array"));
        assert_eq!(decoded.1, sentence("object"));
        assert!(decoded.2 == sentence("array") || decoded.2 == sentence("object"));
        // Within the limit, deep values still decode.
        let fine = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(loads(&fine).is_ok());
    }


    /// Every text in `logic_vectors.json` decodes as Python 3.12's
    /// `json.loads` decodes it, or fails with its sentence. Values a JSON
    /// value cannot hold (non-finite floats, integers past 64 bits) and a
    /// repr Python escapes (U+2028) are checked to decode, not by repr.
    #[test]
    fn every_vector_decodes_as_python() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/flow/logic_vectors.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let unrepresentable = |ok: &str| {
            ok.contains("nan") || ok.contains("inf") || ok.contains("\\u2028") || (ok.len() > 25 && ok.chars().all(|c| c.is_ascii_digit()))
        };
        let mut checked = 0;
        for case in v["json"].as_array().unwrap() {
            let text = case["text"].as_str().unwrap();
            let got = loads(text);
            if let Some(err) = case["err"].as_str() {
                assert_eq!(got, Err(err.to_string()), "{:?}", text);
            } else {
                let ok = case["ok"].as_str().unwrap();
                let value = got.unwrap_or_else(|e| panic!("{:?}: {}", text, e));
                if !unrepresentable(ok) {
                    assert_eq!(pyfmt::repr(&value), ok, "{:?}", text);
                }
            }
            checked += 1;
        }
        assert!(checked > 60);
    }

}
