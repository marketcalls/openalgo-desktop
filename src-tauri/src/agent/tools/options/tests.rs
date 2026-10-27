//! The options toolkit against the web's argument tests
//! (`test_agent_tool_arguments.py`, options section) and the mock broker.

use super::*;
use crate::agent::tools::testkit::{body, tag, Kit};
use crate::brokers::types::Quote;
use serde_json::json;

fn listed(symbol: &str, exchange: &str) -> bool {
    [
        ("INFY", "NSE"),
        ("SBIN", "NSE"),
        ("RELIANCE", "NSE"),
        ("NIFTY", "NSE_INDEX"),
        ("SENSEX", "BSE_INDEX"),
    ]
    .contains(&(symbol, exchange))
}

fn v(s: &str) -> Value {
    json!(s)
}

fn refused<T: std::fmt::Debug>(r: Result<T, ToolOutcome>, needle: &str) {
    match r {
        Err(ToolOutcome::Retry(text)) => assert!(text.contains(needle), "{}", text),
        other => panic!("expected a refusal naming {:?}, got {:?}", needle, other),
    }
}

#[test]
fn every_exchange_spelling_reads_the_same_here() {
    assert_eq!(
        normalise_exchange(Some(&v("bse-index")), &UNDERLYING_EXCHANGES).ok(),
        Some("BSE_INDEX".into())
    );
    refused(
        normalise_exchange(Some(&v("LSE")), &UNDERLYING_EXCHANGES),
        "'exchange' argument",
    );
    assert_eq!(
        normalise_exchange(None, &OPTION_EXCHANGES),
        Err(ToolOutcome::Retry(
            "The 'exchange' argument is invalid: it is not an exchange this tool accepts. Use one of: BCD, BFO, CDS, CRYPTO, MCX, NCDEX, NCO, NFO.".into()
        ))
    );
}

#[test]
fn every_expiry_spelling_becomes_ddmmmyy() {
    for spelling in [
        "28OCT26",
        "28-OCT-26",
        "28oct26",
        "28OCT2026",
        "2026-10-28",
        "28 OCT 26",
        "28-Oct-2026",
        "28/OCT/26",
    ] {
        assert_eq!(
            normalise_expiry(Some(&v(spelling)), "expiry_date").ok(),
            Some("28OCT26".into()),
            "{}",
            spelling
        );
    }
    assert_eq!(
        normalise_expiry(Some(&v("8OCT26")), "expiry_date").ok(),
        Some("08OCT26".into())
    );
    for bad in [
        "28XYZ26",
        "2026-02-30",
        "32OCT26",
        "00OCT26",
        "next week",
        "28OCT6",
        "31FEB25",
    ] {
        refused(
            normalise_expiry(Some(&v(bad)), "expiry_date"),
            "'expiry_date' argument",
        );
    }
    assert_eq!(
        normalise_expiry(Some(&v("28XYZ26")), "expiry_date"),
        Err(ToolOutcome::Retry("The 'expiry_date' argument is invalid: '28XYZ26' is not an expiry date. Use DDMMMYY: two digits for the day, a three-letter month and two digits for the year, for example '28NOV25' or '05JAN26'.".into()))
    );
    refused(
        normalise_expiry(Some(&v("")), "expiry_dates"),
        "'expiry_dates' argument",
    );
}

#[test]
fn an_embedded_expiry_still_allows_an_empty_argument() {
    assert_eq!(
        normalise_expiry_argument(Some(&v("")), "NIFTY28OCT25FUT", true, "expiry_date").ok(),
        Some(String::new())
    );
    assert_eq!(
        normalise_expiry_argument(Some(&v("")), "NIFTY", true, "expiry_date"),
        Err(ToolOutcome::Retry("The 'expiry_date' argument is invalid: no expiry was given and the underlying does not carry one. Pass the expiry in DDMMMYY format, for example '28NOV25'. Look up the listed expiries first rather than guessing a date.".into()))
    );
    refused(
        normalise_expiry_argument(None, "NIFTY", false, "expiry_date"),
        "this tool needs one named explicitly",
    );
}

