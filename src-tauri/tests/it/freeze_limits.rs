//! The Freeze Quantities page takes effect where the web's does (web
//! `database/qty_freeze_db.py`, whose cache every lookup reads): an entry
//! saved on the page sets the limit that scalping's order-size check and
//! the symbol and search lists use, at once, and deleting it brings back
//! NSE's built-in limit. Before, the page wrote a table nothing read.

use crate::mcp_support::M;
use axum::http::{Method, StatusCode};
use openalgo_desktop_lib::scalping::monitor::exit_chunk;
use serde_json::{json, Value};

const OPT: &str = "NIFTY06OCT2622000CE";

async fn symbol_freeze(m: &M) -> Value {
    let body = m.h.with_key(json!({"symbol": OPT, "exchange": "NFO"}));
    let (s, v) = m.h.post("/api/v1/symbol", body).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    v["data"]["freeze_qty"].clone()
}

/// A scalping entry of `quantity` units of `OPT`: the answer's message.
async fn scalping_entry(m: &M, quantity: i64) -> String {
    let body = json!({"symbol": OPT, "exchange": "NFO", "action": "BUY",
        "quantity": quantity, "product": "NRML"});
    let (_, v) = m
        .session_json(Method::POST, "/scalping/api/order", Some(body))
        .await;
    v["message"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_on_the_page_sets_the_limit_every_lookup_uses() {
    let m = M::new().await;
    m.h.analyze(true);
    let built_in = symbol_freeze(&m).await;
    assert_eq!(built_in, json!(3510), "NSE's built-in NIFTY limit");

    let (s, v) = m
        .session_json(
            Method::POST,
            "/admin/api/freeze",
            Some(json!({"exchange": "NFO", "symbol": "NIFTY", "freeze_qty": 130})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let id = v["data"]["id"].as_i64().unwrap();

    assert_eq!(symbol_freeze(&m).await, json!(130));
    // Three lots of 65 are over the page's limit, under NSE's.
    assert_eq!(
        scalping_entry(&m, 195).await,
        "quantity exceeds the exchange freeze limit (130)"
    );

    let (s, v) = m
        .session_json(
            Method::PUT,
            &format!("/admin/api/freeze/{}", id),
            Some(json!({"freeze_qty": 260})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(symbol_freeze(&m).await, json!(260));

    // A limit below one lot (65) still caps each exit at one lot, never at
    // the 20-lot chunk used when no limit is known.
    let (s, v) = m
        .session_json(
            Method::PUT,
            &format!("/admin/api/freeze/{}", id),
            Some(json!({"freeze_qty": 30})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        exit_chunk(&m.h.ctx.symbols, &m.h.ctx.freeze, OPT, "NFO", 195),
        Ok(Some(65))
    );

    let (s, v) = m
        .session_json(Method::DELETE, &format!("/admin/api/freeze/{}", id), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(symbol_freeze(&m).await, built_in);
    m.h.shutdown().await;
}
