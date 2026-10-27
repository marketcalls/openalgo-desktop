//! The web's `test/test_agent_combined_premium.py` (the candle size and the
//! frame, Q-658..Q-671) and the payoff assembly of `tools/option_viz.py`,
//! ported as pure tests; D-030 is the desktop's.

use super::payoff::{held_lots, one_underlying, payoff_message, priced_legs, Market, PayoffLeg};
use super::premium::{aligned, combine, ist_seconds, premium_spec, FixedLeg, PremiumChart};
use super::*;
use crate::agent::tools::options_common::Contract;
use std::collections::BTreeMap;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn interval(value: &str, accepted: Option<&[&str]>) -> (String, Option<String>) {
    let owned = accepted.map(list);
    premium_interval(Some(&json!(value)), owned.as_deref())
}

/// Q-658.
#[test]
fn five_minutes_is_what_an_unnamed_size_means() {
    assert_eq!(
        interval("", Some(&["1m", "5m", "15m"])),
        ("5m".into(), None)
    );
    assert_eq!(premium_interval(None, None), ("5m".into(), None));
}

/// Q-659.
#[test]
fn a_size_the_broker_serves_is_the_one_drawn() {
    assert_eq!(
        interval("15m", Some(&["1m", "5m", "15m"])),
        ("15m".into(), None)
    );
}

/// Q-660, Q-661.
#[test]
fn a_size_the_broker_does_not_serve_steps_down_and_says_so() {
    let (got, notice) = interval("3m", Some(&["1m", "5m", "15m"]));
    assert_eq!(got, "1m");
    assert_eq!(
        notice.as_deref(),
        Some("This broker does not serve '3m' candles, so the series was drawn at '1m' instead. It is a different picture of the same session.")
    );
    let (_, notice) = interval("1h", Some(&["1m", "5m"]));
    let notice = notice.unwrap_or_default();
    assert!(
        notice.contains("'1h'") && notice.contains("'1m'"),
        "{}",
        notice
    );
}

/// Q-662.
#[test]
fn case_alone_is_corrected_not_stepped_down() {
    let (got, notice) = interval("5M", Some(&["1m", "5m"]));
    assert_eq!(got, "5m");
    assert_eq!(
        notice.as_deref(),
        Some("The interval was read as '5m' rather than '5M'. Interval names are case sensitive: 'm' is minutes and 'M' is months.")
    );
}

/// Q-663.
#[test]
fn a_broker_without_minute_candles_uses_what_it_has() {
    let (got, notice) = interval("3m", Some(&["15m", "1h"]));
    assert_eq!(got, "15m");
    assert_eq!(
        notice.as_deref(),
        Some("This broker does not serve '3m' candles, so the series was drawn at '15m', the first size it does serve.")
    );
}

/// Q-664.
#[test]
fn an_unknown_listing_is_passed_through_rather_than_guessed() {
    assert_eq!(interval("7m", None), ("7m".into(), None));
    assert_eq!(interval("7m", Some(&[])), ("7m".into(), None));
}

fn chart<'a>(
    times: Vec<i64>,
    combined: Vec<Option<f64>>,
    indicators: &'a [Value],
) -> PremiumChart<'a> {
    PremiumChart {
        times,
        combined,
        title: "NIFTY 08SEP26 rolling ATM straddle, 5m".into(),
        subtitle: "Rolling ATM: the strike follows spot. 1 ATM strike used (23,850).".into(),
        axis: "Straddle premium",
        interval: "5m",
        indicators,
        source: "straddle_chart_service",
    }
}

fn times(n: usize) -> Vec<i64> {
    let start = ist_seconds(2026, 9, 3, 9, 15);
    (0..n as i64).map(|i| start + 300 * i).collect()
}

/// Q-665..Q-670: the combined series is the frame's bars, flat, drawn as a
/// line, with the studies and without the legs.
#[test]
fn the_combined_series_is_a_flat_line_of_bars() {
    let studies = vec![json!({"id": "supertrend", "inputs": {"length": 10}})];
    let combined = [120.0, 124.5, 118.25, 130.0];
    let mut notices = Vec::new();
    let spec = premium_spec(
        &chart(
            times(4),
            combined.iter().map(|v| Some(*v)).collect(),
            &studies,
        ),
        &mut notices,
    )
    .unwrap_or_default();
    let bars = spec["bars"].as_array().cloned().unwrap_or_default();
    let closes: Vec<f64> = bars.iter().filter_map(|b| b["close"].as_f64()).collect();
    assert_eq!(closes, combined);
    let stamps: Vec<i64> = bars.iter().filter_map(|b| b["time"].as_i64()).collect();
    assert_eq!(stamps, times(4));
    for bar in &bars {
        assert_eq!(bar["open"], bar["close"]);
        assert_eq!(bar["high"], bar["close"]);
        assert_eq!(bar["low"], bar["close"]);
    }
    assert_eq!(bars[1]["open"], json!(124.5));
    assert_eq!(spec["chart_type"], "line");
    assert!(!spec.contains_key("series"));
    assert_eq!(spec["indicators"], json!(studies));
    assert_eq!(spec["exchange"], "");
    assert_eq!(spec["value_label"], "Straddle premium");
    assert_eq!(spec["bar_count"], 4);
    assert_eq!(spec["timezone"], "Asia/Kolkata");
    assert!(!spec.contains_key("notices"));
}

