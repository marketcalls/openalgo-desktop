//! Web `test/sandbox/test_charges.py`, case for case, and the published
//! calculator's figures (`tests/fixtures/sandbox/charges_reference.json`,
//! the web's own file): 117 round trips read on 23 November 2026. 103 match
//! on every line; the other fourteen are pinned by name with their reason.

use super::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// The plan the reference calculator prices under.
fn reference_plan(segment: &str) -> (Decimal, Decimal) {
    match segment {
        "equity_delivery" => (dec!(0), dec!(0)),
        "equity_intraday" | "fno_futures" | "currency_futures" | "commodity_futures" => {
            (dec!(20), dec!(0.03))
        }
        "fno_options" | "currency_options" | "commodity_options" => (dec!(20), dec!(0)),
        other => panic!("no reference plan for {other}"),
    }
}

fn round_trip(segment: &str, exchange: &str, buy: &str, sell: &str, qty: &str) -> RoundTrip {
    let (flat, percent) = reference_plan(segment);
    estimate_round_trip(segment, exchange, d(buy), d(sell), d(qty), flat, percent)
        .expect("a trade within range")
}

fn reference() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/sandbox/charges_reference.json"
    ))
    .unwrap()
}

fn s(t: &Value, k: &str) -> String {
    t[k].as_str().unwrap().to_string()
}

fn name(t: &Value) -> String {
    format!(
        "{} {} {}/{} x {}",
        s(t, "segment"),
        s(t, "exchange"),
        s(t, "buy"),
        s(t, "sell"),
        s(t, "quantity")
    )
}

fn priced(t: &Value) -> Charges {
    estimate_round_trip(
        &s(t, "segment"),
        &s(t, "exchange"),
        d(&s(t, "buy")),
        d(&s(t, "sell")),
        d(&s(t, "quantity")),
        d(&s(t, "flat")),
        d(&s(t, "percent")),
    )
    .expect("a trade within range")
    .total
}

fn line(c: &Charges, name: &str) -> Decimal {
    match name {
        "brokerage" => c.brokerage,
        "stt" => c.stt,
        "exchange_charges" => c.exchange_charges,
        "sebi_charges" => c.sebi_charges,
        "stamp_duty" => c.stamp_duty,
        "gst" => c.gst,
        "total" => c.total(),
        other => panic!("unknown line {other}"),
    }
}

/// The lines where this module and the published calculator disagree:
/// `(ours, theirs)`.
fn differences(t: &Value) -> BTreeMap<String, (Decimal, Decimal)> {
    let c = priced(t);
    let mut out = BTreeMap::new();
    for (l, shown) in t["shown"].as_object().unwrap() {
        let ours = line(&c, l);
        let theirs = d(shown.as_str().unwrap());
        if ours != theirs {
            out.insert(l.clone(), (ours, theirs));
        }
    }
    out
}

/// A paisa over on one line, the exact figure a half paisa: the line and the
/// exact figure.
const HALF_PAISA: &[(&str, &str, &str)] = &[
    ("equity_intraday NSE 1000/1001 x 50", "brokerage", "30.015"),
    ("equity_intraday NSE 1000/1010 x 5", "brokerage", "3.015"),
    ("fno_futures NFO 1000/1001 x 50", "brokerage", "30.015"),
    ("fno_futures NFO 1000/1010 x 5", "brokerage", "3.015"),
    (
        "fno_options BFO 0.85/1.15 x 6500",
        "exchange_charges",
        "4.225",
    ),
    (
        "fno_options BFO 18.75/19.2 x 12000",
        "exchange_charges",
        "148.005",
    ),
    (
        "commodity_futures MCX 5425/5460 x 3000",
        "exchange_charges",
        "685.755",
    ),
    ("commodity_options MCX 4.35/5.1 x 7500", "stt", "19.125"),
];

fn is_half_paisa(t: &Value) -> bool {
    HALF_PAISA.iter().any(|(n, _, _)| *n == name(t))
}

fn is_levy_case(t: &Value) -> bool {
    s(t, "segment") == "currency_futures" && s(t, "exchange") == "CDS"
}

