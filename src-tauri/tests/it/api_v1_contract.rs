//! The `/api/v1` contract test: every golden fixture recorded from OpenAlgo
//! web (`tests/fixtures/web/rest/**`) replayed in-process against the full
//! router, with the mock broker scripted from the fixtures (market data,
//! history, margin) and the sandbox engine on a manual clock at the
//! recording time (analyzer-mode fixtures).
//!
//! Session 2 (the order-mutating capture) is replayed in its recorded
//! order, so each request meets the state it was recorded against; order
//! and GTT ids the desktop generates are mapped onto the recorded ones.
//! Session 1 (read-only) follows.
//!
//! Errors are compared exactly (status and body); successes by status and
//! shape (keys and value types). Fixtures that record a web defect are
//! asserted against the desktop's corrected behaviour (see `DIVERGENT`).

use crate::api_v1_support;

use api_v1_support::{all_fixtures, fixture, same_shape, H};
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use openalgo_desktop_lib::brokers::types::{Candle, MarginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Fixtures not replayed here, each with the reason it still holds.
const SKIP: &[(&str, &str)] = &[
    (
        "chart/",
        "/api/v1/chart (the web's chart preference store) is not served by the desktop: \
         the /trading terminal keeps its layout in the browser and no page calls it; \
         the gap is listed in docs/audit/2026-10-03/04-api-parity.md, section 2.6",
    ),
    (
        "portfolio/",
        "/api/v1/portfolio is the web's Portfolio Backtester, out of scope because it \
         needs the web's Python backtester (CLAUDE.md, scope decisions, 2026-10-03)",
    ),
    (
        "errors/rate_limit_probe_summary.json",
        "a summary record of the burst, not a request",
    ),
];

/// Session 2 in recorded order (ANALYZER_SESSION.md). One deliberate
/// change: the two trigger-pending cancels run before `cancelallorder`,
/// because the web's cancel-all skipped trigger-pending orders (the first
/// web defect) and the desktop cancels them, so the recorded order would
/// make the two single cancels meet already-cancelled orders.
const SESSION2: &[&str] = &[
    "analyzer/status_before_mutations",
    "expiry/nifty_nfo_futures_session2",
    "expiry/crudeoil_mcx_futures_session2",
    "expiry/nifty_nfo_options_session2",
    "optionsymbol/nifty_atm_ce_session2",
    "symbol/nifty_future_session2",
    "symbol/crudeoil_future_session2",
    "quotes/reliance_session2",
    "quotes/sbin_session2",
    "placeorder/market_buy_mis_reliance",
    "placeorder/market_sell_mis_sbin",
    "placeorder/limit_buy_cnc_sbin_far_below",
    "placeorder/limit_buy_mis_reliance_far_below",
    "placeorder/limit_sell_mis_reliance_far_above",
    "placeorder/sl_buy_mis_reliance",
    "placeorder/slm_sell_mis_sbin",
    "placeorder/market_buy_cnc_sbin",
    "placeorder/market_buy_nrml_nifty_future",
    "placeorder/market_buy_nrml_nifty_option",
    "placeorder/market_buy_mis_nifty_option",
    "placeorder/market_buy_nrml_crudeoil_future_mcx",
    "placeorder/lowercase_action_buy",
    "placeorder/error_unknown_symbol",
    "placeorder/error_quantity_zero",
    "placeorder/error_quantity_negative",
    "placeorder/error_quantity_fractional_nse",
    "placeorder/error_negative_price",
    "placeorder/error_bad_product",
    "placeorder/error_bad_pricetype",
    "placeorder/error_bad_action",
    "placeorder/error_bad_exchange",
    "placeorder/error_missing_symbol",
    "placeorder/error_missing_strategy",
    "placeorder/error_limit_price_zero",
    "placeorder/error_sl_without_trigger",
    "placeorder/error_option_qty_not_lot_multiple",
    "placeorder/error_invalid_apikey",
    "orderstatus/market_buy_mis_reliance",
    "orderstatus/limit_buy_cnc_sbin_far_below",
    "orderstatus/sl_buy_mis_reliance",
    "orderstatus/slm_sell_mis_sbin",
    "orderstatus/market_buy_nrml_nifty_option",
    "orderstatus/unknown_orderid_session2",
    "openposition/reliance_mis_long",
    "openposition/sbin_mis_short",
    "openposition/nifty_future_nrml",
    "openposition/crudeoil_future_nrml",
    "openposition/no_position_session2",
    "modifyorder/open_limit_price_and_qty",
    "orderstatus/after_modify",
    "modifyorder/sl_order_trigger",
    "modifyorder/error_unknown_orderid",
    "modifyorder/error_completed_order",
    "modifyorder/error_missing_price",
    "modifyorder/error_negative_price",
    "cancelorder/open_limit",
    "orderstatus/after_cancel",
    "cancelorder/error_already_cancelled",
    "cancelorder/error_completed_order",
    "cancelorder/error_unknown_orderid",
    "cancelorder/error_missing_orderid",
    "modifyorder/error_cancelled_order",
    "placesmartorder/open_from_flat_to_10",
    "openposition/infy_after_smart_open",
    "placesmartorder/raise_10_to_15",
    "placesmartorder/reduce_15_to_5",
    "placesmartorder/no_action_already_at_5",
    "placesmartorder/to_zero",
    "placesmartorder/flat_to_short_minus_3",
    "openposition/infy_after_smart_short",
    "placesmartorder/error_missing_position_size",
    "placesmartorder/error_unknown_symbol",
    "basketorder/three_legs_mixed",
    "basketorder/one_leg_unknown_symbol",
    "basketorder/error_empty_orders",
    "basketorder/error_leg_bad_product",
    "splitorder/sbin_10_split_3",
    "splitorder/error_splitsize_zero",
    "splitorder/error_missing_splitsize",
    "optionsorder/atm_ce_buy",
    "optionsorder/itm2_pe_buy",
    "optionsorder/otm3_ce_sell",
    "optionsorder/atm_ce_buy_with_splitsize",
    "optionsorder/error_bad_offset",
    "optionsorder/error_bad_expiry",
    "optionsorder/error_qty_not_lot_multiple",
    "optionsmultiorder/bull_call_spread_2_legs",
    "optionsmultiorder/error_empty_legs",
    "optionsmultiorder/one_leg_bad_offset",
    "placegttorder/single_buy_cnc_trigger_below",
    "placegttorder/oco_sell_cnc",
    "placegttorder/error_mis_product",
    "placegttorder/error_bad_trigger_type",
    "placegttorder/error_oco_missing_legs",
    "placegttorder/error_unknown_symbol",
    "gttorderbook/after_place",
    "gttorderbook/status_active",
    "modifygttorder/single_change_trigger_and_qty",
    "modifygttorder/oco_change_target",
    "modifygttorder/error_unknown_trigger_id",
    "modifygttorder/error_missing_trigger_id",
    "gttorderbook/after_modify",
    "cancelgttorder/single",
    "cancelgttorder/error_already_cancelled",
    "cancelgttorder/error_unknown_trigger_id",
    "cancelgttorder/error_missing_trigger_id",
    "modifygttorder/error_cancelled_trigger",
    "gttorderbook/after_cancel_all_statuses",
    "gttorderbook/status_cancelled",
    "gttorderbook/error_bad_status",
    "placesmartorder/no_action_qty_nonzero_position_matches",
    "orderbook/populated",
    "tradebook/populated",
    "positionbook/populated",
    "holdings/after_cnc_buys",
    "funds/after_activity",
    "pnl/symbols_populated",
    "cancelorder/trigger_pending_sl",
    "cancelorder/trigger_pending_slm",
    "cancelallorder/with_open_orders",
    "cancelallorder/nothing_open",
    "cancelallorder/error_missing_strategy",
    "closeposition/with_open_positions",
    "positionbook/after_closeposition",
    "closeposition/no_open_positions",
    "closeposition/error_missing_strategy",
    "orderbook/after_cleanup",
    "tradebook/after_cleanup",
    "funds/after_cleanup",
    "pnl/symbols_after_cleanup",
    "cancelgttorder/oco",
    "gttorderbook/after_all_cancelled",
    "analyzer/toggle_error_missing_mode",
    "analyzer/toggle_error_invalid_mode",
    "analyzer/toggle_to_live_false",
    "analyzer/status_while_live",
    "analyzer/toggle_back_to_analyze_true",
    "analyzer/status_after_round_trip",
    "cancelgttorder/oco_retry",
    "cancelallorder/final_cleanup",
    "closeposition/final_cleanup",
    "gttorderbook/final",
    "positionbook/final",
    "funds/final",
    "analyzer/status_final",
    "gttorderbook/status_all",
];

/// What the desktop answers where the fixture records a web defect.
enum Divergent {
    /// Status and a message prefix.
    Error(u16, &'static str),
    /// A success with this exact body shape.
    Shape(u16, Value),
    /// Text with this status and content type.
    Text(u16, &'static str),
}

fn divergent(rel: &str) -> Option<(Divergent, &'static str)> {
    Some(match rel {
        // Web defect 2: GTTModifyFailedEvent has no `exchange` field, so every
        // failed sandbox modify became a 500.
        "modifygttorder/error_unknown_trigger_id.json"
        | "modifygttorder/error_cancelled_trigger.json" => (
            Divergent::Error(404, "No active GTT with trigger_id"),
            "web defect 2: the sandbox's own 404 instead of the event TypeError 500",
        ),
        // Web defect 3: the OCO cancel could not release its margin.
        "cancelgttorder/oco.json" => (
            Divergent::Shape(
                200,
                json!({"mode": "analyze", "status": "success", "trigger_id": "x"}),
            ),
            "web defect 3: the margin release succeeds, so the OCO cancels",
        ),
        "cancelgttorder/oco_retry.json" => (
            Divergent::Error(404, "No active GTT with trigger_id"),
            "web defect 3: the OCO was already cancelled by the first attempt",
        ),
        // Web: every text-format ticker error is a 500 from flask-restx.
        "ticker/invalid_apikey_txt.json" => (
            Divergent::Text(403, "text/plain"),
            "web defect (INDEX 'BUG'): text errors answered as text with their own status",
        ),
        _ => return None,
    })
}

/// Recorded id -> desktop id (order ids, GTT ids).
#[derive(Default)]
struct Ids(Vec<(String, String)>);

impl Ids {
    fn learn(&mut self, want: &Value, got: &Value) {
        match (want, got) {
            (Value::Object(w), Value::Object(g)) => {
                for (k, wv) in w {
                    if let Some(gv) = g.get(k) {
                        if k == "orderid" || k == "trigger_id" {
                            if let (Some(a), Some(b)) = (wv.as_str(), gv.as_str()) {
                                if a != b && !self.0.iter().any(|(x, _)| x == a) {
                                    self.0.push((a.to_string(), b.to_string()));
                                }
                            }
                        } else {
                            self.learn(wv, gv);
                        }
                    }
                }
            }
            (Value::Array(w), Value::Array(g)) => {
                for (a, b) in w.iter().zip(g.iter()) {
                    self.learn(a, b);
                }
            }
            _ => {}
        }
    }

    fn apply(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (a, b) in &self.0 {
            out = out.replace(a.as_str(), b.as_str());
        }
        out
    }
}

fn request_of(f: &Value, key: &str, ids: &Ids) -> Request<Body> {
    let req = &f["request"];
    let method = Method::from_bytes(req["method"].as_str().unwrap().as_bytes()).unwrap();
    let path = ids.apply(&req["path"].as_str().unwrap().replace("<APIKEY>", key));
    let mut b = Request::builder().method(method).uri(path);
    if let Some(h) = req["headers"].as_object() {
        for (k, v) in h {
            b = b.header(k.as_str(), v.as_str().unwrap().replace("<APIKEY>", key));
        }
    }
    let body = match &req["body"] {
        Value::Null => Body::empty(),
        Value::String(s) => Body::from(ids.apply(&s.replace("<APIKEY>", key))),
        other => Body::from(ids.apply(&other.to_string().replace("<APIKEY>", key))),
    };
    b.body(body).unwrap()
}

fn candle_of(v: &Value) -> Candle {
    let f = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Candle {
        timestamp: f("timestamp") as i64,
        open: f("open"),
        high: f("high"),
        low: f("low"),
        close: f("close"),
        volume: f("volume") as i64,
        oi: f("oi") as i64,
    }
}

/// Candles from a ticker text body (`EXCH:SYM,date[,time],o,h,l,c,v`, IST).
fn candles_from_text(text: &str) -> Vec<Candle> {
    text.lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            let (ts, rest) = if c.len() == 8 {
                (format!("{} {}", c[1], c[2]), &c[3..])
            } else if c.len() == 7 {
                (format!("{} 00:00:00", c[1]), &c[2..])
            } else {
                return None;
            };
            let naive = NaiveDateTime::parse_from_str(&ts, "%Y-%m-%d %H:%M:%S").ok()?;
            let t = Kolkata.from_local_datetime(&naive).single()?.timestamp();
            Some(Candle {
                timestamp: t,
                open: rest[0].parse().ok()?,
                high: rest[1].parse().ok()?,
                low: rest[2].parse().ok()?,
                close: rest[3].parse().ok()?,
                volume: rest[4].parse().ok()?,
                oi: 0,
            })
        })
        .collect()
}

/// Script the mock broker for one fixture (history, ticker, margin).
fn script(h: &H, rel: &str, f: &Value) {
    let body = &f["response"]["body"];
    if rel.starts_with("history/") || rel.starts_with("ticker/") {
        let candles: Vec<Candle> = if let Some(t) = body.get("_raw_text").and_then(Value::as_str) {
            candles_from_text(t)
        } else {
            body["data"]
                .as_array()
                .map(|a| a.iter().map(candle_of).collect())
                .unwrap_or_default()
        };
        *h.mock.history.lock() = Some(Ok(candles));
    }
    if rel.starts_with("margin/") {
        let d = &body["data"];
        let f = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        *h.mock.margin.lock() = Some(Ok(MarginResult {
            total_margin_required: f("total_margin_required"),
            span_margin: f("span_margin"),
            exposure_margin: f("exposure_margin"),
        }));
    }
}

async fn replay(h: &H, rel: &str, ids: &mut Ids) -> Result<(), String> {
    let f = fixture(rel);
    script(h, rel, &f);
    let (status, headers, bytes) = h.send(request_of(&f, &h.key, ids)).await;
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let want = &f["response"]["body"];
    let got: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some((d, _why)) = divergent(rel) {
        return match d {
            Divergent::Error(st, prefix) => {
                let msg = got["message"].as_str().unwrap_or_default();
                if status.as_u16() == st && msg.starts_with(prefix) {
                    Ok(())
                } else {
                    Err(format!(
                        "divergent: want {} '{}...', got {} {}",
                        st, prefix, status, got
                    ))
                }
            }
            Divergent::Shape(st, shape) => {
                if status.as_u16() != st {
                    return Err(format!("divergent: want {}, got {} {}", st, status, got));
                }
                same_shape(&shape, &got)
            }
            Divergent::Text(st, ctype) => {
                if status.as_u16() == st && ct.starts_with(ctype) {
                    Ok(())
                } else {
                    Err(format!(
                        "divergent: want {} {}, got {} {}",
                        st, ctype, status, ct
                    ))
                }
            }
        };
    }
    let want_status = f["response"]["status_code"].as_u64().unwrap() as u16;
    if status.as_u16() != want_status {
        return Err(format!(
            "status: want {}, got {} {}",
            want_status,
            status,
            String::from_utf8_lossy(&bytes)
        ));
    }
    if let Some(raw) = want.get("_raw_text").and_then(Value::as_str) {
        let want_ct = f["response"]["headers"]["Content-Type"]
            .as_str()
            .unwrap_or("");
        if want_ct.starts_with("text/html") {
            if !ct.starts_with("text/html") {
                return Err(format!("content type {}", ct));
            }
            return Ok(());
        }
        if !ct.starts_with(want_ct) {
            return Err(format!("content type: want {}, got {}", want_ct, ct));
        }
        let text = String::from_utf8_lossy(&bytes);
        let (w, g) = (raw.lines().next(), text.lines().next());
        return if w == g {
            Ok(())
        } else {
            Err(format!("first line: want {:?}, got {:?}", w, g))
        };
    }
    if let Some(lines) = want.get("_csv_first_lines").and_then(Value::as_array) {
        if !ct.starts_with("text/csv") {
            return Err(format!("content type {}", ct));
        }
        let text = String::from_utf8_lossy(&bytes);
        let got_lines: Vec<&str> = text.split("\r\n").collect();
        for (n, l) in lines.iter().take(3).enumerate() {
            if Some(&l.as_str().unwrap_or_default()) != got_lines.get(n) {
                return Err(format!(
                    "csv line {}: want {:?}, got {:?}",
                    n,
                    l,
                    got_lines.get(n)
                ));
            }
        }
        return Ok(());
    }
    if want_status >= 400 {
        let want = serde_json::from_str::<Value>(&ids.apply(&want.to_string())).unwrap();
        return if got == want {
            Ok(())
        } else {
            Err(format!("body: want {}, got {}", want, got))
        };
    }
    // A list the web returned entries in must not come back empty, except
    // where the web state itself differs (the OCO the web could not cancel)
    // or the mock broker offers fewer timeframes than Kite (the bucketing
    // is unit-tested with Kite's map).
    let lenient = matches!(
        rel,
        "gttorderbook/after_all_cancelled.json"
            | "gttorderbook/final.json"
            | "intervals/default.json"
    );
    api_v1_support::STRICT_ARRAYS.with(|c| c.set(!lenient));
    let r = same_shape(want, &got);
    api_v1_support::STRICT_ARRAYS.with(|c| c.set(false));
    r?;
    ids.learn(want, &got);
    Ok(())
}

/// Web `ORDER_RATE_LIMIT` (10/s per address): ten requests, then the
/// fixture's 429 body exactly.
async fn replay_rate_limit(h: &H, rel: &str) -> Result<(), String> {
    let f = fixture(rel);
    let ip = h.next_ip();
    for _ in 0..10 {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/placeorder")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let _ = h.send_from(req, ip).await;
    }
    let (status, headers, bytes) = h
        .send_from(request_of(&f, &h.key, &Ids::default()), ip)
        .await;
    let got: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status != StatusCode::TOO_MANY_REQUESTS || got != f["response"]["body"] {
        return Err(format!("429: got {} {}", status, got));
    }
    if headers.get("retry-after").is_some() || headers.get("x-ratelimit-limit").is_some() {
        return Err("rate-limit headers present".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_web_fixture_replays_against_the_desktop() {
    let h = H::new().await;
    h.analyze(true);
    let all = all_fixtures();
    let mut skipped: BTreeMap<String, &str> = BTreeMap::new();
    let mut to_run: Vec<String> = Vec::new();
    for rel in &all {
        match SKIP.iter().find(|(p, _)| rel.starts_with(p)) {
            Some((_, why)) => {
                skipped.insert(rel.clone(), why);
            }
            None => to_run.push(rel.clone()),
        }
    }
    let mut ids = Ids::default();
    let mut failures: Vec<String> = Vec::new();
    let mut passed = 0usize;
    let mut divergent_passed = Vec::new();
    let mut done = std::collections::BTreeSet::new();
    let session2: Vec<String> = SESSION2.iter().map(|s| format!("{}.json", s)).collect();
    for rel in session2.iter().chain(to_run.iter()) {
        if !to_run.contains(rel) || !done.insert(rel.clone()) {
            continue;
        }
        let r = if rel == "errors/rate_limit_429_placeorder.json" {
            replay_rate_limit(&h, rel).await
        } else {
            replay(&h, rel, &mut ids).await
        };
        match r {
            Ok(()) => {
                passed += 1;
                if let Some((_, why)) = divergent(rel) {
                    divergent_passed.push(format!("{} ({})", rel, why));
                }
            }
            Err(e) => failures.push(format!("{}: {}", rel, e)),
        }
    }
    let total = to_run.len();
    println!(
        "api_v1 contract: {} of {} fixtures pass ({} skipped, {} files in all)",
        passed,
        total,
        skipped.len(),
        all.len()
    );
    for d in &divergent_passed {
        println!("  divergent by design: {}", d);
    }
    for (rel, why) in &skipped {
        println!("  skipped {}: {}", rel, why);
    }
    for f in &failures {
        println!("  FAIL {}", f);
    }
    h.shutdown().await;
    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed:\n{}",
        failures.len(),
        total,
        failures.join("\n")
    );
    assert_eq!(passed, total);
}
