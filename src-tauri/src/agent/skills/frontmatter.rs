//! A `SKILL.md`'s frontmatter and body (the web's `loader.parse_skill_md`,
//! which reads the frontmatter with `yaml.safe_load`).
//!
//! The frontmatter is read as the YAML a skill file uses: a mapping of keys
//! to plain, quoted or block scalars, with flow and nested collections
//! recognised (and checked for balance) but not read, since only `name` and
//! `description` matter here. Anything outside that subset is refused as
//! not valid YAML, which skips the skill; the shipped skills are parsed by a
//! test, so a frontmatter this reader cannot take fails the build's tests,
//! never a trader's run.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The web's `_FRONTMATTER`: `---`, the block, `---`, then the body.
static FRONTMATTER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?s)\A---[ \t]*\n(.*?)\n---[ \t]*(?:\n|\z)(.*)\z").ok());

/// A frontmatter value: text, or anything else (a number, a switch, null,
/// a list or a mapping), which no key read here accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Other,
}

/// Why a `SKILL.md` was refused, in the web's words (logged, never shown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Malformed {
    NoFrontmatter,
    NotYaml(String),
    NotAMapping,
    NoDescription,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Malformed::NoFrontmatter => {
                f.write_str("it has no frontmatter block between --- lines")
            }
            Malformed::NotYaml(why) => write!(f, "its frontmatter is not valid YAML ({})", why),
            Malformed::NotAMapping => {
                f.write_str("its frontmatter is not a mapping of keys to values")
            }
            Malformed::NoDescription => f.write_str("its frontmatter has no description"),
        }
    }
}

/// A parsed `SKILL.md`: its frontmatter and its instructions (the body
/// after the frontmatter, stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMd {
    pub frontmatter: BTreeMap<String, Value>,
    pub body: String,
}

impl SkillMd {
    /// A text value of the frontmatter.
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.frontmatter.get(key) {
            Some(Value::Text(t)) => Some(t),
            _ => None,
        }
    }
}

/// Split a `SKILL.md` (line endings already normalised) into frontmatter
/// and body; refused unless the frontmatter is a mapping with a non-empty
/// text description.
pub fn parse_skill_md(text: &str) -> Result<SkillMd, Malformed> {
    let caps = FRONTMATTER
        .as_ref()
        .and_then(|re| re.captures(text))
        .ok_or(Malformed::NoFrontmatter)?;
    let block = caps.get(1).map_or("", |m| m.as_str());
    let body = caps.get(2).map_or("", |m| m.as_str()).trim().to_string();
    let frontmatter = mapping(block)?;
    let described =
        matches!(frontmatter.get("description"), Some(Value::Text(d)) if !d.trim().is_empty());
    if !described {
        return Err(Malformed::NoDescription);
    }
    Ok(SkillMd { frontmatter, body })
}

fn not_yaml(why: &str) -> Malformed {
    Malformed::NotYaml(why.to_string())
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn is_blank_or_comment(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

/// The top-level mapping of a frontmatter block.
fn mapping(block: &str) -> Result<BTreeMap<String, Value>, Malformed> {
    let lines: Vec<&str> = block.split('\n').collect();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if is_blank_or_comment(line) {
            continue;
        }
        if is_indented(line) {
            return Err(not_yaml("an indented line belongs to no key"));
        }
        if line == "-" || line.starts_with("- ") {
            return Err(if out.is_empty() {
                Malformed::NotAMapping
            } else {
                not_yaml("a list item inside a mapping")
            });
        }
        let Some((key, rest)) = split_key(line) else {
            return Err(if out.is_empty() {
                Malformed::NotAMapping
            } else {
                not_yaml("a line that is not a key and a value")
            });
        };
        let start = i;
        while i < lines.len() && (lines[i].trim().is_empty() || is_indented(lines[i])) {
            i += 1;
        }
        let value = value(rest.trim(), &lines[start..i])?;
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

/// `key: rest` with a plain key (letters, digits, `_`, `-`, `.`).
fn split_key(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':')?;
    let (key, rest) = (&line[..colon], &line[colon + 1..]);
    let plain = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
    (plain && (rest.is_empty() || rest.starts_with([' ', '\t']))).then_some((key, rest))
}

/// One key's value from the rest of its line and its indented lines.
fn value(first: &str, more: &[&str]) -> Result<Value, Malformed> {
    let first = if first.starts_with('#') { "" } else { first };
    match first.chars().next() {
        None => next_line_value(more),
        Some('"') | Some('\'') => quoted(first, more).map(Value::Text),
        Some('|') | Some('>') => block_scalar(first, more).map(Value::Text),
        Some('[') | Some('{') => flow(first, more),
        Some('&') | Some('*') | Some('!') | Some('@') | Some('`') | Some('%') => {
            Err(not_yaml("an anchor, alias, tag or reserved character"))
        }
        Some(_) => plain(first, more),
    }
}

/// A value that starts on the line after its key: a nested list or mapping
/// (not read), a scalar, or nothing (null).
fn next_line_value(more: &[&str]) -> Result<Value, Malformed> {
    let Some(at) = more.iter().position(|l| !is_blank_or_comment(l)) else {
        return Ok(Value::Other);
    };
    let head = more[at].trim();
    if head == "-" || head.starts_with("- ") || split_key(head).is_some() {
        return Ok(Value::Other);
    }
    value(head, &more[at + 1..])
}

/// Lines folded as YAML folds a multi-line scalar: a line break between two
/// lines is a space, an empty line is a newline.
fn fold<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    let mut breaks = 0usize;
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            breaks += 1;
            continue;
        }
        if !out.is_empty() {
            if breaks == 0 {
                out.push(' ');
            } else {
                out.push_str(&"\n".repeat(breaks));
            }
        }
        breaks = 0;
        out.push_str(t);
    }
    out
}

