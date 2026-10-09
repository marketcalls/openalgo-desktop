//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`, aligned with Groww's API docs in #2194).

use super::mapping::{self, SEGMENT_CASH, SEGMENT_FNO};
use super::orders;
use super::{groww_error, Category, GrowwCore};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

fn num(v: &Value, pointer: &str) -> f64 {
    match v.pointer(pointer) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `/v1/margins/detail/user` payload -> funds (web `get_margin_data`):
/// cash = `clear_cash`, collateral = `collateral_available`, used =
/// `net_margin_used`; the day's M2M comes from the positions
/// (`realised`, `unrealised`), as the margins endpoint carries no P&L.
pub fn funds_from_payload(p: &Value, realised: f64, unrealised: f64) -> Funds {
    let used = num(p, "/net_margin_used");
    let cash = num(p, "/clear_cash");
    let collateral = num(p, "/collateral_available");
    Funds {
        available_cash: cash,
        used_margin: used,
        total_margin: cash + used,
        collateral,
        utilised_debits: used,
        m2m_realized: realised,
        m2m_unrealized: unrealised,
        ..Default::default()
    }
}

/// Realised and unrealised P&L of the day's positions (web `_day_m2m`): a
/// strict read with live prices. What is missing is logged rather than
/// hidden: an FNO segment that was not read, and open positions Groww gave
/// no live price for. (0, 0) when the book cannot be read at all; the cash
/// figures are still worth showing.
pub fn day_m2m(rows: &[Position]) -> (f64, f64) {
    let unpriced: Vec<&str> = rows
        .iter()
        .filter(|p| p.quantity != 0 && p.ltp == 0.0)
        .map(|p| p.symbol.as_str())
        .collect();
    if !unpriced.is_empty() {
        tracing::warn!(
            "Groww funds unrealised P&L excludes open positions with no live price: {:?}",
            unpriced
        );
    }
    (
        rows.iter().map(|p| p.realized_pnl).sum(),
        rows.iter().map(|p| p.unrealized_pnl).sum(),
    )
}

pub async fn get_funds(core: &GrowwCore, auth: &AuthToken) -> Result<Funds> {
    let payload = core
        .call(
            Method::GET,
            "/v1/margins/detail/user",
            auth,
            None,
            Category::NonTrading,
        )
        .await?;
    let (realised, unrealised) = match orders::read_positions(core, auth, true, true).await {
        Ok(read) => {
            if !read.failed.is_empty() {
                tracing::warn!(
                    "Groww funds P&L excludes the {:?} positions, which could not be read",
                    read.failed
                );
            }
            day_m2m(&read.rows)
        }
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => {
            tracing::warn!("Groww positions not read for funds P&L: {}", e.code());
            (0.0, 0.0)
        }
    };
    Ok(funds_from_payload(&payload, realised, unrealised))
}

/// Margin order items grouped by segment (web
/// `transform_margin_positions`): every field 07-margin marks required,
/// plus `price` when given. A position that cannot be sent to Groww
/// refuses the whole request, naming it: margin for part of a basket would
/// understate what the trader needs.
pub fn margin_groups(
    core: &GrowwCore,
    legs: &[MarginLeg],
) -> Result<Vec<(&'static str, Vec<Value>)>> {
    let mut groups: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for leg in legs {
        let (symbol, exchange) = (&leg.key.symbol, &leg.key.exchange);
        let refuse = |why: String| {
            AppError::Validation(format!(
                "Cannot calculate margin for {} on {}: {}",
                symbol, exchange, why
            ))
        };
        let segment = mapping::order_segment(exchange).map_err(|e| refuse(e.client_message()))?;
        let groww_exchange =
            mapping::order_exchange(exchange).map_err(|e| refuse(e.client_message()))?;
        let br = core
            .symbols
            .br_symbol(symbol, exchange)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                refuse(format!(
                    "{} is not in the {} master contract",
                    symbol, exchange
                ))
            })?;
        let mut m = Map::new();
        m.insert("trading_symbol".into(), json!(br));
        m.insert("quantity".into(), json!(leg.quantity));
        m.insert("exchange".into(), json!(groww_exchange));
        m.insert("segment".into(), json!(segment));
        m.insert("product".into(), json!(mapping::product(leg.product)));
        m.insert(
            "order_type".into(),
            json!(mapping::order_type(leg.pricetype)),
        );
        m.insert("transaction_type".into(), json!(leg.action.as_str()));
        if leg.price > 0.0 {
            m.insert("price".into(), json!(leg.price));
        }
        match groups.iter_mut().find(|(s, _)| *s == segment) {
            Some((_, v)) => v.push(Value::Object(m)),
            None => groups.push((segment, vec![Value::Object(m)])),
        }
    }
    Ok(groups)
}

/// The requests to send (web `calculate_margin_api`): every FNO item as one
/// basket, each CASH item on its own (Groww has no CASH basket).
pub fn margin_requests(groups: Vec<(&'static str, Vec<Value>)>) -> Vec<(&'static str, Vec<Value>)> {
    let mut out = Vec::new();
    let mut cash = Vec::new();
    for (seg, items) in groups {
        if seg == SEGMENT_FNO {
            out.push((SEGMENT_FNO, items));
        } else {
            cash.extend(items);
        }
    }
    out.extend(cash.into_iter().map(|i| (SEGMENT_CASH, vec![i])));
    out
}

/// Response payload -> web `/margin` data.
pub fn parse_margin(p: &Value) -> MarginResult {
    MarginResult {
        total_margin_required: num(p, "/total_requirement"),
        span_margin: num(p, "/span_required"),
        exposure_margin: num(p, "/exposure_required"),
    }
}

/// Margin for a basket: the FNO basket plus each CASH order, added. Cash
/// and F&O margins are not offset, so the sum is what the basket needs; a
/// refused request fails the whole answer rather than understate it.
pub async fn calculate_margin(
    core: &GrowwCore,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let groups = margin_groups(core, legs)?;
    if groups.is_empty() {
        return Err(AppError::Validation(
            "No positions to calculate margin for.".into(),
        ));
    }
    let mut total = MarginResult::default();
    for (segment, items) in margin_requests(groups) {
        let body = Value::Array(items);
        let r = core
            .send(
                Method::POST,
                &format!("/v1/margins/detail/orders?segment={}", segment),
                auth,
                Some(&body),
                Category::NonTrading,
            )
            .await?;
        if !r.is_success() {
            tracing::warn!(
                "Groww margin refused for {}: {}",
                segment,
                r.error_message()
            );
            let m = r.error_message();
            return Err(if m.is_empty() {
                groww_error(&r)
            } else {
                AppError::Broker(m)
            });
        }
        let m = parse_margin(r.payload());
        total.total_margin_required += m.total_margin_required;
        total.span_margin += m.span_margin;
        total.exposure_margin += m.exposure_margin;
    }
    Ok(total)
}
