//! The web's tests of `services/flow_node_contracts.py`
//! (`test/test_flow_custom_legs.py::TestOneExpiryRuleForTheRunAndThePicker`,
//! `test/test_flow_default_product.py::TestTheRuleItself`), plus the rules
//! they leave unpinned.

use super::*;
use crate::brokers::common::mapping::{Exchange, OrderStatus};

const WEEKLY: [&str; 5] = [
    "28-AUG-26",
    "04-SEP-26",
    "11-SEP-26",
    "25-SEP-26",
    "30-OCT-26",
];
const MONTHLY: [&str; 3] = ["28-AUG-26", "25-SEP-26", "29-OCT-26"];

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

fn at() -> NaiveDate {
    day("2026-08-24")
}

#[test]
fn current_week_is_the_nearest_listed_expiry() {
    assert_eq!(
        select_expiry(&WEEKLY, "current_week", at()).as_deref(),
        Some("28AUG26")
    );
}

#[test]
fn next_week_is_the_second() {
    assert_eq!(
        select_expiry(&WEEKLY, "next_week", at()).as_deref(),
        Some("04SEP26")
    );
}

#[test]
fn the_monthly_types_take_the_last_expiry_in_the_month() {
    assert_eq!(
        select_expiry(&WEEKLY, "next_month", at()).as_deref(),
        Some("25SEP26")
    );
    assert_eq!(
        select_expiry(&WEEKLY, "current_month", at()).as_deref(),
        Some("28AUG26")
    );
}

#[test]
fn a_monthly_only_product_reads_current_week_as_nearest() {
    assert_eq!(
        select_expiry(&MONTHLY, "current_week", at()).as_deref(),
        Some("28AUG26")
    );
    assert_eq!(
        select_expiry(&MONTHLY, "next_week", at()).as_deref(),
        Some("25SEP26")
    );
}

#[test]
fn it_rolls_into_the_next_year_in_december() {
    let expiries = ["31-DEC-26", "28-JAN-27"];
    assert_eq!(
        select_expiry(&expiries, "next_month", day("2026-12-10")).as_deref(),
        Some("28JAN27")
    );
}

#[test]
fn an_unsatisfiable_type_resolves_to_nothing() {
    assert_eq!(select_expiry(&["28-AUG-26"], "next_week", at()), None);
    assert_eq!(select_expiry(&WEEKLY, "next_year", at()), None);
    assert_eq!(select_expiry::<&str>(&[], "current_week", at()), None);
    assert_eq!(
        select_expiry(&WEEKLY, "current_month", day("2027-01-05")),
        None
    );
}

#[test]
fn unparseable_entries_are_ignored_not_fatal() {
    assert_eq!(
        select_expiry(&["junk", "28-AUG-26"], "current_week", at()).as_deref(),
        Some("28AUG26")
    );
}

#[test]
fn an_unsorted_list_is_sorted_by_date_before_choosing() {
    let listed = ["25-SEP-26", "28-AUG-26", "04-SEP-26"];
    assert_eq!(
        select_expiry(&listed, "current_week", at()).as_deref(),
        Some("28AUG26")
    );
    assert_eq!(
        select_expiry(&listed, "next_week", at()).as_deref(),
        Some("04SEP26")
    );
}

#[test]
fn every_broker_format_normalizes_to_the_api_form() {
    assert_eq!(format_expiry_for_api("28-AUG-26"), "28AUG26");
    assert_eq!(format_expiry_for_api("28AUG26"), "28AUG26");
    assert_eq!(format_expiry_for_api("28-August-2026"), "28AUGUST2026");
}

#[test]
fn every_master_contract_expiry_shape_parses() {
    let want = Some(day("2026-08-28"));
    for raw in [
        "28-AUG-26",
        "28aug26",
        "28-August-2026",
        "28AUGUST2026",
        "28-aug-26",
    ] {
        assert_eq!(parse_expiry_date(raw), want, "{raw}");
    }
    assert_eq!(parse_expiry_date("1-AUG-26"), Some(day("2026-08-01")));
}

#[test]
fn an_impossible_or_foreign_expiry_does_not_parse() {
    for raw in [
        "31-FEB-26",
        "2026-08-28",
        "28-AUGUST-26",
        "28-AUG-2026",
        "",
        "28AUG26FUT",
    ] {
        assert_eq!(parse_expiry_date(raw), None, "{raw}");
    }
}

#[test]
fn two_digit_years_follow_python_strptime() {
    // 69-99 are the 1900s, 00-68 the 2000s.
    assert_eq!(parse_expiry_date("01-JAN-68"), Some(day("2068-01-01")));
    assert_eq!(parse_expiry_date("01-JAN-69"), Some(day("1969-01-01")));
}

