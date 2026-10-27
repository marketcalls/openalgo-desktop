//! The live cards against the web's tests (`test_agent_order_tool_arguments.py`
//! live section) and through the mock broker: what the card carries, what
//! the model reads, and the caps.

use super::combo::{formula_text, STRUCTURES};
use super::*;
use crate::agent::contract::frames::Frame;
use crate::agent::contract::tool::Surface;
use crate::agent::contract::viz::VizSink;
use crate::agent::tools::testkit::{map, Kit};
use crate::brokers::types::Quote;
use serde_json::json;

fn quote_of(symbol: &str, exchange: &str, ltp: f64) -> Quote {
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp,
        close: ltp - 10.0,
        ..Default::default()
    }
}

fn card(sink: &VizSink, kind: &str) -> (Map<String, Value>, String) {
    let frames = sink.drain();
    assert_eq!(frames.len(), 1, "{:?}", frames);
    match frames.into_iter().next() {
        Some(Frame::Viz {
            kind: k,
            spec,
            title,
            ..
        }) => {
            assert_eq!(k, kind);
            (spec, title)
        }
        other => panic!("{:?}", other),
    }
}

#[test]
fn modes_accept_protocol_numbers() {
    for (given, want) in [
        (json!(1), "LTP"),
        (json!("1"), "LTP"),
        (json!(2), "Quote"),
        (json!(3), "Depth"),
        (json!(" 3 "), "Depth"),
        (json!("quote"), "Quote"),
        (json!(null), "Quote"),
        (json!(""), "Quote"),
    ] {
        assert_eq!(normalise_mode(Some(&given)).ok(), Some(want), "{}", given);
    }
    assert_eq!(normalise_mode(None).ok(), Some("Quote"));
    assert_eq!(
        normalise_mode(Some(&json!("FULL"))),
        Err(ToolOutcome::Retry("The 'mode' argument is invalid: 'full' is not a subscription mode Pass one of LTP, Quote, Depth (or 1, 2, 3). LTP is the last price only, Quote adds the day's open, high, low, volume and the touch, and Depth adds the order book.".into()))
    );
}

#[test]
fn a_bare_exchange_symbol_is_a_one_entry_list() {
    assert_eq!(
        symbols_argument(Some(&json!("NSE:INFY"))),
        Some(json!(["NSE:INFY"]))
    );
    assert_eq!(
        symbols_argument(Some(&json!("NSE:INFY, NSE:SBIN"))),
        Some(json!(["NSE:INFY", "NSE:SBIN"]))
    );
    let text = json!("[{\"symbol\": \"INFY\", \"exchange\": \"NSE\"}]");
    assert_eq!(symbols_argument(Some(&text)), Some(text.clone()));
    let list = json!(["NSE:INFY"]);
    assert_eq!(symbols_argument(Some(&list)), Some(list.clone()));
}

#[test]
fn small_helpers_read_as_the_web_reads_them() {
    assert_eq!(calendar_exchange("NSE_INDEX"), "NSE");
    assert_eq!(calendar_exchange("NFO"), "NFO");
    assert_eq!(
        strike_spacing(&[100.0, 150.0, 200.0, 300.0, 400.0]),
        Some(50.0)
    );
    assert_eq!(
        strike_spacing(&[100.0, 200.0, 250.0, 350.0, 400.0]),
        Some(50.0)
    );
    assert_eq!(strike_spacing(&[100.0]), None);
    for (x, s) in [
        (24500.0, "24500"),
        (0.5, "0.5"),
        (2.0, "2"),
        (24512.75, "24512.8"),
        (1_000_000.0, "1e+06"),
        (0.00001, "1e-05"),
    ] {
        assert_eq!(py_g(x), s, "{}", x);
    }
    let legs = [
        map(json!({"symbol": "A", "multiplier": 1})),
        map(json!({"symbol": "B", "multiplier": -2})),
    ];
    assert_eq!(formula_text(&legs, Some(24500.0)), "24500 + A - 2 x B");
    assert_eq!(formula_text(&legs, None), "A - 2 x B");
    assert_eq!(
        STRUCTURES.iter().map(|s| s.name).collect::<Vec<_>>(),
        [
            "basis",
            "call_ratio",
            "call_spread",
            "put_ratio",
            "put_spread",
            "straddle",
            "strangle",
            "synthetic"
        ]
    );
}

