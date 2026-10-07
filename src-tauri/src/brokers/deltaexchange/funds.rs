//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping::{self, py_str, DeltaPosition, WalletBalance, EXCHANGE};
use super::DeltaBroker;
use crate::brokers::common::mapping::PriceType;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

fn f(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Funds from wallet balances and position P&L (web `get_margin_data`):
/// `availablecash = sum(balance_inr)`, `collateral =
/// sum(cross_locked_collateral)`, `utiliseddebits = sum(blocked_margin)`,
/// realised / unrealised P&L summed over `/v2/positions/margined`.
pub fn funds_from(balances: &[WalletBalance], positions: &[DeltaPosition]) -> Funds {
    let cash: f64 = balances.iter().map(|a| f(&a.balance_inr)).sum();
    let blocked: f64 = balances.iter().map(|a| f(&a.blocked_margin)).sum();
    let collateral: f64 = balances.iter().map(|a| f(&a.cross_locked_collateral)).sum();
    let realized: f64 = positions.iter().map(|p| f(&p.realized_pnl)).sum();
    let unrealized: f64 = positions.iter().map(|p| f(&p.unrealized_pnl)).sum();
    Funds {
        available_cash: round2(cash),
        used_margin: round2(blocked),
        collateral: round2(collateral),
        m2m_realized: round2(realized),
        m2m_unrealized: round2(unrealized),
        utilised_debits: round2(blocked),
        ..Default::default()
    }
}

pub async fn get_funds(b: &DeltaBroker, auth: &AuthToken) -> Result<Funds> {
    let balances: Vec<WalletBalance> = b
        .signed(auth, Method::GET, "/v2/wallet/balances", &[], None)
        .await?;
    // P&L comes from positions; a failure there leaves it at zero (web).
    let positions: Vec<DeltaPosition> = match b
        .signed(auth, Method::GET, "/v2/positions/margined", &[], None)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Delta Exchange position P&L could not be read: {}",
                e.code()
            );
            Vec::new()
        }
    };
    Ok(funds_from(&balances, &positions))
}

/// A margin call: the path and its query parameters.
pub type MarginCall = (String, Vec<(&'static str, String)>);

/// One leg as `GET /v2/products/{id}/margin_required` parameters (web
/// `transform_margin_positions`). `None` when the symbol is not in the
/// master.
pub fn margin_params(b: &DeltaBroker, leg: &MarginLeg) -> Result<Option<MarginCall>> {
    let Some(row) = b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol) else {
        tracing::warn!(
            "Margin leg {} ({}) is not in the master contract",
            leg.key.symbol,
            leg.key.exchange
        );
        return Ok(None);
    };
    let Ok(product_id) = row.token.trim().parse::<i64>() else {
        return Ok(None);
    };
    let size = mapping::order_size(&CryptoQuantity::whole(leg.quantity), &row)?;
    let mut order_type = match leg.pricetype {
        PriceType::Limit | PriceType::Sl => "limit_order",
        _ => "market_order",
    };
    let mut params = vec![
        ("size", size.to_string()),
        ("side", leg.action.as_str().to_ascii_lowercase()),
    ];
    if order_type == "limit_order" {
        if leg.price > 0.0 {
            params.push(("limit_price", py_str(leg.price)));
        } else {
            // A limit leg without a price is priced as a market leg.
            order_type = "market_order";
        }
    }
    params.push(("order_type", order_type.to_string()));
    Ok(Some((
        format!("/v2/products/{}/margin_required", product_id),
        params,
    )))
}

/// Basket margin: one call per leg, summed (web `calculate_margin_api`).
/// `total = span = initial_margin`, exposure 0.
pub async fn calculate_margin(
    b: &DeltaBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let mut calls = Vec::new();
    for leg in legs {
        if leg.key.exchange != EXCHANGE {
            continue;
        }
        if let Some(c) = margin_params(b, leg)? {
            calls.push(c);
        }
    }
    if calls.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check the symbols are in the master contract."
                .into(),
        ));
    }
    let mut out = MarginResult::default();
    let mut ok = 0;
    for (path, params) in &calls {
        match b
            .signed::<Value>(auth, Method::GET, path, params, None)
            .await
        {
            Ok(r) => {
                let im = match r.get("initial_margin") {
                    Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                    Some(Value::String(s)) => f(s),
                    _ => 0.0,
                };
                out.total_margin_required += im;
                out.span_margin += im;
                ok += 1;
            }
            // A spent quota fails the whole basket: a total that silently
            // leaves out a leg would understate the margin.
            Err(e) if e.client_message() == super::rate_limited().client_message() => {
                return Err(e)
            }
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => tracing::warn!("Delta Exchange margin for {} failed: {}", path, e.code()),
        }
    }
    if ok == 0 {
        return Err(AppError::Broker(
            "Delta Exchange could not calculate the margin for any of these positions.".into(),
        ));
    }
    Ok(out)
}
