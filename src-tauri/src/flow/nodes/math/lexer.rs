//! Python 3.12's tokenizer, as much of it as an expression needs: numbers
//! in every literal form, names, string and bytes literals (f-strings only
//! as far as finding their end), operators, comments, line continuation
//! and the bracket checks whose sentences the tokenizer reports.
//!
//! An error is a [`SyntaxError`]; its text is Python's where the web's
//! golden pins it (an unclosed bracket) and in the common cases, else
//! `invalid syntax` (lead decision 5 keeps only the prefix the evaluator
//! adds).

use super::bigint::BigInt;
use super::num::Num;

/// A syntax error and the line it is reported on.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub message: String,
    pub line: usize,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, line: usize) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }

    /// Python's `str(SyntaxError)` for code parsed from a string.
    pub fn text(&self) -> String {
        format!("{} (<unknown>, line {})", self.message, self.line)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// A number: an int or float value, or an imaginary literal's text.
    Number(Num),
    Imaginary(f64),
    Name(String),
    /// A string or bytes literal: its decoded text; `None` for an
    /// f-string.
    Str {
        text: Option<String>,
        bytes: bool,
    },
    Op(&'static str),
    Newline,
    End,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
}

const OPS: &[&str] = &[
    "**=", "//=", ">>=", "<<=", "...", "->", ":=", "**", "//", "<<", ">>", "<=", ">=", "==", "!=",
    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "@=", "+", "-", "*", "/", "%", "@", "&", "|",
    "^", "~", "<", ">", "(", ")", "[", "]", "{", "}", ",", ":", ";", ".", "=", "!",
];

/// Deepest bracket nesting accepted (Python's tokenizer allows 200; each
/// level costs the parser a chain of frames, so the desktop stops at 50).
const MAX_BRACKETS: usize = 50;

/// Keywords a number may run into without a space (`1if x else y`).
const NUMBER_FOLLOWERS: &[&str] = &["and", "else", "for", "if", "in", "is", "not", "or"];

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    brackets: Vec<(char, usize)>,
    out: Vec<Token>,
}

pub fn tokenize(src: &str) -> Result<Vec<Token>, SyntaxError> {
    let mut lx = Lexer {
        chars: src.chars().collect(),
        pos: 0,
        line: 1,
        brackets: Vec::new(),
        out: Vec::new(),
    };
    lx.run()?;
    Ok(lx.out)
}

fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

impl Lexer {
    fn peek(&self, off: usize) -> Option<char> {
        self.chars.get(self.pos + off).copied()
    }

    fn push(&mut self, tok: Tok) {
        self.out.push(Token {
            tok,
            line: self.line,
        });
    }

    fn err(&self, message: impl Into<String>) -> SyntaxError {
        SyntaxError::new(message, self.line)
    }

