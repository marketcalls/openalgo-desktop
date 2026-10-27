//! The symbols toolkit against the instrument master through the in-process
//! `/api/v1`, and the web's argument tests (`test_agent_tool_arguments.py`).

use super::*;
use crate::agent::tools::testkit::{body, tag, Kit};
use serde_json::json;

#[test]
fn exchange_spellings_and_refusals() {
    assert_eq!(
        exchange_argument(Some(&json!("nse index")), false).ok(),
        Some("NSE_INDEX".into())
    );
    assert_eq!(
        exchange_argument(Some(&json!("")), true).ok(),
        Some(String::new())
    );
    let sorted = "BCD, BFO, BSE, BSE_INDEX, CDS, CRYPTO, GLOBAL_INDEX, MCX, MCX_INDEX, NCDEX, NCO, NFO, NSE, NSE_INDEX";
    assert_eq!(
        exchange_argument(Some(&json!("NASDAQ")), false),
        Err(ToolOutcome::Retry(format!(
            "The 'exchange' argument is invalid: 'NASDAQ' is not an OpenAlgo exchange code. Pass one of: {}.",
            sorted
        )))
    );
    assert_eq!(
        exchange_argument(None, false),
        Err(ToolOutcome::Retry(format!(
            "The 'exchange' argument is invalid: it was empty, and a symbol only identifies a contract together with its exchange. Pass one of: {}.",
            sorted
        )))
    );
}

#[test]
fn a_cash_or_index_exchange_moves_to_its_derivatives() {
    for (given, venue) in [
        ("NSE", "NFO"),
        ("nse index", "NFO"),
        ("BSE", "BFO"),
        ("BSE_INDEX", "BFO"),
        ("MCX_INDEX", "MCX"),
    ] {
        let (got, notice) =
            expiry_exchange(Some(&json!(given))).unwrap_or_else(|e| panic!("{:?}", e));
        assert_eq!(got, venue);
        assert!(notice.unwrap_or_default().contains(venue));
    }
    assert_eq!(
        expiry_exchange(Some(&json!("NSE_INDEX"))).ok().and_then(|r| r.1),
        Some("NSE_INDEX lists no futures or options and is a quote-only index feed, so the expiries were read from NFO, where its derivatives are listed.".into())
    );
    assert_eq!(
        expiry_exchange(Some(&json!("NFO"))).ok(),
        Some(("NFO".into(), None))
    );
    let refused = expiry_exchange(Some(&json!("GLOBAL_INDEX")));
    assert!(
        matches!(&refused, Err(ToolOutcome::Retry(t)) if t.contains("derivatives exchange")),
        "{:?}",
        refused
    );
}

#[test]
fn instrument_type_aliases() {
    for (given, kind) in [
        ("Options", "options"),
        ("OPT-IDX", "options"),
        ("pe", "options"),
        ("FUT_STK", "futures"),
        ("future", "futures"),
    ] {
        assert_eq!(
            instrument_type(Some(&json!(given))).ok(),
            Some(kind),
            "{}",
            given
        );
    }
    assert_eq!(
        instrument_type(Some(&json!("swap"))),
        Err(ToolOutcome::Retry("The 'instrument_type' argument is invalid: 'swap' is not an instrument type. Pass 'options' or 'futures'.".into()))
    );
}

#[test]
fn master_expiries_are_spelled_as_symbols_spell_them() {
    assert_eq!(
        symbol_expiry(&json!("31-JUL-25")).as_deref(),
        Some("31JUL25")
    );
    assert_eq!(
        symbol_expiry(&json!("31-JUL-2025")).as_deref(),
        Some("31JUL25")
    );
    assert_eq!(symbol_expiry(&json!("31JUL25")).as_deref(), Some("31JUL25"));
    assert_eq!(
        symbol_expiry(&json!("5-jan-26")).as_deref(),
        Some("05JAN26")
    );
    assert_eq!(symbol_expiry(&json!("2025-07-31")), None);
    assert_eq!(symbol_expiry(&json!(5)), None);
    assert_eq!(root(" nifty28OCT2624000CE"), "NIFTY");
    assert_eq!(root("726GS2033"), "");
}

#[test]
fn underlyings_stop_at_the_first_real_date() {
    let rows: Vec<Map<String, Value>> = [
        ("726GS203325APR24FUT", "NFO"),
        ("NIFTY28OCT2624000CE", "NFO"),
        ("NIFTY28OCT2624000PE", "NFO"),
        ("bad symbol!", "NFO"),
    ]
    .iter()
    .map(|(s, e)| crate::agent::tools::testkit::map(json!({"symbol": s, "exchange": e})))
    .collect();
    assert_eq!(underlyings(&rows, "", false), vec!["726GS2033", "NIFTY"]);
    assert_eq!(
        underlyings(&rows, "NIFTY", true),
        vec!["726GS2033 (NFO)", "NIFTY (NFO)"]
    );
    assert_eq!(underlyings(&rows, "NIFTY", false), vec!["726GS2033"]);
    assert_eq!(pairs(&rows, None).len(), 3);
}