/// Web test_charges.py::test_the_reference_file_is_the_one_this_was_checked_against
#[test]
fn the_reference_file_is_the_one_this_was_checked_against() {
    let r = reference();
    assert_eq!(r["read_on"], "2026-11-23");
    let trades = r["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 117);
    let mut by_market: BTreeMap<(String, String), usize> = BTreeMap::new();
    for t in trades {
        *by_market
            .entry((s(t, "segment"), s(t, "exchange")))
            .or_default() += 1;
    }
    let want: BTreeMap<(String, String), usize> = [
        ("equity_intraday", "NSE", 16),
        ("equity_delivery", "NSE", 16),
        ("fno_futures", "NFO", 16),
        ("fno_options", "NFO", 16),
        ("fno_options", "BFO", 16),
        ("currency_futures", "CDS", 6),
        ("currency_futures", "BCD", 6),
        ("currency_options", "CDS", 6),
        ("currency_options", "BCD", 6),
        ("commodity_futures", "MCX", 5),
        ("commodity_futures", "NCO", 2),
        ("commodity_options", "MCX", 5),
        ("commodity_options", "NCO", 1),
    ]
    .into_iter()
    .map(|(a, b, n)| ((a.to_string(), b.to_string()), n))
    .collect();
    assert_eq!(by_market, want);
    for t in trades {
        assert_eq!(
            (d(&s(t, "flat")), d(&s(t, "percent"))),
            reference_plan(&s(t, "segment"))
        );
    }
}

/// Web test_charges.py::test_a_round_trip_shows_the_lines_the_published_calculator_shows
#[test]
fn a_round_trip_shows_the_lines_the_published_calculator_shows() {
    for t in reference()["trades"].as_array().unwrap() {
        if is_half_paisa(t) || is_levy_case(t) {
            continue;
        }
        assert_eq!(differences(t), BTreeMap::new(), "{}", name(t));
    }
}

/// Web test_charges.py::test_all_but_fourteen_of_the_reference_trades_match_on_every_line
#[test]
fn all_but_fourteen_of_the_reference_trades_match_on_every_line() {
    let r = reference();
    let trades = r["trades"].as_array().unwrap();
    let matching = trades.iter().filter(|t| differences(t).is_empty()).count();
    let differing: BTreeSet<String> = trades
        .iter()
        .filter(|t| !differences(t).is_empty())
        .map(name)
        .collect();
    assert_eq!(matching, 103);
    let mut want: BTreeSet<String> = HALF_PAISA.iter().map(|(n, _, _)| n.to_string()).collect();
    want.extend(trades.iter().filter(|t| is_levy_case(t)).map(name));
    assert_eq!(differing, want);
}

/// Web test_charges.py::test_a_half_paisa_is_rounded_up_where_the_published_calculator_rounds_it_down
#[test]
fn a_half_paisa_is_rounded_up_where_the_published_calculator_rounds_it_down() {
    let r = reference();
    let trades = r["trades"].as_array().unwrap();
    for (n, l, exact) in HALF_PAISA {
        let t = trades.iter().find(|t| name(t) == *n).unwrap();
        let diffs = differences(t);
        let keys: BTreeSet<&str> = diffs.keys().map(String::as_str).collect();
        let a: BTreeSet<&str> = [*l, "total"].into_iter().collect();
        let b: BTreeSet<&str> = [*l, "gst", "total"].into_iter().collect();
        assert!(keys == a || keys == b, "{n}: {keys:?}");
        let (ours, theirs) = diffs[*l];
        assert_eq!(d(exact) - theirs, dec!(0.005), "{n}");
        assert_eq!(ours - d(exact), dec!(0.005), "{n}");
        let over: Decimal = diffs
            .iter()
            .filter(|(k, _)| k.as_str() != "total")
            .map(|(_, (o, t))| o - t)
            .sum();
        assert_eq!(diffs["total"].0 - diffs["total"].1, over, "{n}");
        assert!(over <= dec!(0.02), "{n}");
    }
}

/// Web test_charges.py::test_currency_futures_charge_the_levy_the_published_total_leaves_out
#[test]
fn currency_futures_charge_the_levy_the_published_total_leaves_out() {
    let r = reference();
    let cases: Vec<&Value> = r["trades"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| is_levy_case(t))
        .collect();
    assert_eq!(cases.len(), 6);
    for t in cases {
        let shown: BTreeMap<String, Decimal> = t["shown"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), d(v.as_str().unwrap())))
            .collect();
        let c = priced(t);
        let turnover = (d(&s(t, "buy")) + d(&s(t, "sell"))) * d(&s(t, "quantity"));
        let (_tx, levy) = split_exchange_charges(
            segment("currency_futures"),
            "CDS",
            turnover,
            c.exchange_charges,
        );
        assert_eq!(c.brokerage, shown["brokerage"]);
        assert_eq!(c.exchange_charges, shown["exchange_charges"]);
        assert_eq!(c.sebi_charges, shown["sebi_charges"]);
        assert_eq!(c.stamp_duty, shown["stamp_duty"]);
        assert!(levy > Decimal::ZERO);
        let their_lines: Decimal = shown
            .iter()
            .filter(|(k, _)| k.as_str() != "total")
            .map(|(_, v)| *v)
            .sum();
        assert_eq!(their_lines - shown["total"], levy, "{}", name(t));
        assert_eq!(
            c.total(),
            c.brokerage + c.exchange_charges + c.sebi_charges + c.stamp_duty + c.gst
        );
        assert_eq!(
            c.gst,
            paisa((c.brokerage + c.exchange_charges + c.sebi_charges) * dec!(0.18))
        );
        assert!(c.gst >= shown["gst"]);
    }
}

