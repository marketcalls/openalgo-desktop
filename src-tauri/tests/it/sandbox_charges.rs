//! Sandbox brokerage and statutory charges on every fill (web
//! marketcalls/openalgo#2496): the web's test/test_gthread_sandbox_charges.py
//! and test/test_sandbox_charges_api.py, through the sandbox's own calls.
//! Every expected figure is worked by hand from the published rates (as the
//! web's tests pin them), never from the code under test.

use crate::sandbox_support::*;
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::sandbox::charge_manager::OrderCharges;
use openalgo_desktop_lib::sandbox::charges::{Charges, Market};
use openalgo_desktop_lib::sandbox::db::TradeRow;
use rust_decimal::Decimal;
use serde_json::{json, Value};

/// A Monday with the October NIFTY contracts still live.
const T: &str = "2026-10-05 10:00:00";
const CAPITAL: &str = "10000000";
const OPTION: &str = "NIFTY06OCT2622400CE";

/// 50 RELIANCE bought for delivery at 2,500 (1,25,000) under the default
/// plan, line by line: brokerage 20 (lower than 2.5%), STT 0.1% = 125,
/// exchange 3.71 + levy 0.13 (0.125 up), SEBI 0.13, stamp 0.015% = 18.75
/// to the rupee, GST 18% of 23.97 = 4.31.
fn buy_50_at_2500() -> Charges {
    Charges {
        brokerage: d("20.00"),
        stt: d("125.00"),
        exchange_charges: d("3.84"),
        sebi_charges: d("0.13"),
        stamp_duty: d("19.00"),
        gst: d("4.31"),
    }
}

async fn env() -> Env {
    let env = Env::at(T);
    env.charges(true);
    env.ltp("RELIANCE", "NSE", "2500");
    env
}

/// A marketable LIMIT at the last price, which fills at once.
async fn fill(
    env: &Env,
    sym: &str,
    ex: &str,
    side: &str,
    qty: i64,
    price: &str,
    product: &str,
) -> String {
    env.ltp(sym, ex, price);
    let id = place(env, limit(req(sym, ex, side, qty, "LIMIT", product), price)).await;
    assert_eq!(env.status(&id).await, "complete", "{sym} {side}");
    id
}

fn trade(env: &Env, orderid: &str) -> TradeRow {
    env.sb
        .db()
        .with_conn(|c| {
            c.query_row(
                &format!(
                    "SELECT {} FROM sandbox_trades WHERE orderid = ?1",
                    TradeRow::COLUMNS
                ),
                [orderid],
                TradeRow::from_row,
            )
        })
        .unwrap()
}

fn stored_total(env: &Env, orderid: &str) -> String {
    env.sb
        .db()
        .with_conn(|c| {
            c.query_row(
                "SELECT total_charges FROM sandbox_trades WHERE orderid = ?1",
                [orderid],
                |r| r.get(0),
            )
        })
        .unwrap()
}

