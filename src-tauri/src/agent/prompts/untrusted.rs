//! The one primitive that delimits untrusted text (the web's
//! `prompts.wrap_untrusted` and its helpers).
//!
//! Everything the model reads that it did not write itself (a tool result,
//! a symbol name, a broker rejection, a web page, a file the trader
//! attached, the trader's own earlier message) is wrapped in a labelled
//! block, with every opening and closing form of every reserved tag inside
//! it defanged, so the text cannot end its block early or forge a sibling
//! block that claims the platform's authority. Case, inner spaces and
//! malformed closers (`</ Tool_Result >`, `< /tool_result>`,
//! `<//tool_result>`) are caught too: the reader is a language model, not
//! an XML parser. Attribute values are escaped and stripped of control
//! characters.
//!
//! This is defence in depth; the controls are structural (the risk guard
//! after approval, tools withheld by session state).

use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// Anything a tool returned from the platform's own services.
pub const TAG_TOOL_RESULT: &str = "tool_result";
/// Search results and page text: lower trust than platform data.
pub const TAG_WEB_RESULT: &str = "web_result";
/// Text a person typed, the trader's earlier messages included.
pub const TAG_USER_TEXT: &str = "user_text";
/// Generated code echoed back for review.
pub const TAG_CODE: &str = "generated_code";
/// A file the trader attached: they chose to send it, not necessarily wrote it.
pub const TAG_ATTACHMENT: &str = "attachment";

/// Every tag that means something to the model.
pub const RESERVED_TAGS: [&str; 5] = [
    TAG_TOOL_RESULT,
    TAG_WEB_RESULT,
    TAG_USER_TEXT,
    TAG_CODE,
    TAG_ATTACHMENT,
];

/// How many characters of one attribute value survive.
pub const MAX_ATTRIBUTE_CHARS: usize = 200;

static IDENTIFIER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z][A-Za-z0-9_-]{0,63}\z").ok());

static CONTROL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]").ok());

/// A tag or attribute name that is not a plain identifier: a programming
/// error, refused rather than let an attacker shape the prompt's structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnIdentifier(pub String);

fn is_identifier(s: &str) -> bool {
    IDENTIFIER.as_ref().is_some_and(|re| re.is_match(s))
}

/// Escape a value for the opening tag: one line, no control characters,
/// XML-escaped, at most [`MAX_ATTRIBUTE_CHARS`].
pub fn escape_attribute(value: &str) -> String {
    let mut text = match CONTROL.as_ref() {
        Some(re) => re.replace_all(value, "").into_owned(),
        None => value.to_string(),
    };
    text = text.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ");
    text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    if text.chars().count() > MAX_ATTRIBUTE_CHARS {
        let head: String = text.chars().take(MAX_ATTRIBUTE_CHARS - 3).collect();
        text = format!("{}...", head);
    }
    text
}

/// One tag's closer and opener patterns.
struct Defang {
    name: String,
    closer: Regex,
    opener: Regex,
}

fn defang_patterns(name: &str) -> Option<Defang> {
    let e = regex::escape(name);
    Some(Defang {
        name: name.to_string(),
        closer: Regex::new(&format!(r"(?i)<\s*/+\s*{}\s*>", e)).ok()?,
        // The web's lookahead `(?=[\s/>])`, as a captured character put back:
        // that character can never begin another match.
        opener: Regex::new(&format!(r"(?i)<\s*{}([\s/>])", e)).ok()?,
    })
}

static RESERVED_PATTERNS: LazyLock<Vec<Defang>> = LazyLock::new(|| {
    let names: BTreeSet<&str> = RESERVED_TAGS.iter().copied().collect();
    names.into_iter().filter_map(defang_patterns).collect()
});

fn neutralise(tag: &str, text: &str) -> String {
    let own = if RESERVED_TAGS.contains(&tag) {
        None
    } else {
        defang_patterns(tag)
    };
    // The web walks `sorted(RESERVED_TAGS | {tag})`.
    let mut all: Vec<&Defang> = RESERVED_PATTERNS.iter().chain(own.as_ref()).collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = text.to_string();
    for d in all {
        let closed = format!("<\\/{}>", d.name);
        out = d.closer.replace_all(&out, closed.as_str()).into_owned();
        let opened = format!("<\\{}${{1}}", d.name);
        out = d.opener.replace_all(&out, opened.as_str()).into_owned();
    }
    out
}

