//! Funds (web `api/funds.py`). Paytm Money has no margin calculator
//! (`api/margin_api.py` reports it unsupported), so the trait default stays.

use super::mapping::PaytmPosition;
use super::orders::raw_positions;
use super::PaytmBroker;
use crate::brokers::common::de::f64_lenient;
use crate::brokers::types::*;
use crate::error::Result;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;

pub const FUNDS: &str = "/accounts/v1/funds/summary?config=true";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FundsSummary {
    #[serde(deserialize_with = "f64_lenient")]
    pub available_cash: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub collaterals: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub utilised_amount: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub opening_balance: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub funds_added: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub funds_withdrawn: f64,
}

/// `data.funds_summary` of the funds answer.
pub fn summary(data: &Value) -> FundsSummary {
    data.get("funds_summary")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Realised and unrealised P&L summed over the position book.
pub fn m2m(positions: &[PaytmPosition]) -> (f64, f64) {
    positions.iter().fold((0.0, 0.0), |(r, u), p| {
        (r + p.realised_profit, u + p.unrealised_profit)
    })
}

fn r2(v: f64) -> f64 {
    crate::brokers::common::streaming::round2(v)
}

/// web `get_margin_data` output, as `Funds`.
pub fn funds_from(s: &FundsSummary, realised: f64, unrealised: f64) -> Funds {
    Funds {
        available_cash: r2(s.available_cash),
        used_margin: r2(s.utilised_amount),
        total_margin: r2(s.available_cash + s.utilised_amount),
        opening_balance: r2(s.opening_balance),
        payin: r2(s.funds_added),
        payout: r2(s.funds_withdrawn),
        span: 0.0,
        exposure: 0.0,
        collateral: r2(s.collaterals),
        m2m_unrealized: r2(unrealised),
        m2m_realized: r2(realised),
        utilised_debits: r2(s.utilised_amount),
    }
}

pub async fn get_funds(b: &PaytmBroker, auth: &AuthToken) -> Result<Funds> {
    let env = b.call(Method::GET, FUNDS, auth, None).await?;
    let s = summary(&env.data);
    // P&L is best effort, as on the web: an unreadable position book
    // leaves it at zero.
    let (r, u) = match raw_positions(b, auth).await {
        Ok(p) => m2m(&p),
        Err(e) => {
            tracing::warn!("Paytm Money position P&L for funds failed: {}", e.code());
            (0.0, 0.0)
        }
    };
    Ok(funds_from(&s, r, u))
}