/// Web test_charges.py::test_the_examples_the_calculator_page_opens_on_are_the_published_figures
#[test]
fn the_examples_the_calculator_page_opens_on_are_the_published_figures() {
    let intraday = round_trip("equity_intraday", "NSE", "1000", "1100", "400");
    assert_eq!(intraday.total.total(), dec!(200.62));
    assert_eq!(intraday.net_pnl, dec!(39799.38));
    assert_eq!(intraday.points_to_breakeven, dec!(0.50));
    let delivery = round_trip("equity_delivery", "NSE", "1000", "1100", "400");
    assert_eq!(delivery.total.total(), dec!(931.42));
    assert_eq!(delivery.points_to_breakeven, dec!(2.33));
    let futures = round_trip("fno_futures", "NFO", "1000", "1100", "400");
    assert_eq!(futures.total.total(), dec!(294.33));
    let options = round_trip("fno_options", "NFO", "100", "110", "400").total;
    assert_eq!(
        (
            options.exchange_charges,
            options.stamp_duty,
            options.total()
        ),
        (dec!(29.85), dec!(1.00), dec!(149.52))
    );
}

/// Web test_charges.py::test_stt_and_stamp_duty_go_to_the_nearest_rupee
#[test]
fn stt_and_stamp_duty_go_to_the_nearest_rupee() {
    for (value, expected) in [
        ("0.17675", "0.00"),
        ("0.4995", "1.00"),
        ("0.50", "1.00"),
        ("1.20", "1.00"),
        ("1.5", "2.00"),
        ("7.50", "8.00"),
        ("12.2745", "12.00"),
        ("12.5125", "13.00"),
        ("840", "840.00"),
    ] {
        assert_eq!(rupee(d(value)), d(expected), "{value}");
    }
}

fn charge(seg: &str, exchange: &str, action: &str, value: &str, flat: &str, pct: &str) -> Charges {
    compute_charges(segment(seg), exchange, action, d(value), d(flat), d(pct))
}

/// Web test_charges.py::test_a_fill_is_charged_as_a_contract_note_for_that_one_trade
#[test]
fn a_fill_is_charged_as_a_contract_note_for_that_one_trade() {
    let buy = charge("fno_options", "NFO", "BUY", "40000", "20", "0");
    let sell = charge("fno_options", "NFO", "SELL", "44000", "20", "0");
    assert_eq!(buy.stamp_duty, dec!(1.00));
    assert_eq!(sell.stt, dec!(66.00));
    assert_eq!(
        (buy.exchange_charges, sell.exchange_charges),
        (dec!(14.21), dec!(15.63))
    );
    assert_eq!((buy.stt, sell.stamp_duty), (dec!(0.00), dec!(0.00)));
}

/// Web test_charges.py::test_two_fills_can_come_to_a_paisa_less_than_the_round_trip_they_make
#[test]
fn two_fills_can_come_to_a_paisa_less_than_the_round_trip_they_make() {
    let buy = charge("fno_options", "NFO", "BUY", "40000", "20", "0");
    let sell = charge("fno_options", "NFO", "SELL", "44000", "20", "0");
    let together = round_trip("fno_options", "NFO", "100", "110", "400").total;
    assert_eq!(buy.exchange_charges + sell.exchange_charges, dec!(29.84));
    assert_eq!(together.exchange_charges, dec!(29.85));
    assert_eq!(together.total() - (buy.total() + sell.total()), dec!(0.01));
    assert_eq!(together.stt, buy.stt + sell.stt);
    assert_eq!(together.stamp_duty, buy.stamp_duty + sell.stamp_duty);
}

/// Web test_charges.py::test_commodity_tax_is_kept_to_the_paisa_where_stt_is_not
#[test]
fn commodity_tax_is_kept_to_the_paisa_where_stt_is_not() {
    let futures = round_trip("commodity_futures", "MCX", "250", "252", "1000").total;
    assert_eq!(futures.stt, dec!(25.20));
    assert_eq!(futures.stamp_duty, dec!(5.00));
    assert_eq!(futures.total(), dec!(90.43));
}

/// Web test_charges.py::test_the_exchange_charge_and_its_levy_are_rounded_apart_and_then_added
#[test]
fn the_exchange_charge_and_its_levy_are_rounded_apart_and_then_added() {
    let c = round_trip("equity_intraday", "NSE", "333", "334", "10").total;
    let (tx, levy) = split_exchange_charges(
        segment("equity_intraday"),
        "NSE",
        dec!(6670),
        c.exchange_charges,
    );
    assert_eq!(c.exchange_charges, dec!(0.21));
    assert_eq!((tx, levy), (dec!(0.20), dec!(0.01)));
    assert_eq!(paisa(dec!(6670) * dec!(0.00307) / dec!(100)), dec!(0.20));
}

/// Web test_charges.py::test_a_round_trip_reports_one_figure_per_line_and_no_sides
#[test]
fn a_round_trip_reports_one_figure_per_line_and_no_sides() {
    let r = round_trip("equity_intraday", "NSE", "1000", "1100", "400");
    assert_eq!(r.net_pnl, r.gross_pnl - r.total.total());
}

