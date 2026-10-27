//! The guard against the web's own verdicts.
//!
//! `tests/fixtures/agent/risk_vectors.json` was recorded by running the
//! web's `RiskGuard` (`scripts/agent/capture_web_orders.py`): sessions of
//! checks, claims, releases and commits on a fixed clock, with every
//! verdict field the model and the audit trail read. The desktop's pure
//! guard must give the same answer at every step.

use super::book::Book;
use super::*;
use crate::agent::safety::limits::RiskLimits;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;

const VECTORS: &str = include_str!("../../../../tests/fixtures/agent/risk_vectors.json");

fn set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn dec(v: &Value) -> Decimal {
    Decimal::from_str(v.as_str().unwrap_or("0")).unwrap_or_default()
}

fn limits_of(l: &Value) -> RiskLimits {
    RiskLimits {
        trading_enabled: l["trading_enabled"].as_bool().unwrap_or(false),
        require_analyzer_mode: l["require_analyzer_mode"].as_bool().unwrap_or(false),
        max_orders_per_session: l["max_orders_per_session"].as_i64().unwrap_or(0),
        max_order_quantity: l["max_order_quantity"].as_i64().unwrap_or(0),
        max_order_value: dec(&l["max_order_value"]),
        max_price_deviation_pct: dec(&l["max_price_deviation_pct"]),
        duplicate_order_window_seconds: l["duplicate_order_window_seconds"].as_i64().unwrap_or(0),
        allowed_exchanges: set(&l["allowed_exchanges"]),
        allowed_products: set(&l["allowed_products"]),
        symbol_allowlist: set(&l["symbol_allowlist"]),
        symbol_blocklist: set(&l["symbol_blocklist"]),
        max_funds_utilization_pct: dec(&l["max_funds_utilization_pct"]),
        allow_bulk_destructive: l["allow_bulk_destructive"].as_bool().unwrap_or(false),
        kill_switch_engaged: l["kill_switch"].as_bool().unwrap_or(false),
        kill_switch_file: "<kill_switch_file>".into(),
        kill_switch_path: PathBuf::from("<kill_switch_file>"),
    }
}

fn answer(v: &Verdict) -> Value {
    let mut d = v.as_dict();
    if let Some(details) = d["details"].as_object_mut() {
        if details.contains_key("claim_token") {
            details.insert("claim_token".into(), json!("<claim_token>"));
        }
    }
    d["message"] = json!(v.as_message());
    d
}

fn at(start: DateTime<Utc>, step: &Value) -> DateTime<Utc> {
    let seconds = step["at"].as_f64().unwrap_or(0.0);
    start + Duration::microseconds((seconds * 1_000_000.0).round() as i64)
}

fn obj(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// One step of a recorded session, run on the desktop's guard.
fn run_step(
    limits: &RiskLimits,
    file: bool,
    book: &mut Book,
    verdicts: &mut Vec<Option<Verdict>>,
    step: &Value,
    now: DateTime<Utc>,
) -> Value {
    let gate = Gate {
        kill_switch_file_present: file,
        analyzer_mode: step["analyzer_mode"].as_bool(),
    };
    let token = format!("token-{}", verdicts.len());
    let token = step["claim"]
        .as_bool()
        .unwrap_or(true)
        .then_some(token.as_str());
    let v = match step["call"].as_str().unwrap_or("") {
        "check_order" => check_order(limits, &gate, book, &obj(&step["args"]), now, token),
        "check_basket" => {
            let legs = step["legs"].as_array().cloned().unwrap_or_default();
            check_basket(
                limits,
                &gate,
                book,
                &legs,
                &step["available_funds"],
                now,
                token,
            )
        }
        "check_destructive" => {
            check_destructive(limits, &gate, &step["operation"], obj(&step["details"]))
        }
        call => {
            let of = step["of"].as_u64().unwrap_or(0) as usize;
            let target = verdicts[of].clone().expect("a verdict to settle");
            let done = if call == "release" {
                release(book, &target)
            } else {
                commit(book, &target)
            };
            verdicts.push(None);
            return json!(done);
        }
    };
    let out = answer(&v);
    verdicts.push(Some(v));
    out
}

#[test]
fn every_verdict_matches_the_webs() {
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    let start: DateTime<Utc> = vectors["start"].as_str().unwrap().parse().unwrap();
    let mut steps_run = 0;
    for case in vectors["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let limits = limits_of(&case["limits"]);
        let file = case["limits"]["kill_switch_file_present"]
            .as_bool()
            .unwrap();
        let mut book = Book::default();
        let mut verdicts = Vec::new();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let got = run_step(
                &limits,
                file,
                &mut book,
                &mut verdicts,
                step,
                at(start, step),
            );
            assert_eq!(got, step["expect"], "case '{}' step {}", name, i);
            assert_eq!(
                json!(book.orders_claimed),
                step["orders_claimed"],
                "case '{}' step {} count",
                name,
                i
            );
            steps_run += 1;
        }
    }
    assert!(steps_run > 100, "{} steps", steps_run);
}

