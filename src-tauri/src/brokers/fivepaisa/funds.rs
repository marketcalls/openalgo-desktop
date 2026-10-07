//! Funds (web `api/funds.py`). Margin calculation is not offered by the
//! 5paisa adapter on the web (`margin_api.py` raises), so the trait default
//! (`Unsupported`) stands.

use super::mapping::{body_rows, num};
use super::orders::positions_raw;
use super::{session, FivepaisaBroker};
use crate::brokers::common::mpp::py_round;
use crate::brokers::types::{AuthToken, Funds};
use crate::error::{AppError, Result};
use serde_json::{json, Value};

pub const MARGIN: &str = "/VendorsAPI/Service1.svc/V4/Margin";

/// `EquityMargin[0]` plus the summed `MTOM` / `BookedPL` of the position
/// rows -> OpenAlgo funds.
pub fn to_funds(equity: &Value, positions: &[Value]) -> Funds {
    let mtom: f64 = positions.iter().map(|p| num(p, "MTOM")).sum();
    let booked: f64 = positions.iter().map(|p| num(p, "BookedPL")).sum();
    let used = py_round(num(equity, "MarginUtilized"), 2);
    Funds {
        available_cash: py_round(num(equity, "NetAvailableMargin"), 2),
        collateral: py_round(num(equity, "TotalCollateralValue"), 2),
        m2m_unrealized: py_round(mtom, 2),
        m2m_realized: py_round(booked, 2),
        utilised_debits: used,
        used_margin: used,
        ..Default::default()
    }
}

pub async fn get_funds(b: &FivepaisaBroker, auth: &AuthToken) -> Result<Funds> {
    let s = session(auth)?;
    let v = b
        .post(MARGIN, json!({"ClientCode": s.client_code}), &s)
        .await?;
    let Some(equity) = body_rows(&v, "EquityMargin").into_iter().next() else {
        tracing::warn!(
            broker = "fivepaisa",
            "5paisa margin returned no EquityMargin entry: {}",
            super::message(&v)
        );
        return Err(AppError::Broker(
            "5paisa returned no margin details for this account. Try again shortly.".into(),
        ));
    };
    // The web reads positions through a helper that turns any failure into
    // an empty book, so a positions outage leaves M2M at zero.
    let positions = match positions_raw(b, &s).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                broker = "fivepaisa",
                "Positions for M2M failed: {}",
                e.code()
            );
            Vec::new()
        }
    };
    Ok(to_funds(&equity, &positions))
}