#[test]
fn accepted_offsets_and_types() {
    for (given, want) in [
        ("atm", "ATM"),
        ("ATM0", "ATM"),
        ("ATM+0", "ATM"),
        ("otm 2", "OTM2"),
        ("OTM-2", "OTM2"),
        (" itm 10 ", "ITM10"),
        ("OTM50", "OTM50"),
    ] {
        assert_eq!(
            normalise_offset(Some(&v(given))).ok(),
            Some(want.to_string()),
            "{}",
            given
        );
    }
    for step in ["ATM+2", "ATM-1", "atm 3"] {
        let r = normalise_offset(Some(&v(step)));
        refused(r.clone(), "OTM2");
        refused(r, "ITM2");
    }
    for bad in ["OTM51", "OTM0", "NEAR", ""] {
        refused(normalise_offset(Some(&v(bad))), "'offset' argument");
    }
    for (given, want) in [
        ("CE", "CE"),
        ("c", "CE"),
        ("Call", "CE"),
        ("pe", "PE"),
        ("P", "PE"),
        ("PUT", "PE"),
    ] {
        assert_eq!(
            normalise_option_type(Some(&v(given))).ok(),
            Some(want.to_string())
        );
    }
    for bad in ["X", "FUT", ""] {
        refused(
            normalise_option_type(Some(&v(bad))),
            "'option_type' argument",
        );
    }
}

#[test]
fn an_index_on_its_cash_exchange_moves_and_a_stock_stays() {
    let (ex, notice) = index_underlying(&listed, "NIFTY", "NSE");
    assert_eq!(ex, "NSE_INDEX");
    assert!(notice.is_some());
    assert_eq!(index_underlying(&listed, "SENSEX", "BSE").0, "BSE_INDEX");
    assert_eq!(
        index_underlying(&listed, "NIFTY28OCT25FUT", "NSE").0,
        "NSE_INDEX"
    );
    for (u, e) in [
        ("RELIANCE", "NSE"),
        ("NIFTY", "NSE_INDEX"),
        ("NIFTY", "NFO"),
        ("CRUDEOIL", "MCX"),
        ("NOSUCH", "NSE"),
    ] {
        assert_eq!(
            index_underlying(&listed, u, e),
            (e.to_string(), None),
            "{} {}",
            u,
            e
        );
    }
}

#[test]
fn whole_numbers_and_the_refusal_naming_the_default() {
    refused(
        normalise_int(Some(&json!(0)), "strike_count", 1, 100, Some(15)),
        "the default strike_count is 15",
    );
    refused(
        normalise_int(Some(&v("ten")), "days", 1, 5, Some(1)),
        "the default days is 1",
    );
    assert_eq!(
        normalise_int(Some(&v("ten")), "days", 1, 5, Some(1)),
        Err(ToolOutcome::Retry("The 'days' argument is invalid: 'ten' is not a whole number between 1 and 5. Pass an integer in that range; the default days is 1.".into()))
    );
    let no_default = normalise_int(Some(&json!(30)), "width", 1, 20, None);
    let text = format!("{:?}", no_default);
    assert!(
        !text.contains("default") && !text.contains("sensible"),
        "{}",
        text
    );
    assert_eq!(normalise_int(Some(&v("5")), "x", 1, 10, None).ok(), Some(5));
    assert_eq!(
        normalise_int(Some(&json!(7.0)), "x", 1, 10, None).ok(),
        Some(7)
    );
    assert!(normalise_int(Some(&json!(true)), "x", 0, 10, None).is_err());
    assert!(normalise_int(Some(&json!(2.5)), "x", 0, 10, None).is_err());
}

#[test]
fn true_and_false_from_the_words_a_model_uses() {
    for yes in [json!(true), json!("yes"), json!("ON"), json!(1), json!("t")] {
        assert_eq!(
            normalise_bool(Some(&yes), "with_greeks").ok(),
            Some(true),
            "{}",
            yes
        );
    }
    for no in [json!(false), json!("no"), json!(""), json!(0), json!(null)] {
        assert_eq!(
            normalise_bool(Some(&no), "with_greeks").ok(),
            Some(false),
            "{}",
            no
        );
    }
    assert_eq!(
        normalise_bool(Some(&v("maybe")), "with_greeks"),
        Err(ToolOutcome::Retry("The 'with_greeks' argument is invalid: 'maybe' is not true or false. Pass true or false.".into()))
    );
}

