//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! Funds read pooled `/limits` plus `/limits/equity` and `/limits/fno`. The
//! pooled answer wins when it carries a non-zero value (segment endpoints
//! can mirror the pool, so summing them would double count); otherwise the
//! segments are summed.

use super::mapping::{self, num};
use super::IiflCapitalBroker;
use crate::brokers::common::mapping::Action;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

const LIMIT_KEYS: &[&str] = &[
    "tradingLimit",
    "openingCashLimit",
    "intradayPayin",
    "collateralMargin",
    "utilizedMargin",
    "creditForSell",
    "adhocMargin",
    "utilizedSpanMargin",
    "utilizedExposureMargin",
];

/// web `funds._extract_result`.
pub fn extract_result(payload: &Value) -> Value {
    let r = payload.get("result").unwrap_or(payload);
    match r {
        Value::Object(_) => r.clone(),
        Value::Array(list) => list
            .first()
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn val_present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !(s.is_empty() || s == "-"),
        Some(_) => true,
    }
}

fn has_limits(v: &Value) -> bool {
    v.is_object() && LIMIT_KEYS.iter().any(|k| val_present(v.get(*k)))
}

fn has_nonzero(v: &Value) -> bool {
    v.is_object() && LIMIT_KEYS.iter().any(|k| num(v.get(*k)).abs() > 0.0)
}

fn sum_field(rows: &[&Value], field: &str, fallback: Option<&str>) -> f64 {
    rows.iter()
        .map(|r| {
            let v = r.get(field);
            if !val_present(v) {
                if let Some(f) = fallback {
                    return num(r.get(f));
                }
            }
            num(v)
        })
        .sum()
}

fn format_funds(l: &Value) -> Funds {
    let available = match l.get("tradingLimit") {
        Some(v) => num(Some(v)),
        None => num(l.get("openingCashLimit")),
    };
    let utilized = num(l.get("utilizedMargin"));
    let collateral = num(l.get("collateralMargin"));
    Funds {
        available_cash: mapping::round2(available),
        used_margin: mapping::round2(utilized),
        total_margin: mapping::round2(available + utilized),
        opening_balance: mapping::round2(num(l.get("openingCashLimit"))),
        payin: mapping::round2(num(l.get("intradayPayin"))),
        payout: 0.0,
        span: mapping::round2(num(l.get("utilizedSpanMargin"))),
        exposure: mapping::round2(num(l.get("utilizedExposureMargin"))),
        collateral: mapping::round2(collateral),
        m2m_unrealized: 0.0,
        m2m_realized: 0.0,
        utilised_debits: mapping::round2(utilized),
    }
}

/// Choose between pooled and segment limits (web `get_margin_data`).
pub fn combine(pooled: &Value, equity: &Value, fno: &Value) -> Option<Funds> {
    if has_nonzero(pooled) {
        return Some(format_funds(pooled));
    }
    let has_pooled = has_limits(pooled);
    let any_segment = has_limits(equity) || has_limits(fno);
    let nonzero_segment = has_nonzero(equity) || has_nonzero(fno);
    if any_segment && (!has_pooled || nonzero_segment) {
        let seg = [equity, fno];
        let combined = json!({
            "tradingLimit": sum_field(&seg, "tradingLimit", Some("openingCashLimit")),
            "openingCashLimit": sum_field(&seg, "openingCashLimit", None),
            "collateralMargin": sum_field(&seg, "collateralMargin", None),
            "utilizedMargin": sum_field(&seg, "utilizedMargin", None),
            "creditForSell": sum_field(&seg, "creditForSell", None),
            "adhocMargin": sum_field(&seg, "adhocMargin", None),
            "utilizedSpanMargin": sum_field(&seg, "utilizedSpanMargin", None),
            "utilizedExposureMargin": sum_field(&seg, "utilizedExposureMargin", None),
            "intradayPayin": sum_field(&seg, "intradayPayin", None),
        });
        return Some(format_funds(&combined));
    }
    if has_pooled {
        return Some(format_funds(pooled));
    }
    None
}

/// One limits endpoint; any failure is an empty answer, like the web.
async fn limits(b: &IiflCapitalBroker, auth: &AuthToken, path: &str) -> Result<Value> {
    match b.call(Method::GET, path, auth, None, true).await {
        Ok((StatusCode::OK, v)) => Ok(extract_result(&v)),
        Ok((status, _)) => {
            tracing::warn!(
                status = status.as_u16(),
                "IIFL Capital limits {} failed",
                path
            );
            Ok(Value::Null)
        }
        Err(e @ AppError::Auth(_)) => Err(e),
        Err(e) => {
            tracing::warn!("IIFL Capital limits {} failed: {}", path, e.code());
            Ok(Value::Null)
        }
    }
}

pub async fn get_funds(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<Funds> {
    let pooled = limits(b, auth, "/limits").await?;
    let equity = limits(b, auth, "/limits/equity").await?;
    let fno = limits(b, auth, "/limits/fno").await?;
    combine(&pooled, &equity, &fno).ok_or_else(|| {
        AppError::Broker("IIFL Capital did not return your funds. Try again shortly.".into())
    })
}

/// `POST /spanexposure` body (web `transform_margin_positions`); legs whose
/// instrument is unknown are skipped.
pub fn margin_body(b: &IiflCapitalBroker, legs: &[MarginLeg]) -> Vec<Value> {
    let mut out = Vec::new();
    for leg in legs {
        let Some(row) = b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol) else {
            tracing::warn!(
                "Margin leg skipped: {} is not in the master contract on {}",
                leg.key.symbol,
                leg.key.exchange
            );
            continue;
        };
        if leg.quantity <= 0 {
            continue;
        }
        out.push(json!({
            "instrumentId": row.token,
            "exchange": mapping::to_segment(&leg.key.exchange),
            "transactionType": match leg.action { Action::Buy => "BUY", Action::Sell => "SELL" },
            "quantity": leg.quantity,
        }));
    }
    out
}

/// web `parse_margin_response`.
pub fn parse_margin(v: &Value) -> Result<MarginResult> {
    if !v.is_object() {
        return Err(AppError::Broker(
            "IIFL Capital sent an invalid margin answer.".into(),
        ));
    }
    if v.get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("error"))
    {
        return Err(AppError::Broker(
            mapping::message_of(v).unwrap_or_else(|| "Failed to calculate margin".into()),
        ));
    }
    let r = v.get("result").unwrap_or(v);
    if !r.is_object() {
        return Err(AppError::Broker(
            "IIFL Capital sent an invalid margin answer.".into(),
        ));
    }
    let span = num(r.get("span"));
    let exposure = num(r.get("exposureMargin"));
    let total = if val_present(r.get("totalMargin")) {
        num(r.get("totalMargin"))
    } else {
        span + exposure
    };
    Ok(MarginResult {
        total_margin_required: total,
        span_margin: span,
        exposure_margin: exposure,
    })
}

pub async fn calculate_margin(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let body = margin_body(b, legs);
    if body.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let (_, v) = b
        .call(
            Method::POST,
            "/spanexposure",
            auth,
            Some(&Value::Array(body)),
            true,
        )
        .await?;
    parse_margin(&v)
}