/// Web test_charges.py::test_every_segment_belongs_to_one_market_and_crypto_is_the_crypto_exchange
#[test]
fn every_segment_belongs_to_one_market_and_crypto_is_the_crypto_exchange() {
    let indian: Vec<&str> = segments_for(Some(Market::Indian)).map(|s| s.key).collect();
    let crypto: Vec<&str> = segments_for(Some(Market::Crypto)).map(|s| s.key).collect();
    assert_eq!(crypto, ["crypto_futures", "crypto_options"]);
    assert_eq!(indian.len(), 8);
    assert!(indian.iter().all(|k| !crypto.contains(k)));
    assert_eq!(segments_for(None).count(), SEGMENTS.len());
    for s in SEGMENTS.iter() {
        let on_crypto = s.exchanges() == ["CRYPTO"];
        assert_eq!(s.market == Market::Crypto, on_crypto, "{}", s.key);
    }
}

/// Web test_charges.py::test_only_a_plugin_that_says_crypto_is_shown_the_crypto_rate_card
#[test]
fn only_a_plugin_that_says_crypto_is_shown_the_crypto_rate_card() {
    for (t, m) in [
        (Some("crypto"), Market::Crypto),
        (Some("CRYPTO"), Market::Crypto),
        (Some(" crypto "), Market::Crypto),
        (Some("IN_stock"), Market::Indian),
        (Some(""), Market::Indian),
        (None, Market::Indian),
        (Some("something_new"), Market::Indian),
    ] {
        assert_eq!(market_for_broker_type(t), m, "{t:?}");
    }
}

/// Web test_charges.py::test_a_segment_lists_its_main_exchange_first
#[test]
fn a_segment_lists_its_main_exchange_first() {
    let got: Vec<(&str, Vec<&str>)> = SEGMENTS.iter().map(|s| (s.key, s.exchanges())).collect();
    let want: Vec<(&str, Vec<&str>)> = vec![
        ("equity_delivery", vec!["NSE", "BSE"]),
        ("equity_intraday", vec!["NSE", "BSE"]),
        ("fno_futures", vec!["NFO", "BFO"]),
        ("fno_options", vec!["NFO", "BFO"]),
        ("currency_futures", vec!["CDS", "BCD"]),
        ("currency_options", vec!["CDS", "BCD"]),
        ("commodity_futures", vec!["MCX", "NCDEX", "NCO"]),
        ("commodity_options", vec!["MCX", "NCDEX", "NCO"]),
        ("crypto_futures", vec!["CRYPTO"]),
        ("crypto_options", vec!["CRYPTO"]),
    ];
    assert_eq!(got, want);
}

fn default_plan(key: &str) -> (Decimal, Decimal) {
    let s = segment(key).unwrap();
    (s.default_flat, s.default_percent)
}

/// Web test_charges.py::test_delivery_is_charged_brokerage_by_default_though_the_reference_charges_none
#[test]
fn delivery_is_charged_brokerage_by_default_though_the_reference_charges_none() {
    let (flat, percent) = default_plan("equity_delivery");
    assert_eq!((flat, percent), (dec!(20), dec!(2.5)));
    let r = estimate_round_trip(
        "equity_delivery",
        "NSE",
        dec!(1000),
        dec!(1100),
        dec!(400),
        flat,
        percent,
    )
    .expect("a trade within range");
    assert_eq!(r.total.brokerage, dec!(40.00));
    assert_eq!(r.total.total(), dec!(931.42) + dec!(40) + dec!(7.20));
    let small = compute_charges(
        segment("equity_delivery"),
        "NSE",
        "BUY",
        dec!(500),
        flat,
        percent,
    );
    assert_eq!(small.brokerage, dec!(12.50));
}

/// Web test_charges.py::test_intraday_is_twenty_or_a_tenth_of_a_percent_whichever_is_lower
#[test]
fn intraday_is_twenty_or_a_tenth_of_a_percent_whichever_is_lower() {
    let (flat, percent) = default_plan("equity_intraday");
    assert_eq!((flat, percent), (dec!(20), dec!(0.1)));
    let at = |v: Decimal| {
        compute_charges(segment("equity_intraday"), "NSE", "BUY", v, flat, percent).brokerage
    };
    assert_eq!(at(dec!(400000)), dec!(20.00));
    assert_eq!(at(dec!(15000)), dec!(15.00));
    assert_eq!(at(dec!(20000)), dec!(20.00));
}

/// Web test_charges.py::test_the_other_defaults_are_the_calculators
#[test]
fn the_other_defaults_are_the_calculators() {
    for k in [
        "fno_futures",
        "fno_options",
        "currency_futures",
        "currency_options",
        "commodity_futures",
        "commodity_options",
    ] {
        assert_eq!(default_plan(k), reference_plan(k), "{k}");
    }
}