    fn run(&mut self) -> Result<(), SyntaxError> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\x0c' => self.pos += 1,
                '#' => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                '\\' => {
                    if self.peek(1) == Some('\n') {
                        self.pos += 2;
                        self.line += 1;
                    } else {
                        return Err(
                            self.err("unexpected character after line continuation character")
                        );
                    }
                }
                '\n' | '\r' => {
                    self.pos += 1;
                    if c == '\r' && self.peek(0) == Some('\n') {
                        self.pos += 1;
                    }
                    if self.brackets.is_empty() {
                        self.push(Tok::Newline);
                    }
                    self.line += 1;
                }
                c if c.is_ascii_digit()
                    || (c == '.' && self.peek(1).is_some_and(|d| d.is_ascii_digit())) =>
                {
                    self.number()?
                }
                c if is_id_start(c) => self.name_or_string()?,
                '\'' | '"' => self.string(String::new())?,
                _ => self.operator()?,
            }
        }
        if let Some((open, line)) = self.brackets.first().copied() {
            return Err(SyntaxError::new(
                format!("'{}' was never closed", open),
                line,
            ));
        }
        self.push(Tok::Newline);
        self.push(Tok::End);
        Ok(())
    }

    fn operator(&mut self) -> Result<(), SyntaxError> {
        let rest: String = self.chars[self.pos..].iter().take(3).collect();
        let Some(op) = OPS.iter().copied().find(|op| rest.starts_with(op)) else {
            let c = self.chars[self.pos];
            if c.is_ascii() || c.is_whitespace() {
                return Err(self.err("invalid syntax"));
            }
            return Err(self.err(format!("invalid character '{}' (U+{:04X})", c, c as u32)));
        };
        match op {
            "(" | "[" | "{" => {
                if self.brackets.len() >= MAX_BRACKETS {
                    return Err(self.err("too many nested parentheses"));
                }
                self.brackets
                    .push((op.chars().next().unwrap_or('('), self.line));
            }
            ")" | "]" | "}" => {
                let close = op.chars().next().unwrap_or(')');
                let Some((open, open_line)) = self.brackets.pop() else {
                    return Err(self.err(format!("unmatched '{}'", close)));
                };
                let want = match open {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                };
                if close != want {
                    let msg = if open_line == self.line {
                        format!("closing parenthesis '{close}' does not match opening parenthesis '{open}'")
                    } else {
                        format!("closing parenthesis '{close}' does not match opening parenthesis '{open}' on line {open_line}")
                    };
                    return Err(self.err(msg));
                }
            }
            _ => {}
        }
        self.pos += op.chars().count();
        self.push(Tok::Op(op));
        Ok(())
    }

    fn name_or_string(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        while self.peek(0).is_some_and(is_id_continue) {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        let lower = word.to_ascii_lowercase();
        let is_prefix = matches!(
            lower.as_str(),
            "r" | "u" | "f" | "b" | "br" | "rb" | "fr" | "rf"
        );
        if is_prefix && matches!(self.peek(0), Some('\'') | Some('"')) {
            return self.string(lower);
        }
        self.push(Tok::Name(word));
        Ok(())
    }

    /// Digits of `radix`, each optionally preceded by one underscore (the
    /// first only when `leading_underscore`), from the cursor; the digits
    /// without underscores, or `None` when an underscore is misplaced.
    fn digits(&mut self, radix: u32, leading_underscore: bool) -> Option<String> {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some('_') => {
                    if out.is_empty() && !leading_underscore {
                        return None;
                    }
                    if !self.peek(1).is_some_and(|c| c.is_digit(radix)) {
                        return None;
                    }
                    self.pos += 1;
                }
                Some(c) if c.is_digit(radix) => {
                    out.push(c);
                    self.pos += 1;
                }
                _ => return Some(out),
            }
        }
    }

    /// After a number: a name character may only start one of the
    /// keywords Python lets a number run into.
    fn end_of_number(&self, kind: &str) -> Result<(), SyntaxError> {
        match self.peek(0) {
            Some(c) if is_id_continue(c) => {
                let rest: String = self.chars[self.pos..].iter().take(5).collect();
                if NUMBER_FOLLOWERS.iter().any(|k| rest.starts_with(k)) {
                    Ok(())
                } else {
                    Err(self.err(format!("invalid {} literal", kind)))
                }
            }
            _ => Ok(()),
        }
    }

    fn number(&mut self) -> Result<(), SyntaxError> {
        if self.peek(0) == Some('0') {
            let radix = match self.peek(1).map(|c| c.to_ascii_lowercase()) {
                Some('x') => Some((16, "hexadecimal")),
                Some('o') => Some((8, "octal")),
                Some('b') => Some((2, "binary")),
                _ => None,
            };
            if let Some((radix, kind)) = radix {
                self.pos += 2;
                let digits = self
                    .digits(radix, true)
                    .ok_or_else(|| self.err(format!("invalid {} literal", kind)))?;
                if let Some(c) = self.peek(0).filter(|c| c.is_ascii_digit()) {
                    return Err(self.err(format!("invalid digit '{}' in {} literal", c, kind)));
                }
                if digits.is_empty() {
                    return Err(self.err(format!("invalid {} literal", kind)));
                }
                self.end_of_number(kind)?;
                let v = BigInt::parse_radix(&digits, radix)
                    .ok_or_else(|| self.err(format!("invalid {} literal", kind)))?;
                self.push(Tok::Number(Num::Int(v)));
                return Ok(());
            }
        }
        self.decimal()
    }

    fn decimal(&mut self) -> Result<(), SyntaxError> {
        let bad = |lx: &Self| lx.err("invalid decimal literal");
        let int_part = self.digits(10, false).ok_or_else(|| bad(self))?;
        let mut text = int_part.clone();
        let mut is_float = false;
        if self.peek(0) == Some('.') {
            self.pos += 1;
            is_float = true;
            text.push('.');
            if self.peek(0) == Some('_') {
                return Err(bad(self));
            }
            let frac = self.digits(10, false).ok_or_else(|| bad(self))?;
            text.push_str(&frac);
        }
        if matches!(self.peek(0), Some('e') | Some('E')) {
            let sign = matches!(self.peek(1), Some('+') | Some('-'));
            let digit_at = if sign { 2 } else { 1 };
            if self.peek(digit_at).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += digit_at;
                let sign_text = if sign {
                    self.chars[self.pos - 1].to_string()
                } else {
                    String::new()
                };
                let exp = self.digits(10, false).ok_or_else(|| bad(self))?;
                text.push('e');
                text.push_str(&sign_text);
                text.push_str(&exp);
                is_float = true;
            } else {
                // `1else`: the tokenizer lets a keyword follow; `1e` is not one.
                self.end_of_number("decimal")?;
            }
        }
        if matches!(self.peek(0), Some('j') | Some('J')) {
            self.pos += 1;
            self.end_of_number("imaginary")?;
            let v: f64 = text.parse().map_err(|_| bad(self))?;
            self.push(Tok::Imaginary(v));
            return Ok(());
        }
        self.end_of_number("decimal")?;
        if is_float {
            let v: f64 = text.parse().map_err(|_| bad(self))?;
            self.push(Tok::Number(Num::Float(v)));
            return Ok(());
        }
        if int_part.len() > 1 && int_part.starts_with('0') && int_part.chars().any(|c| c != '0') {
            return Err(self.err(
                "leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers",
            ));
        }
        let v = BigInt::parse_radix(&int_part, 10).ok_or_else(|| bad(self))?;
        self.push(Tok::Number(Num::Int(v)));
        Ok(())
    }

    /// A string literal from its opening quote; `prefix` is lower case.
    fn string(&mut self, prefix: String) -> Result<(), SyntaxError> {
        let quote = self.chars[self.pos];
        let triple = self.peek(1) == Some(quote) && self.peek(2) == Some(quote);
        let start_line = self.line;
        self.pos += if triple { 3 } else { 1 };
        let raw = prefix.contains('r');
        let bytes = prefix.contains('b');
        let fstring = prefix.contains('f');
        let body = if fstring {
            self.fstring_body(quote, triple, start_line)?;
            self.push(Tok::Str { text: None, bytes });
            return Ok(());
        } else {
            self.plain_body(quote, triple, start_line)?
        };
        let text = if raw {
            body
        } else {
            decode_escapes(&body, bytes).map_err(|m| SyntaxError::new(m, start_line))?
        };
        if bytes && !text.is_ascii() {
            return Err(SyntaxError::new(
                "bytes can only contain ASCII literal characters",
                start_line,
            ));
        }
        self.push(Tok::Str {
            text: Some(text),
            bytes,
        });
        Ok(())
    }

    fn unterminated(&self, triple: bool, start_line: usize) -> SyntaxError {
        if triple {
            SyntaxError::new(
                format!(
                    "unterminated triple-quoted string literal (detected at line {})",
                    self.line
                ),
                start_line,
            )
        } else {
            SyntaxError::new(
                format!(
                    "unterminated string literal (detected at line {})",
                    self.line
                ),
                start_line,
            )
        }
    }

    /// The raw text between the quotes (escapes undecoded).
    fn plain_body(
        &mut self,
        quote: char,
        triple: bool,
        start_line: usize,
    ) -> Result<String, SyntaxError> {
        let mut body = String::new();
        loop {
            let Some(c) = self.peek(0) else {
                return Err(self.unterminated(triple, start_line));
            };
            if c == quote
                && (!triple || (self.peek(1) == Some(quote) && self.peek(2) == Some(quote)))
            {
                self.pos += if triple { 3 } else { 1 };
                return Ok(body);
            }
            if c == '\n' {
                if !triple {
                    return Err(self.unterminated(false, start_line));
                }
                self.line += 1;
            }
            if c == '\\' {
                if let Some(next) = self.peek(1) {
                    body.push(c);
                    body.push(next);
                    if next == '\n' {
                        self.line += 1;
                    }
                    self.pos += 2;
                    continue;
                }
            }
            body.push(c);
            self.pos += 1;
        }
    }

    /// Skip an f-string's body: its replacement fields may hold nested
    /// strings of any quote (Python 3.12).
    fn fstring_body(
        &mut self,
        quote: char,
        triple: bool,
        start_line: usize,
    ) -> Result<(), SyntaxError> {
        let mut depth = 0usize;
        loop {
            let Some(c) = self.peek(0) else {
                return Err(self.unterminated(triple, start_line));
            };
            if depth == 0 {
                if c == quote
                    && (!triple || (self.peek(1) == Some(quote) && self.peek(2) == Some(quote)))
                {
                    self.pos += if triple { 3 } else { 1 };
                    return Ok(());
                }
                if (c == '{' && self.peek(1) == Some('{'))
                    || (c == '}' && self.peek(1) == Some('}'))
                {
                    self.pos += 2;
                    continue;
                }
                if c == '\n' && !triple {
                    return Err(self.unterminated(false, start_line));
                }
                if c == '\\' && self.peek(1).is_some() {
                    self.pos += 2;
                    continue;
                }
            } else if c == '\'' || c == '"' {
                let inner_triple = self.peek(1) == Some(c) && self.peek(2) == Some(c);
                self.pos += if inner_triple { 3 } else { 1 };
                self.plain_body(c, inner_triple, self.line)?;
                continue;
            }
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                '\n' => self.line += 1,
                _ => {}
            }
            self.pos += 1;
        }
    }
}

