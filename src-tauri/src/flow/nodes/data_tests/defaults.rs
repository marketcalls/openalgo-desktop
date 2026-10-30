//! What a data node sends when a field is left out (the web executor's
//! `get_str(..., default)`), what it keeps of what it is given, the reply it
//! stores when the service fails, and the fallbacks it reads a reply with.
//! Each test fails if one default or fallback changes (decision 35).

use super::support::{ctx, ctx_at, env_with, logs, run, FakeBook, FakeCalendar};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::node::NodeCtx;
use crate::flow::nodes::history::shared_cache;
use serde_json::{json, Value};
use std::sync::Arc;

/// Run `node` with `data` on a client answering `endpoint` with `reply`;
/// the payload sent and the outcome's value.
async fn sent(node: &str, data: Value, endpoint: &str, reply: Value) -> (Value, Option<Value>) {
    let client = Arc::new(FakeFlowClient::new());
    client.reply(endpoint, reply);
    let c = ctx(client.clone());
    let out = run(&c, node, data).await;
    let payload = client
        .calls_to(endpoint)
        .first()
        .map(|call| call.payload.clone())
        .unwrap_or(Value::Null);
    (payload, out.value().cloned())
}

/// N-optionSymbol-02: exchange NSE_INDEX, offset ATM and type CE when the
/// node leaves them out.
#[tokio::test]
async fn an_option_symbol_defaults_to_the_atm_call_on_nse_index() {
    let (payload, _) = sent(
        "optionSymbol",
        json!({"underlying": "NIFTY", "expiryDate": "27AUG26"}),
        "optionsymbol",
        json!({"status": "success"}),
    )
    .await;
    assert_eq!(
        payload,
        json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "27AUG26",
               "offset": "ATM", "option_type": "CE"})
    );
}

/// N-optionChain-01: the chain's exchange defaults to NSE_INDEX (and the
/// strike count to 10).
#[tokio::test]
async fn an_option_chain_defaults_to_nse_index() {
    let (payload, _) = sent(
        "optionChain",
        json!({"underlying": "NIFTY", "expiryDate": "27AUG26"}),
        "optionchain",
        json!({"status": "success"}),
    )
    .await;
    assert_eq!(
        payload,
        json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "27AUG26",
               "strike_count": 10})
    );
}

/// N-syntheticFuture-01: the synthetic future's exchange defaults to
/// NSE_INDEX.
#[tokio::test]
async fn a_synthetic_future_defaults_to_nse_index() {
    let (payload, _) = sent(
        "syntheticFuture",
        json!({"underlying": "BANKNIFTY", "expiryDate": "27AUG26"}),
        "syntheticfuture",
        json!({"status": "success"}),
    )
    .await;
    assert_eq!(
        payload,
        json!({"underlying": "BANKNIFTY", "exchange": "NSE_INDEX", "expiry_date": "27AUG26"})
    );
}

/// N-multiQuotes-01: the symbols are trimmed and split, never upper-cased,
/// and each carries the node's exchange as given (NSE when absent).
#[tokio::test]
async fn multi_quotes_keep_the_symbols_case() {
    let (payload, _) = sent(
        "multiQuotes",
        json!({"symbols": " sbin, Infy ,,TCS"}),
        "multiquotes",
        json!({"status": "success", "results": []}),
    )
    .await;
    assert_eq!(
        payload,
        json!({"symbols": [
            {"symbol": "sbin", "exchange": "NSE"},
            {"symbol": "Infy", "exchange": "NSE"},
            {"symbol": "TCS", "exchange": "NSE"}
        ]})
    );
    let (payload, _) = sent(
        "multiQuotes",
        json!({"symbols": "reliance", "exchange": "bse"}),
        "multiquotes",
        json!({"status": "success", "results": []}),
    )
    .await;
    assert_eq!(
        payload,
        json!({"symbols": [{"symbol": "reliance", "exchange": "bse"}]})
    );
}

/// Daily bars on 22..28 Oct 2026 (IST midnights), closes 22..28.
fn week_of_bars() -> Value {
    let bars: Vec<Value> = (0..7)
        .map(|i| {
            let ts = 1_792_607_400 + i * 86_400;
            json!({"timestamp": ts, "open": 1.0, "high": 2.0, "low": 0.5, "close": 22 + i, "volume": 10})
        })
        .collect();
    json!({"status": "success", "data": bars})
}