/// Web test_charges.py::test_every_crypto_value_is_zero_until_the_trader_enters_one
#[test]
fn every_crypto_value_is_zero_until_the_trader_enters_one() {
    for key in ["crypto_futures", "crypto_options"] {
        let row = segment(key).unwrap();
        assert_eq!(default_plan(key), (dec!(0), dec!(0)));
        let z = Decimal::ZERO;
        assert_eq!(
            (row.stt_buy, row.stt_sell, row.stamp_buy, row.sebi, row.gst),
            (z, z, z, z, z)
        );
        assert!(row.exchange_txn.is_empty() && row.ipft.is_empty());
        for action in ["BUY", "SELL"] {
            assert_eq!(
                charge(key, "CRYPTO", action, "100000", "0", "0"),
                Charges::default()
            );
        }
    }
    let charged = charge("crypto_futures", "CRYPTO", "SELL", "100000", "0", "0.05");
    assert_eq!(charged.brokerage, dec!(50.00));
    assert_eq!(charged.total(), dec!(50.00));
    assert_eq!(
        charged,
        Charges {
            brokerage: dec!(50.00),
            ..Charges::default()
        }
    );
}

/// Web test_charges.py::test_the_exchange_levy_is_part_of_the_exchange_charge_not_an_addition
#[test]
fn the_exchange_levy_is_part_of_the_exchange_charge_not_an_addition() {
    let value = dec!(10000000);
    let split = |seg: &str, exchange: &str| {
        let paid = compute_charges(segment(seg), exchange, "SELL", value, dec!(0), dec!(0))
            .exchange_charges;
        let (tx, levy) = split_exchange_charges(segment(seg), exchange, value, paid);
        assert_eq!(tx + levy, paid);
        (tx, levy)
    };
    assert_eq!(split("equity_delivery", "NSE"), (dec!(297.00), dec!(10.00)));
    assert_eq!(split("equity_intraday", "NSE"), (dec!(297.00), dec!(10.00)));
    assert_eq!(split("fno_futures", "NFO"), (dec!(173.00), dec!(10.00)));
    assert_eq!(split("fno_options", "NFO"), (dec!(3503.00), dec!(50.00)));
    assert_eq!(split("currency_futures", "CDS"), (dec!(35.00), dec!(5.00)));
    assert_eq!(
        split("currency_options", "CDS"),
        (dec!(3110.00), dec!(200.00))
    );
    for (seg, exchange) in [
        ("equity_delivery", "BSE"),
        ("fno_options", "BFO"),
        ("currency_futures", "BCD"),
        ("commodity_futures", "MCX"),
        ("crypto_futures", "CRYPTO"),
    ] {
        assert_eq!(split(seg, exchange).1, Decimal::ZERO, "{seg} {exchange}");
    }
    for row in SEGMENTS.iter() {
        for (exchange, rate) in row.ipft {
            assert!(
                *rate < row.exchange_rate(exchange),
                "{} {}",
                row.key,
                exchange
            );
        }
    }
    assert_eq!(
        split_exchange_charges(None, "NSE", value, dec!(12.34)),
        (dec!(12.34), dec!(0.00))
    );
}

/// Web test_charges.py::test_brokerage_is_the_lower_of_the_two_when_both_are_set
#[test]
fn brokerage_is_the_lower_of_the_two_when_both_are_set() {
    for (value, flat, pct, want) in [
        ("100000", "20", "0.03", "20"),
        ("50000", "20", "0.03", "15"),
        ("100000", "20", "0", "20"),
        ("10", "20", "0", "20"),
        ("100000", "0", "0.5", "500"),
        ("100000", "0", "0", "0"),
        ("0", "20", "0.03", "0"),
    ] {
        assert_eq!(brokerage_for(d(value), d(flat), d(pct)), d(want), "{value}");
    }
}

/// Web test_charges.py::test_a_plan_reads_as_a_brokers_pricing_page_would_put_it
#[test]
fn a_plan_reads_as_a_brokers_pricing_page_would_put_it() {
    assert_eq!(
        describe_brokerage(dec!(20), dec!(0.1)),
        "20 per executed order or 0.1% of the trade value, whichever is lower"
    );
    assert_eq!(
        describe_brokerage(dec!(20), dec!(0)),
        "20 per executed order"
    );
    assert_eq!(
        describe_brokerage(dec!(0), dec!(0.5)),
        "0.5% of the trade value"
    );
    assert_eq!(describe_brokerage(dec!(0), dec!(0)), "No brokerage");
}

/// Web test_charges.py::test_stamp_duty_is_paid_on_the_buy_only
#[test]
fn stamp_duty_is_paid_on_the_buy_only() {
    for s in SEGMENTS.iter() {
        let exchange = s.exchanges()[0];
        let sell = compute_charges(Some(s), exchange, "SELL", dec!(1000000), dec!(0), dec!(0));
        assert_eq!(sell.stamp_duty, dec!(0.00), "{}", s.key);
    }
}

