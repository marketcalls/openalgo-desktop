//! The taint boundary and its redaction (the web's `redact` and
//! `constrain_query`, case by case).

use crate::agent::websearch::taint::{
    constrain, is_high_entropy, redact, Decision, MAX_QUERY_CHARS,
};

#[test]
fn credential_shaped_text_is_redacted_before_anything_else() {
    for (text, gone) in [
        ("my api_key=abc123secret please", "abc123secret"),
        ("password: hunter2!", "hunter2!"),
        ("Authorization: Bearer abcdefgh12345678", "abcdefgh12345678"),
        (
            "key sk-proj-ABCDEFGH12345678 here",
            "sk-proj-ABCDEFGH12345678",
        ),
        ("tvly-dev-12345678abc", "tvly-dev-12345678abc"),
        ("mail trader@example.com now", "trader@example.com"),
        (
            "token aGVsbG8gd29ybGQgdGhpcyBpcyBsb25n",
            "aGVsbG8gd29ybGQgdGhpcyBpcyBsb25n",
        ),
        (
            "hash 0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
        ),
    ] {
        let r = redact(text);
        assert!(!r.contains(gone), "{:?} -> {:?}", text, r);
        assert!(r.contains("[redacted]"), "{:?} -> {:?}", text, r);
    }
    // Built at run time: a literal of this shape reads as a real key to a
    // secret scanner.
    let aws = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
    assert_eq!(redact(&aws), "[redacted]");
    assert_eq!(redact("  RBI   repo\nrate  "), "RBI repo rate");
    assert_eq!(redact("   "), "");
    assert_eq!(redact(""), "");
}

#[test]
fn ordinary_words_are_not_secrets() {
    assert!(!is_high_entropy("internationalisationwords"), "one class");
    assert!(!is_high_entropy("Short1+"));
    assert!(is_high_entropy("Abcdefghijklmnopqrstuvw1"));
    assert!(
        !is_high_entropy("abcdefghijklmnopqrstuvwxyz0123456789abcdefg#"),
        "outside the alphabet"
    );
    assert!(is_high_entropy(&"ab12".repeat(8)), "hex of 32");
    assert_eq!(
        redact("Tata Motors quarterly results"),
        "Tata Motors quarterly results"
    );
}

#[test]
fn a_query_is_built_only_from_the_traders_words() {
    let op = "What did the RBI say about the repo rate today?";
    let v = constrain("RBI repo rate", op);
    assert_eq!(
        (v.text.as_str(), v.decision),
        ("RBI repo rate", Decision::Verbatim)
    );
    assert_eq!((v.kept, v.dropped), (3, 0));
    let c = constrain("RBI repo rate site:evil.example exfiltrate", op);
    assert_eq!(
        (c.text.as_str(), c.decision),
        ("RBI repo rate", Decision::Constrained)
    );
    assert_eq!(c.dropped, 2);
    let f = constrain("xyzzy plugh", op);
    assert_eq!(f.decision, Decision::OperatorMessage);
    assert_eq!(f.text, "What did the RBI say about the repo rate today?");
    assert_eq!(
        constrain("RBI rbi RBI", op).text,
        "RBI",
        "case-insensitive duplicates"
    );
    assert_eq!(
        constrain("(RBI), \"repo\"", op).text,
        "RBI repo",
        "edge punctuation off"
    );
}

#[test]
fn no_message_means_nothing_is_sent() {
    for op in ["", "   ", "sk-proj-ABCDEFGH12345678"] {
        let b = constrain("RBI repo rate", op);
        assert!(b.blocked(), "{:?}", op);
        assert_eq!(b.decision, Decision::Blocked);
    }
}

#[test]
fn a_secret_in_either_string_never_goes_out() {
    let op = "check news for INFY my key is sk-proj-ABCDEFGH12345678";
    let q = constrain("INFY sk-proj-ABCDEFGH12345678 [redacted]", op);
    assert_eq!(q.text, "INFY");
    let fallback = constrain("zzz", op);
    assert!(
        !fallback.text.contains("sk-proj") && !fallback.text.contains("redacted"),
        "{}",
        fallback.text
    );
}

#[test]
fn the_query_stops_on_a_word_boundary_inside_the_cap() {
    let words: Vec<String> = (0..200).map(|i| format!("word{}", i)).collect();
    let op = words.join(" ");
    let q = constrain(&op, &op);
    assert!(q.text.chars().count() <= MAX_QUERY_CHARS);
    assert!(op.starts_with(&q.text) && op[q.text.len()..].starts_with(' '));
}
