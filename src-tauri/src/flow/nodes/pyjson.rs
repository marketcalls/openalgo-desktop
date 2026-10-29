//! Python's `json.loads` (CPython 3.12's C scanner) over text: what it
//! accepts and the exact error it raises, `Expecting value: line 1 column 1
//! (char 0)`. The web's nodes put that error text in front of the trader
//! (the Margin node's basket: `Margin positions is not valid JSON after
//! variable interpolation: {exc}`), and accept what Python accepts (`NaN`,
//! `Infinity`), so `serde_json` alone answers differently.
//!
//! `NaN`, `Infinity` and `-Infinity` have no JSON value; they come back as
//! the text `"nan"`, `"inf"` and `"-inf"`, which every numeric reader here
//! (Python's `float()` rules) reads as the same number. A whole number too
//! large for 64 bits comes back as the nearest float.

use serde_json::{Map, Number, Value};

/// A `json.JSONDecodeError`: the message and the character index it points
/// at, shown as Python shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct PyJsonError {
    pub msg: &'static str,
    /// Character (not byte) index into the text.
    pub pos: usize,
    lineno: usize,
    colno: usize,
}

impl std::fmt::Display for PyJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: line {} column {} (char {})",
            self.msg, self.lineno, self.colno, self.pos
        )
    }
}