#[tokio::test]
async fn a_quotes_card_is_seeded_bounded_and_names_what_it_refused() {
    let kit = Kit::new();
    kit.mock.set_quote(quote_of("INFY", "NSE", 1520.5));
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "stream_quotes",
            json!({"symbols": "NSE:INFY, NSE:SBIN, NSE:NOSUCH", "mode": 2}),
        )
        .await;
    let (spec, title) = card(&ctx.viz, "live_quotes");
    assert_eq!(title, "Live Quote: INFY, SBIN");
    assert_eq!(spec["mode"], "Quote");
    assert_eq!(spec["account_mode"], "live");
    assert_eq!(
        spec["subscribe"],
        json!([{"symbol": "INFY", "exchange": "NSE"}, {"symbol": "SBIN", "exchange": "NSE"}])
    );
    assert_eq!(spec["instruments"][0]["seed"]["ltp"], 1520.5);
    assert_eq!(
        spec["instruments"][1]["unavailable"]["seed"],
        "the broker returned no quote for this instrument"
    );
    assert_eq!(spec["refused"][0]["symbol"], "NOSUCH");
    assert_eq!(spec["market"]["exchanges"][0]["exchange"], "NSE");
    assert_eq!(
        crate::agent::tools::testkit::tag(&out),
        "<tool_result tool=\"stream_quotes\" count=\"2\" mode=\"Quote\">"
    );
    let text = out.text();
    assert!(
        text.contains("Opened a live Quote card on 2 instruments: INFY NSE at 1,520.50; SBIN NSE."),
        "{}",
        text
    );
    assert!(text.contains("NOSUCH on NSE could not be resolved in the instrument master and is not on the card. Say so."), "{}", text);
}

#[tokio::test]
async fn depth_cards_hold_four_and_say_what_was_dropped() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chart);
    let six = json!([
        "NSE:INFY",
        "NSE:SBIN",
        "NSE:RELIANCE",
        "BSE:RELIANCE",
        "NSE_INDEX:NIFTY",
        "BSE_INDEX:SENSEX"
    ]);
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "stream_quotes",
            json!({"symbols": six, "mode": "Depth"}),
        )
        .await;
    let (spec, _) = card(&ctx.viz, "live_quotes");
    assert_eq!(spec["instruments"].as_array().map(Vec::len), Some(4));
    assert_eq!(
        spec["notices"][0],
        "2 of 6 entries were dropped, because this call carries at most 4."
    );
    assert!(spec["instruments"][0]["unavailable"]["depth"].is_string());
    assert!(
        out.text().contains(" Note: 2 of 6 entries were dropped"),
        "{}",
        out.text()
    );
}

#[tokio::test]
async fn nothing_resolved_opens_nothing_and_no_sink_claims_nothing() {
    let kit = Kit::new();
    let out = kit
        .call(&GROUP, "stream_quotes", json!({"symbols": ["NSE:NOSUCH"]}))
        .await;
    assert!(out.text().contains("None of the instruments asked for are in the instrument master (NOSUCH on NSE), so no card was opened."), "{}", out.text());
    let mut ctx = kit.ctx(Surface::Voice);
    ctx.viz = VizSink::none();
    let out = kit
        .call_in(
            &GROUP,
            &ctx,
            "stream_quotes",
            json!({"symbols": "NSE:INFY"}),
        )
        .await;
    assert!(
        out.text()
            .contains("The live quotes card could not be delivered to this surface"),
        "{}",
        out.text()
    );
}

#[tokio::test]
async fn every_documented_symbols_shape_passes_the_schema() {
    let kit = Kit::new();
    for shape in [
        json!("NSE:INFY"),
        json!("[{\"symbol\": \"INFY\", \"exchange\": \"NSE\"}]"),
        json!({"symbol": "INFY", "exchange": "NSE"}),
        json!([{"symbol": "INFY", "exchange": "NSE", "lots": 2, "note": null}]),
        json!(["NSE:INFY", {"symbol": "SBIN", "exchange": "NSE"}]),
    ] {
        let out = kit
            .call(
                &GROUP,
                "stream_quotes",
                json!({"symbols": shape, "mode": 2}),
            )
            .await;
        assert!(out.is_ok(), "{}: {:?}", shape, out);
    }
}

fn price_the_straddle(kit: &Kit) {
    kit.mock.set_quote(quote_of("NIFTY", "NSE_INDEX", 24510.0));
    kit.mock
        .set_quote(quote_of("NIFTY13OCT2624500CE", "NFO", 120.0));
    kit.mock
        .set_quote(quote_of("NIFTY13OCT2624500PE", "NFO", 100.0));
}

#[tokio::test]
async fn the_default_combo_is_the_nearest_atm_straddle() {
    let kit = Kit::new();
    price_the_straddle(&kit);
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "stream_combo", json!({"underlying": "NIFTY", "exchange": null, "structure": null, "expiry": null, "width": null}))
        .await;
    let (spec, title) = card(&ctx.viz, "live_combo");
    assert_eq!(title, "NIFTY 13OCT26 24500 straddle");
    assert_eq!(spec["underlying_exchange"], "NSE_INDEX");
    assert_eq!(spec["expiry"], "13OCT26");
    assert_eq!(
        spec["seed"],
        json!({"value": 220.0, "complete": true, "legs_priced": 2})
    );
    assert_eq!(
        spec["formula"]["expression"],
        "NIFTY13OCT2624500CE + NIFTY13OCT2624500PE"
    );
    assert_eq!(spec["atm"]["strike"], 24500.0);
    assert_eq!(spec["atm"]["roll_threshold"], 50.0);
    assert_eq!(spec["legs"][0]["role"], "atm_call");
    assert_eq!(spec["lot_size"], 75);
    assert_eq!(spec["subscribe"].as_array().map(Vec::len), Some(3));
    let text = out.text();
    assert!(
        text.starts_with(
            "<tool_result tool=\"stream_combo\" underlying=\"NIFTY\" expiry=\"13OCT26\">"
        ),
        "{}",
        text
    );
    assert!(text.contains("Opened a live card on the NIFTY 13OCT26 24500 straddle: BUY NIFTY13OCT2624500CE; BUY NIFTY13OCT2624500PE."), "{}", text);
    assert!(text.contains("It is the call and the put at the ATM strike, added together, seeded at 220.00 with spot 24,510.00."), "{}", text);
    assert!(
        text.contains("The legs are pinned at the 24500 strike. If spot moves more than 50 away"),
        "{}",
        text
    );
    assert!(
        text.contains("NIFTY was resolved to NSE_INDEX from the instrument master."),
        "{}",
        text
    );
}

