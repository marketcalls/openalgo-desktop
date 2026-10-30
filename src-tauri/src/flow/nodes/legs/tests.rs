//! The readymade strategies' legs against the editor's templates
//! (`test_flow_custom_legs.py` TestTemplatesSeedWhatTheGeneratorBuilds,
//! parity X-021..X-023) and the leg log line.

use super::*;
use crate::flow::nodes::order_testing::Run;

/// The (offset, option type, action) triples of a generated strategy.
fn generated(strategy: &str, action: &str, width: &str) -> Vec<(String, String, String)> {
    let run = Run::new();
    let g = Generated {
        action,
        quantity: 1,
        product: "MIS",
        strangle_width: width,
        price_type: "MARKET",
        price: 0.0,
    };
    generate_strategy_legs(&run.ctx, strategy, &g)
        .iter()
        .map(|l| {
            let t = |k: &str| l[k].as_str().unwrap_or_default().to_string();
            (t("offset"), t("option_type"), t("action"))
        })
        .collect()
}

/// The editor's `seedLegsFromStrategy` case for `strategy`, read from
/// `src/lib/flow/customLegs.ts` (`leg(offset, type, action)` calls), with
/// `action` and `width` substituted for the variables.
fn seeded(
    source: &str,
    strategy: &str,
    action: &str,
    width: &str,
) -> Vec<(String, String, String)> {
    let Some(start) = source.find(&format!("case '{}'", strategy)) else {
        return Vec::new();
    };
    let rest = &source[start..];
    let end = ["case '", "default:"]
        .iter()
        .filter_map(|m| rest[1..].find(m).map(|e| e + 1))
        .min()
        .unwrap_or(rest.len());
    let block = &rest[..end];
    let unquote = |t: &str| {
        let t = t.trim();
        match t {
            "action" => action.to_string(),
            "width" => width.to_string(),
            _ => t.trim_matches('\'').to_string(),
        }
    };
    block
        .split("leg(")
        .skip(1)
        .filter_map(|call| {
            let args: Vec<&str> = call.split(')').next()?.split(',').collect();
            (args.len() == 3).then(|| (unquote(args[0]), unquote(args[1]), unquote(args[2])))
        })
        .collect()
}

fn custom_legs_ts() -> Option<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/flow/customLegs.ts");
    std::fs::read_to_string(path).ok()
}

/// X-021..X-023: loading a template and saving it untouched trades what
/// the generated strategy would have traded.
#[test]
fn the_editor_templates_match_the_generated_legs() {
    let source = custom_legs_ts().unwrap_or_default();
    assert!(
        source.contains("seedLegsFromStrategy"),
        "customLegs.ts not found"
    );
    for strategy in ["iron_condor", "bull_call_spread", "bear_put_spread"] {
        let backend = generated(strategy, "BUY", "OTM2");
        assert!(!backend.is_empty());
        assert_eq!(
            backend,
            seeded(&source, strategy, "BUY", "OTM2"),
            "{strategy}"
        );
    }
    for action in ["BUY", "SELL"] {
        assert_eq!(
            generated("straddle", action, "OTM2"),
            seeded(&source, "straddle", action, "OTM2")
        );
    }
    for width in ["OTM2", "OTM5"] {
        assert_eq!(
            generated("strangle", "SELL", width),
            seeded(&source, "strangle", "SELL", width)
        );
    }
}

/// parity N-optionsMultiOrder-09: each strategy's legs, and the warnings
/// for a custom strategy without legs and an unknown one.
#[test]
fn the_readymade_strategies_build_the_web_legs() {
    let triples = |v: &[(&str, &str, &str)]| {
        v.iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        generated("iron_condor", "BUY", "OTM3"),
        triples(&[
            ("OTM2", "CE", "SELL"),
            ("OTM4", "CE", "BUY"),
            ("OTM2", "PE", "SELL"),
            ("OTM4", "PE", "BUY")
        ])
    );
    assert_eq!(
        generated("strangle", "SELL", "OTM5"),
        triples(&[("OTM5", "CE", "SELL"), ("OTM5", "PE", "SELL")])
    );
    let run = Run::new();
    let g = Generated {
        action: "BUY",
        quantity: 65,
        product: "NRML",
        strangle_width: "OTM2",
        price_type: "LIMIT",
        price: 120.5,
    };
    let legs = generate_strategy_legs(&run.ctx, "straddle", &g);
    assert_eq!(
        legs[0],
        json!({"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 65,
               "pricetype": "LIMIT", "product": "NRML", "price": 120.5, "splitsize": 0})
    );
    assert!(generate_strategy_legs(&run.ctx, "custom", &g).is_empty());
    assert!(generate_strategy_legs(&run.ctx, "butterfly", &g).is_empty());
    assert_eq!(
        run.log(),
        [
            (
                "warning".to_string(),
                "Custom strategy selected but no legs provided".to_string()
            ),
            (
                "warning".to_string(),
                "Unknown strategy type: butterfly".to_string()
            )
        ]
    );
}

/// Python's `:g`, as the leg line prints a strike.
#[test]
fn strikes_print_as_python_g() {
    for (x, want) in [
        (24500.0, "24500"),
        (24600.5, "24600.5"),
        (292.55, "292.55"),
        (0.5, "0.5"),
        (1234567.0, "1.23457e+06"),
        (999999.5, "1e+06"),
        (0.00001, "1e-05"),
        (100000.0, "100000"),
    ] {
        assert_eq!(py_g(x), want, "{x}");
    }
}

/// parity N-optionsMultiOrder-10: the leg line, with the strike or offset
/// and the leg's own expiry.
#[test]
fn a_leg_line_names_its_selector_and_expiry() {
    let strike = json!({"strike": 24500.0, "expiry_date": "28OCT25", "option_type": "CE",
        "action": "SELL", "quantity": 65});
    assert_eq!(
        leg_line(1, &strike),
        "  Leg 1: 24500 28OCT25 CE SELL qty=65"
    );
    let offset = json!({"offset": "OTM2", "option_type": "PE", "action": "BUY", "quantity": 130});
    assert_eq!(leg_line(2, &offset), "  Leg 2: OTM2 PE BUY qty=130");
}
