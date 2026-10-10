//! One deployment's orders, fills and positions (web
//! `services/openscript_books.py`).
//!
//! Read from the platform's own global book on the side the run traded on,
//! then narrowed to this deployment: for money the destination is the
//! authority, and this deployment's own order rows only decide which of its
//! rows belong here (matched by order id, or by the `strategy` tag where the
//! book carries it). The envelope comes back in the shape it arrived in so
//! the page renders it with the same table; only aggregates are recounted.
//!
//! Positions are weaker and say so: a position row is per contract and may
//! hold size somebody else opened. The profit beside them is this
//! deployment's own, worked out from its own fills, never the account's.

use super::services::RunnerServices;
use super::store::{holdings, OrderRow};
use crate::strategy::dispatch::{Book, RunMode};
use serde_json::{json, Value};
use std::collections::HashSet;

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

const ROW_KEYS: &[&str] = &["orders", "trades", "positions", "positionbook", "data"];

/// The rows a book answered and the key they were under (`None`: a bare list).
fn rows_and_shape(data: &Value, key: &str) -> (Vec<Value>, Option<String>) {
    let rows = |v: &Value| -> Vec<Value> {
        v.as_array()
            .map(|a| a.iter().filter(|r| r.is_object()).cloned().collect())
            .unwrap_or_default()
    };
    if data.is_array() {
        return (rows(data), None);
    }
    if let Some(m) = data.as_object() {
        for k in std::iter::once(key).chain(ROW_KEYS.iter().copied()) {
            if let Some(v) = m.get(k).filter(|v| v.is_array()) {
                return (rows(v), Some(k.to_string()));
            }
        }
    }
    (Vec::new(), None)
}

fn narrowed(payload: &mut Value, data: &Value, kept: Vec<Value>, under: Option<String>) {
    let new_data = match under {
        None => Value::Array(kept),
        Some(k) => {
            let mut m = data.as_object().cloned().unwrap_or_default();
            m.insert(k, Value::Array(kept));
            Value::Object(m)
        }
    };
    if let Some(p) = payload.as_object_mut() {
        p.insert("data".into(), new_data);
        p.entry("status").or_insert(json!("success"));
    }
}

fn as_error(response: Value, fallback: &str) -> Value {
    let m = text(&response["message"]);
    json!({"status": "error", "message": if m.is_empty() { fallback.to_string() } else { m }})
}

fn mine(rows: Vec<Value>, tag: &str, ids: &HashSet<String>) -> Vec<Value> {
    rows.into_iter()
        .filter(|r| {
            let id = text(&r["orderid"]);
            (!id.is_empty() && ids.contains(&id))
                || (!tag.is_empty() && text(&r["strategy"]) == tag)
        })
        .collect()
}

fn ids_of(orders: &[OrderRow]) -> HashSet<String> {
    orders
        .iter()
        .filter_map(|o| o.orderid.clone())
        .filter(|s| !s.is_empty())
        .collect()
}

/// This deployment's orders, in the global orderbook's envelope.
pub async fn orderbook(
    services: &dyn RunnerServices,
    tag: &str,
    mode: RunMode,
    own: &[OrderRow],
) -> Value {
    let mut payload = match services.book(mode, Book::Orders).await {
        Ok(p) => p,
        Err(e) => return as_error(e, "Could not read the orderbook"),
    };
    let data = payload["data"].clone();
    let (found, under) = rows_and_shape(&data, "orders");
    let orders = mine(found, tag, &ids_of(own));
    let stats = crate::strategy::views::statistics(&orders, &data["statistics"]);
    narrowed(&mut payload, &data, orders, under);
    if let Some(d) = payload["data"].as_object_mut() {
        if d.contains_key("statistics") {
            d.insert("statistics".into(), stats);
        }
    }
    payload
}

/// This deployment's fills.
pub async fn tradebook(
    services: &dyn RunnerServices,
    tag: &str,
    mode: RunMode,
    own: &[OrderRow],
) -> Value {
    let mut payload = match services.book(mode, Book::Trades).await {
        Ok(p) => p,
        Err(e) => return as_error(e, "Could not read the tradebook"),
    };
    let data = payload["data"].clone();
    let (found, under) = rows_and_shape(&data, "trades");
    let kept = mine(found, tag, &ids_of(own));
    narrowed(&mut payload, &data, kept, under);
    payload
}

