//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping;
use super::{is_success, refusal, MstockBroker};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::json;

pub const FUNDS_PATH: &str = "/user/fundsummary";
pub const MARGIN_PATH: &str = "/margins/orders";

/// web `get_margin_data`: `GET /user/fundsummary`, `data[0]`.
pub async fn get_funds(b: &MstockBroker, auth: &AuthToken) -> Result<Funds> {
    let v = b.call(Method::GET, FUNDS_PATH, auth, None).await?;
    let first = mapping::rows(&v).into_iter().next();
    match first {
        Some(d) if is_success(&v) => Ok(mapping::funds_from_summary(&d)),
        _ => Err(refusal(
            &v,
            "mStock did not return your fund details. Try again shortly.",
        )),
    }
}

/// web `calculate_margin_api`: legs without a usable token are skipped;
/// none left is a validation error.
pub async fn calculate_margin(
    b: &MstockBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let mut orders = Vec::with_capacity(legs.len());
    for leg in legs {
        match b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol) {
            Some(row) if mapping::valid_margin_token(&row.token) => {
                orders.push(mapping::margin_leg(leg, row.br_symbol(), row.token.trim()))
            }
            _ => tracing::warn!(
                "Margin leg skipped, no usable token for {} on {}",
                leg.key.symbol,
                leg.key.exchange
            ),
        }
    }
    if orders.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let body = json!({ "orders": orders });
    let v = b.call(Method::POST, MARGIN_PATH, auth, Some(&body)).await?;
    mapping::parse_margin(&v).ok_or_else(|| refusal(&v, "Failed to calculate margin"))
}