/// `json.loads(text)`.
pub fn loads(text: &str) -> Result<Value, PyJsonError> {
    let s: Vec<char> = text.chars().collect();
    let fail = |msg: &'static str, pos: usize| error(&s, msg, pos);
    if s.first() == Some(&'\u{feff}') {
        return Err(fail("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let scan = Scanner { s: &s };
    let start = scan.ws(0);
    let (value, end) = match scan.value(start) {
        Ok(v) => v,
        Err(Fail::Stop(at)) => return Err(fail("Expecting value", at)),
        Err(Fail::Error(msg, at)) => return Err(fail(msg, at)),
    };
    let end = scan.ws(end);
    if end != s.len() {
        return Err(fail("Extra data", end));
    }
    Ok(value)
}

fn error(s: &[char], msg: &'static str, pos: usize) -> PyJsonError {
    let before = &s[..pos.min(s.len())];
    let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
    let colno = match before.iter().rposition(|c| *c == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    PyJsonError {
        msg,
        pos,
        lineno,
        colno,
    }
}

/// How a scan failed: Python's `StopIteration(idx)` (no value starts here,
/// reported by the caller as `Expecting value`) or an error raised in place.
enum Fail {
    Stop(usize),
    Error(&'static str, usize),
}

type Scan<T> = Result<T, Fail>;

struct Scanner<'a> {
    s: &'a [char],
}

impl Scanner<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn ws(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    }

    fn starts(&self, i: usize, word: &str) -> bool {
        word.chars()
            .enumerate()
            .all(|(k, c)| self.at(i + k) == Some(c))
    }

    /// `scan_once`: one value starting exactly at `i`.
    fn value(&self, i: usize) -> Scan<(Value, usize)> {
        let Some(c) = self.at(i) else {
            return Err(Fail::Stop(i));
        };
        let word = |w: &str, v: Value| self.starts(i, w).then(|| (v, i + w.chars().count()));
        let named = match c {
            '"' => return self.string(i + 1).map(|(t, e)| (Value::String(t), e)),
            '{' => return self.object(i + 1),
            '[' => return self.array(i + 1),
            'n' => word("null", Value::Null),
            't' => word("true", Value::Bool(true)),
            'f' => word("false", Value::Bool(false)),
            'N' => word("NaN", Value::from("nan")),
            'I' => word("Infinity", Value::from("inf")),
            '-' => word("-Infinity", Value::from("-inf")),
            _ => None,
        };
        match named {
            Some(found) => Ok(found),
            None => self.number(i),
        }
    }

    fn digit(&self, i: usize) -> bool {
        self.at(i).is_some_and(|c| c.is_ascii_digit())
    }

    /// `_match_number_unicode`: `-?(0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`,
    /// backing off a fraction or exponent that has no digits.
    fn number(&self, start: usize) -> Scan<(Value, usize)> {
        let len = self.s.len();
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
            if i >= len {
                return Err(Fail::Stop(start));
            }
        }
        match self.at(i) {
            Some('1'..='9') => {
                i += 1;
                while self.digit(i) {
                    i += 1;
                }
            }
            Some('0') => i += 1,
            _ => return Err(Fail::Stop(start)),
        }
        let mut is_float = false;
        if i + 1 < len && self.at(i) == Some('.') && self.digit(i + 1) {
            is_float = true;
            i += 2;
            while self.digit(i) {
                i += 1;
            }
        }
        if i + 1 < len && matches!(self.at(i), Some('e' | 'E')) {
            let e_start = i;
            i += 1;
            if i + 1 < len && matches!(self.at(i), Some('-' | '+')) {
                i += 1;
            }
            while self.digit(i) {
                i += 1;
            }
            if self.digit(i - 1) {
                is_float = true;
            } else {
                i = e_start;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        Ok((number_value(&text, is_float), i))
    }

    /// `scanstring`: the text after an opening quote at `begin - 1`.
    fn string(&self, begin: usize) -> Scan<(String, usize)> {
        let unterminated = Fail::Error("Unterminated string starting at", begin - 1);
        let mut out = String::new();
        let mut i = begin;
        loop {
            let Some(c) = self.at(i) else {
                return Err(unterminated);
            };
            match c {
                '"' => return Ok((out, i + 1)),
                '\\' => {
                    let (ch, next) = self.escape(i, begin)?;
                    out.push(ch);
                    i = next;
                }
                c if (c as u32) <= 0x1f => {
                    return Err(Fail::Error("Invalid control character at", i));
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
    }

    /// One escape whose backslash is at `slash`: the character and where
    /// the text continues.
    fn escape(&self, slash: usize, begin: usize) -> Scan<(char, usize)> {
        let Some(c) = self.at(slash + 1) else {
            return Err(Fail::Error("Unterminated string starting at", begin - 1));
        };
        let simple = match c {
            '"' => Some('"'),
            '\\' => Some('\\'),
            '/' => Some('/'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'u' => None,
            _ => return Err(Fail::Error("Invalid \\escape", slash)),
        };
        if let Some(ch) = simple {
            return Ok((ch, slash + 2));
        }
        let first = slash + 2;
        let end = first + 4;
        if end >= self.s.len() {
            return Err(Fail::Error("Invalid \\uXXXX escape", first - 1));
        }
        let unit = self
            .hex4(first)
            .ok_or(Fail::Error("Invalid \\uXXXX escape", first - 1))?;
        if (0xd800..=0xdbff).contains(&unit)
            && end + 6 < self.s.len()
            && self.at(end) == Some('\\')
            && self.at(end + 1) == Some('u')
        {
            let low = self
                .hex4(end + 2)
                .ok_or(Fail::Error("Invalid \\uXXXX escape", end + 1))?;
            if (0xdc00..=0xdfff).contains(&low) {
                let joined = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                return Ok((char::from_u32(joined).unwrap_or('\u{fffd}'), end + 6));
            }
        }
        Ok((char::from_u32(unit).unwrap_or('\u{fffd}'), end))
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        (i..i + 4).try_fold(0u32, |acc, k| Some(acc * 16 + self.at(k)?.to_digit(16)?))
    }

    /// `_parse_object_unicode`: the members after `{`.
    fn object(&self, open: usize) -> Scan<(Value, usize)> {
        let mut map = Map::new();
        let mut i = self.ws(open);
        if self.at(i) == Some('}') {
            return Ok((Value::Object(map), i + 1));
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(Fail::Error(
                    "Expecting property name enclosed in double quotes",
                    i,
                ));
            }
            let (key, after_key) = self.string(i + 1)?;
            i = self.ws(after_key);
            if self.at(i) != Some(':') {
                return Err(Fail::Error("Expecting ':' delimiter", i));
            }
            i = self.ws(i + 1);
            let (value, after_value) = self.value(i)?;
            map.insert(key, value);
            i = self.ws(after_value);
            match self.at(i) {
                Some('}') => return Ok((Value::Object(map), i + 1)),
                Some(',') => i = self.ws(i + 1),
                _ => return Err(Fail::Error("Expecting ',' delimiter", i)),
            }
        }
    }

    /// `_parse_array_unicode`: the items after `[`.
    fn array(&self, open: usize) -> Scan<(Value, usize)> {
        let mut items = Vec::new();
        let mut i = self.ws(open);
        if self.at(i) == Some(']') {
            return Ok((Value::Array(items), i + 1));
        }
        loop {
            let (value, after) = self.value(i)?;
            items.push(value);
            i = self.ws(after);
            match self.at(i) {
                Some(']') => return Ok((Value::Array(items), i + 1)),
                Some(',') => i = self.ws(i + 1),
                _ => return Err(Fail::Error("Expecting ',' delimiter", i)),
            }
        }
    }
}

/// A matched number: an integer as an integer when it fits, otherwise a
/// float; a float that overflows is Python's `inf`.
fn number_value(text: &str, is_float: bool) -> Value {
    if !is_float {
        if let Ok(n) = text.parse::<i64>() {
            return Value::from(n);
        }
        if let Ok(n) = text.parse::<u64>() {
            return Value::from(n);
        }
    }
    let f: f64 = text.parse().unwrap_or(f64::NAN);
    match Number::from_f64(f) {
        Some(n) => Value::Number(n),
        None if f > 0.0 => Value::from("inf"),
        None if f < 0.0 => Value::from("-inf"),
        None => Value::from("nan"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(text: &str) -> String {
        loads(text)
            .map(|v| v.to_string())
            .unwrap_or_else(|e| e.to_string())
    }

    /// Every message pinned here is what CPython 3.12's `json.loads` raises
    /// for the same text.
    #[test]
    fn errors_read_as_pythons() {
        let cases = [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            ("   ", "Expecting value: line 1 column 4 (char 3)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "[{\"symbol\": \"SBIN\",]",
                "Expecting property name enclosed in double quotes: line 1 column 20 (char 19)",
            ),
            (
                "{\"a\" 1}",
                "Expecting ':' delimiter: line 1 column 6 (char 5)",
            ),
            (
                "{\"a\": 1",
                "Expecting ',' delimiter: line 1 column 8 (char 7)",
            ),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            (
                "{period: 5}",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            ("\"a\\x\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            (
                "\"a\\u12\"",
                "Invalid \\uXXXX escape: line 1 column 4 (char 3)",
            ),
            (
                "\"a\tb\"",
                "Invalid control character at: line 1 column 3 (char 2)",
            ),
            ("1 2", "Extra data: line 1 column 3 (char 2)"),
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("1.", "Extra data: line 1 column 2 (char 1)"),
            ("-", "Expecting value: line 1 column 1 (char 0)"),
            ("[\n  1,\n  x]", "Expecting value: line 3 column 3 (char 9)"),
            (
                "\u{feff}[]",
                "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(err(text), want, "{:?}", text);
        }
    }

    #[test]
    fn values_read_as_pythons() {
        assert_eq!(
            loads(" [1, 2.5, -0, 1e2, \"\\u00e9\\n\", true, null, {\"k\": [ ]}] ").unwrap(),
            json!([1, 2.5, 0, 100.0, "\u{e9}\n", true, null, {"k": []}])
        );
        assert_eq!(loads("1E5").unwrap(), json!(100000.0));
        assert!(loads("1e5").unwrap().is_f64());
        assert_eq!(
            loads("[NaN, Infinity, -Infinity]").unwrap(),
            json!(["nan", "inf", "-inf"])
        );
        assert_eq!(loads("1e400").unwrap(), json!("inf"));
        assert_eq!(loads("\"\\ud83d\\ude00\"").unwrap(), json!("\u{1f600}"));
        assert_eq!(loads("{\"a\": 1, \"a\": 2}").unwrap(), json!({"a": 2}));
    }
}
