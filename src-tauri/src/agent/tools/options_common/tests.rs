//! The shared option vocabulary against the web's tests
//! (`test_agent_viz_tool_arguments.py` legs and expiries,
//! `test_agent_combined_premium.py` expiry choices).

use super::*;
use crate::agent::tools::testkit::{map, Kit};
use serde_json::json;

const LISTED: [&str; 5] = ["16SEP26", "23SEP26", "29SEP26", "06OCT26", "27OCT26"];

fn listed() -> Vec<String> {
    LISTED.iter().map(|s| s.to_string()).collect()
}

/// A Monday in September 2026, before the first listed expiry.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 14).unwrap_or_default()
}

fn refused<T: std::fmt::Debug>(r: Result<T, ToolOutcome>, needle: &str) {
    match r {
        Err(ToolOutcome::Retry(t)) => assert!(t.contains(needle), "{}", t),
        other => panic!("expected a refusal naming {:?}, got {:?}", needle, other),
    }
}

async fn side_and_lots(
    entry: Value,
    size: Option<i64>,
) -> (Result<(&'static str, i64), ToolOutcome>, Vec<String>) {
    let mut notices = Vec::new();
    let r = leg_side_and_lots(&entry, 2, || async move { Ok(size) }, &mut notices).await;
    (r, notices)
}

#[tokio::test]
async fn the_side_is_read_whatever_the_key_is_called() {
    for (entry, want) in [
        (json!({"action": "SELL"}), (SELL, 1)),
        (json!({"Side": "sell", "Lots": 2}), (SELL, 2)),
        (json!({"transaction_type": "S"}), (SELL, 1)),
        (json!({"ACTION": "buy", "lots": "3"}), (BUY, 3)),
        (json!("NIFTY28NOV2523850CE"), (BUY, 1)),
    ] {
        assert_eq!(
            side_and_lots(entry.clone(), None).await.0.ok(),
            Some(want),
            "{}",
            entry
        );
    }
}

#[tokio::test]
async fn a_quantity_becomes_lots_with_the_master_lot_size() {
    assert_eq!(
        side_and_lots(json!({"action": "SELL", "quantity": 150}), Some(75))
            .await
            .0
            .ok(),
        Some((SELL, 2))
    );
    refused(
        side_and_lots(json!({"quantity": 150}), None).await.0,
        "no lot size",
    );
    refused(
        side_and_lots(json!({"qty": 100}), Some(75)).await.0,
        "multiple of 75",
    );
    let (r, notices) = side_and_lots(json!({"side": "BUY", "lots": null}), None).await;
    assert_eq!(r.ok(), Some((BUY, 1)));
    assert_eq!(
        notices,
        vec!["Leg 2 gave no lot count, so one lot was used."]
    );
    refused(
        side_and_lots(json!({"action": "HOLD"}), None).await.0,
        "not a side",
    );
    refused(
        side_and_lots(json!({"lots": 101}), None).await.0,
        "legs (leg 2 lots)",
    );
}

#[test]
fn a_strike_and_right_object_expands_like_the_shorthand() {
    assert_eq!(
        leg_symbol(
            &json!({"strike": 23850.5, "option_type": "CE"}),
            1,
            "NIFTY",
            "28NOV25"
        )
        .ok(),
        Some(("NIFTY28NOV2523850.5CE".to_string(), String::new()))
    );
    assert_eq!(
        leg_symbol(
            &json!({"strike": 23850.0, "right": "pe"}),
            1,
            "NIFTY",
            "28NOV25"
        )
        .ok(),
        Some(("NIFTY28NOV2523850PE".to_string(), String::new()))
    );
    refused(
        leg_symbol(&json!({"strike": 23850, "option_type": "CE"}), 1, "", ""),
        "strike and a right",
    );
    assert_eq!(
        leg_symbol(&json!("nfo:nifty28nov2523850ce"), 1, "", "").ok(),
        Some(("NIFTY28NOV2523850CE".to_string(), "NFO".to_string()))
    );
    assert_eq!(
        leg_symbol(&json!("24500.50CE"), 3, "NIFTY", "28NOV25").ok(),
        Some(("NIFTY28NOV2524500.5CE".to_string(), String::new()))
    );
    refused(
        leg_symbol(&json!(5), 4, "", ""),
        "leg 4 is int, not a contract",
    );
    refused(
        leg_symbol(&json!({"side": "BUY"}), 2, "", ""),
        "leg 2 names no symbol",
    );
}

#[test]
fn leg_lists_take_text_objects_and_refuse_broken_json() {
    assert_eq!(
        leg_entries(
            Some(&json!(
                "{\"symbol\":\"NIFTY28NOV2523850CE\",\"action\":\"SELL\"}"
            )),
            "legs"
        )
        .ok(),
        Some(vec![
            json!({"symbol": "NIFTY28NOV2523850CE", "action": "SELL"})
        ])
    );
    refused(
        leg_entries(Some(&json!("{\"symbol\": ")), "legs"),
        "not valid JSON",
    );
    assert_eq!(
        leg_entries(Some(&json!("A, B\nC")), "legs")
            .map(|v| v.len())
            .ok(),
        Some(3)
    );
    assert_eq!(leg_entries(None, "legs").ok(), Some(vec![]));
    assert_eq!(leg_entries(Some(&json!("")), "legs").ok(), Some(vec![]));
    refused(
        leg_entries(Some(&json!(5)), "legs"),
        "it is int, not a list",
    );
    let nine: Vec<Value> = (0..9).map(|i| json!(i.to_string())).collect();
    refused(
        leg_entries(Some(&Value::Array(nine)), "legs"),
        "it carries 9 legs, more than the 8 one chart shows",
    );
}

#[test]
fn every_common_expiry_spelling_comes_out_as_ddmmmyy() {
    for spelling in [
        "28NOV25",
        "28-NOV-25",
        "28NOV2025",
        "2025-11-28",
        "28 NOV 25",
        "28 nov 2025",
    ] {
        assert_eq!(
            normalise_leg_expiry(Some(&json!(spelling)), "expiry").ok(),
            Some("28NOV25".into()),
            "{}",
            spelling
        );
    }
    assert_eq!(
        normalise_leg_expiry(Some(&json!("5JAN26")), "expiry").ok(),
        Some("05JAN26".into())
    );
    for bad in ["31FEB25", "28XYZ25", "tomorrow", "", "2025-13-01"] {
        refused(normalise_leg_expiry(Some(&json!(bad)), "expiry"), "DDMMMYY");
    }
}

#[test]
fn the_monthly_contract_is_the_last_weekly_of_its_month() {
    for (said, want) in [
        ("current_month", "29SEP26"),
        ("current month", "29SEP26"),
        ("Current Month", "29SEP26"),
        ("current-month", "29SEP26"),
        ("monthly", "29SEP26"),
        ("this_month", "29SEP26"),
        ("next_month", "27OCT26"),
        ("next month", "27OCT26"),
    ] {
        let mut notices = Vec::new();
        assert_eq!(
            choose_expiry(&listed(), Some(&json!(said)), &mut notices, today()).ok(),
            Some(want.into()),
            "{}",
            said
        );
    }
    let mut notices = Vec::new();
    assert_eq!(
        choose_expiry(&listed(), Some(&json!("")), &mut notices, today()).ok(),
        Some("16SEP26".into())
    );
    assert_eq!(notices, vec!["16SEP26 is this week's expiry and was used."]);
    assert_eq!(
        choose_expiry(
            &listed(),
            Some(&json!("next_week")),
            &mut Vec::new(),
            today()
        )
        .ok(),
        Some("23SEP26".into())
    );
    let mut notices = Vec::new();
    assert_eq!(
        choose_expiry(
            &["27OCT26".to_string()],
            Some(&json!("next_week")),
            &mut notices,
            today()
        )
        .ok(),
        Some("27OCT26".into())
    );
    assert!(notices[0].contains("only listed expiry"));
    refused(
        choose_expiry(&listed(), Some(&json!("31DEC26")), &mut Vec::new(), today()),
        "16SEP26",
    );
    for choice in EXPIRY_CHOICES {
        let got = choose_expiry(&listed(), Some(&json!(choice)), &mut Vec::new(), today())
            .unwrap_or_default();
        assert!(LISTED.contains(&got.as_str()), "{}", choice);
    }
    let mut notices = Vec::new();
    let _ = choose_expiry(
        &listed(),
        Some(&json!("current_month")),
        &mut notices,
        today(),
    );
    assert!(notices[0].contains("29SEP26"));
    assert_eq!(
        monthly_expiry(&["24SEP26".into(), "29OCT26".into()], false),
        "24SEP26"
    );
    assert_eq!(
        monthly_expiry(&["24SEP26".into(), "29OCT26".into()], true),
        "29OCT26"
    );
    let monday = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap_or_default();
    let mut notices = Vec::new();
    let _ = choose_expiry(&["29SEP26".into()], None, &mut notices, monday);
    assert_eq!(notices, vec!["29SEP26 is this week's expiry and was used."]);
    let thursday = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap_or_default();
    let mut notices = Vec::new();
    let _ = choose_expiry(&["29SEP26".into()], None, &mut notices, thursday);
    assert_eq!(
        notices,
        vec!["No expiry is listed inside the current week, so 29SEP26, the nearest listed one, was used."]
    );
    assert!(!within_this_week("06OCT26", today()));
}

#[tokio::test]
async fn the_chart_resolves_the_words_it_is_given() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Chat);
    let mut notices = Vec::new();
    // The master lists NIFTY options on 13OCT26 and 27OCT26.
    assert_eq!(
        resolve_expiry(
            &ctx,
            "NIFTY",
            "NSE_INDEX",
            Some(&json!("current month")),
            &mut notices
        )
        .await
        .ok(),
        Some("27OCT26".into())
    );
    // An exact date is taken as it is: no listing is read to check it.
    assert_eq!(
        resolve_expiry(
            &ctx,
            "NIFTY",
            "NSE_INDEX",
            Some(&json!("2025-11-28")),
            &mut Vec::new()
        )
        .await
        .ok(),
        Some("28NOV25".into())
    );
    refused(
        resolve_expiry(
            &ctx,
            "SENSEX",
            "BSE_INDEX",
            Some(&json!("current_month")),
            &mut Vec::new(),
        )
        .await,
        "SENSEX lists no option expiries on BFO",
    );
    assert_eq!(
        listed_expiries(&ctx, "NIFTY", "NSE_INDEX").await.ok(),
        Some(vec!["13OCT26".to_string(), "27OCT26".to_string()])
    );
}

