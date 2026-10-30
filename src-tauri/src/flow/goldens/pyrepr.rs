//! Reading the Python dict and list reprs the web writes into log lines, so
//! a line can be compared with dict keys in any order (lead decision 19).
//!
//! A line splits into text and the `{...}` / `[...]` literals inside it.
//! A literal keeps each leaf's spelling (`'a'` and `"a"` differ, `10` and
//! `10.0` differ, `True` is not `true`); only the order of a dict's keys is
//! free. Anything that does not parse as a whole literal stays text, so
//! `{{webhook.qty}}` is text.

/// A piece of a log line.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Lit(Lit),
}

/// A Python literal as written.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// A string, number or keyword, as spelled.
    Leaf(String),
    List(Vec<Lit>),
    Tuple(Vec<Lit>),
    /// Entries in written order; compared as a set of keys.
    Dict(Vec<(Lit, Lit)>),
}

/// Split a line into text and literals.
pub fn split(line: &str) -> Vec<Piece> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' || c == '[' {
            if let Some((lit, end)) = parse(&chars, i) {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                }
                out.push(Piece::Lit(lit));
                i = end;
                continue;
            }
        }
        text.push(c);
        i += 1;
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

fn skip_ws(s: &[char], mut i: usize) -> usize {
    while i < s.len() && s[i] == ' ' {
        i += 1;
    }
    i
}

/// Parse one literal starting at `i`; the literal and the index after it.
fn parse(s: &[char], i: usize) -> Option<(Lit, usize)> {
    let i = skip_ws(s, i);
    match *s.get(i)? {
        '[' => seq(s, i + 1, ']').map(|(v, e)| (Lit::List(v), e)),
        '(' => seq(s, i + 1, ')').map(|(v, e)| (Lit::Tuple(v), e)),
        '{' => dict(s, i + 1),
        '\'' | '"' => string(s, i),
        _ => leaf(s, i),
    }
}

fn seq(s: &[char], mut i: usize, close: char) -> Option<(Vec<Lit>, usize)> {
    let mut items = Vec::new();
    i = skip_ws(s, i);
    if s.get(i) == Some(&close) {
        return Some((items, i + 1));
    }
    loop {
        let (item, e) = parse(s, i)?;
        items.push(item);
        i = skip_ws(s, e);
        match *s.get(i)? {
            ',' => i = skip_ws(s, i + 1),
            c if c == close => return Some((items, i + 1)),
            _ => return None,
        }
        if s.get(i) == Some(&close) {
            return Some((items, i + 1)); // a one-tuple's trailing comma
        }
    }
}

fn dict(s: &[char], mut i: usize) -> Option<(Lit, usize)> {
    let mut entries = Vec::new();
    i = skip_ws(s, i);
    if s.get(i) == Some(&'}') {
        return Some((Lit::Dict(entries), i + 1));
    }
    loop {
        let (k, e) = parse(s, i)?;
        i = skip_ws(s, e);
        if s.get(i) != Some(&':') {
            return None;
        }
        let (v, e) = parse(s, i + 1)?;
        entries.push((k, v));
        i = skip_ws(s, e);
        match *s.get(i)? {
            ',' => i = skip_ws(s, i + 1),
            '}' => return Some((Lit::Dict(entries), i + 1)),
            _ => return None,
        }
    }
}

fn string(s: &[char], i: usize) -> Option<(Lit, usize)> {
    let q = s[i];
    let mut j = i + 1;
    while j < s.len() {
        match s[j] {
            '\\' => j += 2,
            c if c == q => {
                let spelled: String = s[i..=j].iter().collect();
                return Some((Lit::Leaf(spelled), j + 1));
            }
            _ => j += 1,
        }
    }
    None
}

fn leaf(s: &[char], i: usize) -> Option<(Lit, usize)> {
    let mut j = i;
    while j < s.len() && (s[j].is_ascii_alphanumeric() || matches!(s[j], '.' | '-' | '+' | '_')) {
        j += 1;
    }
    let word: String = s[i..j].iter().collect();
    let ok = matches!(
        word.as_str(),
        "True" | "False" | "None" | "inf" | "-inf" | "nan"
    ) || word.parse::<f64>().is_ok();
    ok.then_some((Lit::Leaf(word), j))
}

/// A leaf that is a number: whether it is spelled as a float, and its value.
fn number(leaf: &str) -> Option<(bool, f64)> {
    if leaf.starts_with('\'') || leaf.starts_with('"') {
        return None;
    }
    let is_float = leaf.contains(['.', 'e', 'E']) || leaf.ends_with("inf") || leaf == "nan";
    let v = match leaf {
        "inf" => f64::INFINITY,
        "-inf" => f64::NEG_INFINITY,
        "nan" => f64::NAN,
        _ => leaf.parse::<f64>().ok()?,
    };
    Some((is_float, v))
}

/// Two pieces are the same: text byte for byte, literals as values.
pub fn same_piece(a: &Piece, b: &Piece, close: fn(f64, f64) -> bool) -> bool {
    match (a, b) {
        (Piece::Text(x), Piece::Text(y)) => x == y,
        (Piece::Lit(x), Piece::Lit(y)) => same_lit(x, y, close),
        _ => false,
    }
}

/// A list of a dict's keys (the web's `... keys: [...]` line lists a node's
/// data keys as stored; the desktop keeps JSON maps in key order): the same
/// keys in any order, each spelled the same.
pub fn same_key_list(a: &Piece, b: &Piece) -> bool {
    match (a, b) {
        (Piece::Lit(Lit::List(x)), Piece::Lit(Lit::List(y))) => {
            let spelled = |l: &[Lit]| {
                let mut v: Vec<String> = l.iter().map(|e| format!("{e:?}")).collect();
                v.sort();
                v
            };
            spelled(x) == spelled(y)
        }
        _ => false,
    }
}

fn same_lit(a: &Lit, b: &Lit, close: fn(f64, f64) -> bool) -> bool {
    match (a, b) {
        (Lit::Leaf(x), Lit::Leaf(y)) => match (number(x), number(y)) {
            (Some((fx, vx)), Some((fy, vy))) => {
                fx == fy && (vx == vy || (vx.is_nan() && vy.is_nan()) || close(vy, vx))
            }
            _ => x == y,
        },
        (Lit::List(x), Lit::List(y)) | (Lit::Tuple(x), Lit::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_lit(p, q, close))
        }
        (Lit::Dict(x), Lit::Dict(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .find(|(k2, _)| k2 == k)
                        .is_some_and(|(_, v2)| same_lit(v, v2, close))
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_split_out_of_the_text() {
        let p = split("Result: {'a': [1, 2.5, None], 'b': (True,)} done");
        assert_eq!(p.len(), 3);
        assert!(matches!(&p[1], Piece::Lit(Lit::Dict(e)) if e.len() == 2));
        assert_eq!(split("{{x}} and [unclosed").len(), 1);
        assert_eq!(split("keys: ['a', 'b']").len(), 2);
    }

    #[test]
    fn a_key_list_is_a_set() {
        let a = &split("['b', 'a']")[0];
        assert!(same_key_list(a, &split("['a', 'b']")[0]));
        assert!(!same_key_list(a, &split("['a', 'c']")[0]));
        assert!(!same_key_list(a, &split("[\"a\", 'b']")[0]));
    }
}
