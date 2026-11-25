//! The BFO stem of a trading symbol (web tradesmart `extract_name`,
//! marketcalls/openalgo#2475, 9a12bd0f7): the pattern
//! `^(.+?){yy}(?:[A-Z]{3}|[1-9OND]\d{2})(?:\d+(?:\.\d+)?)?(?:CE|PE|FUT)$`,
//! case by case. The rows themselves are pinned in `brokers::tradesmart`.

use super::bfo_stem;

/// The commit's examples, monthly and weekly, with digits and punctuation
/// in the underlying; the weekly month is 1-9 or O, N, D.
#[test]
fn the_stem_is_the_symbol_less_its_expiry_strike_and_type() {
    for (tsym, expiry, stem) in [
        ("SENSEX5026NOVFUT", "26-NOV-26", "SENSEX50"),
        ("SENSEX5026NOV26500CE", "26-NOV-26", "SENSEX50"),
        ("SENSEX26NOVFUT", "26-NOV-26", "SENSEX"),
        ("SENSEX26D0389500PE", "03-DEC-26", "SENSEX"),
        ("SENSEX26O0882000CE", "08-OCT-26", "SENSEX"),
        ("SENSEX2610282000CE", "02-JAN-26", "SENSEX"),
        ("BANKEX26NOV26000CE", "26-NOV-26", "BANKEX"),
        ("M&M26NOVFUT", "26-NOV-26", "M&M"),
        ("BAJAJ-AUTO26NOV9000CE", "26-NOV-26", "BAJAJ-AUTO"),
        ("NAM-INDIA26NOVFUT", "26-NOV-26", "NAM-INDIA"),
        ("GVT&D26NOV2500PE", "26-NOV-26", "GVT&D"),
        ("360ONE26NOV1012.5CE", "26-NOV-26", "360ONE"),
    ] {
        assert_eq!(bfo_stem(tsym, expiry), stem, "{tsym}");
    }
}

/// The stem is anchored on the expiry's year: another year finds no
/// suffix. A blank expiry takes any two digits. A symbol without a
/// contract suffix, or with a malformed strike, is its own stem (the web's
/// `match.group(1) if match else ts`), and the stem is never empty.
#[test]
fn the_year_anchors_the_stem_and_a_symbol_without_a_suffix_is_kept() {
    assert_eq!(
        bfo_stem("SENSEX5026NOVFUT", "26-NOV-27"),
        "SENSEX5026NOVFUT"
    );
    assert_eq!(bfo_stem("SENSEX5026NOVFUT", ""), "SENSEX50");
    assert_eq!(bfo_stem("SENSEX", "26-NOV-26"), "SENSEX");
    assert_eq!(bfo_stem("SENSEX26NOVXX", "26-NOV-26"), "SENSEX26NOVXX");
    assert_eq!(
        bfo_stem("SENSEX26NOV1012.CE", "26-NOV-26"),
        "SENSEX26NOV1012.CE"
    );
    assert_eq!(
        bfo_stem("SENSEX26Nov85000CE", "26-NOV-26"),
        "SENSEX26Nov85000CE"
    );
    assert_eq!(bfo_stem("26NOVFUT", "26-NOV-26"), "26NOVFUT");
}

/// A multibyte character in a malformed row is stepped over, never split.
#[test]
fn a_multibyte_symbol_is_read_without_splitting_a_character() {
    assert_eq!(bfo_stem("\u{c9}26NOVFUT", "26-NOV-26"), "\u{c9}");
    assert_eq!(
        bfo_stem("S\u{c9}26NOV\u{c9}FUT", "26-NOV-26"),
        "S\u{c9}26NOV\u{c9}FUT"
    );
    assert_eq!(bfo_stem("SENSEX26NOVFUT", "\u{c9}\u{c9}"), "SENSEX26NOVFUT");
}
