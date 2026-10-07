//! Funds (web `api/funds.py`). Margin is not offered by AliceBlue's API
//! (web `margin_api.py`), so `calculate_margin` keeps the trait default.

use super::mapping::{num, s};
use super::orders::POSITIONS;
use super::{broker_error, status_ok, AliceBlueBroker};
use crate::brokers::common::mpp::py_round;
use crate::brokers::types::*;
use crate::error::Result;
use reqwest::Method;
use serde_json::Value;

pub const LIMITS: &str = "/open-api/od/v1/limits/";

/// web `get_margin_data` mapping of `result[0]`.
pub fn funds_from_limits(item: &Value, realized: f64) -> Funds {
    let cash = py_round(num(item.get("tradingLimit")), 2);
    let used = py_round(num(item.get("utilizedMargin")), 2);
    Funds {
        available_cash: cash,
        used_margin: used,
        total_margin: 0.0,
        opening_balance: 0.0,
        payin: 0.0,
        payout: 0.0,
        span: 0.0,
        exposure: 0.0,
        collateral: py_round(num(item.get("collateralMargin")), 2),
        m2m_unrealized: 0.0,
        m2m_realized: py_round(realized, 2),
        utilised_debits: used,
    }
}

/// Sum of `realizedPnl` over the position book (web `_get_realized_pnl`);
/// any failure counts as 0.
pub fn realized_from_positions(v: &Value) -> f64 {
    if !status_ok(v) {
        return 0.0;
    }
    v.get("result")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(|r| num(r.get("realizedPnl"))).sum())
        .unwrap_or(0.0)
}

pub async fn get_funds(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Funds> {
    let (_, v) = b.call(Method::GET, LIMITS, auth, None, false).await?;
    if !status_ok(&v) {
        tracing::warn!("AliceBlue limits refused: {}", s(&v, "message"));
        return Err(broker_error(
            &v,
            "AliceBlue could not return your funds. Try again shortly.",
        ));
    }
    let Some(item) = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
    else {
        tracing::warn!("AliceBlue returned no margin data");
        return Ok(funds_from_limits(&Value::Null, 0.0));
    };
    let realized = match b.call(Method::GET, POSITIONS, auth, None, false).await {
        Ok((_, p)) => realized_from_positions(&p),
        Err(e) => {
            tracing::warn!("AliceBlue positions for realised P&L failed: {}", e.code());
            0.0
        }
    };
    Ok(funds_from_limits(&item, realized))
}
