//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! Funds: `POST {base}/quick/user/limits` with the literal
//! `{"seg":"ALL","exch":"ALL","prod":"ALL"}`. `CollateralValue` is the cash
//! balance (misnamed by Kotak, verified live by the web); `Collateral` is
//! the pledged-shares margin.
//!
//! Margin: `POST {base}/quick/user/check-margin`, one order per request;
//! several legs are requested one by one and each order's own margin
//! (`ordMrgn`) summed, with no hedge benefit between legs (Kotak prices one
//! order at a time, Kotak-Neo/kotak-neo-python#52).

use super::mapping::{n, order_type, reverse_map_exchange, s};
use super::{kotak_error, KotakBroker, KotakSession};
use crate::brokers::common::mpp::py_round;
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

/// web `parse_margin_response`: `stat == "Ok"` -> `ordMrgn`, the margin
/// this order needs. `reqdMrgn` is `ordMrgn` plus `mrgnUsd`, the margin
/// already used on the account (12500 + 40.4 = 12540.4 in Kotak's docs),
/// which counted the account's usage into every leg of a basket (web
/// #2156).
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
    if s(v, "ordMrgn").is_empty() {
        tracing::warn!("Kotak check-margin returned no ordMrgn");
        return Err("Broker did not return the order margin".into());
    }
    Ok(n(v, "ordMrgn"))
}

/// Each leg's check-margin body. A leg that cannot be prepared (not in
/// the master, or on an exchange Kotak does not price) fails the basket,
/// naming it, before any leg is priced: pricing the rest reported the
/// margin of a different basket (web #2156).
fn margin_bodies(b: &KotakBroker, legs: &[MarginLeg]) -> Result<Vec<Value>> {
    let mut bodies = Vec::with_capacity(legs.len());
    let mut missing = Vec::new();
    for l in legs {
        match margin_body(b, l) {
            Some(body) => bodies.push(body),
            None => missing.push(format!("{} ({})", l.key.symbol, l.key.exchange)),
        }
    }
    if !missing.is_empty() {
        tracing::warn!("Kotak margin legs not prepared: {}", missing.join(", "));
        return Err(AppError::Validation(format!(
            "Could not prepare these legs for the margin check: {}. Check the symbol, exchange and expiry date.",
            missing.join(", ")
        )));
    }
    if bodies.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    Ok(bodies)
}

/// The sum of the legs' own margins. One failed leg fails the basket with
/// Kotak's reason, and no further leg is asked for: skipping it reported
/// the margin of a different basket, and a basket whose every leg failed
/// came back as a success of 0 (web #2156).
pub async fn calculate_margin(
    b: &KotakBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let sess = KotakSession::parse(auth)?;
    let bodies = margin_bodies(b, legs)?;
    let mut total = 0.0;
    for body in &bodies {
        let (status, v) = b
            .trading_post(&sess, "/quick/user/check-margin", body)
            .await?;
        match parse_margin(&v) {
            Ok(m) => total += m,
            Err(msg) => {
                tracing::warn!(status = status.as_u16(), "Kotak margin leg failed");
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
    Ok(MarginResult {
        total_margin_required: py_round(total, 2),
        span_margin: 0.0,
        exposure_margin: 0.0,
    })
}
