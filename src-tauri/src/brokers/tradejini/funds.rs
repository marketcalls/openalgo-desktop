//! Funds (web `api/funds.py`): `GET /api/oms/limits`. Margin calculation is
//! not offered by Tradejini (web `margin_api.py` raises), so the trait
//! default (unsupported) applies.

use super::{mapping, Body, TradejiniBroker};
use crate::brokers::types::{AuthToken, Funds};
use crate::error::{AppError, Result};
use reqwest::Method;

pub async fn get_funds(b: &TradejiniBroker, auth: &AuthToken) -> Result<Funds> {
    let v = b
        .call(Method::GET, "/api/oms/limits", &[], auth, Body::None)
        .await?;
    v.get("d").and_then(mapping::funds).ok_or_else(|| {
        AppError::Broker("Tradejini returned no fund limits. Try again shortly.".into())
    })
}
