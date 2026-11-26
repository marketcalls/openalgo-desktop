//! Web test/test_slm_trigger_pricing_2390.py for Firstock
//! (marketcalls/openalgo#2495, issue #2390): an SL-M goes out as SL-LMT
//! priced off its own trigger, never the LTP, on a place and on a modify.
//! Firstock's quote has no tick size, so the master contract's prices the
//! limit and no quote is read for an SL-M (the requests are pinned in
//! `tests/it/brokers_noren.rs`). Shoonya's and Flattrade's half is in
//! `families::noren::slm_tests`.

use super::mapping::{modify_body, place_body};
use super::master_contract::parse_file;
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::error::AppError;
use serde_json::Value;

const LTP: Option<f64> = Some(1000.0);
const OPTION: &str = "NIFTY28NOV2526000CE";
const NO_TRIGGER: &str = "A stop-loss market (SL-M) order needs a trigger price. Set a trigger price above zero and place the order again.";

/// SBIN on NSE, tick 0.05 in Firstock's master.
fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_file(
        "NSE",
        include_str!("../../../tests/fixtures/brokers/firstock/NSE.csv"),
    ));
    r
}

fn order(side: &str, pricetype: &str, trigger: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: side.into(),
        quantity: 1,
        price: 0.0,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(trigger),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

/// An SL-M on a cheap option (master tick 0.05).
fn option_slm(side: &str, trigger: f64) -> ResolvedOrder {
    let o = order(side, "SL-M", trigger);
    ResolvedOrder {
        symbol: OPTION.into(),
        exchange: Exchange::Nfo,
        instrument: SymToken {
            symbol: OPTION.into(),
            brsymbol: "NIFTY28NOV25C26000".into(),
            exchange: "NFO".into(),
            brexchange: "NFO".into(),
            ..o.instrument.clone()
        },
        ..o
    }
}

fn modify(trigger: f64) -> ResolvedModify {
    let req = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "SELL".into(),
        product: "MIS".into(),
        pricetype: "SL-M".into(),
        quantity: 1,
        price: 0.0,
        trigger_price: trigger,
        disclosed_quantity: 0,
    };
    ResolvedModify::resolve("1", &req, &master()).unwrap()
}

/// `(priceType, price, triggerPrice, mkt_protection)` of a body.
fn priced(b: &Value) -> (&str, &str, &str, &str) {
    let s = |k: &str| b[k].as_str().unwrap_or_default();
    (
        s("priceType"),
        s("price"),
        s("triggerPrice"),
        s("mkt_protection"),
    )
}

fn refused(e: &AppError) -> bool {
    matches!(e, AppError::Validation(m) if m == NO_TRIGGER)
}

/// Web `test_sell_stop_limit_lands_below_the_trigger` (it was 995, off
/// the LTP of 1000) and `test_buy_stop_limit_lands_above_the_trigger`.
#[test]
fn stop_limits_land_past_the_trigger() {
    let b = place_body(&order("SELL", "SL-M", 980.0), LTP).unwrap();
    assert_eq!(priced(&b), ("SL-LMT", "975.1", "980", "0"));
    let b = place_body(&order("BUY", "SL-M", 1020.0), LTP).unwrap();
    assert_eq!(priced(&b), ("SL-LMT", "1025.1", "1020", "0"));
}

/// Web `test_sl_m_without_a_trigger_is_refused` (the web's `""`, `None`
/// and `"abc"` are refused by the request schema before an adapter sees
/// them, and a missing trigger reaches it as 0).
#[test]
fn sl_m_without_a_trigger_is_refused() {
    for trigger in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        let e = place_body(&order("SELL", "SL-M", trigger), LTP).unwrap_err();
        assert!(refused(&e), "{}: {:?}", trigger, e);
    }
}

/// Web `test_limit_is_at_least_one_tick_past_the_trigger` and
/// `test_sell_trigger_one_tick_above_zero_keeps_the_trigger`.
#[test]
fn the_limit_is_a_tick_past_the_trigger_or_the_trigger() {
    let price = |o: &ResolvedOrder| place_body(o, LTP).unwrap()["price"].clone();
    assert_eq!(price(&option_slm("SELL", 0.5)), "0.45");
    assert_eq!(price(&option_slm("BUY", 0.5)), "0.55");
    assert_eq!(price(&option_slm("SELL", 0.05)), "0.05");
}

/// Web `test_no_tick_size_anywhere_uses_the_trigger`.
#[test]
fn no_tick_size_uses_the_trigger() {
    let mut o = order("SELL", "SL-M", 980.0);
    o.instrument.tick_size = 0.0;
    assert_eq!(
        priced(&place_body(&o, LTP).unwrap()),
        ("SL-LMT", "980", "980", "0")
    );
}

/// An SL-M needs no quote: without one it still goes as SL-LMT off its
/// trigger (it went as SL-MKT, left to Firstock's own protection).
#[test]
fn an_sl_m_needs_no_quote() {
    let b = place_body(&order("SELL", "SL-M", 980.0), None).unwrap();
    assert_eq!(priced(&b), ("SL-LMT", "975.1", "980", "0"));
}

/// Web `test_market_orders_still_price_off_the_ltp`.
#[test]
fn market_orders_still_price_off_the_ltp() {
    let b = place_body(&order("BUY", "MARKET", 0.0), LTP).unwrap();
    assert_eq!(priced(&b), ("LMT", "1005", "0", "0"));
}

/// Web `test_firstock_modify_prices_sl_m_off_the_trigger`, with a quote
/// and without one (it went as SL-MKT with `mkt_protection` 1).
#[test]
fn modify_prices_sl_m_off_the_trigger() {
    for ltp in [LTP, None] {
        let b = modify_body(&modify(980.0), ltp).unwrap();
        assert_eq!(priced(&b), ("SL-LMT", "975.1", "980", "0"), "{:?}", ltp);
    }
}

/// Web `test_firstock_modify_refuses_sl_m_without_trigger`.
#[test]
fn modify_refuses_sl_m_without_a_trigger() {
    let e = modify_body(&modify(0.0), LTP).unwrap_err();
    assert!(refused(&e), "{:?}", e);
}
