//! The order boundary between the engine and the platform (web
//! `openscript_host/openscript_runner.py`, `_route`, `_units`, `_frame_for`).
//!
//! An intent is not an order: the engine states what the strategy decided and
//! this turns it into the platform's own order call. A quantity is converted
//! once, here (units as written, lots times the master contract's lot), and a
//! quantity that does not come to a whole number is refused rather than
//! rounded, because rounding is choosing how much to trade. Every order goes
//! out for the deployment's own instrument and product and carries the
//! deployment's id as its `strategy`, which is the whole of the attribution
//! its books are filtered on.

use serde_json::{json, Value};

/// The engine's order types, as the platform's price types.
pub fn price_type(engine_type: &str) -> Option<&'static str> {
    match engine_type {
        "market" => Some("MARKET"),
        "limit" => Some("LIMIT"),
        "stop" => Some("SL-M"),
        "stopLimit" => Some("SL"),
        _ => None,
    }
}

/// The platform's word for an order, as the engine's status vocabulary.
/// A word outside it is `None`, which leaves the engine's row alone.
pub fn status_word(platform: &str) -> Option<&'static str> {
    match platform.trim().to_ascii_lowercase().as_str() {
        "complete" | "filled" => Some("filled"),
        "open" => Some("working"),
        "trigger pending" | "trigger_pending" => Some("triggerPending"),
        "rejected" => Some("rejected"),
        "cancelled" | "canceled" => Some("cancelled"),
        _ => None,
    }
}

/// One order a confirmed bar asked for, ready for the order path.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub intent_id: i64,
    pub tag: String,
    pub action: &'static str,
    pub quantity: i64,
    pub pricetype: &'static str,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
}

/// What one intent becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Planned {
    Place(Placement),
    /// Withdraw this run's working orders that carry the tag.
    Cancel {
        intent_id: i64,
        tag: String,
    },
    /// Not sent; the reason goes back to the engine as a rejection.
    Refuse {
        intent_id: i64,
        reason: String,
    },
    /// A shape this runner cannot send as one piece: the whole bar is refused
    /// and the run stops, as on the web.
    Unroutable {
        intent_id: i64,
        kind: String,
    },
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// The whole units an intent's quantity comes to, or why it cannot be sent.
pub fn units(qty: Option<f64>, qty_type: &str, lot: Option<i64>) -> Result<i64, String> {
    let Some(qty) = qty else {
        return Err("the order had no quantity to send".into());
    };
    let units = match qty_type {
        "units" | "" => qty,
        "lots" => match lot {
            Some(l) if l > 0 => qty * l as f64,
            _ => {
                return Err(
                    "it is sized in lots and the lot size of this instrument is not known. Download the master contract again"
                        .into(),
                )
            }
        },
        other => {
            return Err(format!(
                "a quantity counted in {} cannot be sent from this runner",
                other
            ))
        }
    };
    if units <= 0.0 {
        return Err("the order had no quantity to send".into());
    }
    if units.fract() != 0.0 || units > i64::MAX as f64 {
        return Err(format!(
            "a quantity of {} cannot be sent as a whole number",
            units
        ));
    }
    Ok(units as i64)
}

/// Plan one intent from the engine.
pub fn plan(intent: &Value, lot: Option<i64>) -> Planned {
    let intent_id = intent.get("intentId").and_then(Value::as_i64).unwrap_or(-1);
    let tag = intent
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let kind = intent
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "cancel" => return Planned::Cancel { intent_id, tag },
        "place" => {}
        other => {
            return Planned::Unroutable {
                intent_id,
                kind: other.to_string(),
            }
        }
    }
    let engine_type = intent
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("market");
    let Some(pricetype) = price_type(engine_type) else {
        return Planned::Refuse {
            intent_id,
            reason: format!("the order type {} is not sent here", engine_type),
        };
    };
    let action = match intent.get("side").and_then(Value::as_str) {
        Some("buy") => "BUY",
        Some("sell") => "SELL",
        _ => {
            return Planned::Refuse {
                intent_id,
                reason: "the order had no side".into(),
            }
        }
    };
    let qty_type = intent
        .get("qtyType")
        .and_then(Value::as_str)
        .unwrap_or("units");
    let quantity = match units(num(intent, "qty"), qty_type, lot) {
        Ok(q) => q,
        Err(reason) => return Planned::Refuse { intent_id, reason },
    };
    let price = num(intent, "limit");
    let trigger = num(intent, "trigger");
    if matches!(pricetype, "LIMIT" | "SL") && price.is_none_or(|p| p <= 0.0) {
        return Planned::Refuse {
            intent_id,
            reason: "a limit order needs a price".into(),
        };
    }
    if matches!(pricetype, "SL" | "SL-M") && trigger.is_none_or(|p| p <= 0.0) {
        return Planned::Refuse {
            intent_id,
            reason: "a stop order needs a trigger price".into(),
        };
    }
    Planned::Place(Placement {
        intent_id,
        tag,
        action,
        quantity,
        pricetype,
        price,
        trigger_price: trigger,
    })
}

/// Where a deployment's orders go: its own instrument, product and tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Target<'a> {
    pub strategy: &'a str,
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub product: &'a str,
}