/// Q-671: a value that did not print is absent, never zero.
#[test]
fn a_combined_value_that_did_not_print_is_absent_rather_than_zero() {
    let mut notices = Vec::new();
    let spec = premium_spec(
        &chart(times(3), vec![Some(120.0), Some(f64::NAN), None], &[]),
        &mut notices,
    )
    .unwrap_or_default();
    let closes: Vec<f64> = spec["bars"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["close"].as_f64())
        .collect();
    assert_eq!(closes, [120.0]);
}

/// L-057: at most 2,500 points, newest kept, with a notice.
#[test]
fn a_long_series_keeps_the_newest_points_and_says_so() {
    let n = MAX_PREMIUM_POINTS + 100;
    let mut notices = Vec::new();
    let spec =
        premium_spec(&chart(times(n), vec![Some(1.0); n], &[]), &mut notices).unwrap_or_default();
    assert_eq!(spec["bar_count"], MAX_PREMIUM_POINTS);
    assert_eq!(spec["bars"][0]["time"], json!(times(n)[100]));
    assert_eq!(
        notices,
        ["100 of 2600 points were older than the 2500 the chart carries, so it starts later than the range asked for."]
    );
    assert_eq!(spec["notices"], json!(notices));
}

/// D-030: a series with no finite point is not a delivery failure. The
/// web's `_deliver_premium` returned False for it, and the caller then told
/// the model the chart "could not be delivered to this surface".
#[test]
fn a_series_with_no_priced_point_is_not_a_delivery_failure() {
    let mut notices = Vec::new();
    assert!(premium_spec(
        &chart(times(2), vec![None, Some(f64::INFINITY)], &[]),
        &mut notices
    )
    .is_none());
}

/// D-030: the caller tells "no priced point" from "no sink", so the model
/// is never told a chart could not be delivered when the series was empty.
#[tokio::test]
async fn no_priced_point_is_told_apart_from_a_missing_sink() {
    use super::premium::{deliver_premium, Delivered};
    use crate::agent::contract::frames::Frame;
    use crate::agent::contract::tool::Surface;
    use crate::agent::contract::viz::VizSink;
    let app =
        crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now()).ctx;
    let mut ctx = crate::agent::tools::testing::ctx(app, Surface::Chat, Default::default());
    let empty = chart(times(2), vec![None, Some(f64::NAN)], &[]);
    assert_eq!(
        deliver_premium(&ctx, &empty, &mut Vec::new()),
        Delivered::NoPoints
    );
    assert!(ctx.viz.drain().is_empty());
    let priced = chart(times(1), vec![Some(215.75)], &[]);
    assert_eq!(
        deliver_premium(&ctx, &priced, &mut Vec::new()),
        Delivered::Drawn
    );
    let frames = ctx.viz.drain();
    assert!(matches!(&frames[..], [Frame::Viz { kind, .. }] if kind == "candles"));
    ctx.viz = VizSink::none();
    assert_eq!(
        deliver_premium(&ctx, &priced, &mut Vec::new()),
        Delivered::NoSink
    );
}

fn fixed(symbol: &str, side: &str, lots: i64) -> FixedLeg {
    FixedLeg {
        symbol: symbol.into(),
        exchange: "NFO".into(),
        side: side.into(),
        lots,
    }
}

/// T-239: legs inner-joined on the timestamp, the drop named per leg, the
/// sum signed by side and scaled by lots, rounded to 4 places.
#[test]
fn fixed_legs_join_on_time_and_sum_by_side() {
    let legs = [
        fixed("NIFTY08SEP2623850CE", "SELL", 1),
        fixed("NIFTY08SEP2623850PE", "BUY", 2),
    ];
    let closes: Vec<BTreeMap<i64, f64>> = vec![
        [(1, 100.1), (2, 101.2), (3, 102.3)].into_iter().collect(),
        [(2, 50.05), (3, 49.95)].into_iter().collect(),
    ];
    let mut notices = Vec::new();
    let (common, dropped) = aligned(&legs, &closes, &mut notices);
    assert_eq!(common, [2, 3]);
    assert_eq!(dropped, 1);
    assert_eq!(
        notices,
        ["NIFTY08SEP2623850CE printed 3 candles, 1 of which had no matching candle on every other leg and were dropped from the combined series."]
    );
    assert_eq!(combine(&legs, &closes, &common), [-1.1, -2.4]);
}

