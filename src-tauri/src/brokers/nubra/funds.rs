//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! * `GET /sentinel/portfolio/user_funds_and_margin` -> `portFundsAndMargin`
//!   (paise).
//! * `POST /sentinel/orders/funds_required` with `{"requestType": "NEW",
//!   "orders": [place items]}`; the answer has `marginInfo.totalMargin`,
//!   `totalFundsRequired` and estimated charges, no span/exposure split.

use super::mapping;
use super::{refused, NubraBroker};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

pub async fn get_funds(b: &NubraBroker, auth: &AuthToken) -> Result<Funds> {
    let v = b
        .get_ok("/sentinel/portfolio/user_funds_and_margin", auth)
        .await?;
    mapping::funds(&v).ok_or_else(|| {
        tracing::warn!("Nubra funds answer had no portFundsAndMargin");
        AppError::Broker("Nubra did not return your funds. Try again shortly.".into())
    })
}

/// The `funds_required` payload; legs without a numeric ref id are skipped
/// (web `transform_margin_positions`). `None` when nothing is left.
pub fn margin_payload(b: &NubraBroker, legs: &[MarginLeg]) -> Option<Value> {
    let tag = mapping::sanitize_strat_tag(Some("openalgo-margin"));
    let items: Vec<Value> = legs
        .iter()
        .filter_map(|l| {
            let token = b.resolver().token(&l.key.symbol, &l.key.exchange);
            let Some(rid) = token.as_deref().and_then(mapping::ref_id) else {
                tracing::warn!(
                    "Margin leg {} ({}) has no Nubra instrument id; skipped",
                    l.key.symbol,
                    l.key.exchange
                );
                return None;
            };
            Some(mapping::order_item(
                rid,
                l.quantity,
                l.action,
                l.product,
                l.pricetype,
                l.price,
                l.trigger_price,
                &tag,
            ))
        })
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(json!({"requestType": "NEW", "orders": items}))
    }
}

pub async fn calculate_margin(
    b: &NubraBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let body = margin_payload(b, legs).ok_or_else(|| {
        AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        )
    })?;
    let (status, v) = b
        .call(
            Method::POST,
            "/sentinel/orders/funds_required",
            auth,
            Some(&body),
        )
        .await?;
    if !(200..300).contains(&status) && v.get("marginInfo").is_none() {
        return Err(refused(&v, status));
    }
    mapping::margin(&v).map_err(AppError::Broker)
}
