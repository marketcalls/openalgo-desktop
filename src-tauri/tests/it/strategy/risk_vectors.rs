//! The web's risk vectors (`test/risk/vectors.json`) through the Rust risk
//! core.

use openalgo_desktop_lib::risk::{evaluate_position, evaluate_trail, value_to_f64, PositionRisk};
use serde_json::Value;

fn close(a: Option<f64>, e: &Value, tol: f64) -> bool {
    match (a, e) {
        (None, Value::Null) => true,
        (Some(a), e) => e.as_f64().map(|e| (a - e).abs() <= tol).unwrap_or(false),
        _ => false,
    }
}

/// Every case in the web's `test/risk/vectors.json`, through both entry
/// points. The count is asserted so a dropped case cannot pass silently.
#[test]
fn every_case_in_vectors_json_passes() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/risk/vectors.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let tol = v["tolerance"].as_f64().unwrap();
    let cases = v["cases"].as_array().unwrap();
    let mut passed = 0;
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let d = evaluate_position(
            &PositionRisk::from_state(&c["state"]),
            value_to_f64(&c["ltp"]),
        );
        for (k, want) in c["expected"].as_object().unwrap() {
            let ok = match k.as_str() {
                "reason" => d.reason.map(|r| r.as_str()) == want.as_str(),
                "evaluated" => Some(d.evaluated) == want.as_bool(),
                "breached" => Some(d.breached) == want.as_bool(),
                "stop_moved" => Some(d.stop_moved) == want.as_bool(),
                "trail_armed" => Some(d.trail_armed) == want.as_bool(),
                "current_sl" => close(d.stop_price, want, tol),
                "highest_price" => close(d.highest_price, want, tol),
                "lowest_price" => close(d.lowest_price, want, tol),
                "pnl" => close(Some(d.pnl), want, tol),
                other => panic!("{}: unknown key {}", name, other),
            };
            assert!(ok, "{}: {} mismatched: {:?}", name, k, d);
        }
        let legacy = evaluate_trail(&c["state"], &c["ltp"]);
        assert_eq!(legacy["breached"], c["expected"]["breached"], "{}", name);
        assert_eq!(legacy["reason"], c["expected"]["reason"], "{}", name);
        passed += 1;
    }
    assert_eq!(passed, cases.len());
    assert_eq!(passed, 35, "35 golden vectors expected");
}
