//! Web `test/test_instruments_from_symbol_cache.py` (#2406, ea90e4ffc):
//! `/api/v1/instruments` answers from the symbol master in memory. The web
//! queried the database and built a dict per row, over a second for NFO's
//! 90,000 rows, while every other request waited.
//!
//! The desktop has always answered from the master in memory
//! (`symbol_service::instruments`, one snapshot per request), so only the
//! web's answers are ported. The master here is loaded into memory and
//! never stored, so each answer also shows where it came from. Not ported:
//! the database fallback (the desktop's master lives in one place, the
//! generation every symbol lookup reads) and the call without an exchange
//! (the web's `/api/v1` refuses it: `tests/fixtures/web/rest/instruments/all_exchanges_json.json`).

use crate::brokers::common::symbols::SymToken;

use super::*;

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: format!("{}-BR", symbol),
        name: symbol.chars().take(5).collect(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: token.into(),
        expiry: String::new(),
        // The web test's rows carry None; a master row holds 0.
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn option(symbol: &str, token: &str, kind: &str) -> SymToken {
    SymToken {
        expiry: "27-NOV-26".into(),
        strike: 26000.0,
        lot_size: 75,
        instrument_type: kind.into(),
        ..row(symbol, "NFO", token)
    }
}

/// The web test's `ROWS`, in master order.
fn master() -> Vec<SymToken> {
    vec![
        row("SBIN", "NSE", "3045"),
        option("NIFTY27NOV2626000CE", "50001", "CE"),
        row("RELIANCE", "NSE", "2885"),
        option("NIFTY27NOV2626000PE", "50002", "PE"),
    ]
}

/// The web's row dict (`INSTRUMENT_FIELDS`, in that order).
fn expected(rows: &[&SymToken]) -> Value {
    rows.iter()
        .map(|r| {
            json!({"symbol": r.symbol, "brsymbol": r.brsymbol, "name": r.name,
                "exchange": r.exchange, "brexchange": r.brexchange, "token": r.token,
                "expiry": r.expiry, "strike": r.strike, "lotsize": r.lot_size,
                "instrumenttype": r.instrument_type, "tick_size": r.tick_size})
        })
        .collect()
}

/// The master in memory only, and a valid key.
fn loaded() -> (H, String) {
    let h = H::new();
    let key = h.setup();
    h.ctx().symbols.load(master());
    (h, key)
}

fn instruments(key: &str, query: &str) -> Request<Body> {
    get(&format!("/api/v1/instruments?apikey={}&{}", key, query))
}

/// Web `test_exchange_request_is_served_from_the_cache_without_a_query`:
/// one exchange's rows, in master order, with the web's message.
#[tokio::test]
async fn one_exchange_is_answered_from_the_master_in_memory() {
    let (h, key) = loaded();
    let (s, v) = h.json(instruments(&key, "exchange=NFO")).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let rows = master();
    assert_eq!(
        v,
        json!({"status": "success", "message": "Found 2 instruments",
            "data": expected(&[&rows[1], &rows[3]])})
    );
}

/// Web `test_unknown_exchange_from_the_cache_is_an_empty_success`.
#[tokio::test]
async fn an_exchange_with_no_rows_is_an_empty_success() {
    let (h, key) = loaded();
    let (s, v) = h.json(instruments(&key, "exchange=MCX")).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "No instruments found", "data": []})
        )
    );
}

/// Web `test_csv_from_the_cache_keeps_the_column_order`.
#[tokio::test]
async fn the_csv_keeps_the_column_order() {
    let (h, key) = loaded();
    let (s, headers, body) = h.send(instruments(&key, "exchange=NSE&format=csv")).await;
    assert_eq!(s, StatusCode::OK);
    let csv = String::from_utf8(body).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(
        lines[0],
        "symbol,brsymbol,name,exchange,brexchange,token,expiry,strike,lotsize,instrumenttype,tick_size"
    );
    assert!(
        lines[1].starts_with("SBIN,SBIN-BR,SBIN,NSE,NSE,3045"),
        "{}",
        lines[1]
    );
    assert_eq!(lines.len(), 3, "{}", csv);
    assert!(headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .ends_with("instruments_NSE.csv"));
}
