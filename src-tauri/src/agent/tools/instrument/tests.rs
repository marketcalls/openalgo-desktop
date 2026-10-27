//! The instrument card through the in-process `/api/v1` and the mock
//! broker: what reaches the viz side channel, what the model reads, and the
//! sections that cannot be gathered.

use super::*;
use crate::agent::contract::frames::Frame;
use crate::agent::contract::tool::Surface;
use crate::agent::contract::viz::VizSink;
use crate::agent::tools::testkit::{map, Kit};
use crate::brokers::types::{Candle, DepthLevel, MarketDepth, Position, Quote};
use serde_json::json;

fn infy_quote(ltp: f64) -> Quote {
    Quote {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        ltp,
        open: 1500.0,
        high: 1530.0,
        low: 1495.0,
        close: 1510.0,
        volume: 120_000,
        ..Default::default()
    }
}

fn candles() -> Vec<Candle> {
    // Two sessions of 5 minute bars: 8 and 9 October 2026, 09:15 IST on.
    let mut out = Vec::new();
    for (day_start, n) in [(1_791_431_100_i64, 3), (1_791_517_500_i64, 4)] {
        for i in 0..n {
            out.push(Candle {
                timestamp: day_start + i * 300,
                open: 1500.0,
                high: 1505.0 + i as f64,
                low: 1490.0,
                close: 1502.0 + i as f64,
                volume: 100,
                oi: 0,
            });
        }
    }
    out
}

fn drawn(sink: &VizSink) -> Map<String, Value> {
    let frames = sink.drain();
    assert_eq!(frames.len(), 1, "{:?}", frames);
    match frames.into_iter().next() {
        Some(Frame::Viz {
            kind,
            spec,
            title,
            source,
        }) => {
            assert_eq!(kind, "instrument");
            assert_eq!(title, "INFY NSE");
            assert!(source.starts_with("quotes_service"), "{}", source);
            spec
        }
        other => panic!("{:?}", other),
    }
}

#[tokio::test]
async fn the_card_carries_every_section_and_the_model_reads_two_lines() {
    let kit = Kit::new();
    kit.mock.set_quote(infy_quote(1520.5));
    *kit.mock.history.lock() = Some(Ok(candles()));
    *kit.mock.depth.lock() = Some(Ok(MarketDepth {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        bids: vec![
            DepthLevel {
                price: 1520.0,
                quantity: 10,
                orders: 1,
            },
            DepthLevel::default(),
        ],
        asks: vec![DepthLevel {
            price: 1521.0,
            quantity: 5,
            orders: 1,
        }],
        total_buy_qty: 10,
        total_sell_qty: 5,
        ..Default::default()
    }));
    *kit.mock.positions.lock() = Some(Ok(vec![Position {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: 10,
        overnight_quantity: 0,
        average_price: 1500.0,
        ltp: 1520.5,
        pnl: 205.0,
        realized_pnl: 0.0,
        unrealized_pnl: 205.0,
        buy_quantity: 10,
        buy_value: 15000.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }]));
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "show_instrument",
            json!({"symbol": "infy", "exchange": "NSE", "days": 2}),
        )
        .await;
    let spec = drawn(&ctx.viz);
    assert_eq!(spec["mode"], "live");
    assert_eq!(spec["quote"]["ltp"], 1520.5);
    assert_eq!(spec["quote"].get("oi"), None, "a zero is an absence");
    assert_eq!(spec["change"], 10.5);
    assert_eq!(spec["change_percent"], 0.7);
    assert_eq!(spec["instrument"]["lot_size"], 1);
    assert_eq!(
        spec["intraday"]["sessions"],
        json!(["2026-10-08", "2026-10-09"])
    );
    assert_eq!(spec["intraday"]["bar_count"], 7);
    assert_eq!(spec["intraday"]["has_volume"], true);
    assert_eq!(
        spec["depth"]["bids"],
        json!([{"price": 1520.0, "quantity": 10.0}])
    );
    assert_eq!(spec["depth"]["spread"], 1.0);
    assert_eq!(spec["position"]["held"], true);
    assert_eq!(spec["position"]["side"], "long");
    assert_eq!(spec["position"]["pnl_percent"], 1.37);
    assert_eq!(spec["week_52"]["full_year"], false);
    assert_eq!(
        out.text(),
        "<tool_result tool=\"show_instrument\" symbol=\"INFY\" exchange=\"NSE\">\nINFY on NSE is 1,520.50, up 10.50 (0.70 percent) from the previous close of 1,510.00. Day 1,495.00 to 1,530.00. The operator is long 10 at 1,500.00, unrealised P and L 205.00. The card is on the operator's screen with the intraday chart, the 52 week range and the order book, so describe what it shows rather than listing its numbers. It carries no fundamentals because this platform has none; do not add any.\n</tool_result>"
    );
}