fn contract(
    symbol: &str,
    base: &str,
    segment: &str,
    strike: Option<f64>,
    option_type: &str,
) -> Contract {
    Contract {
        symbol: symbol.into(),
        exchange: "NFO".into(),
        segment: segment.into(),
        strike,
        option_type: option_type.into(),
        lot_size: 75,
        tick_size: Some(0.05),
        expiry: "08SEP26".into(),
        expiry_ts: Some(1_788_852_600),
        base: base.into(),
        underlying_exchange: "NSE_INDEX".into(),
    }
}

fn leg(c: Contract, side: &str, origin: &'static str, entry: Option<f64>) -> PayoffLeg {
    PayoffLeg {
        contract: c,
        side: side.into(),
        lots: 1,
        origin,
        entry_price: entry,
    }
}

fn call() -> Contract {
    contract(
        "NIFTY08SEP2623850CE",
        "NIFTY",
        "OPTION",
        Some(23850.0),
        "CE",
    )
}

fn put() -> Contract {
    contract(
        "NIFTY08SEP2623850PE",
        "NIFTY",
        "OPTION",
        Some(23850.0),
        "PE",
    )
}

/// T-242: one curve has one underlying, the one with most legs.
#[test]
fn one_underlying_keeps_the_base_with_most_legs() {
    let bank = contract(
        "BANKNIFTY29SEP2652000CE",
        "BANKNIFTY",
        "OPTION",
        Some(52000.0),
        "CE",
    );
    let (chosen, rest) = one_underlying(vec![
        leg(bank.clone(), "BUY", "named", None),
        leg(call(), "SELL", "named", None),
        leg(put(), "SELL", "named", None),
    ]);
    assert_eq!(chosen.len(), 2);
    assert!(chosen.iter().all(|l| l.contract.base == "NIFTY"));
    assert_eq!(rest.len(), 1);
    // A tie goes to the greater name, as Python's max over (count, name).
    let (chosen, _) = one_underlying(vec![
        leg(call(), "SELL", "named", None),
        leg(bank, "BUY", "named", None),
    ]);
    assert_eq!(chosen[0].contract.base, "NIFTY");
}

/// T-242: lots from a held quantity, Python's banker's rounding, at least one.
#[test]
fn a_held_quantity_becomes_whole_lots() {
    assert_eq!(held_lots(150.0, 75), 2);
    assert_eq!(held_lots(-75.0, 75), 1);
    assert_eq!(held_lots(112.5, 75), 2);
    assert_eq!(held_lots(187.5, 75), 2);
    assert_eq!(held_lots(30.0, 75), 1);
    assert_eq!(held_lots(30.0, 0), 1);
}

/// T-243, T-244: the StrategyLeg shape, the IV against the forward, the
/// spot, a held leg priced at its average, and the not-quoting notice.
#[test]
fn legs_are_priced_in_the_strategy_builder_shape() {
    let mut market = Market::default();
    market
        .quotes
        .insert(("NIFTY08SEP2623850CE".into(), "NFO".into()), 120.5);
    market
        .quotes
        .insert(("NIFTY".into(), "NSE_INDEX".into()), 23873.45);
    market.forwards.insert("08SEP26".into(), 23900.0);
    let legs = [
        leg(call(), "SELL", "named", None),
        leg(put(), "BUY", "position", Some(95.123_456_7)),
        leg(
            contract(
                "NIFTY08SEP2624000CE",
                "NIFTY",
                "OPTION",
                Some(24000.0),
                "CE",
            ),
            "BUY",
            "named",
            None,
        ),
    ];
    let mut notices = Vec::new();
    let (priced, spot, atm_iv) = priced_legs(
        &legs,
        &market,
        |_, f, m| {
            if f == 23900.0 && m == 120.5 {
                12.345_67
            } else {
                0.0
            }
        },
        &mut notices,
    );
    assert_eq!(spot, Some(23873.45));
    assert_eq!(atm_iv, Some(12.3457));
    assert_eq!(priced.len(), 2);
    assert_eq!(
        priced[0],
        json!({
            "id": "agent-1", "segment": "OPTION", "side": "SELL", "lots": 1, "lotSize": 75,
            "expiry": "08SEP26", "price": 120.5, "iv": 12.3457, "active": true,
            "symbol": "NIFTY08SEP2623850CE", "exchange": "NFO", "contractValid": true,
            "origin": "named", "strike": 23850.0, "optionType": "CE", "tickSize": 0.05,
            "expiryTs": 1_788_852_600, "marketPrice": 120.5, "referenceUnderlying": 23873.45,
            "forwardPrice": 23900.0,
        })
    );
    // Held: priced at its own average, no market price, no IV of its own.
    assert_eq!(priced[1]["price"], json!(95.123457));
    assert_eq!(priced[1]["iv"], json!(0.0));
    assert!(priced[1].get("marketPrice").is_none());
    assert_eq!(priced[1]["id"], "agent-2");
    assert_eq!(
        notices,
        ["NIFTY08SEP2624000CE is not quoting, so it could not be priced and is not on the curve. The diagram is therefore incomplete."]
    );
}

