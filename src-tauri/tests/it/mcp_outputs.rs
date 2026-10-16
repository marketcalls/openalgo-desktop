//! MCP-05: what tools return and how their arguments are read, beyond the
//! descriptors `mcp_contract` pins.
//!
//! The web's read tools return the SDK's reply, which is the `/api/v1`
//! body (`mcp/mcpserver.py`, e.g. `get_quote`:
//! `json.dumps(client.quotes(...))`), inside the trust envelope. So a read
//! tool's `data` must have the shape of the web's recorded REST body for
//! the same request (`same_shape`: keys and value types) and equal the
//! desktop's own `/api/v1` answer. Arguments are compared with the web's
//! HTTP transport, which calls the tool function with the raw arguments
//! (`blueprints/mcp_http.py`, `fn(**arguments)`).

use crate::api_v1_support::{fixture, same_shape};
use crate::mcp_support::M;
use openalgo_desktop_lib::mcp::store::TokenScope;
use serde_json::{json, Value};

/// Read tools and the web recording of the `/api/v1` call each makes.
const READS: &[(&str, &str, &str)] = &[
    (
        "get_quote",
        r#"{"symbol": "SBIN", "exchange": "NSE"}"#,
        "quotes/sbin_nse.json",
    ),
    ("get_funds", "{}", "funds/final.json"),
    ("get_order_book", "{}", "orderbook/default.json"),
    (
        "get_expiry_dates",
        r#"{"symbol": "NIFTY", "exchange": "NFO", "instrument_type": "options"}"#,
        "expiry/nifty_nfo_options.json",
    ),
    (
        "get_holidays",
        r#"{"year": 2026}"#,
        "market/holidays/year_2026.json",
    ),
];

/// The read tools answer what `/api/v1` answers, shaped as the web
/// recorded it (sandbox mode, as the recordings were made).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tools_return_the_recorded_api_shape() {
    let m = M::new().await;
    m.h.analyze(true);
    let t = m.token(TokenScope::Read);
    for (tool, args, rel) in READS {
        let args: Value = serde_json::from_str(args).unwrap();
        let out = m.output(&t, tool, args).await;
        let data = &out["data"];
        let recorded = fixture(rel);
        let req = &recorded["request"];
        let path = req["path"].as_str().unwrap();
        let (status, own) = m.h.post(path, m.h.with_key(req["body"].clone())).await;
        assert_eq!(status.as_u16(), 200, "{} {}", path, own);
        assert_eq!(data, &own, "{}: the tool's data is the /api/v1 body", tool);
        if let Err(e) = same_shape(&recorded["response"]["body"], data) {
            panic!("{} against {}: {}", tool, rel, e);
        }
    }
    m.h.shutdown().await;
}

/// Coercion, against the web's transport:
/// * a number sent as a numeric string reaches the API as the web's does,
///   and the order is placed the same way;
/// * an argument the tool does not take is refused before anything runs,
///   with the web's JSON-RPC error (its `TypeError` branch);
/// * `null` for a string the tool requires is refused without running
///   anything, but differently: the desktop answers the API's validation
///   error as the tool output, where the web's tool raises calling a string
///   method on `None` and answers "Tool execution failed". Recorded as a
///   difference; neither places or reads anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arguments_are_read_like_the_web_transport() {
    let m = M::new().await;
    m.h.analyze(true);
    let t = m.token(TokenScope::ReadWrite);
    let order = |quantity: Value| {
        json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
               "quantity": quantity, "price_type": "MARKET", "product": "MIS"})
    };
    for quantity in [json!(1), json!("1")] {
        let out = m.output(&t, "place_order", order(quantity.clone())).await;
        assert_eq!(out["data"]["status"], "success", "{}: {}", quantity, out);
    }

    let v = m
        .call(
            &t,
            "get_quote",
            json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D"}),
        )
        .await;
    assert_eq!(v["error"]["code"], -32603, "{}", v);
    assert_eq!(v["error"]["message"], "tool_error", "{}", v);
    assert_eq!(
        v["error"]["data"]["reason"],
        "Invalid arguments. Check the tool schema."
    );

    let out = m
        .output(&t, "get_quote", json!({"symbol": null, "exchange": "NSE"}))
        .await;
    assert_eq!(out["data"]["status"], "error", "{}", out);
    m.h.shutdown().await;
}
