//! Funds and span margin (web `api/funds.py`, `api/margin_api.py`).

use super::mapping;
use super::{is_success, samco_error, SamcoBroker};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::json;

pub async fn get_funds(b: &SamcoBroker, auth: &AuthToken) -> Result<Funds> {
    let (status, v) = b.send(Method::GET, "/limit/getLimits", auth, None).await?;
    if !is_success(&v) {
        return Err(samco_error(status, &v, "Samco could not load your funds."));
    }
    Ok(mapping::funds(&v))
}

pub async fn calculate_margin(
    b: &SamcoBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let request: Vec<_> = legs
        .iter()
        .filter_map(|l| {
            let br = b.resolver().br_symbol(&l.key.symbol, &l.key.exchange);
            let v = mapping::margin_leg(l, br.as_deref());
            if v.is_none() {
                tracing::warn!(
                    "Margin leg {}:{} skipped (not a derivative or unknown symbol)",
                    l.key.exchange,
                    l.key.symbol
                );
            }
            v
        })
        .collect();
    if request.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let (status, v) = b
        .send(
            Method::POST,
            "/spanMargin",
            auth,
            Some(&json!({ "request": request })),
        )
        .await?;
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(samco_error(status, &v, ""));
    }
    mapping::parse_margin(&v).map_err(|m| AppError::Broker(format!("Samco: {}", m)))
}