/// Web test_charges.py::test_stt_falls_on_the_sell_except_for_delivery_which_pays_both_ways
#[test]
fn stt_falls_on_the_sell_except_for_delivery_which_pays_both_ways() {
    for (key, buy, sell) in [
        ("equity_delivery", "1000.00", "1000.00"),
        ("equity_intraday", "0.00", "250.00"),
        ("fno_futures", "0.00", "500.00"),
        ("fno_options", "0.00", "1500.00"),
        ("currency_futures", "0.00", "0.00"),
        ("currency_options", "0.00", "0.00"),
        ("commodity_futures", "0.00", "100.00"),
        ("commodity_options", "0.00", "500.00"),
    ] {
        let exchange = segment(key).unwrap().exchanges()[0];
        assert_eq!(
            charge(key, exchange, "BUY", "1000000", "0", "0").stt,
            d(buy)
        );
        assert_eq!(
            charge(key, exchange, "SELL", "1000000", "0", "0").stt,
            d(sell)
        );
    }
}

/// Web test_charges.py::test_gst_is_eighteen_percent_of_the_lines_it_is_charged_on
#[test]
fn gst_is_eighteen_percent_of_the_lines_it_is_charged_on() {
    let c = charge("equity_intraday", "NSE", "SELL", "440000", "20", "0.1");
    let base = c.brokerage + c.exchange_charges + c.sebi_charges;
    assert_eq!(c.gst, paisa(base * dec!(0.18)));
    assert!(c.stt > Decimal::ZERO);
}

/// Web test_charges.py::test_each_exchange_is_charged_its_own_transaction_rate
#[test]
fn each_exchange_is_charged_its_own_transaction_rate() {
    let x = |seg: &str, exchange: &str| {
        charge(seg, exchange, "SELL", "10000000", "0", "0").exchange_charges
    };
    assert_eq!(x("equity_intraday", "NSE"), dec!(307.00));
    assert_eq!(x("equity_intraday", "BSE"), dec!(375.00));
    assert_eq!(x("fno_futures", "NFO"), dec!(183.00));
    assert_eq!(x("fno_futures", "BFO"), dec!(0.00));
    assert_eq!(x("fno_options", "NFO"), dec!(3553.00));
    assert_eq!(x("fno_options", "BFO"), dec!(3250.00));
    assert_eq!(x("currency_futures", "CDS"), dec!(40.00));
    assert_eq!(x("currency_futures", "BCD"), dec!(45.00));
    assert_eq!(x("currency_options", "CDS"), dec!(3310.00));
    assert_eq!(x("currency_options", "BCD"), dec!(100.00));
    assert_eq!(x("commodity_futures", "MCX"), dec!(210.00));
    assert_eq!(x("commodity_futures", "NCO"), dec!(10.00));
    assert_eq!(x("commodity_options", "MCX"), dec!(4180.00));
    assert_eq!(x("commodity_options", "NCO"), dec!(100.00));
}

/// Web test_charges.py::test_every_line_is_held_to_two_places_and_the_total_is_their_sum
#[test]
fn every_line_is_held_to_two_places_and_the_total_is_their_sum() {
    let c = charge("fno_options", "NFO", "BUY", "1234.56", "20", "0");
    let cols = c.columns();
    for v in cols {
        assert_eq!(v, paisa(v));
    }
    assert_eq!(cols[6], cols[..6].iter().copied().sum::<Decimal>());
}

/// Web test_charges.py::test_an_unknown_segment_or_an_empty_trade_is_charged_nothing
#[test]
fn an_unknown_segment_or_an_empty_trade_is_charged_nothing() {
    let none = Charges::default();
    assert_eq!(
        compute_charges(None, "NSE", "BUY", dec!(1000), dec!(20), dec!(0.1)),
        none
    );
    assert_eq!(charge("nonsense", "NSE", "BUY", "1000", "20", "0.1"), none);
    assert_eq!(
        charge("equity_intraday", "NSE", "BUY", "0", "20", "0.1"),
        none
    );
}