#[tokio::test]
async fn a_contract_is_read_from_the_master() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Chat);
    let listed = crate::agent::tools::market::master(&ctx.app);
    let c = resolve_contract(&ctx, &listed, "NIFTY13OCT2624500CE", "", Some("leg 1"))
        .await
        .unwrap_or_else(|e| panic!("{:?}", e));
    assert_eq!(c.exchange, "NFO");
    assert_eq!(c.segment, SEGMENT_OPTION);
    assert_eq!(c.strike, Some(24500.0));
    assert_eq!(c.option_type, "CE");
    assert_eq!(c.lot_size, 75);
    assert_eq!(c.expiry, "13OCT26");
    assert_eq!(c.base, "NIFTY");
    assert_eq!(c.underlying_exchange, "NSE_INDEX");
    // 15:30 IST on 13 October 2026.
    assert_eq!(c.expiry_ts, Some(1_791_885_600));
    let j = c.to_json();
    assert_eq!(j["lotSize"], 75);
    assert_eq!(j["expiryTs"], 1_791_885_600_i64);
    let fut = resolve_contract(&ctx, &listed, "NIFTY27OCT26FUT", "NFO", Some("leg 2")).await;
    assert_eq!(fut.map(|c| c.segment).ok(), Some(SEGMENT_FUTURE.into()));
    refused(
        resolve_contract(&ctx, &listed, "NIFTY13OCT2624550CE", "", Some("leg 3")).await,
        "leg 3, NIFTY13OCT2624550CE, is not listed on any derivatives exchange",
    );
    refused(
        resolve_contract(&ctx, &listed, "INFY", "NSE_INDEX", Some("leg 4")).await,
        "leg 4 names exchange 'NSE_INDEX', which lists no futures or options",
    );
    assert_eq!(
        master_lot_size(&ctx, "NIFTY13OCT2624500CE", "NFO")
            .await
            .ok(),
        Some(Some(75))
    );
}