/// The 24 codes, as the web's `RiskCode` lists them, and every one of them
/// reached by a recorded verdict.
#[test]
fn the_refusal_codes_are_the_webs_twenty_four() {
    let web = [
        "ok",
        "kill_switch",
        "trading_disabled",
        "analyzer_required",
        "analyzer_unknown",
        "invalid_arguments",
        "invalid_symbol",
        "symbol_blocked",
        "symbol_not_allowed",
        "exchange_not_allowed",
        "product_not_allowed",
        "invalid_action",
        "invalid_price_type",
        "invalid_price",
        "invalid_quantity",
        "quantity_exceeded",
        "session_cap_reached",
        "duplicate_order",
        "notional_exceeded",
        "price_deviation",
        "insufficient_funds",
        "empty_basket",
        "unknown_operation",
        "bulk_operation_not_allowed",
    ];
    let ours: Vec<&str> = RiskCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(ours, web);
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    let reached: BTreeSet<String> = vectors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["steps"].as_array().unwrap().iter())
        .filter_map(|s| s["expect"]["code"].as_str().map(str::to_string))
        .collect();
    for code in web {
        assert!(
            reached.contains(code),
            "no recorded verdict reaches {}",
            code
        );
    }
}

/// The eleven rules run in the web's order: with every rule failing at
/// once, each refusal appears only when every earlier rule passes.
#[test]
fn the_eleven_rules_run_in_order() {
    let base = |trading| {
        let mut l = RiskLimits::defaults(std::path::Path::new("/data"));
        l.trading_enabled = trading;
        l
    };
    let mut l = base(true);
    l.kill_switch_engaged = true;
    l.require_analyzer_mode = true;
    l.symbol_blocklist.insert("SBIN".into());
    l.allowed_exchanges.remove("NSE");
    l.allowed_products.remove("MIS");
    l.max_order_quantity = 1;
    l.max_orders_per_session = 0;
    l.max_order_value = Decimal::from(1);
    l.max_funds_utilization_pct = Decimal::ZERO;
    let order = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10,
        "product": "MIS", "price_type": "LIMIT", "price": 200, "ltp": 100, "available_funds": 1});
    let order = obj(&order);
    let gate = |file, analyzer| Gate {
        kill_switch_file_present: file,
        analyzer_mode: analyzer,
    };
    let now = Utc::now();
    let mut seen = Vec::new();
    fn lift(l: &mut RiskLimits, step: usize) {
        match step {
            0 => l.kill_switch_engaged = false,
            1 => {}
            2 => l.require_analyzer_mode = false,
            3 => {
                l.symbol_blocklist.clear();
            }
            4 => {
                l.allowed_exchanges.insert("NSE".into());
            }
            5 => {
                l.allowed_products.insert("MIS".into());
            }
            6 => l.max_order_quantity = 100,
            7 => l.max_orders_per_session = 20,
            8 => l.max_order_value = Decimal::from(10_000_000),
            9 => l.max_price_deviation_pct = Decimal::ONE_HUNDRED,
            _ => {}
        }
    }
    for step in 0..11 {
        let file = step < 2;
        let v = check_order(
            &l,
            &gate(file, Some(false)),
            &mut Book::default(),
            &order,
            now,
            Some("t"),
        );
        seen.push(v.code.as_str());
        lift(&mut l, step);
    }
    assert_eq!(
        seen,
        [
            "kill_switch",
            "kill_switch",
            "analyzer_required",
            "symbol_blocked",
            "exchange_not_allowed",
            "product_not_allowed",
            "quantity_exceeded",
            "session_cap_reached",
            "notional_exceeded",
            "price_deviation",
            "insufficient_funds"
        ]
    );
    let trading_off = base(false);
    assert_eq!(
        check_order(
            &trading_off,
            &gate(false, None),
            &mut Book::default(),
            &order,
            now,
            None
        )
        .code,
        RiskCode::TradingDisabled
    );
}