/// A plain scalar, possibly over several lines; text unless YAML would
/// read it as null, a switch or a number.
fn plain(first: &str, more: &[&str]) -> Result<Value, Malformed> {
    let mut lines = vec![strip_comment(first)];
    for l in more {
        if l.trim_start().starts_with('#') {
            break;
        }
        lines.push(strip_comment(l));
    }
    let text = fold(lines.into_iter());
    if text.contains(": ") || text.ends_with(':') {
        return Err(not_yaml("a colon and a space inside a plain value"));
    }
    if is_resolved(&text) {
        return Ok(Value::Other);
    }
    Ok(Value::Text(text))
}

/// A plain line without its trailing ` # comment`.
fn strip_comment(line: &str) -> &str {
    match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Whether YAML 1.1 (PyYAML's) reads a plain scalar as something other than
/// text: null, a switch or a number.
fn is_resolved(text: &str) -> bool {
    const WORDS: &[&str] = &[
        "~", "null", "Null", "NULL", "true", "True", "TRUE", "false", "False", "FALSE", "yes",
        "Yes", "YES", "no", "No", "NO", "on", "On", "ON", "off", "Off", "OFF", "y", "Y", "n", "N",
    ];
    if text.is_empty() || WORDS.contains(&text) {
        return true;
    }
    let digits = text.trim_start_matches(['+', '-']).replace('_', "");
    let number = digits.parse::<f64>().is_ok() && !digits.contains(['e', 'E', 'i', 'I', 'n', 'N']);
    let hex = digits.starts_with("0x") && digits.len() > 2;
    !digits.is_empty() && (number || hex)
}

/// A single- or double-quoted scalar, possibly over several lines.
fn quoted(first: &str, more: &[&str]) -> Result<String, Malformed> {
    let joined = std::iter::once(first)
        .chain(more.iter().copied())
        .collect::<Vec<_>>()
        .join("\n");
    let quote = joined.chars().next().unwrap_or('"');
    let mut raw = String::new();
    let mut chars = joined.chars().skip(1).peekable();
    let mut closed = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            ('\'', '\'') if chars.peek() == Some(&'\'') => {
                chars.next();
                raw.push('\'');
            }
            ('"', '\\') => match chars.next() {
                Some('n') => raw.push('\n'),
                Some('t') => raw.push('\t'),
                Some(e @ ('"' | '\\' | '/')) => raw.push(e),
                Some('\n') => {}
                _ => return Err(not_yaml("an escape this reader does not take")),
            },
            (q, c) if c == q => {
                closed = true;
                break;
            }
            _ => raw.push(c),
        }
    }
    if !closed {
        return Err(not_yaml("a quoted value is never closed"));
    }
    let rest: String = chars.collect();
    if !is_blank_or_comment(&rest) {
        return Err(not_yaml("text after a quoted value"));
    }
    Ok(fold(raw.split('\n')))
}

/// A `|` (literal) or `>` (folded) block scalar, clipped to one final
/// newline unless its header says strip (`-`) or keep (`+`).
fn block_scalar(header: &str, more: &[&str]) -> Result<String, Malformed> {
    let indicators = strip_comment(header).trim();
    let literal = indicators.starts_with('|');
    let flags = &indicators[1..];
    if !flags
        .chars()
        .all(|c| matches!(c, '+' | '-') || c.is_ascii_digit())
    {
        return Err(not_yaml("a block header this reader does not take"));
    }
    let indent = more
        .iter()
        .find(|l| !l.trim().is_empty())
        .map_or(0, |l| l.len() - l.trim_start().len());
    let lines: Vec<&str> = more
        .iter()
        .map(|l| if l.len() >= indent { &l[indent..] } else { "" })
        .collect();
    let mut text = if literal {
        lines.join("\n")
    } else {
        fold(lines.iter().copied())
    };
    let trimmed = text.trim_end_matches('\n').len();
    if flags.contains('+') {
        text.push('\n');
    } else {
        text.truncate(trimmed);
        if !flags.contains('-') && !text.is_empty() {
            text.push('\n');
        }
    }
    Ok(text)
}

/// A flow collection (`[...]` or `{...}`): checked for balance, not read.
fn flow(first: &str, more: &[&str]) -> Result<Value, Malformed> {
    let mut depth: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    for c in std::iter::once(first)
        .chain(more.iter().copied())
        .flat_map(str::chars)
    {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => depth.push(']'),
            (None, '{') => depth.push('}'),
            (None, ']' | '}') if depth.pop() != Some(c) => {
                return Err(not_yaml("a bracket closes nothing it opened"));
            }
            _ => {}
        }
    }
    if quote.is_some() || !depth.is_empty() {
        return Err(not_yaml("a list or mapping is never closed"));
    }
    Ok(Value::Other)
}
