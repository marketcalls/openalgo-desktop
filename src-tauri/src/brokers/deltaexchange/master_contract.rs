//! The product master (web `database/master_contract_db.py`), normalised to
//! the OpenAlgo crypto symbology (`docs/prompt/crypto-symbol-format.md`):
//!
//! | Delta | OpenAlgo |
//! | --- | --- |
//! | perpetual `BTCUSD` | `BTCUSDFUT` (`PERPFUT`) |
//! | dated future `BTCUSD27Nov2026` | `BTC27NOV26FUT` (`FUT`) |
//! | option `C-BTC-62000-271126` | `BTC27NOV2662000CE` (`CE`; turbo/synth too) |
//! | spot `BTC_INR` | `BTCINR` (`SPOT`) |
//! | move, IRS, spread, combo | native symbol |
//!
//! `GET /v2/products?page_size=500&states=live`, cursor-paginated on
//! `meta.after`. Any page error fails the whole download so a truncated
//! list never replaces the stored master.

use super::{DeltaBroker, BREXCHANGE};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::MasterContract;
use crate::error::{AppError, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Safety ceiling on pages (100 x 500 products).
pub const MAX_PAGES: usize = 100;
pub const PAGE_SIZE: &str = "500";

/// Delta `contract_type` -> OpenAlgo `instrumenttype` (web
/// `CONTRACT_TYPE_MAP`); unknown types are uppercased.
pub fn instrument_type(contract_type: &str) -> String {
    match contract_type {
        "perpetual_futures" => "PERPFUT".into(),
        "futures" => "FUT".into(),
        "call_options" => "CE".into(),
        "put_options" => "PE".into(),
        "spot" => "SPOT".into(),
        "move_options" => "MOVE".into(),
        "interest_rate_swaps" => "IRS".into(),
        "spreads" => "SPREAD".into(),
        "options_combos" => "COMBO".into(),
        "turbo_call_options" => "TCE".into(),
        "turbo_put_options" => "TPE".into(),
        "synth_call_options" => "SYNCE".into(),
        "synth_put_options" => "SYNPE".into(),
        "" => "OTHER".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn is_call(t: &str) -> bool {
    matches!(t, "CE" | "TCE" | "SYNCE")
}

fn is_put(t: &str) -> bool {
    matches!(t, "PE" | "TPE" | "SYNPE")
}

/// `settlement_time` (ISO-8601, UTC) -> `DD-MMM-YY`; empty for perpetuals
/// and spot.
pub fn expiry(settlement_time: &str) -> String {
    let s = settlement_time.trim();
    if s.is_empty() {
        return String::new();
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| {
            d.with_timezone(&chrono::Utc)
                .format("%d-%b-%y")
                .to_string()
                .to_ascii_uppercase()
        })
        .unwrap_or_else(|_| s.to_string())
}

/// The OpenAlgo symbol of a Delta product (web `_to_canonical_symbol`).
pub fn canonical_symbol(delta_symbol: &str, instrument_type: &str, expiry: &str) -> String {
    if is_call(instrument_type) || is_put(instrument_type) {
        let parts: Vec<&str> = delta_symbol.split('-').collect();
        if parts.len() == 4 && !expiry.is_empty() {
            let suffix = if is_call(instrument_type) { "CE" } else { "PE" };
            return format!(
                "{}{}{}{}",
                parts[1].to_ascii_uppercase(),
                expiry.replace('-', ""),
                parts[2],
                suffix
            );
        }
        return delta_symbol.to_string();
    }
    if instrument_type == "FUT" && !expiry.is_empty() {
        let compact = expiry.replace('-', "");
        let upper = delta_symbol.to_ascii_uppercase();
        let parts: Vec<&str> = expiry.split('-').collect();
        let base = if parts.len() == 3 {
            let suffix = format!("{}{}20{}", parts[0], parts[1], parts[2]);
            upper.strip_suffix(&suffix).unwrap_or(&upper).to_string()
        } else {
            upper.clone()
        };
        for quote in ["USDT", "USD", "BTC", "ETH"] {
            if base.len() > quote.len() {
                if let Some(underlying) = base.strip_suffix(quote) {
                    return format!("{}{}FUT", underlying, compact);
                }
            }
        }
        return format!("{}{}FUT", base, compact);
    }
    match instrument_type {
        "PERPFUT" => format!("{}FUT", delta_symbol),
        "SPOT" => delta_symbol.replace('_', ""),
        _ => delta_symbol.to_string(),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(x) => x.trim().parse().ok(),
        _ => None,
    }
}

/// Live, operational products -> master rows plus contract multipliers by
/// token (web `process_delta_products`). Duplicate ids keep the first row.
pub fn parse_products(products: &[Value]) -> MasterContract {
    let mut rows = Vec::new();
    let mut contract_values = HashMap::new();
    let mut seen = HashSet::new();
    for p in products {
        if s(p, "state") != "live" || s(p, "trading_status") != "operational" {
            continue;
        }
        let specs = p.get("product_specs").cloned().unwrap_or(Value::Null);
        if specs
            .get("only_reduce_only_orders_allowed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let token = match p.get("id") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(x)) if !x.is_empty() => x.clone(),
            _ => continue,
        };
        if !seen.insert(token.clone()) {
            continue;
        }
        let symbol = s(p, "symbol");
        let itype = instrument_type(s(p, "contract_type"));
        let exp = expiry(s(p, "settlement_time"));
        let mut strike = 0.0;
        if is_call(&itype) || is_put(&itype) {
            strike = num(p.get("strike_price")).unwrap_or(0.0);
            if strike == 0.0 {
                strike = symbol
                    .split('-')
                    .nth(2)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0.0);
            }
        }
        // `min_order_size` is fractional for spot (0.0001 BTC); the shared
        // row carries whole lots, so anything below one unit is lot 1.
        let min_size = num(specs.get("min_order_size")).unwrap_or(1.0);
        let lot_size =
            if min_size >= 1.0 && min_size.fract() == 0.0 && min_size <= f64::from(i32::MAX) {
                min_size as i32
            } else {
                1
            };
        let name = p
            .get("underlying_asset")
            .map(|u| s(u, "symbol"))
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let d = s(p, "description");
                if d.is_empty() { symbol } else { d }.to_string()
            });
        let cv = num(p.get("contract_value"))
            .filter(|v| *v > 0.0)
            .unwrap_or(1.0);
        contract_values.insert(token.clone(), cv);
        rows.push(SymToken {
            symbol: canonical_symbol(symbol, &itype, &exp),
            brsymbol: symbol.to_string(),
            name,
            exchange: "CRYPTO".into(),
            brexchange: BREXCHANGE.into(),
            token,
            expiry: exp,
            strike,
            lot_size,
            instrument_type: itype,
            tick_size: num(p.get("tick_size")).unwrap_or(0.0),
        });
    }
    MasterContract {
        rows,
        contract_values,
    }
}

