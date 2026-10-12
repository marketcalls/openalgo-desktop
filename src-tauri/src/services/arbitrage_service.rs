//! Futures calendar-spread universe (web `arbitrage_service.py`): per
//! underlying, the three nearest futures form near-next and near-third
//! pairs. A pure master-contract read; live prices come from the feed.

use super::core::{float, Reply};
use super::options_service::listed_live;
use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::common::underlying::row_underlying;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;

pub const MAX_LEGS: usize = 3;
pub const DEFAULT_EXCHANGES: &[&str] = &["NFO", "MCX"];
pub const SUPPORTED_EXCHANGES: &[&str] = &["NFO", "MCX", "BFO", "CDS"];

fn leg(r: &SymToken) -> Value {
    json!({
        "symbol": r.symbol,
        "exchange": r.exchange,
        "expiry": r.expiry,
        "lotsize": r.lot_size,
        "tick_size": float(r.tick_size),
    })
}

/// Per underlying (in first-seen order), its live futures nearest first,
/// at most [`MAX_LEGS`]. The underlying is read from the contract's
/// symbol, not the broker's `name` text (a description on Fyers, where
/// grouping by it left every future alone and the universe empty); an
/// expired or unreadable expiry is not a leg.
fn nearest_futures<'a>(
    rows: &'a [SymToken],
    exchange: &str,
    now: DateTime<Utc>,
) -> Vec<(String, Vec<&'a SymToken>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&'a SymToken>> = HashMap::new();
    for r in rows.iter().filter(|r| r.exchange == exchange) {
        if !r.symbol.to_ascii_uppercase().ends_with("FUT")
            || !listed_live(&r.expiry, &r.exchange, now)
        {
            continue;
        }
        let Some(underlying) = row_underlying(r) else {
            continue;
        };
        let g = groups.entry(underlying.clone()).or_insert_with(|| {
            order.push(underlying);
            Vec::new()
        });
        let sym = r.symbol.to_ascii_uppercase();
        match g.iter_mut().find(|x| x.symbol.to_ascii_uppercase() == sym) {
            Some(slot) => *slot = r,
            None => g.push(r),
        }
    }
    order
        .into_iter()
        .map(|name| {
            let mut v = groups.remove(&name).unwrap_or_default();
            v.sort_by_key(|r| parse_oa_expiry(&r.expiry).unwrap_or(NaiveDate::MAX));
            v.truncate(MAX_LEGS);
            (name, v)
        })
        .collect()
}

