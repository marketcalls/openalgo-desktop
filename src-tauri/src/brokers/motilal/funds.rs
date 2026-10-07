//! Funds from the margin detail report (web `api/funds.py`).
//!
//! The report is a list of `{srno, particulars, amount}` rows; `srno` picks
//! the figures and repeats per segment, so repeated rows are summed.

use super::mapping::vf;
use super::{paths, MotilalBroker, MotilalSession};
use crate::brokers::types::{AuthToken, Funds};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

const NET_AVAILABLE_CASH_SEG: i64 = 102;
const CASH_BALANCE: i64 = 201;
const NON_CASH_BALANCE: i64 = 220;
const MARGIN_USAGE_TOTAL: i64 = 300;
const MTM_TOTAL: i64 = 400;
const TOTAL_PL_MTM: i64 = 600;
const MARGIN_USAGE_SEGMENTS: &[i64] = &[301, 321, 340, 360, 380, 381];
const UNREALISED: &[i64] = &[402, 422, 442, 462, 482];
const REALISED: &[i64] = &[403, 423, 443, 463, 483];

fn srno(row: &Value) -> Option<i64> {
    match row.get("srno")? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn first(rows: &[Value], n: i64) -> Option<f64> {
    rows.iter()
        .find(|r| srno(r) == Some(n))
        .map(|r| vf(r, "amount"))
}

fn sum(rows: &[Value], set: &[i64]) -> (f64, usize) {
    rows.iter()
        .filter(|r| srno(r).is_some_and(|s| set.contains(&s)))
        .fold((0.0, 0), |(t, c), r| (t + vf(r, "amount"), c + 1))
}

fn two(v: f64) -> f64 {
    // web formats every figure as "%.2f".
    format!("{:.2}", v).parse().unwrap_or(0.0)
}

/// web `get_margin_data` on the report rows.
pub fn funds_from_rows(rows: &[Value]) -> Funds {
    let rows: Vec<Value> = rows.iter().filter(|r| r.is_object()).cloned().collect();
    let collateral = first(&rows, NON_CASH_BALANCE).unwrap_or(0.0);
    let utilised = first(&rows, MARGIN_USAGE_TOTAL).unwrap_or_else(|| {
        let (t, n) = sum(&rows, MARGIN_USAGE_SEGMENTS);
        if n == 0 {
            0.0
        } else {
            t
        }
    });
    let available = match first(&rows, CASH_BALANCE) {
        Some(c) => c,
        None => match first(&rows, NET_AVAILABLE_CASH_SEG) {
            None => 0.0,
            Some(net) => {
                let mtm = first(&rows, TOTAL_PL_MTM)
                    .or_else(|| first(&rows, MTM_TOTAL))
                    .unwrap_or(0.0);
                net - collateral + utilised - mtm
            }
        },
    };
    let (mut unrealised, nu) = sum(&rows, UNREALISED);
    let (realised, nr) = sum(&rows, REALISED);
    if nu == 0 && nr == 0 {
        if let Some(c) = first(&rows, TOTAL_PL_MTM).or_else(|| first(&rows, MTM_TOTAL)) {
            unrealised = c;
        }
    }
    Funds {
        available_cash: two(available),
        collateral: two(collateral),
        m2m_realized: two(realised),
        m2m_unrealized: two(unrealised),
        utilised_debits: two(utilised),
        used_margin: two(utilised),
        ..Default::default()
    }
}

pub async fn get_funds(b: &MotilalBroker, auth: &AuthToken) -> Result<Funds> {
    let s = MotilalSession::parse(auth)?;
    let v = b.post(&s, paths::MARGIN_DETAIL, Some(&json!({}))).await?;
    match v.get("data").and_then(Value::as_array) {
        Some(rows) if !rows.is_empty() => Ok(funds_from_rows(rows)),
        // web returns {} so an empty answer is not mistaken for a funded
        // account.
        _ => Err(AppError::Broker(
            "Motilal Oswal did not return your margin details. Try again shortly.".into(),
        )),
    }
}
