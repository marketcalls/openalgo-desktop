//! 5paisa XTS reads XTS answers as the XTS v2 docs describe them (web
//! 5749d4126, marketcalls/openalgo#2504): the open position by instrument,
//! trade fills, every documented order status and what cancel-all cancels,
//! cancel and modify answers, the consolidated funds entry, the F&O master
//! layout, no `1s` interval, and the touchline of a 1501 event. Ported from
//! web test/test_fivepaisaxts_xts_docs.py.
//!
//! Where the desktop keeps a rule of its own the test says so: a position
//! book that cannot be read refuses the smart order (the web reads it as 0
//! and refuses through `refuse_smart_order_on_read_failure`).

use super::fake::{broker, history, master, token};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use openalgo_desktop_lib::brokers::families::xts::streaming::normalise_message;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::Broker;
use serde_json::{json, Value};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Open position (web: matched by instrument, segment and product)
// ---------------------------------------------------------------------------

fn position(overrides: Value) -> Value {
    let mut p = json!({
        "TradingSymbol": "NIFTY 27OCT2026 CE 22400",
        "ExchangeSegment": "NSEFO",
        "ExchangeInstrumentId": "48225",
        "ProductType": "NRML",
        "Quantity": "-75",
    });
    if let (Some(p), Some(o)) = (p.as_object_mut(), overrides.as_object()) {
        for (k, v) in o {
            p.insert(k.clone(), v.clone());
        }
    }
    p
}

async fn open_position(book: Value) -> openalgo_desktop_lib::error::Result<i64> {
    let (b, fake, _) = broker().await;
    *fake.positions.lock() = book;
    b.get_open_position(
        &token(),
        "NIFTY27OCT2622400CE",
        Exchange::Nfo,
        Product::Nrml,
    )
    .await
}

fn positions(rows: Vec<Value>) -> Value {
    json!({"type": "success", "result": {"positionList": rows}})
}

/// Web `test_open_position_matches_the_instrument_not_the_name` and
/// `test_open_position_accepts_the_docs_spelling_of_the_instrument_id`.
#[tokio::test]
async fn open_position_matches_the_instrument_not_the_name() {
    assert_eq!(
        open_position(positions(vec![position(json!({}))]))
            .await
            .unwrap(),
        -75
    );
    let mut docs = position(json!({}));
    let id = docs["ExchangeInstrumentId"].take();
    docs.as_object_mut().unwrap().remove("ExchangeInstrumentId");
    docs["ExchangeInstrumentID"] = id;
    assert_eq!(open_position(positions(vec![docs])).await.unwrap(), -75);
}

/// Web `test_open_position_ignores_other_rows`.
#[tokio::test]
async fn open_position_ignores_other_rows() {
    for other in [
        json!({"ProductType": "MIS"}),
        json!({"ExchangeSegment": "BSEFO"}),
        json!({"ExchangeInstrumentId": "99999"}),
    ] {
        let book = positions(vec![position(other.clone())]);
        assert_eq!(open_position(book).await.unwrap(), 0, "{other}");
    }
}

/// Web `test_open_position_reads_zero_from_an_error_answer`: the web reads
/// 0 there and refuses the smart order on the failed read; the desktop's
/// read refuses it itself (a book that cannot be read is never flat).
#[tokio::test]
async fn open_position_refuses_an_error_answer() {
    let read = open_position(json!({"type": "error", "description": "x"})).await;
    assert!(
        read.is_err(),
        "an error answer is not a flat position: {read:?}"
    );
}

// ---------------------------------------------------------------------------
// Trade book: each row is one fill
// ---------------------------------------------------------------------------

async fn trades(rows: Vec<Value>) -> Vec<Trade> {
    let (b, fake, _) = broker().await;
    *fake.trades.lock() = json!({"type": "success", "result": rows});
    b.get_trade_book(&token()).await.unwrap()
}

/// Web `test_trade_book_row_is_one_fill`.
#[tokio::test]
async fn trade_book_row_is_one_fill() {
    let fill = json!({
        "TradingSymbol": "SBIN",
        "ExchangeSegment": "NSECM",
        "OrderSide": "BUY",
        "ProductType": "MIS",
        "AppOrderID": "1100126316",
        "OrderQuantity": 100,
        "OrderAverageTradedPrice": 801.0,
        "LastTradedQuantity": 40,
        "LastTradedPrice": 800.5,
        "LastExecutionTransactTime": "17-11-2026 10:00:05",
        "OrderGeneratedDateTime": "17-11-2026 10:00:00",
    });
    let t = trades(vec![fill]).await;
    assert_eq!(t[0].quantity, 40);
    assert_eq!(t[0].average_price, 800.5);
    assert_eq!(t[0].trade_value, 40.0 * 800.5);
    assert_eq!(t[0].timestamp, "17-11-2026 10:00:05");
}