/// Wrap untrusted text in a labelled block. An attribute whose value is
/// `None` is left out.
pub fn wrap_untrusted(
    tag: &str,
    text: &str,
    attributes: &[(&str, Option<&str>)],
) -> Result<String, NotAnIdentifier> {
    if !is_identifier(tag) {
        return Err(NotAnIdentifier(tag.to_string()));
    }
    let mut rendered: Vec<String> = Vec::new();
    for (name, value) in attributes {
        let Some(value) = value else { continue };
        if !is_identifier(name) {
            return Err(NotAnIdentifier((*name).to_string()));
        }
        rendered.push(format!("{}=\"{}\"", name, escape_attribute(value)));
    }
    let opening = if rendered.is_empty() {
        format!("<{}>", tag)
    } else {
        format!("<{} {}>", tag, rendered.join(" "))
    };
    Ok(format!(
        "{}\n{}\n</{}>",
        opening,
        neutralise(tag, text),
        tag
    ))
}

/// A tool result, labelled with the tool and any extra attributes.
pub fn wrap_tool_result(tool: &str, result: &str, attributes: &[(&str, Option<&str>)]) -> String {
    let mut attrs: Vec<(&str, Option<&str>)> = vec![("tool", Some(tool))];
    attrs.extend_from_slice(attributes);
    wrap_untrusted(TAG_TOOL_RESULT, result, &attrs).unwrap_or_else(|e| refused(&e))
}

/// A web search result, marked third-party.
pub fn wrap_web_result(
    provider: &str,
    result: &str,
    attributes: &[(&str, Option<&str>)],
) -> String {
    let mut attrs: Vec<(&str, Option<&str>)> =
        vec![("provider", Some(provider)), ("trust", Some("third-party"))];
    attrs.extend_from_slice(attributes);
    wrap_untrusted(TAG_WEB_RESULT, result, &attrs).unwrap_or_else(|e| refused(&e))
}

/// A file the trader attached, marked operator-supplied.
pub fn wrap_attachment(name: &str, text: &str, attributes: &[(&str, Option<&str>)]) -> String {
    let mut attrs: Vec<(&str, Option<&str>)> =
        vec![("name", Some(name)), ("trust", Some("operator-supplied"))];
    attrs.extend_from_slice(attributes);
    wrap_untrusted(TAG_ATTACHMENT, text, &attrs).unwrap_or_else(|e| refused(&e))
}

/// A caller passed an attribute name that is not an identifier: nothing of
/// the text goes out, and the log says which name.
fn refused(e: &NotAnIdentifier) -> String {
    tracing::error!(
        "An untrusted block was refused: {:?} is not an identifier",
        e.0
    );
    String::from("<tool_result>\nThis result could not be shown.\n</tool_result>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn every_web_wrapping_is_reproduced() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/agent/prompts.json"
        );
        let f: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for case in f["wrapped"].as_array().unwrap() {
            let attrs: Vec<(String, Option<String>)> = case["attributes"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string)))
                .collect();
            let borrowed: Vec<(&str, Option<&str>)> = attrs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect();
            let got = wrap_untrusted(
                case["tag"].as_str().unwrap(),
                case["text"].as_str().unwrap(),
                &borrowed,
            )
            .unwrap();
            assert_eq!(got, case["wrapped"].as_str().unwrap(), "{}", case["tag"]);
        }
    }

    #[test]
    fn web_and_tool_results_carry_their_labels() {
        assert_eq!(
            wrap_web_result("duckduckgo", "x", &[("kind", Some("links"))]),
            "<web_result provider=\"duckduckgo\" trust=\"third-party\" kind=\"links\">\nx\n</web_result>"
        );
        assert!(wrap_tool_result("get_quote", "{}", &[("symbol", None)])
            .starts_with("<tool_result tool=\"get_quote\">"));
    }

    #[test]
    fn a_name_that_is_not_an_identifier_is_refused() {
        assert!(wrap_untrusted("bad tag", "x", &[]).is_err());
        assert!(wrap_untrusted("ok", "x", &[("a b", Some("1"))]).is_err());
        assert!(
            wrap_tool_result("q", "x", &[("bad name", Some("1"))]).contains("could not be shown")
        );
    }
}
