//! The words of a message as the two taint boundaries read them
//! (`websearch::taint` for an outgoing search, `mcp::taint` for an external
//! data argument): the web's `_EDGE_PUNCTUATION` comes off each end of a
//! word before it is compared, and a message splits as Python's
//! `str.split()` splits it. One list, so the two boundaries cannot drift
//! apart on what a word is. Python's `str.isprintable` lives here too, for
//! the chart contract's bounded text.

/// Stripped from a word's ends before it is compared (the web's
/// `_EDGE_PUNCTUATION`).
const EDGE: &[char] = &[
    ' ', '\t', '\n', '\r', '"', '\'', '`', '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{',
    '}', '<', '>', '|', '\\', '/', '*', '~', '#',
];

/// A word without its edge punctuation.
pub fn strip_edges(token: &str) -> &str {
    token.trim_matches(|c| EDGE.contains(&c))
}

/// Python's `str.isspace` (what `str.split()` splits on): Unicode white
/// space plus the four information separators.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.split()`: runs of white space, none empty.
pub fn split(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_space).filter(|t| !t.is_empty())
}

/// Python's `str.isprintable` for one character, as near as the standard
/// library allows: control, format and separator characters (other than
/// the ASCII space) are not printable.
pub fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let format = matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
        | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}' | '\u{E000}'..='\u{F8FF}');
    !(c.is_control() || c.is_whitespace() || format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_loses_its_edge_punctuation_and_keeps_its_inside() {
        assert_eq!(strip_edges("(\"RELIANCE.NS\"),"), "RELIANCE.NS");
        assert_eq!(strip_edges("#~*|"), "");
        assert_eq!(strip_edges("a-b_c"), "a-b_c");
    }

    #[test]
    fn a_message_splits_as_python_splits_it() {
        let split: Vec<&str> = split(" a\u{1c}b\u{a0}c\t\n d ").collect();
        assert_eq!(split, ["a", "b", "c", "d"]);
    }
}