/// Python's escape sequences in a non-raw literal.
fn decode_escapes(body: &str, bytes: bool) -> Result<String, String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let codec = if bytes { "bytes" } else { "unicodeescape" };
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let e = chars[i + 1];
        i += 2;
        match e {
            '\n' => {}
            '\\' | '\'' | '"' => out.push(e),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.get(i).and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            i += 1;
                        }
                        None => break,
                    }
                }
                let v = if bytes { v & 0xff } else { v };
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'x' | 'u' | 'U' if !(bytes && e != 'x') => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let hex: String = chars.iter().skip(i).take(n).collect();
                if hex.chars().count() != n || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(format!(
                        "(unicode error) '{}' codec can't decode bytes in position {}-{}: truncated \\{}{} escape",
                        codec,
                        0,
                        1 + hex.chars().take_while(char::is_ascii_hexdigit).count(),
                        e,
                        "X".repeat(n)
                    ));
                }
                i += n;
                let v = u32::from_str_radix(&hex, 16).unwrap_or(0xfffd);
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'N' if !bytes => {
                return Err("(unicode error) 'unicodeescape' codec can't decode bytes: named Unicode escapes are not supported here".into());
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Python's `repr()` of a bytes value (every char is below 0x80 here or an
/// escape's byte).
pub fn bytes_repr(text: &str) -> String {
    let q = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from("b");
    out.push(q);
    for c in text.chars() {
        let b = c as u32;
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            _ if !(0x20..0x7f).contains(&b) => out.push_str(&format!("\\x{:02x}", b & 0xff)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}
