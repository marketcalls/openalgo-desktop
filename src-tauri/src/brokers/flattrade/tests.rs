//! Flattrade (PiConnect) cases the web pins in its own tests, run through
//! the generic Noren adapter with Flattrade's config.

use super::config;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::mapping::map_trade;
use crate::brokers::families::noren::NorenConfig;
use crate::brokers::types::Trade;
use crate::brokers::{shoonya, tradesmart, zebu};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Trade book: one row per fill (web test/test_flattrade_issue_2464.py)
// ---------------------------------------------------------------------------

/// The reporter's SELL 120 of SENSEX19NOV2682300PE (web #2464).
const ORDER: &str = "26111700329676";
const SYMBOL: &str = "SENSEX19NOV2682300PE";

/// A PiConnect TradeBook row as the API sends it, every number a string
/// (web `fill`): `qty` and `avgprc` are the order's, `flqty` and `flprc`
/// the fill's own, left out when `None`.
fn fill(
    order: &str,
    trantype: &str,
    qty: i64,
    flqty: Option<&str>,
    flprc: Option<&str>,
    avgprc: &str,
) -> Value {
    let mut row = json!({
        "stat": "Ok",
        "norenordno": order,
        "exch": "BFO",
        "tsym": SYMBOL,
        "prd": "NRML",
        "trantype": trantype,
        "qty": qty.to_string(),
        "fillshares": qty.to_string(),
        "avgprc": avgprc,
        "norentm": "13:09:10 17-11-2026",
    });
    if let Some(q) = flqty {
        row["flqty"] = json!(q);
    }
    if let Some(p) = flprc {
        row["flprc"] = json!(p);
    }
    row
}

/// The SELL 120 order, filled 60 + 60. Each row carries qty 120 and the
/// order's running average at that fill (316.05, then 316.02), as the
/// reporter's trade book showed (web `SPLIT_SELL`).
fn split_sell() -> Vec<Value> {
    vec![
        fill(ORDER, "SELL", 120, Some("60"), Some("316.05"), "316.05"),
        fill(ORDER, "SELL", 120, Some("60"), Some("315.99"), "316.02"),
    ]
}

fn book(cfg: &'static NorenConfig, rows: &[Value]) -> Vec<Trade> {
    let symbols = SymbolResolver::new();
    rows.iter().map(|t| map_trade(cfg, t, &symbols)).collect()
}

fn trades(rows: &[Value]) -> Vec<Trade> {
    book(config(), rows)
}

/// `pytest.approx`: equal within a relative 1e-6.
fn approx(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-6 * expected.abs()
}

fn sum(trades: &[Trade], side: Option<&str>, of: impl Fn(&Trade) -> f64) -> f64 {
    trades
        .iter()
        .filter(|t| side.is_none_or(|s| t.side == s))
        .map(of)
        .sum()
}

/// Web `test_flattrade_issue_2464.py::test_a_part_filled_order_is_counted_once`.
#[test]
fn a_part_filled_order_is_counted_once() {
    let t = trades(&split_sell());
    let quantities: Vec<i32> = t.iter().map(|x| x.quantity).collect();
    assert_eq!(quantities, [60, 60]);
    assert_eq!(sum(&t, None, |x| f64::from(x.quantity)), 120.0);
    let prices: Vec<f64> = t.iter().map(|x| x.average_price).collect();
    assert_eq!(prices, [316.05, 315.99]);
    // Value of the whole order: 120 at the order's average of 316.02.
    let value = sum(&t, None, |x| x.trade_value);
    assert!(approx(value, 120.0 * 316.02), "{value}");
    assert!(t.iter().all(|x| x.order_id == ORDER));
}

/// Web `test_flattrade_issue_2464.py::test_the_reporters_strike_balances_on_a_flat_position`:
/// every SENSEX19NOV2682300PE fill of the reporter's day, 140 bought and
/// 140 sold, realised 579.40.
#[test]
fn the_reporters_strike_balances_on_a_flat_position() {
    let one = |order: &str, side: &str, qty: i64, price: &str| {
        let q = qty.to_string();
        fill(order, side, qty, Some(q.as_str()), Some(price), price)
    };
    let mut day = vec![
        one("26111700089790", "BUY", 20, "365.00"),
        one("26111700096590", "SELL", 20, "375.15"),
        one("26111700105305", "BUY", 20, "376.40"),
        one("26111700122313", "BUY", 20, "317.90"),
        one("26111700157141", "BUY", 20, "322.45"),
        one("26111700167056", "BUY", 40, "290.90"),
        one("26111700183899", "BUY", 20, "278.75"),
    ];
    day.extend(split_sell());
    let t = trades(&day);
    let bought = sum(&t, Some("BUY"), |x| f64::from(x.quantity));
    let sold = sum(&t, Some("SELL"), |x| f64::from(x.quantity));
    assert_eq!((bought, sold), (140.0, 140.0));
    let buy_value = sum(&t, Some("BUY"), |x| x.trade_value);
    let sell_value = sum(&t, Some("SELL"), |x| x.trade_value);
    assert!(
        approx(sell_value - buy_value, 579.40),
        "{}",
        sell_value - buy_value
    );
}

/// Web `test_flattrade_issue_2464.py::test_a_single_fill_order_is_unchanged`.
#[test]
fn a_single_fill_order_is_unchanged() {
    let t = trades(&[fill("1", "BUY", 20, Some("20"), Some("562.30"), "562.30")]);
    assert_eq!(
        t,
        [Trade {
            order_id: "1".into(),
            trade_id: "1".into(),
            symbol: SYMBOL.into(),
            exchange: "BFO".into(),
            product: "NRML".into(),
            side: "BUY".into(),
            quantity: 20,
            average_price: 562.3,
            trade_value: 11246.0,
            timestamp: "13:09:10 17-11-2026".into(),
            order_tag: None,
        }]
    );
}

/// Web `test_flattrade_issue_2464.py::test_a_row_without_fill_fields_falls_back_to_the_order_fields`.
#[test]
fn a_row_without_fill_fields_falls_back_to_the_order_fields() {
    let t = trades(&[fill("1", "BUY", 20, None, None, "562.30")]);
    assert_eq!((t[0].quantity, t[0].average_price), (20, 562.3));
}

/// Web `test_flattrade_issue_2464.py::test_an_unreadable_number_maps_to_zero_instead_of_failing_the_book`.
#[test]
fn an_unreadable_number_maps_to_zero_instead_of_failing_the_book() {
    let t = trades(&[fill("1", "BUY", 20, Some(""), Some(""), "562.30")]);
    assert_eq!(
        (t[0].quantity, t[0].average_price, t[0].trade_value),
        (0, 0.0, 0.0)
    );
}

/// The web fixed Flattrade only (#2480): its Shoonya, Zebu and TradeSmart
/// plugins still read the order's `qty` and `avgprc` from the same rows,
/// and the shared Noren mapping keeps that for them.
#[test]
fn the_other_noren_members_keep_reading_the_order_fields() {
    for cfg in [shoonya::config(), zebu::config(), tradesmart::config()] {
        let t = book(cfg, &split_sell());
        let quantities: Vec<i32> = t.iter().map(|x| x.quantity).collect();
        assert_eq!(quantities, [120, 120], "{}", cfg.id);
        let prices: Vec<f64> = t.iter().map(|x| x.average_price).collect();
        assert_eq!(prices, [316.05, 316.02], "{}", cfg.id);
    }
}