impl Target<'_> {
    /// The `/api/v1/placeorder` body for one order.
    pub fn request(&self, p: &Placement) -> Value {
        json!({
            "strategy": self.strategy,
            "symbol": self.symbol,
            "exchange": self.exchange,
            "action": p.action,
            "quantity": p.quantity,
            "product": self.product,
            "pricetype": p.pricetype,
            "price": p.price.unwrap_or(0.0),
            "trigger_price": p.trigger_price.unwrap_or(0.0),
            "disclosed_quantity": 0,
        })
    }
}

/// A run's product, as the venue names it: what the deployment saved, or
/// intraday when it saved none.
pub fn product_for(saved: &str) -> String {
    if saved.trim().is_empty() {
        "MIS".into()
    } else {
        saved.trim().to_ascii_uppercase()
    }
}

/// What the platform says about an order now.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub status: &'static str,
    pub filled: i64,
    pub average_price: Option<f64>,
}

/// Read an order status row (sandbox or broker) into engine terms.
pub fn progress(row: &Value, ordered: i64) -> Option<Progress> {
    let word = row
        .get("order_status")
        .or_else(|| row.get("status"))
        .and_then(Value::as_str)?;
    let status = status_word(word)?;
    let int = |k: &str| {
        row.get(k).and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_f64().map(|f| f as i64))
                .or_else(|| {
                    v.as_str()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .map(|f| f as i64)
                })
        })
    };
    let reported = int("filled_quantity").unwrap_or(0);
    let filled = if status == "filled" {
        if reported > 0 {
            reported
        } else {
            int("quantity").unwrap_or(ordered)
        }
    } else {
        reported.max(0)
    };
    let average_price = row
        .get("average_price")
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|p| p.is_finite() && *p > 0.0);
    Some(Progress {
        status,
        filled,
        average_price,
    })
}

/// The engine's order frame for one intent.
pub fn frame(
    intent_id: i64,
    status: &str,
    filled: i64,
    average_price: Option<f64>,
    orderid: Option<&str>,
    text: Option<&str>,
    time_ms: i64,
) -> Value {
    json!({
        "intentId": intent_id,
        "status": status,
        "filledQty": filled,
        "avgFillPrice": average_price,
        "orderRef": orderid,
        "text": text,
        "time": time_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(kind: &str, ty: &str, qty: f64, qty_type: &str) -> Value {
        json!({
            "intentId": 4, "kind": kind, "side": "buy", "qty": qty, "qtyType": qty_type,
            "type": ty, "limit": 101.5, "trigger": 100.0, "tag": "long",
            "instrument": {"symbol": "SBIN", "exchange": "NSE"}, "product": "",
            "positionRef": 1, "bar": {"index": 3, "time": 0}
        })
    }

    #[test]
    fn intents_become_orders_in_whole_units() {
        match plan(&intent("place", "market", 2.0, "lots"), Some(75)) {
            Planned::Place(p) => {
                assert_eq!(p.quantity, 150);
                assert_eq!(p.pricetype, "MARKET");
                assert_eq!(p.action, "BUY");
                assert_eq!(p.tag, "long");
            }
            other => panic!("{:?}", other),
        }
        assert!(matches!(
            plan(&intent("place", "stopLimit", 1.0, "units"), None),
            Planned::Place(Placement {
                pricetype: "SL",
                price: Some(_),
                trigger_price: Some(_),
                ..
            })
        ));
        assert!(matches!(
            plan(&intent("place", "market", 1.5, "units"), None),
            Planned::Refuse { reason, .. } if reason.contains("whole number")
        ));
        assert!(matches!(
            plan(&intent("place", "market", 1.0, "cash"), None),
            Planned::Refuse { reason, .. } if reason.contains("counted in cash")
        ));
        assert!(matches!(
            plan(&intent("place", "market", 1.0, "lots"), None),
            Planned::Refuse { reason, .. } if reason.contains("lot size")
        ));
        assert!(matches!(
            plan(&intent("cancel", "market", 1.0, "units"), None),
            Planned::Cancel { tag, .. } if tag == "long"
        ));
        assert!(matches!(
            plan(&intent("bracket", "market", 1.0, "units"), None),
            Planned::Unroutable { .. }
        ));
    }

    #[test]
    fn platform_words_become_engine_frames() {
        let p = progress(
            &json!({"order_status": "complete", "quantity": 10, "average_price": 101.25}),
            10,
        )
        .unwrap();
        assert_eq!(p.status, "filled");
        assert_eq!(p.filled, 10);
        assert_eq!(p.average_price, Some(101.25));
        let p = progress(&json!({"order_status": "open", "filled_quantity": 3}), 10).unwrap();
        assert_eq!((p.status, p.filled), ("working", 3));
        assert!(progress(&json!({"order_status": "validation pending"}), 1).is_none());
        let f = frame(4, "filled", 10, Some(1.0), Some("OID"), None, 5);
        assert_eq!(f["intentId"], 4);
        assert_eq!(f["filledQty"], 10);
        assert_eq!(product_for(""), "MIS");
        assert_eq!(product_for("cnc"), "CNC");
    }
}
