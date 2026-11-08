//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! Funds: `POST {base}/quick/user/limits` with the literal
//! `{"seg":"ALL","exch":"ALL","prod":"ALL"}`. `CollateralValue` is the cash
//! balance (misnamed by Kotak, verified live by the web); `Collateral` is
//! the pledged-shares margin.
//!
//! Margin: `POST {base}/quick/user/check-margin`, one order per request;
//! several legs are requested one by one and `reqdMrgn` summed.

use super::mapping::{n, order_type, reverse_map_exchange, s};
use super::{kotak_error, KotakBroker, KotakSession};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// The funds body the web posts, byte for byte.
pub const LIMITS_BODY: &str =
    "jData=%7B%22seg%22%3A%22ALL%22%2C%22exch%22%3A%22ALL%22%2C%22prod%22%3A%22ALL%22%7D";

/// web `get_margin_data` mapping.
pub fn funds_from_limits(v: &Value) -> Funds {
    let cash = n(v, "CollateralValue");
    let collateral = n(v, "Collateral");
    let used = n(v, "MarginUsed");
    Funds {
        available_cash: cash,
        used_margin: used,
        total_margin: n(v, "Net"),
        opening_balance: 0.0,
        payin: n(v, "RmsPayInAmt"),
        payout: n(v, "RmsPayOutAmt"),
        span: 0.0,
        exposure: 0.0,
        collateral,
        m2m_unrealized: n(v, "UnrealizedMtomPrsnt"),
        m2m_realized: n(v, "RealizedMtomPrsnt"),
        utilised_debits: used,
    }
}

pub async fn get_funds(b: &KotakBroker, auth: &AuthToken) -> Result<Funds> {
    let sess = KotakSession::parse(auth)?;
    let resp = b
        .http
        .post(format!("{}/quick/user/limits", sess.base_url))
        .header("accept", "application/json")
        .header("Sid", &sess.sid)
        .header("Auth", &sess.token)
        .header("neo-fin-key", "neotradeapi")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(LIMITS_BODY)
        .send()
        .await?;
    let (status, v): (reqwest::StatusCode, Value) =
        crate::brokers::common::http::read_json("kotak", resp).await?;
    if s(&v, "stat") != "Ok" {
        tracing::warn!(status = status.as_u16(), "Kotak limits call refused");
        return Err(kotak_error(
            status,
            &v,
            "Kotak did not return your funds. Try again shortly.",
        ));
    }
    // BR-03: an Ok without the cash figure is not a zero balance.
    if v.get("CollateralValue").is_none_or(Value::is_null) {
        tracing::warn!("Kotak answered the limits without its figures");
        return Err(crate::brokers::common::book_read::unreadable(
            "kotak",
            crate::brokers::common::book_read::Book::Funds,
        ));
    }
    Ok(funds_from_limits(&v))
}

/// One leg's check-margin jData (web `transform_margin_position`); `None`
/// when it does not resolve.
pub fn margin_body(b: &KotakBroker, leg: &MarginLeg) -> Option<Value> {
    let row = b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol)?;
    let seg = reverse_map_exchange(&leg.key.exchange)?;
    Some(json!({
        "brkName": "KOTAK",
        "brnchId": "ONLINE",
        "exSeg": seg,
        "prc": super::mapping::py_float(leg.price),
        "prcTp": order_type(leg.pricetype),
        "prod": leg.product.as_str(),
        "qty": leg.quantity.to_string(),
        "tok": row.token,
        "trnsTp": match leg.action {
            crate::brokers::common::mapping::Action::Buy => "B",
            crate::brokers::common::mapping::Action::Sell => "S",
        },
    }))
}

/// web `parse_margin_response`: `stat == "Ok"` -> `reqdMrgn`.
pub fn parse_margin(v: &Value) -> std::result::Result<f64, String> {
    if !v.is_object() {
        return Err("Invalid response from broker".into());
    }
    if s(v, "stat") != "Ok" {
        // Kotak's error field is `emsg` (web marketcalls/openalgo#2367);
        // `errMsg` is read after it.
        return Err(["emsg", "errMsg"]
            .iter()
            .map(|k| s(v, k))
            .find(|m| !m.is_empty())
            .unwrap_or_else(|| "Failed to calculate margin".into()));
    }
    Ok(n(v, "reqdMrgn"))
}

pub async fn calculate_margin(
    b: &KotakBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let sess = KotakSession::parse(auth)?;
    let bodies: Vec<Value> = legs.iter().filter_map(|l| margin_body(b, l)).collect();
    if bodies.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let single = bodies.len() == 1;
    let mut total = 0.0;
    for body in &bodies {
        let (status, v) = b
            .trading_post(&sess, "/quick/user/check-margin", body)
            .await?;
        match parse_margin(&v) {
            Ok(m) => total += m,
            Err(msg) => {
                tracing::warn!(status = status.as_u16(), "Kotak margin leg failed");
                // One leg: its error is the answer. Several: the web sums
                // the legs that succeeded.
                if single {
                    if let AppError::Auth(m) = kotak_error(status, &v, "") {
                        return Err(AppError::Auth(m));
                    }
                    return Err(AppError::Broker(format!(
                        "Kotak could not calculate the margin: {}",
                        msg
                    )));
                }
            }
        }
    }
    Ok(MarginResult {
        total_margin_required: total,
        span_margin: 0.0,
        exposure_margin: 0.0,
    })
}