/// Web test_charges.py::test_an_order_is_filed_under_the_right_segment
#[test]
fn an_order_is_filed_under_the_right_segment() {
    for (exchange, product, symbol, want) in [
        ("NSE", "CNC", "INFY", Some("equity_delivery")),
        ("NSE", "MIS", "INFY", Some("equity_intraday")),
        ("BSE", "CNC", "SBIN", Some("equity_delivery")),
        ("BSE", "MIS", "SBIN", Some("equity_intraday")),
        ("NSE", "MIS", "RELIANCE", Some("equity_intraday")),
        ("NSE", "CNC", "RELIANCE", Some("equity_delivery")),
        ("NFO", "NRML", "BANKNIFTY24APR24FUT", Some("fno_futures")),
        ("NFO", "MIS", "NIFTY28MAR2420800CE", Some("fno_options")),
        ("BFO", "NRML", "SENSEX28MAR2472000PE", Some("fno_options")),
        ("BFO", "NRML", "SENSEX28MAR24FUT", Some("fno_futures")),
        ("CDS", "NRML", "USDINR28NOV25FUT", Some("currency_futures")),
        (
            "CDS",
            "NRML",
            "USDINR28NOV2584.5CE",
            Some("currency_options"),
        ),
        ("BCD", "MIS", "USDINR28NOV25FUT", Some("currency_futures")),
        (
            "MCX",
            "NRML",
            "CRUDEOILM20MAY24FUT",
            Some("commodity_futures"),
        ),
        (
            "MCX",
            "NRML",
            "CRUDEOIL17DEC255500PE",
            Some("commodity_options"),
        ),
        (
            "NCDEX",
            "NRML",
            "JEERAUNJHA20DEC24FUT",
            Some("commodity_futures"),
        ),
        ("NCO", "NRML", "GOLD05DEC24FUT", Some("commodity_futures")),
        ("CRYPTO", "NRML", "BTCUSDFUT", Some("crypto_futures")),
        (
            "CRYPTO",
            "NRML",
            "BTC28FEB2562000CE",
            Some("crypto_options"),
        ),
        ("crypto", "nrml", "btcusd", Some("crypto_futures")),
        ("NSE_INDEX", "MIS", "NIFTY", None),
        ("BSE_INDEX", "MIS", "SENSEX", None),
        ("GLOBAL_INDEX", "MIS", "US30", None),
        ("", "", "", None),
    ] {
        assert_eq!(
            classify_segment(exchange, product, symbol).map(|s| s.key),
            want,
            "{exchange} {product} {symbol}"
        );
    }
}

/// Web test_charges.py::test_every_segment_has_its_two_settings_and_charges_start_switched_on
#[test]
fn every_segment_has_its_two_settings_and_charges_start_switched_on() {
    let values: BTreeMap<String, String> = default_config_values().into_iter().collect();
    assert_eq!(values[CHARGES_ENABLED_KEY], "true");
    for s in SEGMENTS.iter() {
        assert_eq!(d(&values[&brokerage_flat_key(s.key)]), s.default_flat);
        assert_eq!(d(&values[&brokerage_percent_key(s.key)]), s.default_percent);
    }
    assert_eq!(values.len(), 1 + 2 * SEGMENTS.len());
    let keys: BTreeSet<String> = config_keys().into_iter().collect();
    assert_eq!(keys, values.keys().cloned().collect());
    assert!(default_config_rows()
        .iter()
        .all(|(_, _, desc)| !desc.is_empty()));
}

/// Web test_charges.py::test_the_defaults_are_themselves_values_a_trader_could_save
#[test]
fn the_defaults_are_themselves_values_a_trader_could_save() {
    for s in SEGMENTS.iter() {
        let r = validate_brokerage(
            s.key,
            &json!(plain(s.default_flat)),
            &json!(plain(s.default_percent)),
        );
        assert!(r.is_ok(), "{}", s.key);
    }
}

/// Web test_charges.py::test_a_value_that_would_misprice_every_trade_is_refused
#[test]
fn a_value_that_would_misprice_every_trade_is_refused() {
    let over_flat = plain(MAX_BROKERAGE_FLAT + Decimal::ONE);
    let over_pct = plain(MAX_BROKERAGE_PERCENT + Decimal::ONE);
    for (flat, pct, fragment) in [
        ("-1", "0", "cannot be negative"),
        ("0", "-0.01", "cannot be negative"),
        ("abc", "0", "enter a number"),
        ("20", "", "enter a number"),
        ("nan", "0", "enter a number"),
        ("inf", "0", "enter a number"),
        (
            over_flat.as_str(),
            "0",
            "amount per order cannot be more than",
        ),
        ("20", over_pct.as_str(), "percentage cannot be more than"),
    ] {
        let e = validate_brokerage("equity_intraday", &json!(flat), &json!(pct)).unwrap_err();
        assert!(e.contains(fragment), "{flat}/{pct}: {e}");
        assert!(e.starts_with("Equity Intraday"), "{e}");
    }
}

/// Web test_charges.py::test_the_limits_themselves_are_accepted
#[test]
fn the_limits_themselves_are_accepted() {
    let r = validate_brokerage(
        "equity_intraday",
        &json!(plain(MAX_BROKERAGE_FLAT)),
        &json!(plain(MAX_BROKERAGE_PERCENT)),
    );
    assert_eq!(r, Ok((MAX_BROKERAGE_FLAT, MAX_BROKERAGE_PERCENT)));
}

/// Web test_charges.py::test_a_segment_that_does_not_exist_is_refused
#[test]
fn a_segment_that_does_not_exist_is_refused() {
    assert!(validate_brokerage("penny_stocks", &json!("20"), &json!("0")).is_err());
}

