//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! * `GET /funds`: available cash is the equity delivery limit
//!   (`detailed_avl_balance.eq_cnc`, then `eq_mis`, `eq_mtf`), else
//!   `withdrawal_balance`; collateral `pledge_received`; utilised
//!   `max(0, sod_balance - available)`.
//! * `GET /margin` with a JSON body, one leg per request, summed.

use super::mapping::{self, num, num_value};
use super::{token, IndmoneyBroker};
use crate::brokers::common::mpp::py_round;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

/// The funds `data` object as OpenAlgo funds.
pub fn funds_from(data: &Value) -> Funds {
    let withdrawal = num(data, "withdrawal_balance");
    let available = data
        .get("detailed_avl_balance")
        .and_then(Value::as_object)
        .and_then(|d| {
            ["eq_cnc", "eq_mis", "eq_mtf"]
                .iter()
                .find_map(|k| d.get(*k).filter(|v| !v.is_null()))
                .map(|v| num_value(Some(v)))
        })
        .unwrap_or(withdrawal);
    let sod = num(data, "sod_balance");
    let used = (sod - available).max(0.0);
    let r = |v: f64| py_round(v, 2);
    Funds {
        available_cash: r(available),
        used_margin: r(used),
        total_margin: r(sod),
        opening_balance: r(sod),
        collateral: r(num(data, "pledge_received")),
        m2m_unrealized: r(num(data, "unrealized_pnl")),
        m2m_realized: r(num(data, "realized_pnl")),
        utilised_debits: r(used),
        ..Default::default()
    }
}

pub async fn get_funds(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Funds> {
    let r = b
        .send(Method::GET, "/funds", &[], None, token(auth)?)
        .await?;
    if r.status == 403 && {
        let t = r.text.to_ascii_lowercase();
        t.contains("cloudflare") || t.contains("just a moment")
    } {
        tracing::warn!("INDmoney /funds answered with a Cloudflare challenge");
        return Err(AppError::Broker(
            "INDmoney blocked the funds request from this network. Try again later, or check your connection."
                .into(),
        ));
    }
    let data = super::unwrap_account("/funds", &r)?;
    if r.json.get("status").and_then(Value::as_str) != Some("success")
        || !data.is_object()
        || data.as_object().map(|o| o.is_empty()).unwrap_or(true)
    {
        return Err(AppError::Broker(
            "INDmoney did not return your funds. Try again shortly.".into(),
        ));
    }
    Ok(funds_from(&data))
}

/// One margin leg body (web `transform_margin_positions`), `None` when the
/// token is not numeric.
pub fn margin_leg(leg: &MarginLeg, token: &str) -> Option<Value> {
    let t = token.trim();
    if t.is_empty()
        || !t
            .replace(['.', '-'], "")
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let ex = leg.key.exchange.as_str();
    Some(json!({
        "segment": mapping::segment(ex),
        "txnType": leg.action.as_str(),
        "quantity": leg.quantity.to_string(),
        "price": python_str(leg.price),
        "product": mapping::product(leg.product),
        "securityID": t,
        "exchange": mapping::api_exchange(ex),
    }))
}

/// `str(float)` the way Python prints a price (`0` -> `"0"` when the web
/// passed the default int, `101.5` -> `"101.5"`).
fn python_str(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// `(total, span, exposure)` of one margin answer (web
/// `parse_margin_response`); `None` for an error envelope.
pub fn parse_margin(v: &Value) -> Option<(f64, f64, f64)> {
    if !v.is_object() || v.get("status").and_then(Value::as_str) == Some("error") {
        return None;
    }
    let d = v.get("data").unwrap_or(&Value::Null);
    Some((
        num(d, "total_margin"),
        num(d, "span_margin"),
        num(d, "exposure_margin"),
    ))
}

pub async fn calculate_margin(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let tok = token(auth)?;
    let bodies: Vec<Value> = legs
        .iter()
        .filter_map(|l| {
            let t = b.resolver().token(&l.key.symbol, &l.key.exchange);
            let body = t.as_deref().and_then(|t| margin_leg(l, t));
            if body.is_none() {
                tracing::warn!(
                    "No INDmoney security id for {} ({}); leg skipped",
                    l.key.symbol,
                    l.key.exchange
                );
            }
            body
        })
        .collect();
    if bodies.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let mut out = MarginResult::default();
    let mut ok = 0;
    let mut failed = Vec::new();
    for body in &bodies {
        let id = body["securityID"].as_str().unwrap_or("").to_string();
        let r = b.send(Method::GET, "/margin", &[], Some(body), tok).await?;
        if r.status == 401 || r.status == 403 {
            return Err(super::session_expired());
        }
        match parse_margin(&r.json) {
            Some((t, s, e)) if !r.json.is_null() => {
                out.total_margin_required += t;
                out.span_margin += s;
                out.exposure_margin += e;
                ok += 1;
            }
            _ => failed.push(id),
        }
    }
    if ok == 0 {
        return Err(AppError::Broker(format!(
            "Failed to calculate margin for all positions. Failed: {}",
            failed.join(", ")
        )));
    }
    Ok(out)
}