/// N-barOffset-01: exchange NSE, interval D, offset 0 (the last closed
/// bar) and source api when the node leaves them out; a source of db
/// reaches the fetch.
#[tokio::test]
async fn bar_offset_defaults_to_the_last_closed_nse_daily_bar() {
    shared_cache().clear();
    let (payload, out) = sent(
        "barOffset",
        json!({"symbol": "SBIN"}),
        "history",
        week_of_bars(),
    )
    .await;
    assert_eq!(
        payload,
        json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D", "source": "api",
               "start_date": "2026-10-16", "end_date": "2026-10-29"})
    );
    let out = out.unwrap_or(Value::Null);
    assert_eq!(
        (
            out["exchange"].clone(),
            out["offsetBars"].clone(),
            out["close"].clone()
        ),
        (json!("NSE"), json!(0), json!(28))
    );
    shared_cache().clear();
    let (payload, _) = sent(
        "barOffset",
        json!({"symbol": "SBIN", "source": "db"}),
        "history",
        week_of_bars(),
    )
    .await;
    assert_eq!(payload["source"], "db");
}

/// N-openPosition-02: a matching position row without `quantity` or `pnl`
/// reads 0 for the missing one.
#[tokio::test]
async fn an_open_position_row_without_quantity_or_pnl_reads_zero() {
    let rows = [
        (
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "pnl": 7.5}),
            json!({"status": "success", "quantity": 0, "pnl": 7.5}),
        ),
        (
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": -3}),
            json!({"status": "success", "quantity": -3, "pnl": 0}),
        ),
    ];
    for (row, want) in rows {
        let (_, out) = sent(
            "openPosition",
            json!({"symbol": "SBIN"}),
            "positionbook",
            json!({"status": "success", "data": [row]}),
        )
        .await;
        assert_eq!(out, Some(want));
    }
}

/// A context whose strategy book holds no legs.
fn pnl_ctx(client: Arc<FakeFlowClient>) -> NodeCtx {
    ctx_at(
        client,
        "2026-10-29T10:15:00",
        "Golden Flow",
        env_with(FakeBook(Ok(Vec::new())), FakeCalendar::default()),
    )
}

/// N-strategyPnl-02: a failed position book is reported with its `message`
/// when `error` is empty, and as `unavailable` when it carries neither.
#[tokio::test]
async fn a_failed_position_book_reads_message_then_unavailable() {
    let cases = [
        (
            json!({"status": "error", "error": "", "message": "Broker session expired"}),
            "Position book unavailable, cannot value open legs: Broker session expired",
        ),
        (
            json!({"status": "error", "error": null, "message": ""}),
            "Position book unavailable, cannot value open legs: unavailable",
        ),
        (
            json!({"status": "error", "error": "Rate limited", "message": "Busy"}),
            "Position book unavailable, cannot value open legs: Rate limited",
        ),
    ];
    for (book, message) in cases {
        let client = Arc::new(FakeFlowClient::new());
        client.reply("positionbook", book);
        let c = pnl_ctx(client);
        let out = run(&c, "strategyPnl", json!({"outputVariable": "pnl"})).await;
        assert_eq!(
            out.value().cloned(),
            Some(json!({"status": "error", "message": message}))
        );
        assert!(
            logs(&c)
                .iter()
                .any(|(l, m)| l == "error" && *m == format!("Strategy P&L error: {}", message)),
            "{:?}",
            logs(&c)
        );
    }
}

/// N-dataCommon-02, N-dataCommon-17: a failed reply is stored under the
/// node's output variable before it is returned, for every node that
/// stores its reply whole.
#[tokio::test]
async fn a_failed_reply_is_stored_before_it_is_returned() {
    let nodes = [
        ("symbol", json!({"symbol": "NOPE"}), "symbol"),
        ("expiry", json!({"symbol": "NOPE"}), "expiry"),
        ("intervals", json!({}), "intervals"),
        ("multiQuotes", json!({"symbols": "NOPE"}), "multiquotes"),
        ("holidays", json!({"year": 2026}), "market/holidays"),
        ("timings", json!({"date": "2026-10-29"}), "market/timings"),
    ];
    for (node, mut data, endpoint) in nodes {
        data["outputVariable"] = json!("answer");
        let failure = json!({"status": "error", "error": format!("{} failed", node), "code": 500});
        let client = Arc::new(FakeFlowClient::new());
        client.reply(endpoint, failure.clone());
        let c = ctx(client.clone());
        let out = run(&c, node, data).await;
        assert_eq!(out.value().cloned(), Some(failure.clone()), "{}", node);
        assert_eq!(c.vars.get("answer"), Some(failure), "{}", node);
        assert_eq!(
            logs(&c).last().map(|(_, m)| m.clone()),
            Some("Stored result in variable: answer".to_string()),
            "{}",
            node
        );
        assert_eq!(client.calls_to(endpoint).len(), 1, "{}", node);
    }
}
