//! Funds (web `api/funds.py`) and the span calculator (web
//! `api/margin_api.py`, `mapping/margin_data.py`).

use super::{broker_error, num, text, DefinedgeBroker, DefinedgeSession};
use crate::brokers::common::mapping::Action;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

const UNREALIZED_SEGMENTS: &[&str] = &[
    "currentUnrealizedMTOMDerivativeIntraday",
    "currentUnrealizedMTOMDerivativeMargin",
    "currentUnrealizedMTOMEquityIntraday",
    "currentUnrealizedMTOMEquityMargin",
    "currentUnrealizedMTOMCommodityIntraday",
    "currentUnrealizedMTOMCommodityMargin",
];

const REALIZED_SEGMENTS: &[&str] = &[
    "currentRealizedPNLDerivativeIntraday",
    "currentRealizedPNLDerivativeMargin",
    "currentRealizedPNLEquityIntraday",
    "currentRealizedPNLEquityMargin",
    "currentRealizedPNLCommodityIntraday",
    "currentRealizedPNLCommodityMargin",
];

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// `/limits` -> funds. Success is `status` SUCCESS or "200" (the live API)
/// or a `cash` field. The headline MTOM / realized fields fall back to the
/// sum of the segment fields when they are 0. `None` for a failure body.
pub fn funds_from_limits(v: &Value) -> Option<Funds> {
    let status = text(v, "status");
    if !(status == "SUCCESS" || status == "200" || v.get("cash").is_some()) {
        return None;
    }
    let mut unrealized = num(v, "currentUnrealizedMtom");
    if unrealized == 0.0 {
        unrealized = UNREALIZED_SEGMENTS.iter().map(|k| num(v, k)).sum();
    }
    let mut realized = num(v, "currentRealizedPNL");
    if realized == 0.0 {
        realized = REALIZED_SEGMENTS.iter().map(|k| num(v, k)).sum();
    }
    let used = r2(num(v, "marginUsed"));
    Some(Funds {
        available_cash: r2(num(v, "cash")),
        used_margin: used,
        collateral: r2(num(v, "brokerCollateralAmount")),
        m2m_unrealized: r2(unrealized),
        m2m_realized: r2(realized),
        utilised_debits: used,
        ..Default::default()
    })
}

pub async fn get_funds(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Funds> {
    let s = DefinedgeSession::parse(auth)?;
    let v = b.trade_json(&s, Method::GET, "/limits", None).await?;
    funds_from_limits(&v).ok_or_else(|| {
        broker_error(
            &v,
            "Definedge could not return your funds right now. Try again shortly.",
        )
    })
}

/// `DD-MMM-YY` -> `DD-MMM-YYYY` (years below 50 are 20xx), as the span
/// calculator wants it.
pub fn full_year_expiry(expiry: &str) -> String {
    let b = expiry.as_bytes();
    if b.len() == 9 && b[7].is_ascii_digit() && b[8].is_ascii_digit() {
        let yy: u32 = expiry[7..9].parse().unwrap_or(0);
        let century = if yy < 50 { "20" } else { "19" };
        format!("{}{}{}", &expiry[..7], century, &expiry[7..9])
    } else {
        expiry.to_string()
    }
}

/// web `transform_margin_positions`. Legs whose symbol is not in the master
/// are skipped. Note the web quirk kept here: any non-zero strike (equities
/// carry 1.0) is sent as `option_strike`.
pub fn margin_positions(legs: &[MarginLeg], symbols: &SymbolResolver) -> Vec<Value> {
    let mut out = Vec::new();
    for leg in legs {
        let Some(row) = symbols.by_symbol(&leg.key.exchange, &leg.key.symbol) else {
            tracing::warn!(
                "Margin leg {} on {} is not in the master contract",
                leg.key.symbol,
                leg.key.exchange
            );
            continue;
        };
        let mut m = Map::new();
        m.insert(
            "product_type".into(),
            json!(super::mapping::product_code(leg.product)),
        );
        m.insert("exchange".into(), json!(leg.key.exchange));
        m.insert("symbol_name".into(), json!(row.name));
        m.insert("tradingsymbol".into(), json!(row.br_symbol()));
        let (buy, sell) = match leg.action {
            Action::Buy => (leg.quantity, 0),
            Action::Sell => (0, leg.quantity),
        };
        m.insert("open_buy_qty".into(), json!(buy));
        m.insert("open_sell_qty".into(), json!(sell));
        if !row.expiry.is_empty() {
            m.insert("expiry".into(), json!(full_year_expiry(&row.expiry)));
        }
        if row.strike != 0.0 {
            m.insert(
                "option_strike".into(),
                json!((row.strike.trunc() as i64).to_string()),
            );
        }
        if matches!(row.instrument_type.as_str(), "CE" | "PE") {
            m.insert("option_type".into(), json!(row.instrument_type));
        }
        out.push(Value::Object(m));
    }
    out
}

/// web `parse_margin_response`: total = span + exposure.
pub fn parse_margin(v: &Value) -> Result<MarginResult> {
    if text(v, "status") != "SUCCESS" {
        return Err(broker_error(
            v,
            "Definedge could not calculate the margin for this basket.",
        ));
    }
    let span = num(v, "span");
    let exposure = num(v, "exposure");
    Ok(MarginResult {
        total_margin_required: span + exposure,
        span_margin: span,
        exposure_margin: exposure,
    })
}

pub async fn calculate_margin(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let s = DefinedgeSession::parse(auth)?;
    let positions = margin_positions(legs, b.resolver());
    if positions.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let body = json!({ "positions": positions });
    let v = b
        .trade_json(&s, Method::POST, "/spancalculator", Some(&body))
        .await?;
    parse_margin(&v)
}