const ACCOUNT_TOTALS: &[&str] = &[
    "total_pnl",
    "total_pnl_today",
    "total_unrealized_pnl",
    "total_today_realized_pnl",
    "total_realized_pnl",
];

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The contracts this deployment traded, with its own profit as the totals.
pub async fn positions(services: &dyn RunnerServices, mode: RunMode, own: &[OrderRow]) -> Value {
    let traded: HashSet<(String, String)> = own
        .iter()
        .filter(|o| o.filled_quantity > 0 || o.orderid.is_some())
        .map(|o| (o.symbol.clone(), o.exchange.clone()))
        .collect();
    let mut payload = match services.book(mode, Book::Positions).await {
        Ok(p) => p,
        Err(e) => return as_error(e, "Could not read the positions"),
    };
    let data = payload["data"].clone();
    let (found, under) = rows_and_shape(&data, "positions");
    let kept: Vec<Value> = found
        .into_iter()
        .filter(|r| traded.contains(&(text(&r["symbol"]), text(&r["exchange"]))))
        .collect();
    let held = holdings(own);
    let realized: f64 = held.iter().map(|h| h.realized).sum();
    let unrealized: f64 = held
        .iter()
        .filter(|h| h.quantity != 0)
        .filter_map(|h| {
            let avg = h.average_price?;
            let ltp = kept
                .iter()
                .find(|r| text(&r["symbol"]) == h.symbol && text(&r["exchange"]) == h.exchange)
                .and_then(|r| crate::risk::value_to_f64(&r["ltp"]))?;
            Some((ltp - avg) * h.quantity as f64)
        })
        .sum();
    narrowed(&mut payload, &data, kept, under);
    if let Some(p) = payload.as_object_mut() {
        for k in ACCOUNT_TOTALS {
            p.remove(*k);
        }
        p.insert("total_pnl".into(), json!(round2(realized + unrealized)));
        p.insert(
            "total_pnl_today".into(),
            json!(round2(realized + unrealized)),
        );
        p.insert("total_unrealized_pnl".into(), json!(round2(unrealized)));
        p.insert("total_today_realized_pnl".into(), json!(round2(realized)));
        p.insert("total_realized_pnl".into(), json!(round2(realized)));
    }
    payload
}

fn signed_units(v: &Value) -> i64 {
    // Toward zero: a size read here only ever caps an exit, so reading less
    // than is there errs on the side of closing less.
    crate::risk::value_to_f64(v)
        .filter(|q| q.is_finite())
        .map(|q| q.trunc() as i64)
        .unwrap_or(0)
}

fn same_equity(a: &str, b: &str) -> bool {
    a == b || (matches!(a, "NSE" | "BSE") && matches!(b, "NSE" | "BSE"))
}

/// What the destination itself holds in one contract, signed: its position
/// rows for the symbol, exchange and product, plus, for delivery (`CNC`),
/// the shares already settled into its holdings (T+1 moves a delivery
/// position there, and selling from holdings is how it is closed). A Stop
/// caps its exit with this, so it never sells what the account does not hold.
pub fn destination_net(
    positions: &Value,
    holdings: Option<&Value>,
    symbol: &str,
    exchange: &str,
    product: &str,
) -> i64 {
    let (rows, _) = rows_and_shape(&positions["data"], "positions");
    let mut net: i64 = rows
        .iter()
        .filter(|r| {
            text(&r["symbol"]) == symbol
                && text(&r["exchange"]) == exchange
                && text(&r["product"]).eq_ignore_ascii_case(product)
        })
        .map(|r| signed_units(&r["quantity"]))
        .fold(0i64, i64::saturating_add);
    if product.eq_ignore_ascii_case("CNC") {
        if let Some(h) = holdings {
            let (rows, _) = rows_and_shape(&h["data"], "holdings");
            net = rows
                .iter()
                .filter(|r| text(&r["symbol"]) == symbol && same_equity(&text(&r["exchange"]), exchange))
                .map(|r| signed_units(&r["quantity"]))
                .fold(net, i64::saturating_add);
        }
    }
    net
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_destination_net_is_read_per_contract() {
        let positions = json!({"status": "success", "data": [
            {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 7},
            {"symbol": "SBIN", "exchange": "NSE", "product": "CNC", "quantity": "-2"},
            {"symbol": "SBIN", "exchange": "BSE", "product": "MIS", "quantity": 50},
            {"symbol": "INFY", "exchange": "NSE", "product": "MIS", "quantity": 9}
        ]});
        let holdings = json!({"status": "success", "data": {"holdings": [
            {"symbol": "SBIN", "exchange": "BSE", "product": "CNC", "quantity": 10},
            {"symbol": "INFY", "exchange": "NSE", "product": "CNC", "quantity": 4}
        ], "statistics": {}}});
        assert_eq!(
            destination_net(&positions, Some(&holdings), "SBIN", "NSE", "MIS"),
            7
        );
        // Delivery adds settled shares, on either equity exchange.
        assert_eq!(
            destination_net(&positions, Some(&holdings), "SBIN", "NSE", "CNC"),
            8
        );
        assert_eq!(destination_net(&positions, None, "SBIN", "NSE", "CNC"), -2);
        assert_eq!(destination_net(&positions, None, "TCS", "NSE", "MIS"), 0);
        assert_eq!(destination_net(&json!({}), None, "SBIN", "NSE", "MIS"), 0);
    }

    #[test]
    fn shapes_are_kept() {
        let (rows, under) = rows_and_shape(&json!([{"a": 1}, 2]), "orders");
        assert_eq!((rows.len(), under), (1, None));
        let (rows, under) =
            rows_and_shape(&json!({"orders": [{"a": 1}], "statistics": {}}), "orders");
        assert_eq!((rows.len(), under.as_deref()), (1, Some("orders")));
        let ids: HashSet<String> = ["1".to_string()].into_iter().collect();
        let kept = mine(
            vec![
                json!({"orderid": "1"}),
                json!({"orderid": "2", "strategy": "t"}),
                json!({"orderid": "3", "strategy": ""}),
            ],
            "t",
            &ids,
        );
        assert_eq!(kept.len(), 2);
        // No tag and no ids is no rows, never every untagged row.
        assert!(mine(vec![json!({"orderid": "3"})], "", &HashSet::new()).is_empty());
    }
}
