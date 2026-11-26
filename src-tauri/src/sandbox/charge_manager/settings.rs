//! The settings page and the calculator (web `sandbox/charge_manager.py`
//! `get_charge_settings`, `save_charge_settings`, `reset_charge_settings`,
//! `estimate`): what the page is sent, what it may save, and a round trip
//! priced under a plan.

use super::{read_plan, stored_configs};
use crate::brokers::common::python::truthy;
use crate::sandbox::charges::{
    brokerage_flat_key, brokerage_percent_key, config_keys, describe_brokerage,
    estimate_round_trip, is_enabled, plain, segment, segments_for, to_decimal, validate_brokerage,
    value_text, Market, Segment, CHARGES_ENABLED_KEY, MAX_BROKERAGE_FLAT, MAX_BROKERAGE_PERCENT,
    STATUTORY_SCHEDULE_AS_OF,
};
use crate::sandbox::config;
use crate::sandbox::types::float;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// The sentence a save answers with.
pub(crate) const SAVED: &str = "Charges saved. They apply from the next trade.";

/// The plan and the statutory schedule as the settings page shows them,
/// for `market` alone (a page never offers the other market's rate card).
pub(crate) fn settings(conn: &Connection, market: Market) -> rusqlite::Result<Value> {
    let stored = stored_configs(conn, &config_keys())?;
    let get = |k: &str| stored.get(k).map(String::as_str);
    let segments: Vec<Value> = segments_for(Some(market))
        .map(|s| {
            let flat = to_decimal(get(&brokerage_flat_key(s.key)), s.default_flat);
            let percent = to_decimal(get(&brokerage_percent_key(s.key)), s.default_percent);
            segment_json(s, flat, percent)
        })
        .collect();
    Ok(json!({
        "enabled": is_enabled(Some(get(CHARGES_ENABLED_KEY).unwrap_or("true"))),
        "market": market.as_str(),
        "segments": segments,
        "limits": {
            "flat_max": plain(MAX_BROKERAGE_FLAT),
            "percent_max": plain(MAX_BROKERAGE_PERCENT),
        },
        "statutory_as_of": STATUTORY_SCHEDULE_AS_OF,
    }))
}

fn segment_json(s: &Segment, flat: Decimal, percent: Decimal) -> Value {
    // The rates travel as an object, whose keys arrive sorted (as the web's
    // JSON does); the order to show them in is `exchanges`.
    let exchange_txn: BTreeMap<&str, String> = s
        .exchange_txn
        .iter()
        .map(|(e, r)| (*e, plain(*r)))
        .collect();
    json!({
        "key": s.key,
        "label": s.label,
        "group": s.group,
        "applies_to": s.applies_to,
        "flat": plain(flat),
        "percent": plain(percent),
        "default_flat": plain(s.default_flat),
        "default_percent": plain(s.default_percent),
        "summary": describe_brokerage(flat, percent),
        "exchanges": s.exchanges(),
        "statutory": {
            "stt_label": s.stt_label,
            "stt_buy": plain(s.stt_buy),
            "stt_sell": plain(s.stt_sell),
            "stamp_buy": plain(s.stamp_buy),
            "sebi": plain(s.sebi),
            "gst": plain(s.gst),
            "exchange_txn": exchange_txn,
        },
    })
}

