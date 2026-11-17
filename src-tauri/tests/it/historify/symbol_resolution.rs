//! Web `test/test_historify_symbol_resolution.py` (openalgo#2463,
//! `6716dd6f2`), the cases that write a real watchlist: Bulk Add stores
//! master contract symbols, says which it renamed and lists every symbol it
//! could not add with the reason. The `resolve_symbol` cases are unit tests
//! beside it (`src/historify/symbol_resolution/tests.rs`).

use super::*;

/// Web `MASTER`, its NSE rows.
fn resolver() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(
        [
            "M&M",
            "M&MFIN",
            "BAJAJ-AUTO",
            "J&KBANK",
            "NAM-INDIA",
            "RELIANCE",
            "HFCL",
            // Both readings of A_B exist, so A_B is ambiguous.
            "A-B",
            "A&B",
        ]
        .iter()
        .map(|s| sym(s, "NSE", s, "", 0.0, "EQ"))
        .collect(),
    );
    r
}

fn nse(symbols: &[&str]) -> Vec<db::SymbolReq> {
    symbols
        .iter()
        .map(|s| db::SymbolReq {
            symbol: s.to_string(),
            exchange: "NSE".into(),
            display_name: None,
        })
        .collect()
}

/// Web `_watchlist`: the stored rows, sorted.
async fn stored(e: &Engine) -> Vec<(String, String)> {
    let mut rows = e.h.db.run(db::watchlist_symbols).await.unwrap();
    rows.sort();
    rows
}

fn pairs(rows: &[&str]) -> Vec<(String, String)> {
    rows.iter().map(|s| (s.to_string(), "NSE".into())).collect()
}

/// Web test/test_historify_symbol_resolution.py::test_bulk_add_stores_master_contract_symbols_and_reports_every_refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_add_stores_master_contract_symbols_and_reports_every_refusal() {
    let e = engine(false);
    let pasted = [
        "M&M",
        "BAJAJ-AUTO",
        "M_M",
        "J_KBANK",
        "A_B",
        "NOSUCH",
        "RELIANCE",
    ];
    let r = e.h.bulk_add_watchlist(&resolver(), nse(&pasted)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    let v = &r.body;
    assert_eq!(v["status"], "success");
    // M_M resolves to M&M, already in this batch, so it is skipped, not doubled.
    assert_eq!(v["added"], 4);
    assert_eq!(v["skipped"], 1);
    assert_eq!(v["total"], 7);
    assert_eq!(
        v["renamed"],
        json!([
            {"from": "M_M", "to": "M&M", "exchange": "NSE"},
            {"from": "J_KBANK", "to": "J&KBANK", "exchange": "NSE"},
        ])
    );
    let failed = v["failed"].as_array().unwrap();
    let mut refused: Vec<&str> = failed
        .iter()
        .map(|f| f["symbol"].as_str().unwrap())
        .collect();
    refused.sort();
    assert_eq!(refused, ["A_B", "NOSUCH"]);
    assert!(
        failed
            .iter()
            .all(|f| !f["error"].as_str().unwrap_or("").is_empty()),
        "{failed:?}"
    );
    assert_eq!(
        stored(&e).await,
        pairs(&["BAJAJ-AUTO", "J&KBANK", "M&M", "RELIANCE"])
    );
    e.h.shutdown().await;
}

/// Web test/test_historify_symbol_resolution.py::test_single_add_stores_the_master_contract_symbol.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_add_stores_the_master_contract_symbol() {
    let e = engine(false);
    let r =
        e.h.add_watchlist(&resolver(), "BAJAJ_AUTO", "NSE", None)
            .await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["message"], "Added BAJAJ-AUTO to watchlist");
    assert_eq!(stored(&e).await, pairs(&["BAJAJ-AUTO"]));
    e.h.shutdown().await;
}

/// Web test/test_historify_symbol_resolution.py::test_a_failed_write_is_reported_as_an_error_not_a_symbol.
///
/// The web's database layer reported a failed write as a failed symbol
/// named "batch" inside a success; the desktop's write returns an error,
/// which the service answers with the store's 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_write_is_reported_as_an_error_not_a_symbol() {
    let e = engine(false);
    e.h.shutdown().await;
    let r =
        e.h.bulk_add_watchlist(&resolver(), nse(&["RELIANCE"]))
            .await;
    assert_eq!(r.status, 500, "{:?}", r.body);
    assert_eq!(r.body["status"], "error");
    let message = r.body["message"].as_str().unwrap();
    assert!(!message.contains("batch"), "{message}");
}