/// `GET /arbitrage/api/universe`.
pub fn universe(rows: &[SymToken], exchanges: &[String], now: DateTime<Utc>) -> Reply {
    let requested: Vec<String> = exchanges
        .iter()
        .map(|e| e.trim().to_ascii_uppercase())
        .filter(|e| !e.is_empty())
        .collect();
    let scan: Vec<&String> = requested
        .iter()
        .filter(|e| SUPPORTED_EXCHANGES.contains(&e.as_str()))
        .collect();
    if scan.is_empty() {
        let what = if requested.is_empty() {
            "request".to_string()
        } else {
            format!(
                "[{}]",
                requested
                    .iter()
                    .map(|e| format!("'{}'", e))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Reply::error(
            400,
            format!(
                "No supported exchanges in {}. Supported: {}",
                what,
                SUPPORTED_EXCHANGES.join(", ")
            ),
        );
    }
    let mut pairs = Vec::new();
    let mut symbols: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut underlyings = 0;
    for ex in &scan {
        for (name, c) in nearest_futures(rows, ex, now) {
            if c.len() < 2 {
                continue;
            }
            underlyings += 1;
            let near = c[0];
            for (kind, far) in [("near-next", c.get(1)), ("near-third", c.get(2))] {
                let Some(far) = far else { continue };
                pairs.push(json!({
                    "id": format!("{}:{}:{}", ex, name, kind),
                    "underlying": name,
                    "exchange": ex,
                    "type": kind,
                    "near": leg(near),
                    "far": leg(far),
                }));
                for l in [near, *far] {
                    let key = format!("{}:{}", l.exchange, l.symbol);
                    if !seen.contains(&key) {
                        seen.push(key);
                        symbols.push(json!({"symbol": l.symbol, "exchange": l.exchange}));
                    }
                }
            }
        }
    }
    let scanned: Vec<&str> = scan.iter().map(|s| s.as_str()).collect();
    Reply::ok(json!({
        "status": "success",
        "message": format!(
            "Found {} calendar pairs across {} futures in {}",
            pairs.len(),
            symbols.len(),
            scanned.join(", ")
        ),
        "data": {
            "counts": {"underlyings": underlyings, "pairs": pairs.len(), "symbols": symbols.len()},
            "generated_at": now.with_timezone(&Kolkata).naive_local().format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
            "pairs": pairs,
            "symbols": symbols,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fut(name: &str, ex: &str, expiry: &str) -> SymToken {
        SymToken {
            symbol: format!("{}{}FUT", name, expiry.replace('-', "")),
            brsymbol: String::new(),
            name: name.into(),
            exchange: ex.into(),
            brexchange: ex.into(),
            token: format!("{}{}", name, expiry),
            expiry: expiry.into(),
            strike: 0.0,
            lot_size: 75,
            instrument_type: "FUT".into(),
            tick_size: 0.05,
        }
    }

    #[test]
    fn pairs_the_three_nearest_futures() {
        let rows = vec![
            fut("NIFTY", "NFO", "29-DEC-26"),
            fut("NIFTY", "NFO", "27-OCT-26"),
            fut("NIFTY", "NFO", "24-NOV-26"),
            fut("NIFTY", "NFO", "26-JAN-27"),
            fut("SBIN", "NFO", "27-OCT-26"),
            fut("GOLD", "MCX", "05-DEC-26"),
            fut("GOLD", "MCX", "05-FEB-27"),
        ];
        let r = universe(
            &rows,
            &["nfo".into(), "MCX".into(), "XYZ".into()],
            ist_noon_12_oct(),
        );
        assert_eq!(r.status, 200);
        let d = &r.body["data"];
        assert_eq!(
            d["counts"],
            json!({"underlyings": 2, "pairs": 3, "symbols": 5})
        );
        let p = &d["pairs"][0];
        assert_eq!(p["id"], "NFO:NIFTY:near-next");
        assert_eq!(p["near"]["symbol"], "NIFTY27OCT26FUT");
        assert_eq!(p["far"]["symbol"], "NIFTY24NOV26FUT");
        assert_eq!(d["pairs"][1]["far"]["symbol"], "NIFTY29DEC26FUT");
        assert_eq!(d["pairs"][2]["id"], "MCX:GOLD:near-next");
        assert_eq!(
            r.body["message"],
            "Found 3 calendar pairs across 5 futures in NFO, MCX"
        );
    }

    fn ist_noon_12_oct() -> DateTime<Utc> {
        use chrono::TimeZone;
        Kolkata
            .with_ymd_and_hms(2026, 10, 12, 12, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    /// TOOLS-01: Fyers names every future by its description, so grouping
    /// by `name` paired nothing. The symbol decides; an expired future is
    /// never the near leg.
    #[test]
    fn pairs_futures_by_the_underlying_in_their_symbol() {
        let mut rows = vec![
            fut("NIFTY", "NFO", "27-OCT-26"),
            fut("NIFTY", "NFO", "24-NOV-26"),
            fut("NIFTY", "NFO", "29-SEP-26"),
            fut("NIFTYNXT50", "NFO", "27-OCT-26"),
            fut("NIFTYNXT50", "NFO", "24-NOV-26"),
        ];
        for r in &mut rows {
            r.name = format!("{} {} FUT", r.name, r.expiry);
        }
        let r = universe(&rows, &["NFO".into()], ist_noon_12_oct());
        let d = &r.body["data"];
        assert_eq!(
            d["counts"],
            json!({"underlyings": 2, "pairs": 2, "symbols": 4})
        );
        assert_eq!(d["pairs"][0]["id"], "NFO:NIFTY:near-next");
        assert_eq!(d["pairs"][0]["near"]["symbol"], "NIFTY27OCT26FUT");
        assert_eq!(d["pairs"][0]["far"]["symbol"], "NIFTY24NOV26FUT");
        assert_eq!(d["pairs"][1]["underlying"], "NIFTYNXT50");
    }

    #[test]
    fn unsupported_exchanges_are_refused() {
        let r = universe(&[], &["NSE".into()], Utc::now());
        assert_eq!(r.status, 400);
        assert_eq!(
            r.body["message"],
            "No supported exchanges in ['NSE']. Supported: NFO, MCX, BFO, CDS"
        );
        let r = universe(&[], &[], Utc::now());
        assert_eq!(
            r.body["message"],
            "No supported exchanges in request. Supported: NFO, MCX, BFO, CDS"
        );
    }
}