/// The values a save would write, checked whole before anything is written,
/// or the sentence for the trader (web `save_charge_settings`). Segments not
/// named keep their values.
pub(crate) fn values_to_save(payload: &Value) -> Result<Vec<(String, String)>, String> {
    let Some(payload) = payload.as_object() else {
        return Err("Nothing was saved: the settings could not be read.".to_string());
    };
    let mut values = Vec::new();
    if let Some(enabled) = payload.get("enabled") {
        let on = match enabled {
            Value::Bool(b) => *b,
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return Err("Charges can only be switched on or off.".to_string()),
            },
            _ => return Err("Charges can only be switched on or off.".to_string()),
        };
        values.push((CHARGES_ENABLED_KEY.to_string(), on.to_string()));
    }
    let unreadable = || "Nothing was saved: the brokerage plan could not be read.".to_string();
    let empty = Map::new();
    // Python's `payload.get("segments") or {}`.
    let segments = if truthy(payload.get("segments")) {
        payload
            .get("segments")
            .and_then(Value::as_object)
            .ok_or_else(unreadable)?
    } else {
        &empty
    };
    let blank = Value::String(String::new());
    for (key, entry) in segments {
        let entry = entry.as_object().ok_or_else(unreadable)?;
        let (flat, percent) = validate_brokerage(
            key,
            entry.get("flat").unwrap_or(&blank),
            entry.get("percent").unwrap_or(&blank),
        )?;
        values.push((brokerage_flat_key(key), plain(flat)));
        values.push((brokerage_percent_key(key), plain(percent)));
    }
    if values.is_empty() {
        return Err("There was nothing to save.".to_string());
    }
    Ok(values)
}

/// Write checked values in the caller's transaction: one commit, so a fill
/// that lands during a save is charged under the old plan or the new one,
/// never a mixture.
pub(crate) fn write(
    conn: &Connection,
    values: &[(String, String)],
    now: &str,
) -> rusqlite::Result<()> {
    for (k, v) in values {
        config::set_raw(conn, k, v, now)?;
    }
    Ok(())
}

/// A round trip under a plan, for the calculator (web `estimate`): the
/// plan in the payload when it names one, so a trader sees what a plan
/// costs before saving it, else the saved plan. A segment of the other
/// market is refused.
pub(crate) fn estimate(
    conn: &Connection,
    payload: &Value,
    market: Market,
) -> rusqlite::Result<Result<Value, String>> {
    let Some(p) = payload.as_object() else {
        return Ok(Err("Enter the trade to estimate.".to_string()));
    };
    let text = |k: &str| p.get(k).and_then(value_text).unwrap_or_default();
    let Some(s) = segment(&text("segment")) else {
        return Ok(Err("Choose a segment.".to_string()));
    };
    if s.market != market {
        return Ok(Err(format!(
            "{} is not traded with the broker you are logged in to.",
            s.label
        )));
    }
    let exchange = text("exchange").to_uppercase();
    let exchanges = s.exchanges();
    if !exchanges.contains(&exchange.as_str()) {
        return Ok(Err(format!(
            "{} trades on {}. Choose one of them.",
            s.label,
            exchanges.join(" or ")
        )));
    }
    let number = |k: &str| to_decimal(p.get(k).and_then(value_text).as_deref(), -Decimal::ONE);
    let (buy, sell, quantity) = (
        number("buy_price"),
        number("sell_price"),
        number("quantity"),
    );
    if buy <= Decimal::ZERO || sell <= Decimal::ZERO {
        return Ok(Err(
            "Enter a buy price and a sell price above zero.".to_string()
        ));
    }
    if quantity <= Decimal::ZERO {
        return Ok(Err("Enter a quantity above zero.".to_string()));
    }
    let (flat, percent) = if p.contains_key("flat") || p.contains_key("percent") {
        let blank = Value::String(String::new());
        match validate_brokerage(
            s.key,
            p.get("flat").unwrap_or(&blank),
            p.get("percent").unwrap_or(&blank),
        ) {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        }
    } else {
        let plan = read_plan(conn, s)?;
        (plan.flat, plan.percent)
    };
    let r = estimate_round_trip(s.key, &exchange, buy, sell, quantity, flat, percent);
    Ok(Ok(json!({
        "segment": s.key,
        "exchange": exchange,
        "brokerage_plan": describe_brokerage(flat, percent),
        "stt_label": if s.stt_label.is_empty() { "STT" } else { s.stt_label },
        "turnover": float(r.turnover),
        "total": r.total.as_json(),
        "gross_pnl": float(r.gross_pnl),
        "net_pnl": float(r.net_pnl),
        "points_to_breakeven": float(r.points_to_breakeven),
    })))
}
