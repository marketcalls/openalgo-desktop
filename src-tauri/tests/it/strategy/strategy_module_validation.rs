//! web: test/test_strategy_module_api.py (validator half)

use super::*;
use openalgo_desktop_lib::strategy::validate::validate_strategy_config;

fn v(t: &T, cfg: Value) -> Result<Value, String> {
    validate_strategy_config(&cfg, &t.symbols.snapshot())
}

fn base() -> Value {
    config("V", json!([short_call_leg()]), json!({}))
}

#[test]
fn a_valid_config_is_normalised_and_idempotent() {
    let t = t();
    let c = v(&t, base()).unwrap();
    assert_eq!(c["legs"][0]["risk_unit"], "points");
    assert_eq!(c["strategy_kind"], "batch");
    assert_eq!(v(&t, c.clone()).unwrap(), c);
}

#[test]
fn an_unknown_field_is_refused_not_dropped() {
    let t = t();
    let mut c = base();
    c["overall_sl_mtmm"] = json!(5000);
    assert!(v(&t, c)
        .unwrap_err()
        .starts_with("The request does not accept overall_sl_mtmm"));
}

#[test]
fn a_negative_loss_threshold_is_refused() {
    let t = t();
    let mut c = base();
    c["overall_sl_mtm"] = json!(-5000);
    assert!(v(&t, c)
        .unwrap_err()
        .contains("entered as a positive amount"));
}

#[test]
fn a_fractional_strike_is_kept() {
    let t = t();
    let mut leg = short_call_leg();
    leg["strike_mode"] = json!("strike");
    leg.as_object_mut().unwrap().remove("atm_offset");
    leg["strike"] = json!(292.5);
    let c = v(&t, config("F", json!([leg]), json!({}))).unwrap();
    assert_eq!(c["legs"][0]["strike"], 292.5);
}

#[test]
fn intraday_needs_both_times_in_order() {
    let t = t();
    let mut c = base();
    c["strategy_type"] = json!("intraday");
    assert_eq!(
        v(&t, c.clone()).unwrap_err(),
        "entry_time is required for an intraday strategy"
    );
    c["entry_time"] = json!("15:00");
    c["exit_time"] = json!("09:20");
    assert_eq!(
        v(&t, c).unwrap_err(),
        "entry_time must be earlier than exit_time"
    );
}

#[test]
fn a_lock_floor_above_its_threshold_is_refused() {
    let t = t();
    let mut c = base();
    c["lock_profit"] = json!({"mode": "lock", "if_profit_reaches": 1000, "lock_profit": 2000});
    assert!(v(&t, c).unwrap_err().contains("cannot be more than"));
}

#[test]
fn a_cash_leg_outside_the_stocks_tab_is_refused() {
    let t = t();
    let leg = json!({"id": 1, "segment": "cash", "position": "B", "lots": 10});
    let c = config("C", json!([leg]), json!({"universe_tab": "weekly_monthly"}));
    assert!(v(&t, c).unwrap_err().contains("does not offer"));
}

#[test]
fn a_short_cash_leg_under_carry_is_refused() {
    let t = t();
    let leg = json!({"id": 1, "segment": "cash", "position": "S", "lots": 10});
    let c = config(
        "C",
        json!([leg]),
        json!({"universe_tab": "stocks_fno", "underlying": "SBIN", "underlying_exchange": "NSE"}),
    );
    assert!(v(&t, c)
        .unwrap_err()
        .contains("Cash cannot be held short overnight"));
}

#[test]
fn a_signal_quantity_off_the_lot_boundary_is_refused() {
    let t = t();
    let leg = json!({"id": 1, "symbol": "RELIANCE27OCT26FUT", "exchange": "NFO", "side": "both",
                     "qty": 250, "qty_mode": "units", "segment": "futures"});
    let e = v(&t, signal_config(json!([leg]), json!({}))).unwrap_err();
    assert!(e.contains("not a whole number of lots"), "{}", e);
}

#[test]
fn a_leg_side_its_direction_never_acts_on_is_refused() {
    let t = t();
    let c = signal_config(
        json!([signal_leg(1, "RELIANCE", "short")]),
        json!({"direction": "long_only"}),
    );
    assert!(v(&t, c).unwrap_err().contains("never acts on"));
}

#[test]
fn only_market_is_accepted() {
    let t = t();
    let mut c = base();
    c["pricetype"] = json!("LIMIT");
    assert!(v(&t, c)
        .unwrap_err()
        .starts_with("pricetype must be one of: MARKET"));
}

#[test]
fn a_bad_allowlist_entry_is_refused() {
    let t = t();
    let mut c = base();
    c["webhook_ip_allowlist"] = json!(["10.0.0.0/8", "nope"]);
    assert!(v(&t, c)
        .unwrap_err()
        .contains("is not a valid IP address or CIDR range"));
}
