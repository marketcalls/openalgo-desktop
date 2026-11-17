//! Web `test/test_historify_symbol_resolution.py` (openalgo#2463,
//! `6716dd6f2`), the `resolve_symbol` cases. The bulk and single add cases,
//! which need a real watchlist, are in `tests/it/historify/symbol_resolution.rs`.

use super::resolve_symbol;
use std::cell::RefCell;

/// Web `MASTER`: what the master contract lookup would find.
const MASTER: &[(&str, &str)] = &[
    ("M&M", "NSE"),
    ("M&MFIN", "NSE"),
    ("BAJAJ-AUTO", "NSE"),
    ("J&KBANK", "NSE"),
    ("NAM-INDIA", "NSE"),
    ("RELIANCE", "NSE"),
    ("HFCL", "NSE"),
    // Both readings of A_B exist, so A_B is ambiguous.
    ("A-B", "NSE"),
    ("A&B", "NSE"),
    // A symbol that genuinely contains an underscore.
    ("BTC_INR", "CRYPTO"),
    ("BTC-INR", "CRYPTO"),
];

fn listed(symbol: &str, exchange: &str) -> bool {
    MASTER.contains(&(symbol, exchange))
}

/// Web test/test_historify_symbol_resolution.py::test_master_contract_symbols_resolve_as_written.
#[test]
fn master_contract_symbols_resolve_as_written() {
    for symbol in ["M&M", "BAJAJ-AUTO", "J&KBANK", "NAM-INDIA", "m&m", "HFCL"] {
        assert_eq!(
            resolve_symbol(symbol, "NSE", listed),
            Ok(symbol.to_uppercase()),
            "{symbol}"
        );
    }
}

/// Web test/test_historify_symbol_resolution.py::test_underscore_spelling_resolves_to_the_master_contract_symbol.
#[test]
fn underscore_spelling_resolves_to_the_master_contract_symbol() {
    for (typed, expected) in [
        ("M_M", "M&M"),
        ("M_MFIN", "M&MFIN"),
        ("BAJAJ_AUTO", "BAJAJ-AUTO"),
        ("J_KBANK", "J&KBANK"),
        ("nam_india", "NAM-INDIA"),
    ] {
        assert_eq!(
            resolve_symbol(typed, "NSE", listed),
            Ok(expected.to_string()),
            "{typed}"
        );
    }
}

/// Web test/test_historify_symbol_resolution.py::test_a_symbol_that_exists_as_written_is_never_rewritten.
#[test]
fn a_symbol_that_exists_as_written_is_never_rewritten() {
    let lookups = RefCell::new(Vec::new());
    let counted = |symbol: &str, exchange: &str| {
        lookups
            .borrow_mut()
            .push((symbol.to_string(), exchange.to_string()));
        listed(symbol, exchange)
    };
    assert_eq!(
        resolve_symbol("BTC_INR", "CRYPTO", counted),
        Ok("BTC_INR".to_string())
    );
    // Found on the first lookup: no reading of the underscore was tried.
    assert_eq!(
        lookups.into_inner(),
        [("BTC_INR".to_string(), "CRYPTO".to_string())]
    );
}

/// Web test/test_historify_symbol_resolution.py::test_a_series_suffix_is_not_dropped_to_find_a_symbol.
#[test]
fn a_series_suffix_is_not_dropped_to_find_a_symbol() {
    // HFCL exists; HFCL-BE does not. Shortening it would be a guess.
    let error = resolve_symbol("HFCL-BE", "NSE", listed).unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

/// Web test/test_historify_symbol_resolution.py::test_an_underscore_with_two_readings_is_refused_not_guessed.
#[test]
fn an_underscore_with_two_readings_is_refused_not_guessed() {
    let error = resolve_symbol("A_B", "NSE", listed).unwrap_err();
    assert!(error.contains("A-B") && error.contains("A&B"), "{error}");
    assert_eq!(
        error,
        "Symbol 'A_B' could be A-B or A&B on NSE. Enter the exact symbol."
    );
}

/// Web test/test_historify_symbol_resolution.py::test_an_unknown_symbol_is_refused_with_a_reason.
#[test]
fn an_unknown_symbol_is_refused_with_a_reason() {
    let error = resolve_symbol("NOSUCH_SYMBOL", "NSE", listed).unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

/// More underscores than the web reads are not expanded: 32 lookups for
/// five would be a guess, and the symbol is refused as not found.
#[test]
fn a_symbol_with_more_underscores_than_are_read_is_not_expanded() {
    let lookups = RefCell::new(0usize);
    let counted = |symbol: &str, exchange: &str| {
        *lookups.borrow_mut() += 1;
        listed(symbol, exchange)
    };
    let error = resolve_symbol("A_B_C_D_E_F", "NSE", counted).unwrap_err();
    assert!(error.contains("not found"), "{error}");
    assert_eq!(lookups.into_inner(), 1);
}

/// Index exchanges are not in the master and are taken as written.
#[test]
fn index_exchanges_are_not_checked() {
    assert_eq!(
        resolve_symbol("nifty", "nse_index", |_, _| false),
        Ok("NIFTY".to_string())
    );
}