#[test]
fn an_option_with_no_strike_is_left_off_and_named() {
    let mut market = Market::default();
    market
        .quotes
        .insert(("NIFTY08SEP2623850CE".into(), "NFO".into()), 120.5);
    let mut notices = Vec::new();
    let (priced, spot, atm_iv) = priced_legs(
        &[leg(
            contract("NIFTY08SEP2623850CE", "NIFTY", "OPTION", None, "CE"),
            "BUY",
            "named",
            None,
        )],
        &market,
        |_, _, _| 10.0,
        &mut notices,
    );
    assert!(priced.is_empty() && spot.is_none() && atm_iv.is_none());
    assert!(notices[0].ends_with("has no strike in the instrument master, so it could not be priced and is not on the curve. The diagram is therefore incomplete."));
}

#[test]
fn the_answer_names_every_leg_and_the_spot() {
    let legs = vec![
        json!({"side": "SELL", "lots": 1, "lotSize": 75, "symbol": "NIFTY08SEP2623850CE", "price": 120.5}),
        json!({"side": "SELL", "lots": 2, "lotSize": 75, "symbol": "NIFTY08SEP2623850PE", "price": 95.25}),
    ];
    assert_eq!(
        payoff_message(&legs, "NIFTY", "the named legs", 23873.45),
        "Drew the payoff at expiry for 2 legs on NIFTY, from the named legs, priced against spot 23,873.45: SELL 1x75 NIFTY08SEP2623850CE at 120.50; SELL 2x75 NIFTY08SEP2623850PE at 95.25. The operator can see the curve, its breakevens and its maximum profit and loss, so read the shape rather than restating the legs."
    );
}

/// Q-474, Q-479: what models send for `legs` passes the schema and reaches
/// the leg reader (a float strike, a null lot count, one object, JSON
/// text), while a number where a list belongs is still refused.
#[tokio::test]
async fn legs_accept_floats_nulls_a_single_object_and_json_text() {
    use crate::agent::tools::testkit::Kit;
    let kit = Kit::new();
    for tool in ["plot_payoff", "plot_combined_premium"] {
        for legs in [
            json!([{"strike": 23850.5, "option_type": "CE", "lots": null}]),
            json!({"symbol": "NIFTY27OCT2624500CE", "side": "SELL"}),
            json!(r#"[{"symbol": "NIFTY27OCT2624500CE"}]"#),
        ] {
            let args = json!({"legs": legs, "underlying": "NIFTY", "expiry_date": "27OCT26"});
            let out = kit.call(&GROUP, tool, args).await;
            assert!(
                !out.text()
                    .contains("was not run because its arguments were refused")
                    && !out.text().contains("'legs' argument is invalid: it is"),
                "{} refused {}: {}",
                tool,
                legs,
                out.text()
            );
        }
        let out = kit.call(&GROUP, tool, json!({"legs": 123})).await;
        assert!(
            out.text()
                .contains("was not run because its arguments were refused")
                && out.text().contains("legs"),
            "{}: {}",
            tool,
            out.text()
        );
    }
}

/// Q-475: the premium chart's studies arrive as one object or as JSON text.
#[tokio::test]
async fn premium_indicators_accept_a_single_object_and_json_text() {
    let one = json!({"id": "supertrend", "inputs": {"length": 10, "multiplier": 3}});
    assert_eq!(
        normalise_indicators(json_text(Some(&one)).as_ref()).unwrap_or_default(),
        vec![one.clone()]
    );
    let text = json!(serde_json::to_string(&one).unwrap_or_default());
    assert_eq!(
        normalise_indicators(json_text(Some(&text)).as_ref()).unwrap_or_default(),
        vec![one.clone()]
    );
    let list = json!(serde_json::to_string(&json!([one])).unwrap_or_default());
    assert_eq!(
        normalise_indicators(json_text(Some(&list)).as_ref())
            .unwrap_or_default()
            .len(),
        1
    );
}
