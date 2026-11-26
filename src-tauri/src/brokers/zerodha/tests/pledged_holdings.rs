//! Web `test/test_zerodha_pledged_holdings.py` (openalgo#2503, `8128336df`):
//! Zerodha holdings count pledged and T1 shares.
//!
//! Kite reports a pledged holding with `quantity: 0` and the shares in
//! `collateral_quantity`, while its `pnl` covers every share. The rows below
//! are the web's, shaped on a live account: RELIANCE 200 and WAAREEENER 45,
//! both fully pledged, showed quantity 0 and holding value 0 against a P&L
//! of -25,396.
//!
//! The desktop works the portfolio totals out from the mapped rows
//! (`HoldingsBook::statistics`, Zerodha reports none of its own), so the
//! totals are checked on what `/api/v1/holdings` sends: the rows through
//! `holding_row` and the totals through `statistics_json`.

use super::*;
use crate::brokers::common::mpp::py_round;
use crate::services::account_service::{holding_row, statistics_json};
use serde_json::json;

fn round2(v: f64) -> f64 {
    py_round(v, 2)
}

/// Web `PLEDGED`.
fn pledged() -> Vec<KiteHolding> {
    serde_json::from_value(json!([
        {
            "tradingsymbol": "RELIANCE",
            "exchange": "BSE",
            "product": "CNC",
            "quantity": 0,
            "t1_quantity": 0,
            "collateral_quantity": 200,
            "collateral_type": "margin",
            "average_price": 1297.0,
            "last_price": 1380.35,
            "pnl": 16670.0
        },
        {
            "tradingsymbol": "WAAREEENER",
            "exchange": "BSE",
            "product": "CNC",
            "quantity": 0,
            "t1_quantity": 0,
            "collateral_quantity": 45,
            "collateral_type": "margin",
            "average_price": 3336.0,
            "last_price": 2401.2,
            "pnl": -42066.0
        }
    ]))
    .unwrap()
}

/// The holdings book as `account_service::holdings` reads it from Zerodha:
/// the mapped rows, totals worked out from them.
fn book(rows: Vec<KiteHolding>) -> HoldingsBook {
    HoldingsBook {
        holdings: map_holdings(rows, &master()),
        totals: None,
    }
}

/// Web test/test_zerodha_pledged_holdings.py::test_fully_pledged_holding_keeps_its_quantity.
#[test]
fn fully_pledged_holding_keeps_its_quantity() {
    let rows = book(pledged()).holdings;
    let quantities: Vec<i32> = rows.iter().map(|h| h.quantity).collect();
    assert_eq!(quantities, [200, 45]);
    // The row `/api/v1/holdings` sends carries the same count.
    assert_eq!(holding_row(&rows[0])["quantity"], json!(200));
    assert_eq!(holding_row(&rows[1])["quantity"], json!(45));
    assert_eq!(rows[0].current_value, 1380.35 * 200.0);
}

/// Web test/test_zerodha_pledged_holdings.py::test_pledged_holding_value_matches_kite_pnl.
#[test]
fn pledged_holding_value_matches_kite_pnl() {
    let stats = book(pledged()).statistics();
    assert_eq!(
        round2(stats.totalholdingvalue),
        round2(1380.35 * 200.0 + 2401.2 * 45.0)
    );
    assert_eq!(
        round2(stats.totalinvvalue),
        round2(1297.0 * 200.0 + 3336.0 * 45.0)
    );
    assert_eq!(
        round2(stats.totalholdingvalue - stats.totalinvvalue),
        -25396.0
    );
    assert_ne!(stats.totalpnlpercentage, 0.0);
    // On the wire: Kite's own P&L over the investment it was made on.
    let wire = statistics_json(&stats);
    assert_eq!(wire["totalholdingvalue"], json!(384124.0));
    assert_eq!(wire["totalinvvalue"], json!(409520.0));
    assert_eq!(wire["totalprofitandloss"], json!(-25396.0));
    assert_eq!(wire["totalpnlpercentage"], json!(-6.2));
}

/// Web test/test_zerodha_pledged_holdings.py::test_partly_pledged_and_t1_shares_add_up.
#[test]
fn partly_pledged_and_t1_shares_add_up() {
    let rows: Vec<KiteHolding> = serde_json::from_value(json!([{
        "tradingsymbol": "INFY",
        "exchange": "NSE",
        "product": "CNC",
        "quantity": 60,
        "t1_quantity": 5,
        "collateral_quantity": 40,
        "average_price": 1500.0,
        "last_price": 1600.0,
        "pnl": 10500.0
    }]))
    .unwrap();
    let out = book(rows).holdings;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].quantity, 105);
}

/// Web test/test_zerodha_pledged_holdings.py::test_rows_without_collateral_fields_are_unchanged.
#[test]
fn rows_without_collateral_fields_are_unchanged() {
    let rows: Vec<KiteHolding> = serde_json::from_value(json!([{
        "tradingsymbol": "SBIN",
        "exchange": "NSE",
        "product": "CNC",
        "quantity": 10,
        "average_price": 500.0,
        "last_price": 550.0,
        "pnl": 500.0
    }]))
    .unwrap();
    let b = book(rows);
    assert_eq!(b.holdings.len(), 1);
    assert_eq!(b.holdings[0].quantity, 10);
    assert_eq!(b.statistics().totalholdingvalue, 5500.0);
}