#[test]
fn multipliers_labels_and_underlying_exchanges() {
    assert_eq!(signed_multiplier(BUY, 2), 2);
    assert_eq!(signed_multiplier(SELL, 3), -3);
    assert_eq!(
        leg_label(&map(json!({"side": "SELL", "lots": 2, "symbol": "X"}))),
        "SELL 2x X"
    );
    assert_eq!(leg_label(&map(json!({"symbol": "Y"}))), "BUY Y");
    let listed = |s: &str, e: &str| (s, e) == ("NIFTY", "NSE_INDEX");
    let mut notices = Vec::new();
    assert_eq!(
        resolve_underlying_exchange(&listed, "NIFTY", None, &mut notices).ok(),
        Some("NSE_INDEX".into())
    );
    assert_eq!(
        notices,
        vec!["NIFTY was resolved to NSE_INDEX from the instrument master."]
    );
    assert_eq!(
        resolve_underlying_exchange(&listed, "X", Some(&json!(" mcx ")), &mut Vec::new()).ok(),
        Some("MCX".into())
    );
    refused(
        resolve_underlying_exchange(&listed, "ZZZ", None, &mut Vec::new()),
        "ZZZ is not listed on any exchange this tool searched",
    );
    assert_eq!(segment_of("INFY", None).ok(), Some(""));
    refused(
        segment_of("INFY", Some("leg 1")),
        "leg 1, INFY, is neither an option nor a future",
    );
}