#[tokio::test]
async fn a_synthetic_is_quoted_against_its_strike_and_width_is_noted() {
    let kit = Kit::new();
    price_the_straddle(&kit);
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "stream_combo", json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "structure": "Synthetic", "width": 3}))
        .await;
    let (spec, _) = card(&ctx.viz, "live_combo");
    assert_eq!(spec["formula"]["constant"], 24500.0);
    assert_eq!(spec["seed"]["value"], 24520.0);
    assert_eq!(
        spec["formula"]["expression"],
        "24500 + NIFTY13OCT2624500CE - NIFTY13OCT2624500PE"
    );
    assert!(
        out.text()
            .contains("A synthetic has no out of the money leg, so 'width' was not used."),
        "{}",
        out.text()
    );
    let refused = kit
        .call(
            &GROUP,
            "stream_combo",
            json!({"underlying": "NIFTY", "structure": "butterfly"}),
        )
        .await;
    assert_eq!(
        refused,
        ToolOutcome::Retry("The 'structure' argument is invalid: 'butterfly' is not a structure this tool builds Pass one of basis, call_ratio, call_spread, put_ratio, put_spread, straddle, strangle, synthetic, or name the contracts themselves in 'legs'.".into())
    );
    let no_options = kit
        .call(
            &GROUP,
            "stream_combo",
            json!({"underlying": "SENSEX", "exchange": "BSE_INDEX"}),
        )
        .await;
    assert!(
        no_options
            .text()
            .contains("SENSEX lists no option expiries on BFO"),
        "{:?}",
        no_options
    );
}

#[tokio::test]
async fn named_legs_take_nulls_objects_and_text() {
    let kit = Kit::new();
    price_the_straddle(&kit);
    let ctx = kit.ctx(Surface::Chat);
    let out = kit
        .call_in(&GROUP, &ctx, "stream_combo", json!({"underlying": "NIFTY", "legs": {"symbol": "NIFTY13OCT2624500CE", "side": "SELL", "lots": 2}}))
        .await;
    let (spec, title) = card(&ctx.viz, "live_combo");
    assert_eq!(title, "NIFTY 13OCT26 combination");
    assert_eq!(spec["structure"], "custom");
    assert_eq!(spec["legs"][0]["multiplier"], -2);
    assert!(out.is_ok());
    let text_legs = json!("[\"NIFTY13OCT2624500CE\", \"NIFTY13OCT2624500PE\"]");
    let out = kit
        .call_in(&GROUP, &ctx, "stream_combo", json!({"legs": text_legs}))
        .await;
    let (spec, _) = card(&ctx.viz, "live_combo");
    assert_eq!(spec["legs"].as_array().map(Vec::len), Some(2));
    assert_eq!(spec["seed"]["value"], 220.0);
    assert!(out.is_ok());
    // A shorthand leg cannot expand against an expiry keyword.
    let out = kit
        .call(
            &GROUP,
            "stream_combo",
            json!({"underlying": "NIFTY", "expiry": "current_week", "legs": ["24500CE"]}),
        )
        .await;
    assert!(
        out.text()
            .contains("which is a strike and a right with no contract in front of it"),
        "{:?}",
        out
    );
    let out = kit
        .call(
            &GROUP,
            "stream_combo",
            json!({"underlying": "NIFTY", "expiry": "13OCT26", "legs": ["24500CE"]}),
        )
        .await;
    assert!(out.is_ok(), "{:?}", out);
}

#[tokio::test]
async fn the_live_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chat);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}

#[tokio::test]
async fn a_quotes_card_holds_twelve_and_drops_the_rest_with_a_notice() {
    let kit = Kit::new();
    let ctx = kit.ctx(Surface::Chat);
    let thirteen: Vec<Value> = (0..13)
        .map(|i| json!(format!("NSE:SYM{}", i)))
        .chain(std::iter::once(json!("NSE:INFY")))
        .collect();
    let out = kit
        .call_in(&GROUP, &ctx, "stream_quotes", json!({"symbols": thirteen}))
        .await;
    // Fourteen asked for, twelve kept, none of them listed: no card.
    assert!(
        out.text()
            .contains("None of the instruments asked for are in the instrument master"),
        "{}",
        out.text()
    );
    assert!(
        out.text()
            .contains("Note: 2 of 14 entries were dropped, because this call carries at most 12."),
        "{}",
        out.text()
    );
    assert!(ctx.viz.drain().is_empty());
}
