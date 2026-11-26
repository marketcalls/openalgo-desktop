//! Charge settings, a fill's charges and the transaction cost report: the
//! part of the web's `sandbox/charge_manager.py` that touches `sandbox.db`
//! (marketcalls/openalgo#2496). The arithmetic lives in
//! [`super::charges`], which performs no I/O.
//!
//! | File | What |
//! |---|---|
//! | `mod.rs` | the plan a fill is charged under, the fill's charge and its debit, one order's charges for `/api/v1/charges` |
//! | `settings.rs` | the settings page: read, save, reset, the calculator's estimate |
//! | `report.rs` | the transaction cost report and its CSV |
//! | `api.rs` | the [`Sandbox`](super::Sandbox) calls the routes use |

mod api;
mod report;
mod settings;

pub use api::OrderCharges;
pub use report::ReportRange;

use super::charges::{
    brokerage_flat_key, brokerage_percent_key, classify_segment, compute_charges, is_enabled,
    paisa, split_exchange_charges, to_decimal, Charges, Plan, Segment, CHARGES_ENABLED_KEY,
};
use super::config::SandboxConfig;
use super::db::OrderRow;
use super::funds;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The stored values of `keys` (missing keys are absent), in one read, so a
/// segment's brokerage and the switch never come from two sides of a save.
pub(crate) fn stored_configs(
    conn: &Connection,
    keys: &[String],
) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare_cached("SELECT config_key, config_value FROM sandbox_config")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (k, v) = row?;
        if keys.contains(&k) {
            out.insert(k, v);
        }
    }
    Ok(out)
}

/// The switch and one segment's brokerage (web `brokerage_plan`). A value
/// that is missing or unreadable falls back to the segment's default, so a
/// damaged setting never makes trades silently free.
pub(crate) fn read_plan(conn: &Connection, segment: &Segment) -> rusqlite::Result<Plan> {
    let flat_key = brokerage_flat_key(segment.key);
    let percent_key = brokerage_percent_key(segment.key);
    let keys = [
        CHARGES_ENABLED_KEY.to_string(),
        flat_key.clone(),
        percent_key.clone(),
    ];
    let stored = stored_configs(conn, &keys)?;
    let get = |k: &str| stored.get(k).map(String::as_str);
    Ok(Plan {
        enabled: is_enabled(Some(get(CHARGES_ENABLED_KEY).unwrap_or("true"))),
        flat: to_decimal(get(&flat_key), segment.default_flat),
        percent: to_decimal(get(&percent_key), segment.default_percent),
    })
}

/// Charge a fill inside its own transaction (web `_execute_order`): after
/// the claim, so an order two evaluators reach is charged once, and with
/// the trade, so a fill that rolls back takes its charge with it. Returns
/// what was taken, for the trade row.
pub(crate) fn charge_fill(
    conn: &Connection,
    cfg: &SandboxConfig,
    order: &OrderRow,
    price: Decimal,
    contract_value: Decimal,
    now: &str,
) -> rusqlite::Result<Charges> {
    let charges = charges_for_fill(conn, order, price, contract_value);
    debit_fill(conn, cfg, order, charges, now)
}

/// What a fill of `order` at `price` is charged under the saved plan (web
/// `charges_for_fill`): nothing with the switch off or on an exchange the
/// rate card has no segment for. Never fails: an order whose charges cannot
/// be worked out still fills, uncharged, as on the web (a trader must not
/// lose a fill to a fault in the fee calculation).
fn charges_for_fill(
    conn: &Connection,
    order: &OrderRow,
    price: Decimal,
    contract_value: Decimal,
) -> Charges {
    let Some(segment) = classify_segment(&order.exchange, order.product.as_str(), &order.symbol)
    else {
        return Charges::default();
    };
    let plan = match read_plan(conn, segment) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(
                "Could not read the brokerage plan for sandbox order {}; filling it uncharged: {}",
                order.orderid,
                e
            );
            return Charges::default();
        }
    };
    if !plan.enabled {
        return Charges::default();
    }
    let value = funds::notional(order.quantity, price, contract_value);
    compute_charges(
        Some(segment),
        &order.exchange,
        order.action.as_str(),
        value,
        plan.flat,
        plan.percent,
    )
}

/// Take a fill's charges out of the balance inside the fill's transaction
/// (web `FundManager.stage_charges`), so the claim, the trade and the debit
/// land together or not at all. Returns what was actually taken: a debit
/// the funds row refuses leaves the trade uncharged, so a trade never
/// records a charge the balance did not pay.
fn debit_fill(
    conn: &Connection,
    cfg: &SandboxConfig,
    order: &OrderRow,
    charges: Charges,
    now: &str,
) -> rusqlite::Result<Charges> {
    let total = charges.total();
    if total <= Decimal::ZERO {
        return Ok(charges);
    }
    let debited = funds::apply(conn, &order.user_id, cfg.starting_capital, now, |f| {
        funds::charge(f, total)
    })?;
    match debited {
        Ok(_) => Ok(charges),
        Err(refusal) => {
            tracing::warn!(
                "Sandbox order {} filled without charges: {}. The {} it would have paid was not taken from the balance.",
                order.orderid,
                refusal.0,
                total
            );
            Ok(Charges::default())
        }
    }
}

/// One order's charges as `/api/v1/charges` lays them out (web
/// `order_charges`): the segment, the trade value, and the charges as
/// total, brokerage, taxes and other charges. An estimate of what the trade
/// costs, so it does not look at the switch: with it off a sandbox fill pays
/// nothing, and the same order in a live account still pays this. `value` is
/// the order's trade value, already read through
/// [`trade_value`](super::charges::trade_value). `None` when orders cannot
/// be placed on the exchange.
pub(crate) fn order_charges(
    conn: &Connection,
    order: &OrderCharges,
    value: Decimal,
) -> rusqlite::Result<Option<Value>> {
    let Some(segment) = classify_segment(&order.exchange, &order.product, &order.symbol) else {
        return Ok(None);
    };
    let plan = read_plan(conn, segment)?;
    let c = compute_charges(
        Some(segment),
        &order.exchange,
        &order.action,
        value,
        plan.flat,
        plan.percent,
    );
    let (transaction, ipft) =
        split_exchange_charges(Some(segment), &order.exchange, value, c.exchange_charges);
    let f = super::types::float;
    Ok(Some(json!({
        "segment": segment.label,
        "trade_value": f(paisa(value)),
        "charges": {
            "total": f(c.total()),
            "brokerage": f(c.brokerage),
            "taxes": {
                "gst": f(c.gst),
                "stt": f(c.stt),
                "stamp_duty": f(c.stamp_duty),
            },
            "other_charges": {
                "transaction": f(transaction),
                "clearing": 0.0,
                "ipft": f(ipft),
                "sebi_turnover": f(c.sebi_charges),
            },
        },
    })))
}
