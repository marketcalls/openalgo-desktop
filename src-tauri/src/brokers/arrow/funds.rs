//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::{arrow_error, message_of, ArrowBroker, Category};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

fn f(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `/user/limits` `data` -> funds (`funds.py:46-68`): available cash is
/// `allocated - utilized`, collateral the sum of `nonCashCurrent` over the
/// segment allocations, debits the utilised margin.
pub fn funds_from_limits(data: &Value) -> Funds {
    let margin = data.get("margin").cloned().unwrap_or(Value::Null);
    let allocated = f(margin.get("allocated"));
    let utilized = f(margin.get("utilized"));
    let collateral: f64 = data
        .get("allocations")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| f(x.get("nonCashCurrent"))).sum())
        .unwrap_or(0.0);
    Funds {
        available_cash: round2(allocated - utilized),
        used_margin: round2(utilized),
        total_margin: round2(allocated),
        collateral: round2(collateral),
        m2m_unrealized: round2(f(margin.get("unrealizedPnl"))),
        m2m_realized: round2(f(margin.get("realizedPnl"))),
        utilised_debits: round2(utilized),
        ..Default::default()
    }
}

pub async fn get_funds(b: &ArrowBroker, auth: &AuthToken) -> Result<Funds> {
    let data = b
        .call(Method::GET, "/user/limits", auth, None, Category::Other)
        .await?;
    Ok(funds_from_limits(&data))
}

/// OpenAlgo margin legs -> `/margin/order` bodies (`margin_data.py:27-57`):
/// broker trading symbol, product C/I/M, order MKT or LMT (the only two the
/// margin endpoint documents). Legs that do not resolve are skipped.
pub fn margin_bodies(legs: &[MarginLeg], symbols: &SymbolResolver) -> Vec<Value> {
    let mut skipped = Vec::new();
    let out: Vec<Value> = legs
        .iter()
        .filter_map(|l| {
            let Some(br) = symbols.br_symbol(&l.key.symbol, &l.key.exchange) else {
                skipped.push(format!("{} ({})", l.key.symbol, l.key.exchange));
                return None;
            };
            Some(json!({
                "exchange": l.key.exchange,
                "symbol": br,
                "quantity": l.quantity.to_string(),
                "product": match l.product {
                    Product::Cnc => "C",
                    Product::Mis => "I",
                    Product::Nrml => "M",
                },
                "price": super::orders::num(l.price),
                "transactionType": if l.action == Action::Buy { "B" } else { "S" },
                "order": if l.pricetype == PriceType::Market { "MKT" } else { "LMT" },
            }))
        })
        .collect();
    if !skipped.is_empty() {
        tracing::warn!(
            "Skipped {} margin position(s): {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    out
}

/// Sum of per-order `requiredMargin` (`parse_margin_response`); Arrow does
/// not split SPAN / exposure.
pub fn parse_order_margins(data: &[Value]) -> MarginResult {
    MarginResult {
        total_margin_required: round2(data.iter().map(|d| f(d.get("requiredMargin"))).sum()),
        span_margin: 0.0,
        exposure_margin: 0.0,
    }
}

/// Basket `final_margin` (after cross-leg benefit, `parse_basket_margin_response`).
pub fn parse_basket_margin(data: &Value) -> MarginResult {
    MarginResult {
        total_margin_required: round2(f(data.get("final_margin"))),
        span_margin: 0.0,
        exposure_margin: 0.0,
    }
}

/// Total transaction charges reported with a margin answer (web
/// `total_charges`; the shared `MarginResult` has no field for it yet).
pub fn total_charges(orders: &[Value]) -> f64 {
    round2(
        orders
            .iter()
            .map(|o| f(o.get("charge").and_then(|c| c.get("total"))))
            .sum(),
    )
}

async fn post(b: &ArrowBroker, auth: &AuthToken, path: &str, body: &Value) -> Result<Value> {
    let url = format!("{}{}", b.urls().rest, path);
    let (status, v) = b
        .call_raw(Method::POST, &url, auth, Some(body), Category::Other)
        .await?;
    if v.get("status").and_then(Value::as_str) != Some("success") {
        let msg = message_of(&v);
        tracing::warn!(status = status.as_u16(), "Arrow margin refused: {}", msg);
        return Err(if msg.is_empty() {
            AppError::Broker("Arrow could not calculate the margin for this basket.".into())
        } else {
            arrow_error(&msg)
        });
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

async fn order_margin_sum(
    b: &ArrowBroker,
    auth: &AuthToken,
    bodies: &[Value],
) -> Result<MarginResult> {
    let mut data = Vec::with_capacity(bodies.len());
    for body in bodies {
        data.push(post(b, auth, "/margin/order", body).await?);
    }
    Ok(parse_order_margins(&data))
}

/// One leg: `/margin/order`. Several: `/margin/basket` with
/// `includePositions: true`, falling back to the per-order sum when the
/// basket endpoint refuses (`margin_api.py:120-208`).
pub async fn calculate_margin(
    b: &ArrowBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let bodies = margin_bodies(legs, b.resolver());
    if bodies.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    if bodies.len() == 1 {
        let d = post(b, auth, "/margin/order", &bodies[0]).await?;
        return Ok(parse_order_margins(std::slice::from_ref(&d)));
    }
    let basket = json!({"orders": bodies, "includePositions": true});
    match post(b, auth, "/margin/basket", &basket).await {
        Ok(d) => {
            let orders = d.get("orders").and_then(Value::as_array);
            tracing::debug!(
                "Arrow basket charges: {}",
                orders.map(|o| total_charges(o)).unwrap_or(0.0)
            );
            Ok(parse_basket_margin(&d))
        }
        Err(AppError::Auth(m)) => Err(AppError::Auth(m)),
        Err(e) => {
            tracing::warn!(
                "Arrow basket margin failed ({}); using the per-order sum",
                e.code()
            );
            order_margin_sum(b, auth, &bodies).await
        }
    }
}
