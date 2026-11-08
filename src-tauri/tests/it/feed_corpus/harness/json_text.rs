//! JSON read with its numbers exact. serde_json's default float parser is
//! not correctly rounded (its `float_roundtrip` feature is off), so
//! "1485.1000000000001", the web's 148510 * (1/100), reads as 1485.1 and a
//! one-ulp difference between the web, the desktop and a case would never
//! show. Every frame, file and answer the harness compares is read here:
//! numbers by the standard library's parser, which rounds correctly (a
//! number's text is its exact double); strings by serde_json, which reads
//! them exactly. The feature is not turned on for the test build instead:
//! the app's own decoders would then read broker JSON differently under test
//! than in a release.

use serde_json::{Map, Number, Value};

/// `text` as JSON, with exact numbers.
pub(crate) fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        text,
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("text after the JSON value"));
    }
    Ok(v)
}

/// `bytes` (UTF-8) as JSON, with exact numbers.
pub(crate) fn parse_slice(bytes: &[u8]) -> Result<Value, String> {
    parse(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
}

struct Parser<'a> {
    text: &'a str,
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("{} at byte {}", what, self.i)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => self.literal(),
            None => Err(self.err("unexpected end")),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.eat(b'{')?;
        let mut map = Map::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            let v = self.value()?;
            map.insert(key, v);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    /// A string token, unescaped by serde_json.
    fn string(&mut self) -> Result<String, String> {
        let start = self.i;
        self.eat(b'"')?;
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'\\') => self.i += 2,
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(_) => self.i += 1,
            }
        }
        serde_json::from_str(&self.text[start..self.i]).map_err(|e| self.err(&e.to_string()))
    }

    /// An integer as serde_json keeps one (i64, else u64); anything with a
    /// fraction or an exponent as the double its text names exactly.
    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        let mut float = false;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let t = &self.text[start..self.i];
        if !float {
            if let Ok(n) = t.parse::<i64>() {
                return Ok(Value::from(n));
            }
            if let Ok(n) = t.parse::<u64>() {
                return Ok(Value::from(n));
            }
        }
        let f: f64 = t.parse().map_err(|_| self.err("not a number"))?;
        Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| self.err("not a finite number"))
    }

    fn literal(&mut self) -> Result<Value, String> {
        for (word, v) in [
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("null", Value::Null),
        ] {
            if self.text[self.i..].starts_with(word) {
                self.i += word.len();
                return Ok(v);
            }
        }
        Err(self.err("unexpected character"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The web's 148510 * (1/100) is the double just above 1485.1; read
    /// exactly, the two differ.
    #[test]
    fn a_number_one_ulp_away_reads_as_another_number() {
        let near = parse("1485.1000000000001").unwrap();
        let price = parse("1485.1").unwrap();
        assert_ne!(near, price);
        assert_eq!(near.as_f64(), Some(148510.0 * (1.0 / 100.0)));
        assert_eq!(price, parse("1485.10").unwrap());
    }

    #[test]
    fn reads_what_serde_json_reads_otherwise() {
        let text = r#" {"a": [1, -2, 18446744073709551615, 2.5e3, true, false, null],
                        "s": "q\"uote é \\ /", "o": {}, "e": []} "#;
        let want = json!({
            "a": [1, -2, 18446744073709551615u64, 2500.0, true, false, null],
            "s": "q\"uote \u{e9} \\ /",
            "o": {},
            "e": [],
        });
        assert_eq!(parse(text).unwrap(), want);
        assert_eq!(parse("7").unwrap(), json!(7));
        assert!(parse(r#"{"a": 1"#).is_err());
        assert!(parse("[1] x").is_err());
    }
}
