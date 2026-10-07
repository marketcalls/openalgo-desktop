//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::data::fetch_ltp;
use super::mapping::{self, funds_from_view, to_ltp_exchange};
use super::{broker_error, message_of, HdfcSkyBroker};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;

/// `GET /oapi/v1/funds/view?type=all` (the V2 endpoint is not deployed).
pub async fn get_funds(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Funds> {
    let (status, v) = b
        .send(
            Method::GET,
            "/oapi/v1/funds/view",
            auth,
            &[("type", "all".to_string())],
            true,
            None,
        )
        .await?;
    if status.is_client_error()
        || status.is_server_error()
        || v.get("status").and_then(Value::as_str) == Some("error")
        || v.get("error").is_some_and(|e| !e.is_null())
    {
        let msg = message_of(&v);
        tracing::warn!(status = status.as_u16(), "HDFC Sky funds refused: {}", msg);
        return Err(broker_error(&msg));
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    if data
        .get("values")
        .is_some_and(|x| !x.is_array() && !x.is_null())
    {
        return Err(AppError::Broker(
            "HDFC Sky sent funds in a form OpenAlgo could not read. Try again shortly.".into(),
        ));
    }
    Ok(funds_from_view(&data))
}

/// Cash or index rows a derivative's underlying may be listed on.
fn underlying_exchanges(exchange: &str) -> &'static [&'static str] {
    match exchange {
        "NFO" => &["NSE_INDEX", "NSE"],
        "BFO" => &["BSE_INDEX", "BSE"],
        _ => &[],
    }
}

/// Spot price of each derivative leg's underlying, one fetch-ltp batch.
async fn underlying_prices(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    rows: &[SymToken],
) -> HashMap<usize, f64> {
    let mut wanted: HashMap<usize, (String, String)> = HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        if row.instrument_type == "EQ" || row.name.is_empty() {
            continue;
        }
        for ex in underlying_exchanges(&row.exchange) {
            if let Some(m) = b.resolver().by_symbol(ex, &row.name) {
                wanted.insert(
                    idx,
                    (to_ltp_exchange(&m.exchange).to_string(), m.token.clone()),
                );
                break;
            }
        }
    }
    if wanted.is_empty() {
        return HashMap::new();
    }
    let mut instruments: Vec<(String, String)> = wanted.values().cloned().collect();
    instruments.sort();
    instruments.dedup();
    let mut quotes = HashMap::new();
    for batch in instruments.chunks(data_batch()) {
        match fetch_ltp(b, auth, batch).await {
            Ok(q) => quotes.extend(q),
            Err(e) => {
                tracing::debug!("Underlying prices unavailable for margin: {}", e.code());
                return HashMap::new();
            }
        }
    }
    wanted
        .into_iter()
        .map(|(idx, key)| (idx, quotes.get(&key).map(|q| q.0).unwrap_or(0.0)))
        .collect()
}

fn data_batch() -> usize {
    super::data::LTP_BATCH
}

/// `POST /oapi/v1/margin {"data": [legs]}`: one endpoint for single and
/// basket requests; the netted `combined_margin` carries hedge benefit.
pub async fn calculate_margin(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let mut resolved: Vec<(&MarginLeg, SymToken)> = Vec::new();
    for leg in legs {
        match b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol) {
            Some(row) => resolved.push((leg, row)),
            None => tracing::warn!(
                "Margin: no instrument for {}:{}",
                leg.key.exchange,
                leg.key.symbol
            ),
        }
    }
    if resolved.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let rows: Vec<SymToken> = resolved.iter().map(|(_, r)| r.clone()).collect();
    let spots = underlying_prices(b, auth, &rows).await;
    let body_legs: Vec<Value> = resolved
        .iter()
        .enumerate()
        .map(|(idx, (leg, row))| {
            mapping::margin_leg(leg, row, spots.get(&idx).copied().unwrap_or(0.0))
        })
        .collect();
    let (status, v) = b
        .send(
            Method::POST,
            "/oapi/v1/margin",
            auth,
            &[],
            false,
            Some(&json!({ "data": body_legs })),
        )
        .await?;
    let error_code = v
        .get("error")
        .and_then(|e| e.get("code"))
        .is_some_and(|c| !c.is_null() && c != &json!(0));
    if v.get("status").and_then(Value::as_str) == Some("error") || error_code {
        let msg = message_of(&v);
        tracing::warn!(status = status.as_u16(), "HDFC Sky margin refused: {}", msg);
        return Err(broker_error(if msg.is_empty() {
            "Failed to calculate margin"
        } else {
            &msg
        }));
    }
    if !status.is_success() {
        return Err(broker_error(&message_of(&v)));
    }
    Ok(mapping::parse_margin(
        v.get("result").unwrap_or(&Value::Null),
    ))
}