#[test]
fn a_large_chain_keeps_a_window_centred_on_atm() {
    let chain: Vec<Value> = (0..101)
        .map(|i| json!({"strike": 20000 + i * 50, "ce": {"symbol": "X".repeat(40), "ltp": 1.5}, "pe": {"symbol": "Y".repeat(40), "ltp": 2.5}}))
        .collect();
    let payload = json!({"status": "success", "atm_strike": 22500, "chain": chain});
    let narrowed = narrow_chain_to_budget(payload);
    assert!(rendered_length(&narrowed) <= MAX_RESULT_CHARS);
    let kept = narrowed["chain"].as_array().cloned().unwrap_or_default();
    assert_eq!(narrowed["strikes_returned"], kept.len());
    assert_eq!(narrowed["strikes_omitted"], 101 - kept.len());
    assert_eq!(narrowed["note"], CHAIN_TRIMMED_NOTE);
    let strikes: Vec<i64> = kept.iter().filter_map(|r| r["strike"].as_i64()).collect();
    assert!(strikes.contains(&22500));
    let mid = strikes[strikes.len() / 2];
    assert!((mid - 22500).abs() <= 50, "{:?}", strikes);
    let small = json!({"chain": [{"strike": 1}]});
    assert_eq!(narrow_chain_to_budget(small.clone()), small);
}

#[tokio::test]
async fn the_chain_tool_sends_the_corrected_exchange_and_expiry() {
    let kit = Kit::new();
    kit.mock.set_quote(Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24510.0,
        ..Default::default()
    });
    let out = kit
        .call(
            &GROUP,
            "get_option_chain",
            json!({"underlying": "NIFTY", "exchange": "NSE", "expiry_date": "13-OCT-26"}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_option_chain\" underlying=\"NIFTY\" exchange=\"NSE_INDEX\" expiry=\"13OCT26\">"
    );
    let b = body(&out);
    assert!(
        b["notices"][0]
            .as_str()
            .unwrap_or_default()
            .contains("NSE_INDEX"),
        "{}",
        b
    );
    assert_eq!(b["underlying"], "NIFTY");
}

#[tokio::test]
async fn the_synthetic_future_refuses_an_embedded_expiry() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_synthetic_future",
            json!({"underlying": "NIFTY27OCT26FUT", "exchange": "NFO", "expiry_date": "27OCT26"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry("The 'underlying' argument is invalid: 'NIFTY27OCT26FUT' carries its own expiry, which this tool cannot take. Pass the base symbol on its own, such as 'NIFTY', and name the expiry in expiry_date.".into())
    );
    let missing = kit
        .call(
            &GROUP,
            "get_synthetic_future",
            json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": ""}),
        )
        .await;
    assert!(
        missing
            .text()
            .contains("this tool needs one named explicitly"),
        "{:?}",
        missing
    );
}

#[tokio::test]
async fn an_option_symbol_resolves_from_listed_strikes() {
    let kit = Kit::new();
    kit.mock.set_quote(Quote {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ltp: 24510.0,
        ..Default::default()
    });
    let out = kit
        .call(
            &GROUP,
            "get_option_symbol",
            json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "13OCT26", "offset": "otm 2", "option_type": "call"}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_option_symbol\" underlying=\"NIFTY\" exchange=\"NSE_INDEX\" expiry=\"13OCT26\" offset=\"OTM2\" option_type=\"CE\">"
    );
    let b = body(&out);
    assert_eq!(b["symbol"], "NIFTY13OCT2624700CE", "{}", b);
    let refused = kit
        .call(
            &GROUP,
            "get_option_greeks",
            json!({"symbol": "NIFTY13OCT2624700CE", "exchange": "NSE"}),
        )
        .await;
    assert!(
        refused
            .text()
            .contains("Use one of: BCD, BFO, CDS, CRYPTO, MCX, NCDEX, NCO, NFO."),
        "{:?}",
        refused
    );
}

#[tokio::test]
async fn the_options_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Chat);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}