#[tokio::test]
async fn a_search_is_bounded_and_says_how_to_narrow_it() {
    let kit = Kit::new();
    let out = kit
        .call(&GROUP, "search_symbols", json!({"query": "nifty"}))
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"search_symbols\" query=\"nifty\">"
    );
    let b = body(&out);
    assert_eq!(b["exchange"], "all");
    assert_eq!(b["returned"], 40);
    let total = b["total_matches"].as_u64().unwrap_or_default();
    assert!(total > 40);
    assert_eq!(b["truncated"], true);
    assert_eq!(
        b["note"],
        format!("{} further matches were not returned. Add a term to the query (a strike, an expiry, CE or PE) or set the exchange to narrow it.", total - 40)
    );
    let first = &b["matches"][0];
    assert!(
        first.get("brsymbol").is_none() && first.get("token").is_none(),
        "{}",
        first
    );
    let infy = body(
        &kit.call(
            &GROUP,
            "search_symbols",
            json!({"query": "INFY", "exchange": "NSE"}),
        )
        .await,
    );
    assert_eq!(infy["matches"][0]["symbol"], "INFY");
    assert!(
        infy["matches"][0].get("strike").is_none() && infy["matches"][0].get("expiry").is_none()
    );
}

#[tokio::test]
async fn a_search_that_finds_nothing_suggests_the_query_that_works() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "search_symbols",
            json!({"query": "INFY", "exchange": "BSE"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry("Nothing matches 'INFY' on BSE, but dropping the exchange filter BSE finds listed contracts: INFY (NSE). Call search_symbols again with the query and exchange that work, then use a symbol exactly as it comes back.".into())
    );
    let nothing = kit
        .call(&GROUP, "search_symbols", json!({"query": "ZZZZQ"}))
        .await;
    let b = body(&nothing);
    assert_eq!(b["total_matches"], 0);
    assert!(b["message"]
        .as_str()
        .unwrap_or_default()
        .starts_with("Nothing in the instrument master matches that query"));
    let empty = kit
        .call(&GROUP, "search_symbols", json!({"query": "  "}))
        .await;
    assert_eq!(
        empty,
        ToolOutcome::Retry("The 'query' argument is invalid: it was empty. Pass a base symbol, a company name, or the terms to match, such as 'BANKNIFTY 56000 PE'.".into())
    );
}

#[tokio::test]
async fn get_symbol_confirms_a_contract_and_drops_empty_fields() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_symbol",
            json!({"symbol": "nifty13oct2624500ce", "exchange": "NFO"}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_symbol\" symbol=\"NIFTY13OCT2624500CE\" exchange=\"NFO\">"
    );
    let b = body(&out);
    assert_eq!(b["lotsize"], 75);
    assert_eq!(b["strike"], 24500.0);
    assert!(b.get("id").is_none());
    let infy = body(
        &kit.call(
            &GROUP,
            "get_symbol",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await,
    );
    assert!(
        infy.get("expiry").is_none() && infy.get("strike").is_none(),
        "{}",
        infy
    );
}

#[tokio::test]
async fn an_unlisted_contract_is_answered_with_the_nearest_listed_ones() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_symbol",
            json!({"symbol": "NIFTY13OCT2624550CE", "exchange": "NFO"}),
        )
        .await;
    let text = out.text();
    assert!(!out.is_ok());
    assert!(
        text.starts_with(
            "NIFTY13OCT2624550CE is not listed on NFO. These contracts are listed on NFO: NIFTY"
        ),
        "{}",
        text
    );
    assert!(text.ends_with("Do not adjust an expiry or a strike yourself to make a symbol fit."));
    let nothing = kit
        .call(
            &GROUP,
            "get_symbol",
            json!({"symbol": "ZZZZQ", "exchange": "NSE"}),
        )
        .await;
    assert!(
        nothing
            .text()
            .starts_with("symbol_service.get_symbol_info failed (HTTP 404)"),
        "{:?}",
        nothing
    );
}

#[tokio::test]
async fn expiry_dates_carry_the_symbol_spelling_and_the_venue_move() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_expiry_dates",
            json!({"symbol": "nifty", "exchange": "NSE_INDEX", "instrument_type": "options"}),
        )
        .await;
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_expiry_dates\" symbol=\"NIFTY\" exchange=\"NFO\">"
    );
    let b = body(&out);
    assert_eq!(b["exchange"], "NFO");
    assert_eq!(b["count"], 2);
    assert_eq!(
        b["expiries"],
        json!([
            {"expiry": "13-OCT-26", "symbol_expiry": "13OCT26"},
            {"expiry": "27-OCT-26", "symbol_expiry": "27OCT26"}
        ])
    );
    assert!(b["notices"][0]
        .as_str()
        .unwrap_or_default()
        .contains("read from NFO"));
    let futures = body(
        &kit.call(
            &GROUP,
            "get_expiry_dates",
            json!({"symbol": "NIFTY", "exchange": "NFO", "instrument_type": "fut"}),
        )
        .await,
    );
    assert_eq!(futures["count"], 2);
}

#[tokio::test]
async fn an_underlying_with_no_expiries_is_pointed_at_one_that_has_them() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_expiry_dates",
            json!({"symbol": "NIFTY", "exchange": "BFO", "instrument_type": "options"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutcome::Retry("NIFTY has no live options expiries on BFO. These underlyings are listed on other exchanges: NIFTY (NSE_INDEX), NIFTY (NFO). Call get_expiry_dates again with the underlying and exchange that match, or use search_symbols if none of them is what the operator meant.".into())
    );
    let unknown = kit
        .call(
            &GROUP,
            "get_expiry_dates",
            json!({"symbol": "ZZZQ", "exchange": "NFO", "instrument_type": "options"}),
        )
        .await;
    let b = body(&unknown);
    assert_eq!(b["count"], 0);
    assert_eq!(
        b["message"],
        "No live options expiries for ZZZQ on NFO, and no similar underlying is listed there either. Check the underlying with the operator rather than guessing an expiry."
    );
}

#[tokio::test]
async fn the_symbols_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Chart);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}