/// Web test_charges.py::test_the_one_key_update_route_cannot_store_what_the_page_would_refuse
#[test]
fn the_one_key_update_route_cannot_store_what_the_page_would_refuse() {
    assert!(validate_config_value(CHARGES_ENABLED_KEY, "true").is_none());
    assert!(validate_config_value(CHARGES_ENABLED_KEY, "false").is_none());
    assert!(validate_config_value(CHARGES_ENABLED_KEY, "maybe").is_some());
    let flat = brokerage_flat_key("fno_options");
    let pct = brokerage_percent_key("fno_options");
    assert!(validate_config_value(&flat, "20").is_none());
    assert!(validate_config_value(&flat, "-5").is_some());
    assert!(validate_config_value(&flat, "twenty").is_some());
    assert!(validate_config_value(&pct, "0.03").is_none());
    assert!(validate_config_value(&pct, "30").is_some());
    assert!(validate_config_value("starting_capital", "anything").is_none());
}

/// Web test_charges.py::test_a_damaged_switch_reads_as_on_so_trades_are_never_silently_free
#[test]
fn a_damaged_switch_reads_as_on_so_trades_are_never_silently_free() {
    for v in [
        Some("true"),
        Some("True"),
        Some("1"),
        Some(""),
        Some("garbage"),
        None,
    ] {
        assert!(is_enabled(v), "{v:?}");
    }
    for v in ["false", "FALSE", " false ", "0", "off", "no"] {
        assert!(!is_enabled(Some(v)), "{v}");
    }
}

/// A number arrives as JSON text or as a JSON number, as Python's
/// `str(value)` reads either; anything else is not a number.
#[test]
fn a_plan_value_is_read_from_text_or_a_number() {
    assert_eq!(
        validate_brokerage("fno_futures", &json!(20), &json!(0.03)),
        Ok((dec!(20), dec!(0.03)))
    );
    assert_eq!(
        validate_brokerage("fno_futures", &json!("1e1"), &json!(" 0.5 ")),
        Ok((dec!(10), dec!(0.5)))
    );
    for bad in [json!(true), Value::Null, json!([1])] {
        assert!(validate_brokerage("fno_futures", &bad, &json!("0")).is_err());
    }
    assert_eq!(plain(dec!(20.00)), "20");
    assert_eq!(plain(dec!(0.030)), "0.03");
    assert_eq!(plain(Decimal::ZERO), "0");
}

/// Web `_charges_dict`: the lines as floats under the column names
/// `sandbox_trades` stores them in, the total last.
#[test]
fn charges_are_sent_under_their_column_names() {
    let c = charge("equity_delivery", "NSE", "BUY", "125000", "20", "2.5");
    assert_eq!(
        c.as_json(),
        json!({
            "brokerage": 20.0, "stt": 125.0, "exchange_charges": 3.84,
            "sebi_charges": 0.13, "stamp_duty": 19.0, "gst": 4.31,
            "total_charges": 172.28
        })
    );
}

/// Integration review of 1.0.10 (security lens): a trade value is read
/// with checked multiplication and bounded at 10^15 before any line is
/// worked out from it. `price * quantity` with rust_decimal's operator
/// panicked past about 7.9 x 10^28, and the release build aborts on a
/// panic, so one request ended the app.
#[test]
fn a_trade_value_past_the_bound_or_a_decimal_is_none() {
    assert_eq!(
        trade_value(dec!(2500), dec!(50), Decimal::ONE),
        Some(dec!(125000))
    );
    assert_eq!(
        trade_value(dec!(62000), dec!(10), dec!(0.001)),
        Some(dec!(620))
    );
    // The bound itself is priced; a paisa above it is not.
    assert_eq!(
        trade_value(dec!(1000000000000000), Decimal::ONE, Decimal::ONE),
        Some(dec!(1000000000000000))
    );
    assert_eq!(
        trade_value(dec!(1000000000000000.01), Decimal::ONE, Decimal::ONE),
        None
    );
    // 10^20 x 10^9 is past what a decimal holds (the multiply panicked).
    assert_eq!(
        trade_value(dec!(100000000000000000000), dec!(1000000000), Decimal::ONE),
        None
    );
    assert_eq!(
        trade_value(dec!(100000000000000000000), Decimal::MAX, Decimal::ONE),
        None
    );
}

/// The same for the calculator: a side past the bound, or a quantity so
/// small that the points to breakeven (the charges divided by it) overflow,
/// is `None`, never a panic. 10^15 x 10^15 panicked in the multiply, and
/// 47.20 of options charges over 10^-28 lots in the division.
#[test]
fn a_round_trip_out_of_range_is_none_not_a_panic() {
    let (flat, percent) = default_plan("equity_intraday");
    let big = dec!(1000000000000000);
    assert_eq!(
        estimate_round_trip("equity_intraday", "NSE", big, big, big, flat, percent),
        None
    );
    let (flat, percent) = default_plan("fno_options");
    let tiny = dec!(0.0000000000000000000000000001);
    assert_eq!(
        estimate_round_trip(
            "fno_options",
            "NFO",
            dec!(100),
            dec!(101),
            tiny,
            flat,
            percent
        ),
        None
    );
    assert!(estimate_round_trip(
        "fno_options",
        "NFO",
        dec!(100),
        dec!(101),
        dec!(65),
        flat,
        percent
    )
    .is_some());
}