fn modify_limits() -> RiskLimits {
    let mut l = RiskLimits::defaults(std::path::Path::new("/data"));
    l.trading_enabled = true;
    l.max_order_quantity = 100;
    l
}

fn modified(quantity: i64, price: f64, ltp: Option<f64>) -> Map<String, Value> {
    let order = json!({"orderid": "A1", "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
        "quantity": quantity, "product": "MIS", "pricetype": "LIMIT", "price": price,
        "trigger_price": 0.0});
    modify_args(&obj(&order), ltp.map(Value::from).as_ref())
}

/// D-007: the web let a modify raise a quantity or move a price past every
/// limit, because it ran only the gate. The desktop checks the modified
/// order's exchange, product, quantity, notional and price deviation.
#[test]
fn a_modified_order_meets_the_quantity_price_and_notional_rules() {
    let l = modify_limits();
    let open = Gate {
        kill_switch_file_present: false,
        analyzer_mode: Some(false),
    };
    let details = |q: i64| obj(&json!({"orderid": "A1", "quantity": q}));
    let v = check_modify(&l, &open, &modified(101, 100.0, Some(100.0)), details(101));
    assert_eq!(v.code, RiskCode::QuantityExceeded, "{:?}", v);
    let v = check_modify(&l, &open, &modified(10, 120.0, Some(100.0)), details(10));
    assert_eq!(v.code, RiskCode::PriceDeviation, "{:?}", v);
    assert_eq!(v.reason, "The order price 120.0 is 20.00% away from the last traded price 100.0, beyond the 5% limit.");
    let v = check_modify(
        &l,
        &open,
        &modified(100, 6000.0, Some(6000.0)),
        details(100),
    );
    assert_eq!(v.code, RiskCode::NotionalExceeded, "{:?}", v);
    let v = check_modify(&l, &open, &modified(10, 101.0, None), details(10));
    assert!(v.allowed, "{:?}", v);
    assert_eq!(v.reason, "modify_order passed every risk check.");
    assert_eq!(v.details["operation"], "modify_order");
    assert_eq!(v.details["orderid"], "A1");
    assert_eq!(
        v.details["warnings"],
        json!(["No last traded price for SBIN, so the price-deviation check was skipped."])
    );
    // The web's check (the gate alone) let the first one through.
    assert!(check_destructive(&l, &open, &json!("modify_order"), details(101)).allowed);
}

#[test]
fn a_modify_never_claims_and_still_meets_the_gate() {
    let mut l = modify_limits();
    l.max_orders_per_session = 0;
    let open = Gate {
        kill_switch_file_present: false,
        analyzer_mode: None,
    };
    assert!(check_modify(&l, &open, &modified(10, 100.0, Some(100.0)), Map::new()).allowed);
    let killed = Gate {
        kill_switch_file_present: true,
        analyzer_mode: None,
    };
    assert_eq!(
        check_modify(&l, &killed, &modified(10, 100.0, Some(100.0)), Map::new()).code,
        RiskCode::KillSwitch
    );
}

/// Security review (2026-10-27): the same order restated with a price
