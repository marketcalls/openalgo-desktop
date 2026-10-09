//! web: test/test_strategy_module_order_dispatch.py

use openalgo_desktop_lib::strategy::dispatch::{build_order, exit_action, product_for_exchange};

#[test]
fn the_exit_action_is_the_opposite_of_the_held_side() {
    assert_eq!(exit_action("B").unwrap(), "SELL");
    assert_eq!(exit_action("S").unwrap(), "BUY");
    assert_eq!(exit_action("b").unwrap(), "SELL");
}

#[test]
fn an_exit_refuses_to_guess_a_side() {
    // PORTED DEFECT. The original derived the exit action from the leg's
    // configured side, which defaulted to "B", so an exit on a short
    // placed another SELL and doubled the position. Refusing beats
    // defaulting.
    for bad in ["", "LONG", "SHORT", "x"] {
        assert!(exit_action(bad).is_err(), "{:?}", bad);
    }
}

#[test]
fn the_product_is_translated_to_the_venue() {
    assert_eq!(product_for_exchange("MIS", "NFO"), "MIS");
    assert_eq!(product_for_exchange("MIS", "NSE"), "MIS");
    assert_eq!(product_for_exchange("NRML", "NSE"), "CNC");
    assert_eq!(product_for_exchange("CNC", "NFO"), "NRML");
    assert_eq!(product_for_exchange("NRML", "MCX"), "NRML");
}

#[test]
fn an_order_is_tagged_with_the_strategy_and_uppercased() {
    let o = build_order(
        "NIFTY13OCT2624500CE",
        "NFO",
        "sell",
        65,
        "NRML",
        "Iron condor",
        "MARKET",
    );
    assert_eq!(o.action, "SELL");
    assert_eq!(o.strategy, "Iron condor");
    assert_eq!(o.pricetype, "MARKET");
    let r = o.to_request();
    assert_eq!(r["quantity"], 65);
    assert_eq!(r["product"], "NRML");
}