/// Web test_gthread_sandbox_charges.py::test_a_fill_records_its_charges_and_takes_them_from_the_balance
#[tokio::test]
async fn a_fill_records_its_charges_and_takes_them_from_the_balance() {
    let env = env().await;
    let id = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    let t = trade(&env, &id);
    assert_eq!(t.charges, buy_50_at_2500());
    assert_eq!(t.charges.total(), d("172.28"));
    assert_eq!(d(&stored_total(&env, &id)), d("172.28"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.used_margin, d("125000"));
    assert_eq!(f.available_balance, d(CAPITAL) - d("125000") - d("172.28"));
    assert_eq!(
        (f.total_charges, f.today_charges),
        (d("172.28"), d("172.28"))
    );
    // A cost, not a loss on the position: the P&L is untouched.
    assert_eq!(
        (f.realized_pnl, f.today_realized_pnl, f.total_pnl),
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    );
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_a_round_trip_keeps_pnl_gross_and_the_margin_books_balanced
#[tokio::test]
async fn a_round_trip_keeps_pnl_gross_and_the_margin_books_balanced() {
    let env = env().await;
    let buy = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    let sell = fill(&env, "RELIANCE", "NSE", "SELL", 50, "2600", "CNC").await;
    let s = trade(&env, &sell).charges;
    // 1,30,000 sold: brokerage 20, STT 130 (delivery pays both ways),
    // exchange 3.86 + 0.13, SEBI 0.13, no stamp duty (the buyer's), GST 18%
    // of 24.12 = 4.34.
    assert_eq!(s.stamp_duty, Decimal::ZERO);
    assert_eq!(s.stt, d("130.00"));
    assert_eq!(s.total(), d("158.46"));
    let paid = trade(&env, &buy).charges.total() + s.total();
    assert_eq!(paid, d("172.28") + d("158.46"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(
        (f.realized_pnl, f.today_realized_pnl),
        (d("5000"), d("5000"))
    );
    assert_eq!(f.used_margin, Decimal::ZERO);
    assert_eq!(f.total_charges, paid);
    assert_eq!(f.available_balance, d(CAPITAL) + d("5000") - paid);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_an_intraday_fill_is_charged_the_intraday_rates
#[tokio::test]
async fn an_intraday_fill_is_charged_the_intraday_rates() {
    let env = env().await;
    let id = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "MIS").await;
    let c = trade(&env, &id).charges;
    assert_eq!(c.brokerage, d("20.00"));
    assert_eq!(c.stt, Decimal::ZERO);
    // 0.003%: 3.75, to the nearest rupee.
    assert_eq!(c.stamp_duty, d("4.00"));
    assert_eq!(c.total(), d("32.28"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.used_margin, d("25000"));
    assert_eq!(f.available_balance, d(CAPITAL) - d("25000") - d("32.28"));
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_an_option_is_charged_on_its_premium
#[tokio::test]
async fn an_option_is_charged_on_its_premium() {
    let env = env().await;
    let buy = fill(&env, OPTION, "NFO", "BUY", 65, "100", "NRML").await;
    let sell = fill(&env, OPTION, "NFO", "SELL", 65, "110", "NRML").await;
    let (b, s) = (trade(&env, &buy).charges, trade(&env, &sell).charges);
    assert_eq!((b.brokerage, s.brokerage), (d("20.00"), d("20.00")));
    assert_eq!(b.stt, Decimal::ZERO);
    // 0.15% of the 7,150 premium sold is 10.725: 11 to the rupee.
    assert_eq!(s.stt, d("11.00"));
    // 6,500 bought: exchange 2.28 + 0.03, SEBI 0.01, stamp 0.195 rounds to
    // nothing, GST 18% of 22.32 = 4.02.
    assert_eq!(b.total(), d("26.34"));
    // 7,150 sold: exchange 2.50 + 0.04, SEBI 0.01, GST 18% of 22.55 = 4.06.
    assert_eq!(s.total(), d("37.61"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.realized_pnl, d("650"));
    assert_eq!(f.used_margin, Decimal::ZERO);
    assert_eq!(
        f.available_balance,
        d(CAPITAL) + d("650") - d("26.34") - d("37.61")
    );
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_with_charges_off_a_fill_leaves_the_figures_it_always_left
#[tokio::test]
async fn with_charges_off_a_fill_leaves_the_figures_it_always_left() {
    let env = env().await;
    env.charges(false);
    let id = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert_eq!(trade(&env, &id).charges, Charges::default());
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.total_charges, Decimal::ZERO);
    assert_eq!(f.available_balance + f.used_margin, d(CAPITAL));
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_a_saved_plan_applies_from_the_next_fill
#[tokio::test]
async fn a_saved_plan_applies_from_the_next_fill() {
    let env = env().await;
    let first = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert_eq!(trade(&env, &first).charges.brokerage, d("20.00"));
    env.sb
        .save_charge_settings(
            json!({"segments": {"equity_delivery": {"flat": "0", "percent": "0.5"}}}),
            Market::Indian,
        )
        .await
        .unwrap();
    let second = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert_eq!(trade(&env, &second).charges.brokerage, d("625.00"));
    // A plan change is not backdated.
    assert_eq!(trade(&env, &first).charges.brokerage, d("20.00"));
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_a_charge_the_balance_cannot_cover_takes_it_below_zero
#[tokio::test]
async fn a_charge_the_balance_cannot_cover_takes_it_below_zero() {
    let env = env().await;
    env.sb
        .update_config("starting_capital", "100000")
        .await
        .unwrap();
    // 40 at 2,500 holds the whole 1,00,000 at 1x; the fill still pays
    // 142.34: brokerage 20, STT 100, exchange 2.97 + 0.10, SEBI 0.10, stamp
    // 15, GST 18% of 23.17 = 4.17.
    let id = fill(&env, "RELIANCE", "NSE", "BUY", 40, "2500", "CNC").await;
    assert_eq!(trade(&env, &id).charges.total(), d("142.34"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.used_margin, d("100000"));
    assert_eq!(f.available_balance, d("-142.34"));
    assert_eq!(f.total_charges, d("142.34"));
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_funds_report_the_charges_and_a_balance_already_net_of_them
#[tokio::test]
async fn funds_report_the_charges_and_a_balance_already_net_of_them() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    let f = env.sb.funds().await.unwrap().data;
    assert_eq!((f.today_charges, f.total_charges), (172.28, 172.28));
    assert_eq!(f.availablecash, 9_874_827.72);
    assert_eq!(f.m2mrealized, 0.0);
    let body = serde_json::to_value(env.sb.funds().await.unwrap()).unwrap();
    assert_eq!(body["data"]["today_charges"], json!(172.28));
    assert_eq!(body["data"]["total_charges"], json!(172.28));
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_a_capital_change_keeps_the_charges_already_paid
#[tokio::test]
async fn a_capital_change_keeps_the_charges_already_paid() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    env.sb
        .update_config("starting_capital", "5000000")
        .await
        .unwrap();
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.total_capital, d("5000000"));
    assert_eq!(f.total_charges, d("172.28"));
    assert_eq!(
        f.available_balance,
        d("5000000") - d("125000") - d("172.28")
    );
    env.shutdown().await;
}

/// Web test_gthread_sandbox_charges.py::test_one_order_reached_by_two_threads_is_charged_once:
/// a resting order a tick and a polling pass reach together fills once and
/// pays once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_order_reached_by_two_evaluators_is_charged_once() {
    let env = env().await;
    env.ltp("RELIANCE", "NSE", "2600");
    let id = place(
        &env,
        limit(req("RELIANCE", "NSE", "BUY", 50, "LIMIT", "CNC"), "2500"),
    )
    .await;
    assert_eq!(env.status(&id).await, "open");
    env.ltp("RELIANCE", "NSE", "2500");
    let tick = openalgo_desktop_lib::sandbox::Tick::new("RELIANCE", "NSE", d("2500"));
    let (a, b) = tokio::join!(env.sb.on_tick(tick), env.sb.poll_once());
    assert_eq!(a.unwrap().0 + b.unwrap().filled, 1);
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.total_charges, d("172.28"));
    assert_eq!(f.available_balance, d(CAPITAL) - d("125000") - d("172.28"));
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// The settings page and the calculator (test_sandbox_charges_api.py)
// ---------------------------------------------------------------------------

/// Web test_sandbox_charges_api.py::test_the_page_opens_on_the_default_plan_with_charges_on
/// and ::test_each_segment_lists_its_exchanges_main_exchange_first
#[tokio::test]
async fn the_page_opens_on_the_default_plan_with_charges_on() {
    let env = Env::at(T);
    env.charges(true);
    let s = env.sb.charge_settings(Market::Indian).await.unwrap();
    assert_eq!(s["enabled"], json!(true));
    assert_eq!(s["market"], json!("indian"));
    assert_eq!(s["limits"], json!({"flat_max": "1000", "percent_max": "5"}));
    assert_eq!(s["statutory_as_of"], json!("2026-11-23"));
    let segs = s["segments"].as_array().unwrap();
    let keys: Vec<&str> = segs.iter().map(|g| g["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        [
            "equity_delivery",
            "equity_intraday",
            "fno_futures",
            "fno_options",
            "currency_futures",
            "currency_options",
            "commodity_futures",
            "commodity_options"
        ]
    );
    let delivery = &segs[0];
    assert_eq!(delivery["flat"], json!("20"));
    assert_eq!(delivery["percent"], json!("2.5"));
    assert_eq!(
        delivery["summary"],
        json!("20 per executed order or 2.5% of the trade value, whichever is lower")
    );
    assert_eq!(delivery["exchanges"], json!(["NSE", "BSE"]));
    assert_eq!(
        delivery["statutory"],
        json!({"stt_label": "STT", "stt_buy": "0.1", "stt_sell": "0.1", "stamp_buy": "0.015",
               "sebi": "0.0001", "gst": "18",
               "exchange_txn": {"BSE": "0.00375", "NSE": "0.00307"}})
    );
    assert_eq!(segs[6]["exchanges"], json!(["MCX", "NCDEX", "NCO"]));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_a_crypto_venue_is_shown_the_crypto_plan_and_nothing_from_the_indian_one
#[tokio::test]
async fn a_crypto_venue_is_shown_the_crypto_plan_alone() {
    let env = Env::at(T);
    let s = env.sb.charge_settings(Market::Crypto).await.unwrap();
    assert_eq!(s["market"], json!("crypto"));
    let segs = s["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0]["key"], json!("crypto_futures"));
    assert_eq!(segs[0]["exchanges"], json!(["CRYPTO"]));
    assert_eq!(segs[0]["summary"], json!("No brokerage"));
    assert_eq!(segs[0]["statutory"]["exchange_txn"], json!({}));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_a_saved_plan_comes_back_as_saved_and_reads_as_a_sentence
/// and ::test_one_bad_value_saves_nothing_at_all
#[tokio::test]
async fn a_save_is_whole_or_nothing() {
    let env = Env::at(T);
    let (msg, s) = env
        .sb
        .save_charge_settings(
            json!({"enabled": false, "segments": {"fno_options": {"flat": 15, "percent": "0"}}}),
            Market::Indian,
        )
        .await
        .unwrap();
    assert_eq!(msg, "Charges saved. They apply from the next trade.");
    assert_eq!(s["enabled"], json!(false));
    let opt = &s["segments"][3];
    assert_eq!(
        (opt["flat"].clone(), opt["summary"].clone()),
        (json!("15"), json!("15 per executed order"))
    );
    let e = env
        .sb
        .save_charge_settings(
            json!({"enabled": true, "segments": {
                "fno_futures": {"flat": "10", "percent": "0.01"},
                "equity_intraday": {"flat": "20", "percent": "20"}}}),
            Market::Indian,
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert_eq!(
        e.message,
        "Equity Intraday: the percentage cannot be more than 5. Enter 0.03 for 0.03%, not 3."
    );
    // Nothing of the refused save was written, not even the good parts.
    let s = env.sb.charge_settings(Market::Indian).await.unwrap();
    assert_eq!(s["enabled"], json!(false));
    assert_eq!(s["segments"][2]["flat"], json!("20"));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_what_the_page_refuses_and_how_it_says_so
#[tokio::test]
async fn what_the_page_refuses_and_how_it_says_so() {
    let env = Env::at(T);
    for (body, msg) in [
        (
            Value::Null,
            "Nothing was saved: the settings could not be read.",
        ),
        (
            json!([1]),
            "Nothing was saved: the settings could not be read.",
        ),
        (json!({}), "There was nothing to save."),
        (
            json!({"enabled": "maybe"}),
            "Charges can only be switched on or off.",
        ),
        (
            json!({"enabled": 1}),
            "Charges can only be switched on or off.",
        ),
        (
            json!({"segments": [1]}),
            "Nothing was saved: the brokerage plan could not be read.",
        ),
        (
            json!({"segments": {"fno_futures": 5}}),
            "Nothing was saved: the brokerage plan could not be read.",
        ),
        (
            json!({"segments": {"penny": {"flat": "1", "percent": "0"}}}),
            "That segment is not on the rate card.",
        ),
        (
            json!({"segments": {"fno_futures": {"flat": "-1", "percent": "0"}}}),
            "F&O Futures: brokerage cannot be negative.",
        ),
        (
            json!({"segments": {"fno_futures": {"percent": "0"}}}),
            "F&O Futures: enter a number for the amount per order.",
        ),
    ] {
        let e = env
            .sb
            .save_charge_settings(body.clone(), Market::Indian)
            .await
            .unwrap_err();
        assert_eq!((e.http_status, e.message.as_str()), (400, msg), "{body}");
    }
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_switching_charges_off_makes_the_next_fill_free_and_on_again_charges_it
/// and ::test_reset_restores_the_defaults_and_touches_no_trade_or_balance
#[tokio::test]
async fn the_switch_and_the_reset_apply_from_the_next_fill() {
    let env = env().await;
    env.sb
        .save_charge_settings(json!({"enabled": "false"}), Market::Indian)
        .await
        .unwrap();
    let free = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert_eq!(trade(&env, &free).charges, Charges::default());
    env.sb
        .save_charge_settings(
            json!({"enabled": true, "segments": {"equity_delivery": {"flat": "0", "percent": "0"}}}),
            Market::Indian,
        )
        .await
        .unwrap();
    let before = env.sb.funds_row().await.unwrap().unwrap();
    let s = env.sb.reset_charge_settings(Market::Indian).await.unwrap();
    assert_eq!(s["enabled"], json!(true));
    assert_eq!(s["segments"][0]["flat"], json!("20"));
    assert_eq!(env.sb.funds_row().await.unwrap().unwrap(), before);
    let charged = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert_eq!(trade(&env, &charged).charges, buy_50_at_2500());
    assert_eq!(trade(&env, &free).charges, Charges::default());
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_the_general_settings_route_holds_the_plan_to_the_same_rules
#[tokio::test]
async fn the_general_settings_route_holds_the_plan_to_the_same_rules() {
    let env = Env::at(T);
    let e = env
        .sb
        .update_config("brokerage_fno_options_percent", "30")
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(e
        .message
        .starts_with("F&O Options: the percentage cannot be more than 5."));
    env.sb
        .update_config("brokerage_fno_options_flat", "12")
        .await
        .unwrap();
    let s = env.sb.charge_settings(Market::Indian).await.unwrap();
    assert_eq!(s["segments"][3]["flat"], json!("12"));
    assert!(env
        .sb
        .update_config("charges_enabled", "maybe")
        .await
        .is_err());
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_the_calculator_reproduces_the_reference_intraday_trade
#[tokio::test]
async fn the_calculator_reproduces_the_reference_intraday_trade() {
    let env = Env::at(T);
    let r = env
        .sb
        .estimate_charges(
            json!({"segment": "equity_intraday", "exchange": "nse", "buy_price": "1000",
                   "sell_price": 1100, "quantity": "400", "flat": "20", "percent": "0.03"}),
            Market::Indian,
        )
        .await
        .unwrap();
    assert_eq!(
        r,
        json!({
            "segment": "equity_intraday", "exchange": "NSE",
            "brokerage_plan": "20 per executed order or 0.03% of the trade value, whichever is lower",
            "stt_label": "STT", "turnover": 840000.0,
            "total": {"brokerage": 40.0, "stt": 110.0, "exchange_charges": 25.79,
                      "sebi_charges": 0.84, "stamp_duty": 12.0, "gst": 11.99,
                      "total_charges": 200.62},
            "gross_pnl": 40000.0, "net_pnl": 39799.38, "points_to_breakeven": 0.5
        })
    );
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_the_calculator_prices_only_the_market_of_the_broker_logged_in,
/// ::test_without_a_plan_in_the_request_the_calculator_uses_the_saved_one and
/// ::test_the_calculator_asks_for_what_is_missing_in_plain_words
#[tokio::test]
async fn the_calculator_prices_one_market_and_says_what_is_missing() {
    let env = Env::at(T);
    let trade = json!({"segment": "crypto_futures", "exchange": "CRYPTO",
                       "buy_price": "60000", "sell_price": "61000", "quantity": "1"});
    let e = env
        .sb
        .estimate_charges(trade.clone(), Market::Indian)
        .await
        .unwrap_err();
    assert_eq!(
        e.message,
        "Crypto Futures and Spot is not traded with the broker you are logged in to."
    );
    assert!(env.sb.estimate_charges(trade, Market::Crypto).await.is_ok());
    // The saved plan, when the request names none: the default delivery 20.
    let r = env
        .sb
        .estimate_charges(
            json!({"segment": "equity_delivery", "exchange": "NSE", "buy_price": "1000",
                   "sell_price": "1100", "quantity": "400"}),
            Market::Indian,
        )
        .await
        .unwrap();
    assert_eq!(r["total"]["brokerage"], json!(40.0));
    assert_eq!(r["total"]["total_charges"], json!(978.62));
    let base = json!({"segment": "fno_futures", "exchange": "NFO", "buy_price": "100",
                      "sell_price": "101", "quantity": "1"});
    for (change, msg) in [
        (json!({"segment": ""}), "Choose a segment."),
        (
            json!({"exchange": "MCX"}),
            "F&O Futures trades on NFO or BFO. Choose one of them.",
        ),
        (
            json!({"buy_price": "0"}),
            "Enter a buy price and a sell price above zero.",
        ),
        (
            json!({"sell_price": "abc"}),
            "Enter a buy price and a sell price above zero.",
        ),
        (json!({"quantity": null}), "Enter a quantity above zero."),
        (
            json!({"percent": "9"}),
            "F&O Futures: enter a number for the amount per order.",
        ),
    ] {
        let mut body = base.clone();
        for (k, v) in change.as_object().unwrap() {
            body[k] = v.clone();
        }
        let e = env
            .sb
            .estimate_charges(body, Market::Indian)
            .await
            .unwrap_err();
        assert_eq!((e.http_status, e.message.as_str()), (400, msg), "{change}");
    }
    let e = env
        .sb
        .estimate_charges(Value::Null, Market::Indian)
        .await
        .unwrap_err();
    assert_eq!(e.message, "Enter the trade to estimate.");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// The transaction cost report
// ---------------------------------------------------------------------------

/// Web test_sandbox_charges_api.py::test_the_report_totals_are_what_the_fills_paid
/// and ::test_a_share_whose_name_ends_like_an_option_is_reported_as_a_share
#[tokio::test]
async fn the_report_totals_are_what_the_fills_paid() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    fill(&env, "RELIANCE", "NSE", "SELL", 50, "2600", "CNC").await;
    fill(&env, OPTION, "NFO", "BUY", 65, "100", "NRML").await;
    let range = env.sb.report_range(None, None).unwrap();
    assert_eq!(
        (range.from.to_string(), range.to.to_string()),
        ("2026-09-06".to_string(), "2026-10-05".to_string())
    );
    let r = env.sb.cost_report(range).await.unwrap();
    assert_eq!(r["trades_total"], json!(3));
    assert_eq!(r["trades_listed"], json!(3));
    // 172.28 + 158.46 + 26.34, and 1,25,000 + 1,30,000 + 6,500.
    assert_eq!(r["totals"]["total_charges"], json!(357.08));
    assert_eq!(r["totals"]["turnover"], json!(261500.0));
    let segs: Vec<(&str, i64)> = r["by_segment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["segment"].as_str().unwrap(),
                b["trades"].as_i64().unwrap(),
            )
        })
        .collect();
    // RELIANCE ends like an option and is reported as a share.
    assert_eq!(segs, [("equity_delivery", 2), ("fno_options", 1)]);
    assert_eq!(r["by_segment"][0]["label"], json!("Equity Delivery"));
    assert_eq!(r["by_day"][0]["date"], json!("2026-10-05"));
    let newest = &r["trades"][0];
    assert_eq!(newest["symbol"], json!(OPTION));
    assert_eq!(newest["segment_label"], json!("F&O Options"));
    assert_eq!(newest["total_charges"], json!(26.34));
    assert_eq!(newest["turnover"], json!(6500.0));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_a_fill_before_0530_ist_is_reported_under_its_ist_day
/// (c38c5810f): 23:05 UTC is 04:35 IST the next day. The fill is stamped
/// with the IST wall clock and the report counts IST days from the injected
/// clock, never the machine's own date.
#[tokio::test]
async fn a_fill_before_0530_ist_is_reported_under_its_ist_day() {
    let env = Env::at("2026-11-24 04:35:00");
    env.charges(true);
    // A machine on UTC reads its own date as the day before.
    assert_eq!(env.clock.now().date_naive().to_string(), "2026-11-23");
    let id = fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    assert!(trade(&env, &id)
        .trade_timestamp
        .starts_with("2026-11-24 04:35"));
    let today = env.sb.report_range(None, None).unwrap();
    assert_eq!(today.to.to_string(), "2026-11-24");
    let r = env.sb.cost_report(today).await.unwrap();
    assert_eq!(r["trades_total"], json!(1));
    assert_eq!(r["by_day"][0]["date"], json!("2026-11-24"));
    let named = env
        .sb
        .report_range(Some("2026-11-24"), Some("2026-11-24"))
        .unwrap();
    assert_eq!(
        env.sb.cost_report(named).await.unwrap()["trades_total"],
        json!(1)
    );
    let day_before = env
        .sb
        .report_range(Some("2026-11-23"), Some("2026-11-23"))
        .unwrap();
    assert_eq!(
        env.sb.cost_report(day_before).await.unwrap()["trades_total"],
        json!(0)
    );
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_a_range_is_whole_ist_days_and_trades_outside_it_are_left_out,
/// ::test_with_no_dates_the_report_covers_the_last_thirty_days and
/// ::test_a_date_the_report_cannot_read_is_explained
#[tokio::test]
async fn a_range_is_whole_ist_days() {
    let env = env().await;
    env.set_time("2026-10-05 09:16:00");
    fill(&env, "RELIANCE", "NSE", "BUY", 1, "2500", "CNC").await;
    env.set_time("2026-10-06 15:00:00");
    fill(&env, "RELIANCE", "NSE", "BUY", 1, "2500", "CNC").await;
    let r = |from: &str, to: &str| env.sb.report_range(Some(from), Some(to)).unwrap();
    assert_eq!(
        env.sb
            .cost_report(r("2026-10-05", "2026-10-05"))
            .await
            .unwrap()["trades_total"],
        json!(1)
    );
    assert_eq!(
        env.sb
            .cost_report(r("2026-10-05", "2026-10-06"))
            .await
            .unwrap()["trades_total"],
        json!(2)
    );
    assert_eq!(
        env.sb
            .cost_report(r("2026-10-07", "2026-10-08"))
            .await
            .unwrap()["trades_total"],
        json!(0)
    );
    let last = env.sb.report_range(None, Some("2026-10-06")).unwrap();
    assert_eq!(last.from.to_string(), "2026-09-07");
    for (from, to, msg) in [
        (
            "05-10-2026",
            "",
            "Dates must look like 2026-11-23 (year, month, day).",
        ),
        (
            "",
            "yesterday",
            "Dates must look like 2026-11-23 (year, month, day).",
        ),
        (
            "2026-10-06",
            "2026-10-05",
            "The start date is after the end date.",
        ),
    ] {
        let e = env.sb.report_range(Some(from), Some(to)).unwrap_err();
        assert_eq!((e.http_status, e.message.as_str()), (400, msg));
    }
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_the_export_lists_every_trade_with_its_breakdown
#[tokio::test]
async fn the_export_lists_every_trade_with_its_breakdown() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    let range = env.sb.report_range(None, None).unwrap();
    let rows = env.sb.cost_report_trades(range).await.unwrap();
    let csv = openalgo_desktop_lib::services::sandbox_export_service::cost_report_csv(&rows);
    let mut lines = csv.split("\r\n");
    assert_eq!(
        lines.next(),
        Some("Timestamp,Trade ID,Order ID,Symbol,Exchange,Product,Segment,Action,Quantity,Price,Turnover,Brokerage,STT/CTT,Exchange Charges,SEBI Charges,Stamp Duty,GST,Total Charges,Strategy")
    );
    let row = lines.next().unwrap();
    assert!(row.starts_with("2026-10-05 10:00:00,TRADE-"), "{row}");
    assert!(
        row.ends_with(",RELIANCE,NSE,CNC,Equity Delivery,BUY,50,2500.0,125000.0,20.0,125.0,3.84,0.13,19.0,4.31,172.28,test"),
        "{row}"
    );
    let empty = env
        .sb
        .report_range(Some("2026-01-01"), Some("2026-01-02"))
        .unwrap();
    assert!(env.sb.cost_report_trades(empty).await.unwrap().is_empty());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// My P&L, the reset, the day boundary and the snapshots
// ---------------------------------------------------------------------------

/// Web test_sandbox_charges_api.py::test_the_pnl_page_shows_the_charges_beside_a_gross_pnl
#[tokio::test]
async fn the_pnl_page_shows_the_charges_beside_a_gross_pnl() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    fill(&env, "RELIANCE", "NSE", "SELL", 50, "2600", "CNC").await;
    let v = serde_json::to_value(env.sb.mypnl().await.unwrap()).unwrap();
    let s = &v["data"]["summary"];
    assert_eq!(s["today_realized_pnl"], json!(5000.0));
    assert_eq!(s["today_charges"], json!(330.74));
    assert_eq!(s["total_charges"], json!(330.74));
    assert_eq!(s["today_net_mtm"], json!(4669.26));
    assert_eq!(s["all_time_net_realized_pnl"], json!(4669.26));
    assert_eq!(v["data"]["trades"][0]["charges"], json!(158.46));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_resetting_the_sandbox_clears_the_charges_and_restores_the_plan
#[tokio::test]
async fn resetting_the_sandbox_clears_the_charges_and_restores_the_plan() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    env.sb
        .save_charge_settings(
            json!({"segments": {"fno_options": {"flat": "5", "percent": "0"}}}),
            Market::Indian,
        )
        .await
        .unwrap();
    env.sb.reset().await.unwrap();
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(
        (f.total_charges, f.today_charges),
        (Decimal::ZERO, Decimal::ZERO)
    );
    assert_eq!(f.available_balance, d(CAPITAL));
    let s = env.sb.charge_settings(Market::Indian).await.unwrap();
    assert_eq!(s["segments"][3]["flat"], json!("20"));
    assert_eq!(s["enabled"], json!(true));
    env.shutdown().await;
}

/// Web test_sandbox_charges_api.py::test_a_day_that_only_paid_charges_still_has_them_cleared_the_next_day
/// and ::test_a_missed_snapshot_gets_that_days_charges_from_that_days_trades:
/// a day of buying only realizes nothing, so the reset and the backfill key
/// on the charges too.
#[tokio::test]
async fn a_day_that_only_paid_charges_is_reset_and_snapshotted() {
    let env = env().await;
    fill(&env, "RELIANCE", "NSE", "BUY", 50, "2500", "CNC").await;
    // The app was closed over the night: no 03:00 reset, no 23:59 snapshot.
    env.set_time("2026-10-06 09:30:00");
    env.sb.catch_up().await.unwrap().unwrap();
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.today_charges, Decimal::ZERO);
    assert_eq!(f.total_charges, d("172.28"));
    let v = serde_json::to_value(env.sb.mypnl().await.unwrap()).unwrap();
    let day = &v["data"]["daily_pnl"][0];
    assert_eq!(day["date"], json!("2026-10-05"));
    assert_eq!(day["charges"], json!(172.28));
    assert_eq!(day["net_mtm"], json!(-172.28));
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// One order's charges (the /api/v1/charges calculation)
// ---------------------------------------------------------------------------

fn order(sym: &str, ex: &str, product: &str, action: &str, qty: &str, price: &str) -> OrderCharges {
    OrderCharges {
        symbol: sym.into(),
        exchange: ex.into(),
        product: product.into(),
        action: action.into(),
        quantity: d(qty),
        price: d(price),
    }
}

/// Web test_charges_api.py::test_crypto_is_charged_nothing_until_a_fee_is_entered,
/// ::test_a_crypto_fee_is_charged_as_entered_with_nothing_added and
/// ::test_a_fractional_crypto_quantity_is_priced_as_given: the contract
/// value is in the trade value.
#[tokio::test]
async fn crypto_is_charged_only_the_fee_the_trader_enters() {
    let env = Env::at(T);
    let r = env
        .sb
        .order_charges(order("BTCUSD.P", "CRYPTO", "NRML", "BUY", "10", "62000"))
        .await
        .unwrap();
    assert_eq!(r["segment"], json!("Crypto Futures and Spot"));
    assert_eq!(r["trade_value"], json!(620.0));
    assert_eq!(r["charges"]["total"], json!(0.0));
    env.sb
        .save_charge_settings(
            json!({"segments": {"crypto_futures": {"flat": "0", "percent": "0.05"}}}),
            Market::Crypto,
        )
        .await
        .unwrap();
    let r = env
        .sb
        .order_charges(order("BTCUSD.P", "CRYPTO", "NRML", "SELL", "1000", "62000"))
        .await
        .unwrap();
    assert_eq!(r["charges"]["brokerage"], json!(31.0));
    assert_eq!(r["charges"]["total"], json!(31.0));
    assert_eq!(
        r["charges"]["taxes"],
        json!({"gst": 0.0, "stt": 0.0, "stamp_duty": 0.0})
    );
    let r = env
        .sb
        .order_charges(order("BTCUSD.P", "CRYPTO", "NRML", "BUY", "2.5", "60000"))
        .await
        .unwrap();
    assert_eq!(r["trade_value"], json!(150.0));
    env.shutdown().await;
}

/// Web test_charges_api.py::test_the_estimate_does_not_depend_on_the_sandbox_switch,
/// ::test_a_symbol_that_is_not_in_the_master_contract_is_refused and
/// ::test_an_index_cannot_be_ordered_so_it_has_no_charges
#[tokio::test]
async fn an_order_is_priced_whatever_the_switch_and_refused_where_it_cannot_trade() {
    let env = Env::at(T);
    env.charges(false);
    let r = env
        .sb
        .order_charges(order("RELIANCE", "NSE", "CNC", "BUY", "50", "2500"))
        .await
        .unwrap();
    assert_eq!(r["charges"]["total"], json!(172.28));
    let e = env
        .sb
        .order_charges(order("NOSUCHSCRIP", "NSE", "CNC", "BUY", "1", "10"))
        .await
        .unwrap_err();
    assert_eq!(
        (e.http_status, e.message.as_str()),
        (400, "Symbol NOSUCHSCRIP not found on NSE")
    );
    let e = env
        .sb
        .order_charges(order("SENSEX", "BSE_INDEX", "MIS", "BUY", "1", "80000"))
        .await
        .unwrap_err();
    assert_eq!(
        (e.http_status, e.message.as_str()),
        (
            400,
            "Orders cannot be placed on BSE_INDEX, so no charges apply to it"
        )
    );
    env.shutdown().await;
}

/// Integration review of 1.0.10 (security lens): a trade value past what
/// charges are worked out for (10^15) is refused with the reason, for one
/// order and in the calculator. The price times the quantity was multiplied
/// with rust_decimal's operator, which panics past about 7.9 x 10^28, and
/// the release build aborts on a panic: one request (an API key holder's,
/// or the calculator as the trader types) ended the app, live risk
/// monitoring and the feed with it. A test build unwinds instead, and the
/// request failed as a sandbox fault. The web prices any size.
#[tokio::test]
async fn a_trade_too_large_to_price_is_refused_not_a_crash() {
    const ORDER: &str =
        "The trade value is too large to calculate charges. Check the price and the quantity.";
    const ESTIMATE: &str =
        "This trade is too large or too small to estimate. Check the prices and the quantity.";
    let env = Env::at(T);
    // 10^9 shares at 10^20: past what a decimal holds.
    let e = env
        .sb
        .order_charges(order(
            "RELIANCE",
            "NSE",
            "CNC",
            "BUY",
            "1000000000",
            "100000000000000000000",
        ))
        .await
        .unwrap_err();
    assert_eq!((e.http_status, e.message.as_str()), (400, ORDER));
    // A crypto quantity has no 32-bit cap; 10^20 contracts are past the
    // bound even at a contract value of 0.001.
    let e = env
        .sb
        .order_charges(order(
            "BTCUSD.P",
            "CRYPTO",
            "NRML",
            "BUY",
            "100000000000000000000",
            "62000",
        ))
        .await
        .unwrap_err();
    assert_eq!((e.http_status, e.message.as_str()), (400, ORDER));
    // The calculator: 10^15 at 10^15 overflowed the multiply; 10^-28 lots
    // of an option overflowed the points to breakeven (the flat brokerage
    // divided by the quantity).
    for trade in [
        json!({"segment": "equity_intraday", "exchange": "NSE", "buy_price": "1e15",
               "sell_price": "1e15", "quantity": "1e15"}),
        json!({"segment": "fno_options", "exchange": "NFO", "buy_price": "100",
               "sell_price": "101", "quantity": "0.0000000000000000000000000001"}),
    ] {
        let e = env
            .sb
            .estimate_charges(trade.clone(), Market::Indian)
            .await
            .unwrap_err();
        assert_eq!(
            (e.http_status, e.message.as_str()),
            (400, ESTIMATE),
            "{trade}"
        );
    }
    // A large trade inside the bound is still priced.
    let r = env
        .sb
        .order_charges(order("RELIANCE", "NSE", "CNC", "BUY", "1000000", "2500"))
        .await
        .unwrap();
    assert_eq!(r["trade_value"], json!(2500000000.0));
    env.shutdown().await;
}