#[test]
fn derivative_segments_carry_and_cash_and_index_stay_intraday() {
    for exchange in DERIVATIVE_EXCHANGES {
        assert_eq!(
            default_product_for_exchange(Some(exchange)),
            "NRML",
            "{exchange}"
        );
    }
    for exchange in [
        "NSE",
        "BSE",
        "NSE_INDEX",
        "BSE_INDEX",
        "MCX_INDEX",
        "GLOBAL_INDEX",
        "CRYPTO",
    ] {
        assert_eq!(
            default_product_for_exchange(Some(exchange)),
            "MIS",
            "{exchange}"
        );
    }
}

#[test]
fn the_exchange_is_read_case_and_space_insensitively() {
    for value in ["nfo", " NFO ", "NfO"] {
        assert_eq!(default_product_for_exchange(Some(value)), "NRML", "{value}");
    }
}

#[test]
fn an_unusable_exchange_falls_back_to_intraday() {
    for value in [Some(""), None, Some("   "), Some("NOTANEXCHANGE")] {
        assert_eq!(default_product_for_exchange(value), "MIS", "{value:?}");
    }
}

#[test]
fn every_derivative_segment_is_an_exchange_and_no_index_is_one() {
    for exchange in DERIVATIVE_EXCHANGES {
        let parsed: Exchange = exchange.parse().unwrap();
        assert!(!parsed.is_index(), "{exchange}");
    }
}

#[test]
fn the_offset_range_the_executor_accepts() {
    for offset in [
        "ATM", "ITM1", "ITM9", "ITM10", "ITM49", "ITM50", "OTM1", "OTM50",
    ] {
        assert!(is_option_offset(offset), "{offset}");
    }
    for offset in [
        "OTM99", "ATM1", "DEEP", "ITM0", "OTM51", "otm1", "ITM05", "ATM ",
    ] {
        assert!(!is_option_offset(offset), "{offset}");
    }
}

#[test]
fn an_expiry_on_a_leg_is_ddmmmyy() {
    assert!(is_expiry_date("28OCT25"));
    for text in [
        "2025-10-28",
        "28-OCT-25",
        "OCT2825",
        "28OCTOBER25",
        "28oct25",
        "28OCT25\n",
    ] {
        assert!(!is_expiry_date(text), "{text:?}");
    }
}

#[test]
fn statuses_are_the_order_book_vocabulary_plus_any() {
    let mut expected: Vec<&str> = OrderStatus::ALL.iter().map(|s| s.as_str()).collect();
    expected.push("any");
    expected.sort_unstable();
    assert_eq!(VALID_STATUSES.to_vec(), expected);
}

#[test]
fn a_status_is_trimmed_lowered_and_spaced() {
    assert_eq!(normalize_status(" TRIGGER_PENDING "), "trigger pending");
    assert_eq!(normalize_status("Complete"), "complete");
    // Trimmed before the underscores become spaces, as the web does.
    assert_eq!(normalize_status("_open_"), " open ");
}

#[test]
fn an_underlying_may_carry_its_expiry() {
    assert_eq!(
        parse_underlying_symbol("NIFTY27AUG26"),
        ("NIFTY".to_string(), Some("27AUG26".to_string()))
    );
    assert_eq!(
        parse_underlying_symbol("crudeoil19nov25fut"),
        ("CRUDEOIL".to_string(), Some("19NOV25".to_string()))
    );
    assert_eq!(
        parse_underlying_symbol("nifty"),
        ("NIFTY".to_string(), None)
    );
    assert_eq!(
        parse_underlying_symbol(" NIFTY27AUG26"),
        (" NIFTY27AUG26".to_string(), None)
    );
    // Python's `$` also matches before a final newline.
    assert_eq!(
        parse_underlying_symbol("NIFTY27AUG26\n"),
        ("NIFTY".to_string(), Some("27AUG26".to_string()))
    );
}

#[test]
fn price_conditions_accept_the_editor_spellings() {
    for (raw, canonical) in [
        ("above", "greater_than"),
        (" Price-Above ", "greater_than"),
        ("crosses_below", "crossing_down"),
        ("crosses", "crossing"),
        ("entering-channel", "entering_channel"),
        ("abov", "abov"),
        ("", ""),
    ] {
        assert_eq!(normalize_condition(raw), canonical, "{raw}");
    }
}

#[test]
fn the_sorted_vocabularies_are_sorted() {
    for list in [
        &VALID_STATUSES[..],
        &VALID_EXPIRY_TYPES[..],
        &DERIVATIVE_EXCHANGES[..],
    ] {
        let mut sorted = list.to_vec();
        sorted.sort_unstable();
        assert_eq!(list.to_vec(), sorted);
    }
}