/// Fetch every live product page, then parse.
pub async fn download(b: &DeltaBroker) -> Result<MasterContract> {
    let mut products: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut complete = false;
    for _ in 0..MAX_PAGES {
        let mut params = vec![
            ("page_size", PAGE_SIZE.to_string()),
            ("states", "live".to_string()),
        ];
        if let Some(a) = &after {
            params.push(("after", a.clone()));
        }
        let env = b.public_envelope("/v2/products", &params).await?;
        let Some(batch) = env.result.as_array() else {
            return Err(AppError::Broker(
                "Delta Exchange sent a product list OpenAlgo could not read. The existing master contract was kept."
                    .into(),
            ));
        };
        if batch.is_empty() {
            complete = true;
            break;
        }
        products.extend(batch.iter().cloned());
        after = env
            .meta
            .get("after")
            .and_then(Value::as_str)
            .filter(|a| !a.is_empty())
            .map(str::to_string);
        if after.is_none() {
            complete = true;
            break;
        }
    }
    if !complete {
        tracing::warn!(
            "Delta Exchange product list did not finish within {} pages",
            MAX_PAGES
        );
        return Err(AppError::Broker(
            "The Delta Exchange product list did not download completely. The existing master contract was kept; try again later."
                .into(),
        ));
    }
    let master = parse_products(&products);
    if master.rows.is_empty() {
        return Err(AppError::Broker(
            "Delta Exchange returned no live instruments. The existing master contract was kept."
                .into(),
        ));
    }
    tracing::info!(
        "Delta Exchange master: {} live instruments from {} products",
        master.rows.len(),
        products.len()
    );
    Ok(master)
}