/// Web `test_trade_book_row_without_fill_fields_keeps_order_fields`.
#[tokio::test]
async fn trade_book_row_without_fill_fields_keeps_order_fields() {
    let order = json!({
        "ExchangeSegment": "NSECM",
        "AppOrderID": "1",
        "OrderQuantity": 10,
        "OrderAverageTradedPrice": 5.0,
        "OrderGeneratedDateTime": "t",
    });
    let t = trades(vec![order]).await;
    assert_eq!(
        (t[0].quantity, t[0].average_price, t[0].timestamp.as_str()),
        (10, 5.0, "t")
    );
}

// ---------------------------------------------------------------------------
// Order status
// ---------------------------------------------------------------------------

/// Web `test_order_status_maps_every_documented_state` and
/// `test_order_statistics_count_part_filled_and_modified_orders_as_open`
/// (the desktop's statistics count the book's `open` rows).
#[tokio::test]
async fn order_status_maps_every_documented_state() {
    let (b, fake, _) = broker().await;
    let cases = [
        ("New", "open"),
        ("PartiallyFilled", "open"),
        ("Replaced", "open"),
        ("PendingNew", "open"),
        ("PendingReplace", "open"),
        ("PendingCancel", "open"),
        ("Filled", "complete"),
        ("Rejected", "rejected"),
        ("Cancelled", "cancelled"),
    ];
    let rows: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (status, _))| json!({"OrderStatus": status, "AppOrderID": i + 1}))
        .collect();
    *fake.orders.lock() = json!({"type": "success", "result": rows});
    let book = b.get_order_book(&token()).await.unwrap();
    for ((status, expected), order) in cases.iter().zip(&book) {
        assert_eq!(order.status, *expected, "{status}");
    }
    let open = book.iter().filter(|o| o.status == "open").count();
    assert_eq!(open, 6);
}

/// Web `test_cancel_all_cancels_modified_and_part_filled_orders`.
#[tokio::test]
async fn cancel_all_cancels_modified_and_part_filled_orders() {
    let (b, fake, _) = broker().await;
    *fake.orders.lock() = json!({"type": "success", "result": [
        {"AppOrderID": 1, "OrderStatus": "New"},
        {"AppOrderID": 2, "OrderStatus": "Replaced"},
        {"AppOrderID": 3, "OrderStatus": "PartiallyFilled"},
        {"AppOrderID": 4, "OrderStatus": "PendingCancel"},
        {"AppOrderID": 5, "OrderStatus": "Filled"},
    ]});
    *fake.answer.lock() = json!({"type": "success", "result": {}});
    let r = b.cancel_all_orders(&token()).await.unwrap();
    assert_eq!(r.cancelled, ["1", "2", "3"]);
    assert!(r.failed.is_empty());
}

// ---------------------------------------------------------------------------
// Cancel and modify read XTS's {"type": ...} answer
// ---------------------------------------------------------------------------

/// Web `test_cancel_reads_the_xts_success_answer`.
#[tokio::test]
async fn cancel_reads_the_xts_success_answer() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() = json!({"type": "success", "result": [{"AppOrderID": 7}]});
    let r = b.cancel_order(&token(), "7").await.unwrap();
    assert_eq!(r.order_id, "7");
    assert_eq!(fake.lines("cancel|"), ["cancel|appOrderID=7"]);
}

/// Web `test_cancel_reports_the_xts_reason`.
#[tokio::test]
async fn cancel_reports_the_xts_reason() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() = json!({"type": "error", "description": "Order is already cancelled"});
    let e = b.cancel_order(&token(), "7").await.unwrap_err();
    assert_eq!(e.client_message(), "Order is already cancelled");
}

/// Web `test_modify_reads_the_xts_success_answer`: a 20-digit order id
/// comes back digit for digit.
#[tokio::test]
async fn modify_reads_the_xts_success_answer() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() =
        json!({"type": "success", "result": {"AppOrderID": 10001253450000041478u64}});
    let m = ResolvedModify::resolve(
        "10001253450000041478",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "LIMIT".into(),
            quantity: 1,
            price: 2500.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&token(), &m).await.unwrap();
    assert_eq!(r.order_id, "10001253450000041478");
}

