//! Web test/test_kotak_sdk_2365.py (marketcalls/openalgo#2365, #2367,
//! eb4cce0a1): the adapter against Kotak's API docs and its SDK. Holdings
//! resolve by `exchangeIdentifier`, a margin error reads `emsg`, a position
//! average is rounded to the row's `precision`, and a carried cash-segment
//! leg is valued at its Holdings cost.

use super::funds::parse_margin;
use super::mapping::*;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use serde_json::{json, Value};

fn row(symbol: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: format!("{}-EQ", symbol),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "nse_cm".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.01,
    }
}

/// The NSE cash master: ITBEES is pSymbol 19084, and an unrelated scrip
/// happens to have the exchange token 18974 (ITBEES's instrumentToken).
fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![row("ITBEES", "19084"), row("SOMEOTHERSCRIP", "18974")]);
    r
}

/// Kotak's documented example row.
fn holding(overrides: Value) -> Value {
    let mut h = json!({
        "instrumentType": "Equity", "instrumentToken": 18974, "exchangeIdentifier": "19084",
        "displaySymbol": "ITBEES", "symbol": "ITBEES", "exchangeSegment": "nse_cm",
        "quantity": 45, "averagePrice": 38.7162, "holdingCost": 1742.2293, "mktValue": 1771.2,
    });
    for (k, v) in overrides.as_object().unwrap() {
        h[k] = v.clone();
    }
    h
}

/// `test_holdings_look_up_the_exchange_token`
#[test]
fn holdings_look_up_the_exchange_token() {
    assert_eq!(map_holding(&holding(json!({})), &master()).symbol, "ITBEES");
}

/// `test_holdings_report_the_openalgo_symbol_when_kotak_spells_it_differently`
#[test]
fn holdings_report_the_openalgo_symbol_when_kotak_spells_it_differently() {
    let h = holding(json!({"displaySymbol": "ITBEES-EQ"}));
    assert_eq!(map_holding(&h, &master()).symbol, "ITBEES");
}

/// `test_holdings_keep_the_display_symbol_when_the_master_has_no_row`
#[test]
fn holdings_keep_the_display_symbol_when_the_master_has_no_row() {
    let h = holding(json!({"exchangeIdentifier": "99999", "displaySymbol": "NEWLISTING"}));
    assert_eq!(map_holding(&h, &master()).symbol, "NEWLISTING");
}

/// `test_margin_error_reports_kotaks_reason`, and Kotak's own field wins
/// over `errMsg` when both are there.
#[test]
fn a_margin_error_reports_kotaks_reason() {
    let e = parse_margin(&json!({"stat": "Not_Ok", "emsg": "Invalid session", "stCode": 1003}));
    assert_eq!(e, Err("Invalid session".into()));
    let both = json!({"stat": "Not_Ok", "emsg": "Invalid session", "errMsg": "Other"});
    assert_eq!(parse_margin(&both), Err("Invalid session".into()));
}

/// `test_margin_error_without_a_reason_keeps_the_generic_text`
#[test]
fn a_margin_error_without_a_reason_keeps_the_generic_text() {
    let e = parse_margin(&json!({"stat": "Not_Ok", "stCode": 1003}));
    assert_eq!(e, Err("Failed to calculate margin".into()));
}

/// Three USDINR futures bought for 250.3375: 83.4458333... each.
fn intraday(overrides: Value) -> Value {
    let mut p = json!({
        "trdSym": "USDINR26NOVFUT", "exSeg": "cde_fo", "prod": "NRML",
        "flBuyQty": "3", "flSellQty": "0", "cfBuyQty": "0", "cfSellQty": "0",
        "buyAmt": "250.3375", "sellAmt": "0",
    });
    for (k, v) in overrides.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

/// `test_average_is_rounded_to_the_row_precision`
#[test]
fn the_average_is_rounded_to_the_row_precision() {
    let p = map_position(&intraday(json!({"precision": "4"})), &master(), 0.0);
    assert_eq!(p.average_price, 83.4458);
}

/// `test_average_defaults_to_two_places_without_a_precision`
#[test]
fn the_average_defaults_to_two_places_without_a_precision() {
    let p = map_position(&intraday(json!({})), &master(), 0.0);
    assert_eq!(p.average_price, 83.45);
}

/// 100 SBIN carried overnight at Kotak's settlement valuation of 800.00 a
/// share, none traded today; bought at 750.00, which only Holdings knows.
fn carried_equity() -> Value {
    json!({
        "trdSym": "SBIN-EQ", "exSeg": "nse_cm", "tok": "3045", "prod": "CNC",
        "flBuyQty": "0", "flSellQty": "0", "cfBuyQty": "100", "cfSellQty": "0",
        "buyAmt": "0", "sellAmt": "0", "cfBuyAmt": "80000.00", "cfSellAmt": "0",
        "upldPrc": "0.00",
    })
}

/// A carried option, which has no cost anywhere in Kotak's API.
fn carried_option() -> Value {
    json!({
        "trdSym": "NIFTY27OCT2622600CE", "exSeg": "nse_fo", "tok": "45001", "prod": "NRML",
        "flBuyQty": "0", "flSellQty": "0", "cfBuyQty": "0", "cfSellQty": "65",
        "buyAmt": "0", "sellAmt": "0", "cfBuyAmt": "0", "cfSellAmt": "13832.00",
        "upldPrc": "0.00",
    })
}

/// `test_carried_equity_is_valued_at_its_holdings_cost` (the mapping half:
/// the Holdings average stamped on the row is the leg's cost).
#[test]
fn a_carried_equity_is_valued_at_its_holdings_cost() {
    let mut p = carried_equity();
    p[HOLDING_AVERAGE] = json!(750.0);
    assert_eq!(map_position(&p, &master(), 0.0).average_price, 750.0);
    assert!(
        !carry_forward_amounts(&p, 1.0).2,
        "not the carry-forward valuation"
    );
}

/// `test_carried_option_keeps_the_carry_forward_valuation` and
/// `test_a_holdings_failure_leaves_the_carry_forward_valuation` (mapping
/// half): without a Holdings cost the leg keeps Kotak's valuation, flagged.
#[test]
fn without_a_holdings_cost_the_carry_forward_valuation_stays() {
    let option = carried_option();
    assert_eq!(map_position(&option, &master(), 0.0).average_price, 212.8);
    assert!(carry_forward_amounts(&option, 1.0).2);
    let equity = carried_equity();
    assert_eq!(map_position(&equity, &master(), 0.0).average_price, 800.0);
    assert!(carry_forward_amounts(&equity, 1.0).2);
}

/// `test_an_uploaded_price_still_wins_over_holdings` (mapping half)
#[test]
fn an_uploaded_price_still_wins_over_holdings() {
    let mut p = carried_equity();
    p["upldPrc"] = json!("760.00");
    p[HOLDING_AVERAGE] = json!(750.0);
    assert_eq!(map_position(&p, &master(), 0.0).average_price, 760.0);
}
