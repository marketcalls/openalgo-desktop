//! Funds (web `api/funds.py`) and the rmoney margin calculator
//! (`api/margin_api.py`, `mapping/margin_data.py`).

use super::mapping;
use super::XtsBroker;
use crate::brokers::common::mapping::Exchange;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

pub(crate) async fn get_funds(b: &XtsBroker, auth: &AuthToken) -> Result<Funds> {
    let v = b
        .interactive(Method::GET, "/user/balance", auth, None)
        .await?;
    mapping::funds(v.get("result").unwrap_or(&Value::Null), &b.cfg.hooks).ok_or_else(|| {
        AppError::Broker(format!(
            "{} returned no fund details. Try again shortly.",
            b.cfg.name
        ))
    })
}

pub(crate) async fn calculate_margin(
    b: &XtsBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let mut portfolio = Vec::new();
    let mut skipped = Vec::new();
    for leg in legs {
        let entry = leg.key.exchange.parse::<Exchange>().ok().and_then(|ex| {
            let row = b.resolver().by_symbol(ex.as_str(), &leg.key.symbol)?;
            mapping::margin_leg(leg, ex, &row.token)
        });
        match entry {
            Some(e) => portfolio.push(e),
            None => skipped.push(format!("{} ({})", leg.key.symbol, leg.key.exchange)),
        }
    }
    if !skipped.is_empty() {
        tracing::warn!(broker = b.cfg.id, "Margin skipped {} leg(s)", skipped.len());
    }
    if portfolio.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let v = b
        .interactive(
            Method::POST,
            "/orders/margindetails",
            auth,
            Some(&json!({"portfolio": portfolio})),
        )
        .await?;
    mapping::margin_result(v.get("result").unwrap_or(&Value::Null))
        .ok_or_else(|| AppError::Broker("No margin details in response".into()))
}
