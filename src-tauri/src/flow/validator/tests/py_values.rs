//! Python's printing and parsing of JSON values, which every message quotes:
//! core's `pyfmt` (the one copy, lead decision 25) as the validator relies on
//! it, and the validator's own `str.strip()` and `==`. Expected strings are
//! what CPython 3.12 prints and parses for the same values.

use crate::flow::contract::pyfmt;
use crate::flow::validator::py;
use serde_json::{json, Value};

#[test]
fn repr_matches_python() {
    let cases: [(Value, &str); 16] = [
        (json!(null), "None"),
        (json!(true), "True"),
        (json!(false), "False"),
        (json!(0), "0"),
        (json!(-5), "-5"),
        (json!(18446744073709551615u64), "18446744073709551615"),
        (json!(1.0), "1.0"),
        (json!(-0.5), "-0.5"),
        (json!("abc"), "'abc'"),
        (json!("it's"), "\"it's\""),
        (json!("say \"hi\" it's"), "'say \"hi\" it\\'s'"),
        (json!("tab\tnew\nback\\"), "'tab\\tnew\\nback\\\\'"),
        (json!(["a", 1, null]), "['a', 1, None]"),
        (json!({"b": [true], "a": 1.5}), "{'a': 1.5, 'b': [True]}"),
        (json!("\u{7f}\u{1}"), "'\\x7f\\x01'"),
        (
            json!("caf\u{e9} \u{a0}\u{200b}\u{20b9}"),
            "'caf\u{e9} \\xa0\\u200b\u{20b9}'",
        ),
    ];
    for (value, want) in cases {
        assert_eq!(pyfmt::repr(&value), want, "{value}");
    }
}

#[test]
fn float_repr_matches_python() {
    let cases: [(f64, &str); 14] = [
        (100.0, "100.0"),
        (1.25, "1.25"),
        (0.1, "0.1"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1.5e-7, "1.5e-07"),
        (1e15, "1000000000000000.0"),
        (9999999999999998.0, "9999999999999998.0"),
        (1e16, "1e+16"),
        (1.2345678901234568e17, "1.2345678901234568e+17"),
        (1e100, "1e+100"),
        (-0.0, "-0.0"),
        (f64::INFINITY, "inf"),
        (f64::NAN, "nan"),
    ];
    for (value, want) in cases {
        assert_eq!(pyfmt::float(value), want, "{value}");
    }
}

#[test]
fn type_names_match_python() {
    for (value, want) in [
        (json!(null), "NoneType"),
        (json!(true), "bool"),
        (json!(1), "int"),
        (json!(1.0), "float"),
        (json!(1e2), "float"),
        (json!("x"), "str"),
        (json!([]), "list"),
        (json!({}), "dict"),
    ] {
        assert_eq!(pyfmt::type_name(&value), want, "{value}");
    }
}

#[test]
fn float_parsing_matches_python() {
    let accepted: [(&str, f64); 10] = [
        ("1", 1.0),
        (" 1.5 ", 1.5),
        ("\t-2e3\n", -2000.0),
        ("1_000", 1000.0),
        ("1_0.5", 10.5),
        (".5", 0.5),
        ("5.", 5.0),
        ("+3", 3.0),
        ("Infinity", f64::INFINITY),
        ("-inf", f64::NEG_INFINITY),
    ];
    for (text, want) in accepted {
        assert_eq!(pyfmt::parse_float(text), Some(want), "{text:?}");
    }
    assert!(pyfmt::parse_float("NaN").is_some_and(f64::is_nan));
    for text in [
        "", " ", "abc", "1__0", "_1", "1_", "1e", "0x10", "1,000", ".", "infinit",
    ] {
        assert_eq!(pyfmt::parse_float(text), None, "{text:?}");
    }
    assert_eq!(pyfmt::to_float(&json!(true)), Some(1.0));
    assert_eq!(pyfmt::to_float(&json!(null)), None);
    assert_eq!(pyfmt::to_float(&json!([1])), None);
}

#[test]
fn int_parsing_matches_python() {
    for (value, want) in [
        (json!("3"), Some(3)),
        (json!(" +7 "), Some(7)),
        (json!("-2"), Some(-2)),
        (json!("1_0"), Some(10)),
        (json!("007"), Some(7)),
        (json!("2.5"), None),
        (json!("abc"), None),
        (json!(2.9), Some(2)),
        (json!(-2.9), Some(-2)),
        (json!(false), Some(0)),
        (json!([3]), None),
    ] {
        assert_eq!(pyfmt::to_int(&value), want, "{value}");
    }
}

/// `float()` and `int()` strip less than `str.strip()`: CPython refuses
/// `float('\x1c12\x1f')` and `int('\x1c3')` though `str.isspace()` counts
/// those separators, so a quantity written that way is not a number while a
/// name written that way is not blank.
#[test]
fn number_parsing_strips_less_than_strip_does() {
    assert_eq!(pyfmt::parse_float("\u{1c}12\u{1f}"), None);
    assert_eq!(pyfmt::to_int(&json!("\u{1c}3")), None);
    assert_eq!(pyfmt::parse_float("\u{a0}7"), Some(7.0));
    assert_eq!(pyfmt::parse_float("\u{3000}7.5 "), Some(7.5));
    assert_eq!(py::strip("\u{1c}12\u{1f}"), "12");
}

#[test]
fn strip_counts_what_python_counts_as_space() {
    assert_eq!(py::strip("\u{1c}\u{a0} x \u{3000}\u{1f}"), "x");
    assert!(py::is_blank("\u{2028}\t"));
    assert!(!py::is_blank("\u{200b}"));
}

#[test]
fn equality_matches_python() {
    assert!(py::eq(&json!(1), &json!(1.0)));
    assert!(py::eq(&json!(true), &json!(1)));
    assert!(py::eq(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
    assert!(!py::eq(&json!("1"), &json!(1)));
    assert!(!py::eq(&json!(null), &json!(false)));
    assert!(!py::eq(&json!([1]), &json!([1, 1])));
    assert!(py::same_key(&json!(1), &json!(true)));
    assert!(!py::same_key(&json!([1]), &json!([1])));
}

#[test]
fn truthiness_matches_python() {
    for value in [
        json!(null),
        json!(false),
        json!(0),
        json!(0.0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        assert!(!pyfmt::truthy(&value), "{value}");
    }
    for value in [
        json!(true),
        json!(-1),
        json!(" "),
        json!([0]),
        json!({"a": null}),
    ] {
        assert!(pyfmt::truthy(&value), "{value}");
    }
}

#[test]
fn unicode_decimal_digits_read_as_python_reads_them() {
    for (text, want) in [
        ("\u{663}", "3"),
        ("1\u{661}2", "112"),
        ("\u{1d7ce}\u{1d7d9}\u{1d7ff}", "019"),
        ("\u{966}\u{96f}", "09"),
        ("\u{b2}x", "\u{b2}x"),
        ("abc", "abc"),
    ] {
        assert_eq!(py::ascii_digits(text), want, "{text:?}");
    }
}
