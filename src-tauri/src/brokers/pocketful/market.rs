//! Where an exchange's market data is asked for, and the last traded price
//! (web `api/data.py` `_ROUTES` and `get_ltp`). Quotes read the same route;
//! order pricing reads the last price to send MARKET as a protected LIMIT.

use super::client::Request;
use super::PocketfulBroker;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use serde_json::Value;

/// OpenAlgo exchange -> (exchange in Pocketful's path, segment). The
/// segment table of chart-data.md and the SDK's `FutOpt` routes agree on
/// these; indices are quoted on their cash exchange (the docs' own LTP
/// example asks for an index token on `NSE/Capital`).
pub fn route(exchange: &str) -> Option<(&'static str, &'static str)> {
    Some(match exchange {
        "NSE" | "NSE_INDEX" => ("NSE", "Capital"),
        "BSE" | "BSE_INDEX" => ("BSE", "Capital"),
        "NFO" => ("NFO", "FutOpt"),
        "BFO" => ("BFO", "FutOpt"),
        "MCX" => ("MCX", "FutOpt"),
        "CDS" => ("CDS", "Currency"),
        _ => return None,
    })
}

/// `GET /api/v1/marketdata/{exchange}/{segment}` for an OpenAlgo exchange
/// (get-market-data.md), or `None` for one Pocketful does not quote.
pub fn path(exchange: &str) -> Option<String> {
    route(exchange).map(|(ex, segment)| format!("/api/v1/marketdata/{}/{}", ex, segment))
}

/// A finite number from a number or numeric text, else 0 (web `_num`: a
/// boolean is not a price).
pub fn number(v: &Value) -> f64 {
    let n = match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };
    if n.is_finite() {
        n
    } else {
        0.0
    }
}

/// The last traded price from one request (`&key=last_trade_price`,
/// get-last-traded-price.md), or `None` when there is none. Never fails:
/// an exchange Pocketful does not quote, a refusal, a transport failure or
/// a refused turn log their cause (web `get_ltp` answers 0.0).
pub async fn last_price(b: &PocketfulBroker, auth: &AuthToken, row: &SymToken) -> Option<f64> {
    let token = row.token.trim();
    let Some(path) = path(&row.exchange).filter(|_| !token.is_empty()) else {
        tracing::warn!(
            "No Pocketful market data route for {} {}; no last price",
            row.exchange,
            row.symbol
        );
        return None;
    };
    let req = Request::get(path)
        .query("token", token)
        .query("key", "last_trade_price");
    match b.send_data(auth, req).await {
        Ok(a) if a.ok() => {
            let price = a.payload().map(number).unwrap_or(0.0);
            (price > 0.0).then_some(price)
        }
        Ok(a) => {
            tracing::warn!(
                status = a.status.as_u16(),
                "Pocketful gave no last price for {} {}: {}",
                row.exchange,
                row.symbol,
                a.message()
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "Could not read the Pocketful last price of {} {}: {}",
                row.exchange,
                row.symbol,
                e.code()
            );
            None
        }
    }
}