#[tokio::test]
async fn a_missing_section_is_named_not_drawn() {
    let kit = Kit::new();
    kit.mock.set_quote(infy_quote(1520.5));
    kit.analyzer(true);
    let ctx = kit.ctx(Surface::Voice);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "show_instrument",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    let spec = drawn(&ctx.viz);
    assert_eq!(spec["mode"], "analyze");
    let unavailable = spec["unavailable"].as_object().cloned().unwrap_or_default();
    assert!(
        unavailable.contains_key("intraday") && unavailable.contains_key("depth"),
        "{:?}",
        unavailable
    );
    assert_eq!(
        unavailable["depth"],
        "there are no resting orders on either side"
    );
    assert!(spec.get("intraday").is_none() && spec.get("depth").is_none());
    assert_eq!(spec["position"], json!({"held": false}));
    let text = out.text();
    assert!(
        text.contains("Analyzer mode is on, so the position is a sandbox one; say so."),
        "{}",
        text
    );
    assert!(
        text.contains("Not on the card: depth, intraday, week_52."),
        "{}",
        text
    );
}

#[tokio::test]
async fn no_price_no_card_and_no_sink_no_claim() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "show_instrument",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert!(
        out.text().starts_with("quotes_service.get_quotes failed"),
        "{:?}",
        out
    );
    kit.mock.set_quote(infy_quote(1520.5));
    let mut ctx = kit.ctx(Surface::Chat);
    ctx.viz = VizSink::none();
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "show_instrument",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert!(out.text().contains("The instrument card could not be delivered to this surface, so nothing was drawn. Answer in prose and do not tell the operator that a instrument card is on screen."), "{}", out.text());
    let refused = kit
        .call(
            &GROUP,
            "show_instrument",
            json!({"symbol": "INFY", "exchange": "NSE", "days": 9}),
        )
        .await;
    assert_eq!(
        refused,
        ToolOutcome::Retry("The 'days' argument is invalid: 9 is not a whole number between 1 and 5. Pass an integer in that range; the default days is 1.".into())
    );
}

#[test]
fn quotes_moves_and_ladders_read_as_the_web_reads_them() {
    let q = normalise_quote(&json!({"ltp": 0, "open": 0, "high": "12.5", "oi": 0, "extra": 5}));
    assert_eq!(Value::Object(q.clone()), json!({"ltp": 0.0, "high": 12.5}));
    let mut notices = Vec::new();
    let mv = quote_move(
        &map(json!({"ltp": 0.0, "prev_close": 10.0})),
        &mut notices,
        "SBIN",
    );
    assert!(mv.is_empty());
    assert_eq!(notices, vec!["SBIN's last traded price is zero, so no trade has printed yet and the day's change is not stated."]);
    assert!(quote_move(&map(json!({"ltp": 5.0})), &mut Vec::new(), "").is_empty());
    assert_eq!(percent_change(Some(1.0), Some(0.0)), None);
    assert_eq!(percent_change(Some(1.0), Some(3.0)), Some(33.33));
    let ladder: Vec<Value> = (0..7)
        .map(|i| json!({"price": 100 + i, "quantity": 1}))
        .collect();
    assert_eq!(depth_levels(Some(&Value::Array(ladder))).len(), 5);
    assert_eq!(
        depth_levels(Some(
            &json!([{"price": 0, "quantity": 0}, {"price": 1, "quantity": 0}])
        ))
        .len(),
        1
    );
    assert_eq!(
        reason(&ToolOutcome::Retry(
            "history_service.get_history failed (HTTP 500): boom. Retry at most once.".into()
        )),
        "history_service.get_history failed (HTTP 500): boom."
    );
}

#[test]
fn a_position_aggregate_needs_every_leg() {
    let two = position_summary(vec![
        map(json!({"product": "MIS", "quantity": 10.0, "pnl": 5.0, "average_price": 100.0})),
        map(json!({"product": "NRML", "quantity": -4.0, "average_price": 90.0})),
    ]);
    assert_eq!(two["quantity"], 6.0);
    assert_eq!(two["side"], "long");
    assert!(two.get("pnl").is_none() && two.get("average_price").is_none());
}

#[tokio::test]
async fn the_instrument_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chart);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}