// ---------------------------------------------------------------------------
// Funds: the ALL|ALL|ALL entry
// ---------------------------------------------------------------------------

fn balance_entry(header: &str, cash: f64) -> Value {
    json!({
        "limitHeader": header,
        "limitObject": {"RMSSubLimits": {
            "netMarginAvailable": cash,
            "collateral": 0,
            "UnrealizedMTM": 0,
            "RealizedMTM": 0,
            "marginUtilized": 0,
        }},
    })
}

/// Web `test_funds_use_the_consolidated_balance_entry`, and a `BalanceList`
/// sent as one entry (web funds.py).
#[tokio::test]
async fn funds_use_the_consolidated_balance_entry() {
    let (b, fake, _) = broker().await;
    *fake.balance.lock() = json!({"type": "success", "result": {"BalanceList": [
        balance_entry("CASH|NSE|MTF", 0.0),
        balance_entry("ALL|ALL|ALL", 12500.5),
    ]}});
    assert_eq!(b.get_funds(&token()).await.unwrap().available_cash, 12500.5);
    *fake.balance.lock() = json!({"type": "success",
        "result": {"BalanceList": balance_entry("ALL|ALL|ALL", 300.25)}});
    assert_eq!(b.get_funds(&token()).await.unwrap().available_cash, 300.25);
}

// ---------------------------------------------------------------------------
// Master contract: futures and spreads in their own layout
// ---------------------------------------------------------------------------

/// Web `test_futures_rows_are_read_in_their_own_layout`: a future is FUT at
/// strike 0 (it was a put at strike 1 before, typed from the
/// PriceNumerator column), the spread is not a contract.
#[tokio::test]
async fn futures_rows_are_read_in_their_own_layout() {
    let (b, _, _) = broker().await;
    let rows = b.download_master_contract(&token()).await.unwrap();
    let nfo: HashMap<&str, &SymToken> = rows
        .iter()
        .filter(|r| r.exchange == "NFO")
        .map(|r| (r.token.as_str(), r))
        .collect();
    let mut tokens: Vec<&str> = nfo.keys().copied().collect();
    tokens.sort_unstable();
    assert_eq!(tokens, ["48225", "49229"], "the spread is not a contract");
    let future = nfo["49229"];
    assert_eq!(
        (
            future.symbol.as_str(),
            future.instrument_type.as_str(),
            future.strike
        ),
        ("NIFTY27JAN26FUT", "FUT", 0.0)
    );
    assert_eq!(future.lot_size, 65);
    let option = nfo["48225"];
    assert_eq!(
        (
            option.symbol.as_str(),
            option.instrument_type.as_str(),
            option.strike
        ),
        ("NIFTY17FEB2625700PE", "PE", 25700.0)
    );
}

// ---------------------------------------------------------------------------
// History intervals
// ---------------------------------------------------------------------------

/// Web #2504: XTS's smallest OHLC compression is 60, so 5paisa XTS offers no
/// `1s` and refuses it.
#[tokio::test]
async fn there_is_no_one_second_interval() {
    let (b, fake, _) = broker().await;
    assert!(b.timeframe_map().iter().all(|(k, _)| *k != "1s"));
    assert_eq!(b.timeframe_map().first(), Some(&("1m", "60")));
    let e = b
        .get_history(&token(), &history("1s", (2024, 1, 1), (2024, 1, 2)))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
    assert!(fake.lines("ohlc|").is_empty());
}

// ---------------------------------------------------------------------------
// Streaming: the touchline whatever the message code
// ---------------------------------------------------------------------------

/// Web `test_streaming_quote_reads_touchline_for_1501`.
#[test]
fn streaming_quote_reads_touchline_for_1501() {
    let message = json!({
        "MessageCode": 1501,
        "Touchline": {"LastTradedPrice": 1723.0, "Open": 1696.8, "TotalTradedQuantity": 189238},
    });
    let sub = FeedSubscription {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        token: "2885".into(),
        brsymbol: "RELIANCE-EQ".into(),
        brexchange: "NSECM".into(),
        mode: FeedMode::Quote,
        depth: 5,
    };
    let events = normalise_message(&message, &sub);
    let Some(FeedEvent::Tick(t)) = events.first() else {
        panic!("no tick: {events:?}");
    };
    assert_eq!((t.ltp, t.open, t.volume), (1723.0, 1696.8, 189238));
}
