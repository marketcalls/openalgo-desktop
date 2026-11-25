//! XTS order ids kept exact and the traded average of a carried position
//! (web test/test_xts_order_data.py, marketcalls/openalgo#2468, issues
//! #2462 and #633). The web runs each of its seven XTS plugins' own copy of
//! the mapping; the desktop's members share `mapping`, so each case runs
//! once. A position read that fails is driven against the fake XTS in
//! `tests/it/brokers_xts.rs`.

use super::mapping::{order_id, orders, position_list, positions, trades};
use super::XtsHooks;
use crate::brokers::common::symbols::SymbolResolver;
use serde_json::{json, Value};

/// Three consecutive Wisdom ids from issue #2462; read through a float all
/// three were 11001263160000030720.
const IDS: [&str; 3] = [
    "11001263160000030116",
    "11001263160000030117",
    "11001263160000030118",
];

fn book_row(app_order_id: Value) -> Value {
    json!({
        "TradingSymbol": "SBIN",
        "ExchangeSegment": "NSECM",
        "OrderSide": "BUY",
        "OrderQuantity": 1,
        "OrderPrice": 800.0,
        "OrderStopPrice": 0.0,
        "OrderType": "Limit",
        "OrderStatus": "New",
        "ProductType": "MIS",
        "AppOrderID": app_order_id,
        "LastUpdateDateTime": "17-11-2026 10:00:00",
        "OrderAverageTradedPrice": 800.0,
        "OrderGeneratedDateTime": "17-11-2026 10:00:00",
    })
}

/// The ids as XTS sends them: JSON integers (parsed, not built from a
/// float) or strings.
fn ids(as_json_number: bool) -> Vec<Value> {
    IDS.iter()
        .map(|i| {
            if as_json_number {
                serde_json::from_str(i).unwrap()
            } else {
                json!(i)
            }
        })
        .collect()
}

/// Web `test_exact_order_id_keeps_every_digit`.
#[test]
fn exact_order_id_keeps_every_digit() {
    for as_json_number in [true, false] {
        let read: Vec<String> = ids(as_json_number)
            .iter()
            .map(|v| order_id(Some(v)))
            .collect();
        assert_eq!(read, IDS, "as a JSON number: {as_json_number}");
    }
}

/// Web `test_exact_order_id_edges`. A missing or blank id stays empty
/// here, not the web's "0": an empty id is how a placement answered without
/// one is told apart (`orders::place_raw`, an uncertain placement), and no
/// cancel or modify could use "0".
#[test]
fn exact_order_id_edges() {
    for (value, expected) in [
        (None, ""),
        (Some(json!("")), ""),
        (Some(json!(0)), "0"),
        (Some(json!("1234.0")), "1234"),
        (Some(json!(" 42 ")), "42"),
        (Some(json!("ABC-1")), "ABC-1"),
        // A JSON float that is a whole number, as XTS sent ids before.
        (Some(json!(1200002.0)), "1200002"),
        (Some(json!("0042")), "42"),
        (Some(json!("12.5")), "12.5"),
    ] {
        assert_eq!(order_id(value.as_ref()), expected, "{value:?}");
    }
}

/// Web `test_the_order_book_keeps_each_orders_own_id` and
/// `test_the_trade_book_keeps_each_trades_own_id`.
#[test]
fn the_order_and_trade_books_keep_each_orders_own_id() {
    let rows = Value::Array(ids(true).into_iter().map(book_row).collect());
    let symbols = SymbolResolver::new();
    let book: Vec<String> = orders(&rows, &symbols, &XtsHooks::default())
        .into_iter()
        .map(|o| o.order_id)
        .collect();
    assert_eq!(book, IDS);
    let traded: Vec<String> = trades(&rows, &symbols, &XtsHooks::default())
        .into_iter()
        .map(|t| t.order_id)
        .collect();
    assert_eq!(traded, IDS);
}

fn carried(quantity: i64, prices: Value) -> Value {
    let mut p = json!({
        "TradingSymbol": "HAL 23NOV2026 PE 5000",
        "ExchangeSegment": "NSEFO",
        "ExchangeInstrumentId": "41880",
        "ProductType": "NRML",
        "Quantity": quantity.to_string(),
    });
    for (k, v) in prices.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

fn answer(rows: Vec<Value>) -> Value {
    json!({
        "type": "success",
        "code": "s-portfolio-0009",
        "description": "Success get position",
        "result": {"positionList": rows},
    })
}

/// Web `test_a_position_shows_its_traded_average` (the web formats the
/// average as "154.65"; the desktop's number is rounded to the same two
/// places).
#[test]
fn a_position_shows_its_traded_average() {
    let symbols = SymbolResolver::new();
    for (case, row, average) in [
        // A long carried overnight (issue #2462's HAL row): the traded
        // average, not the previous close XTS marked it to.
        (
            "carried long",
            carried(
                75,
                json!({"BuyAveragePrice": "244", "ActualBuyAveragePrice": 154.65}),
            ),
            154.65,
        ),
        (
            "carried short",
            carried(
                -300,
                json!({"SellAveragePrice": "120.5", "ActualSellAveragePrice": 116.075}),
            ),
            116.08,
        ),
        // A deployment that does not send the Actual fields keeps the
        // documented one.
        (
            "no actual field",
            carried(75, json!({"BuyAveragePrice": "79.7"})),
            79.7,
        ),
        // An Actual field of zero is not a price.
        (
            "actual zero",
            carried(
                75,
                json!({"BuyAveragePrice": "79.7", "ActualBuyAveragePrice": 0}),
            ),
            79.7,
        ),
        // A flat position has no average.
        (
            "flat",
            carried(
                0,
                json!({"BuyAveragePrice": "18.34", "ActualBuyAveragePrice": 18.34}),
            ),
            0.0,
        ),
    ] {
        let reply = answer(vec![row]);
        let rows = position_list(&reply["result"]).unwrap();
        let book = positions(rows, &symbols);
        assert_eq!(book.len(), 1, "{case}");
        assert_eq!(book[0].average_price, average, "{case}");
    }
}

/// Web `test_an_empty_book_is_still_empty`.
#[test]
fn an_empty_book_is_still_empty() {
    let reply = answer(vec![]);
    let rows = position_list(&reply["result"]).unwrap();
    assert!(positions(rows, &SymbolResolver::new()).is_empty());
}

/// Web `test_the_issue_633_answer_reads_its_positions`: the live Wisdom
/// NetWise answer quoted on issue #633, one row.
#[test]
fn the_issue_633_answer_reads_its_positions() {
    let reply = answer(vec![json!({
        "TradingSymbol": "NIFTY 16DEC2025 CE 25950",
        "ExchangeSegment": "NSEFO",
        "ExchangeInstrumentId": "48222",
        "ProductType": "NRML",
        "BuyAveragePrice": "0",
        "SellAveragePrice": "116.075",
        "Quantity": "-300",
        "ActualSellAveragePrice": 116.075,
        "ActualBuyAveragePrice": 0,
    })]);
    let rows = position_list(&reply["result"]).unwrap();
    let book = positions(rows, &SymbolResolver::new());
    assert_eq!(book.len(), 1);
    let p = &book[0];
    assert_eq!(
        (p.symbol.as_str(), p.exchange.as_str(), p.quantity),
        ("NIFTY 16DEC2025 CE 25950", "NFO", -300)
    );
    assert_eq!(p.average_price, 116.08);
}
