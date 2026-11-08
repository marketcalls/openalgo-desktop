use super::*;
use serde_json::json;

/// Web test/test_kotak_sdk_2365.py (marketcalls/openalgo#2367): Holdings is
/// asked only for a carried cash-segment leg without an uploaded price; an
/// F&O leg, a leg traded only today, or one Kotak priced itself does not.
#[test]
fn only_a_carried_cash_leg_without_an_uploaded_price_needs_its_cost() {
    let carried = json!({"exSeg": "nse_cm", "tok": "3045", "cfBuyQty": "100", "cfSellQty": "0", "upldPrc": "0.00"});
    assert!(needs_cost(&carried));
    let mut bse = carried.clone();
    bse["exSeg"] = json!("bse_cm");
    assert!(needs_cost(&bse));
    let mut option = carried.clone();
    option["exSeg"] = json!("nse_fo");
    assert!(!needs_cost(&option));
    let mut today = carried.clone();
    today["cfBuyQty"] = json!("0");
    assert!(!needs_cost(&today));
    let mut priced = carried.clone();
    priced["upldPrc"] = json!("760.00");
    assert!(!needs_cost(&priced));
    let mut no_token = carried;
    no_token["tok"] = json!("");
    assert!(!needs_cost(&no_token));
}

#[test]
fn holdings_costs_are_keyed_by_the_exchange_token() {
    let costs = costs_of(&[
        json!({"exchangeIdentifier": "3045", "averagePrice": 750.0, "instrumentToken": 1}),
        json!({"exchangeIdentifier": 11915, "averagePrice": "21.0"}),
        json!({"exchangeIdentifier": "1", "averagePrice": 0}),
        json!({"averagePrice": 5.0}),
    ]);
    assert_eq!(costs.len(), 2);
    assert_eq!(costs.get("3045"), Some(&750.0));
    assert_eq!(costs.get("11915"), Some(&21.0));
}

/// A holdings read that answers after a logout cleared the costs is not
/// kept: the next session never values its rows with the last one's.
#[test]
fn a_late_holdings_answer_after_a_logout_is_not_kept() {
    let costs = HoldingCosts::default();
    let now = Instant::now();
    let (cached, generation) = costs.0.get_with_generation(&"sid-1".to_string(), now);
    assert!(cached.is_none());
    costs.clear();
    let read = HashMap::from([("3045".to_string(), 750.0)]);
    assert!(!costs.0.fill("sid-1".into(), read, generation, now));
    assert!(costs.0.get(&"sid-1".to_string(), now).is_none());
}
