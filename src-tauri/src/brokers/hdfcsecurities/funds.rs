//! Funds (web `api/funds.py`): `GET /oapi/v1/user/margins`.
//!
//! `data.equity.total_available_limit` -> available cash,
//! `total_utilised_limit` -> utilised debits,
//! `totalLimitDetails.pledge_limit` -> collateral. The payload has no P&L,
//! so realised and unrealised M2M are summed from the position book (marked
//! to `/fetch-ltp`); a failure there degrades to zero.

use super::mapping::f;
use super::orders::priced_positions;
use super::HdfcSecuritiesBroker;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

/// `(realised, unrealised)` from raw position rows (web
/// `_mtm_from_positions`).
pub fn mtm(rows: &[Value]) -> (f64, f64) {
    let mut realized = 0.0;
    let mut unrealized = 0.0;
    for r in rows {
        realized += f(r, "realised_pl_overall_position");
        let net = f(r, "net_qty");
        let ltp = f(r, "ltp");
        if net == 0.0 || ltp == 0.0 {
            continue;
        }
        let cost = if net > 0.0 {
            f(r, "average_buy_price")
        } else {
            f(r, "average_sell_price")
        };
        unrealized += (ltp - cost) * net;
    }
    (realized, unrealized)
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Funds from the margins `data` object and the M2M figures.
pub fn funds_from(data: &Value, m2m: (f64, f64)) -> Result<Funds> {
    let equity = data
        .get("equity")
        .filter(|e| e.is_object())
        .ok_or_else(|| {
            AppError::Broker(
                "HDFC Securities returned funds in an unexpected format. Try again shortly.".into(),
            )
        })?;
    let limits = equity
        .get("totalLimitDetails")
        .cloned()
        .unwrap_or(Value::Null);
    let available = f(equity, "total_available_limit");
    let used = f(equity, "total_utilised_limit");
    Ok(Funds {
        available_cash: r2(available),
        used_margin: r2(used),
        total_margin: r2(f(equity, "total_limit")),
        collateral: r2(f(&limits, "pledge_limit")),
        m2m_realized: r2(m2m.0),
        m2m_unrealized: r2(m2m.1),
        utilised_debits: r2(used),
        ..Default::default()
    })
}

pub async fn get_funds(b: &HdfcSecuritiesBroker, auth: &AuthToken) -> Result<Funds> {
    let (status, v) = b
        .request(Method::GET, "/oapi/v1/user/margins", auth, None)
        .await?;
    if status.is_client_error()
        || status.is_server_error()
        || v.get("status").and_then(Value::as_str) == Some("error")
        || v.get("error").is_some_and(|e| !e.is_null())
    {
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Securities funds refused: {}",
            super::error_message(&v)
        );
        return Err(super::broker_error(&v));
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let m2m = match priced_positions(b, auth).await {
        Ok(rows) => mtm(&rows),
        Err(e) => {
            tracing::warn!(
                "HDFC Securities M2M could not be read from positions: {}",
                e.code()
            );
            (0.0, 0.0)
        }
    };
    funds_from(&data, m2m)
}
