//! Baskets whose legs the validator reads one by one: Margin's position list
//! and the Multi-Leg Options node's custom legs (the web's `_margin_errors`,
//! `_options_multi_errors`, `_static_order_leg_errors` and
//! `_option_leg_strike_and_expiry_errors`).
//!
//! A leg accepted here must not fail at run time, because a multi-leg basket
//! fails leg by leg: by the time leg three is refused, legs one and two are
//! already filled and the position is not the one anybody chose.

use super::catalog::Vocabulary;
use super::fields::{leg_constant_error, priced_order_errors, valid_quantity, PriceKeys};
use super::structure::get;
use super::{py, ValidationError};
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::{is_expiry_date, is_option_offset, VALID_EXPIRY_TYPES};
use crate::flow::nodes::pyjson;
use serde_json::{Map, Value};

/// How a leg names its price type and prices.
#[derive(Debug, Clone, Copy)]
struct LegShape {
    required: &'static [&'static str],
    keys: PriceKeys,
    option_leg: bool,
}

/// A Margin position, written in the `/api/v1/margin` spelling.
const MARGIN_LEG: LegShape = LegShape {
    required: &[
        "symbol",
        "exchange",
        "action",
        "quantity",
        "product",
        "pricetype",
    ],
    keys: PriceKeys {
        price_type: "pricetype",
        price: "price",
        trigger_price: "trigger_price",
    },
    option_leg: false,
};

/// Margin prices one legacy symbol, or a complete static basket in
/// `positionsJson` (or `positions`).
pub(super) fn margin_errors(
    base: &str,
    data: &Map<String, Value>,
    strict: bool,
) -> Vec<ValidationError> {
    let raw_key = ["positionsJson", "positions"]
        .into_iter()
        .find(|key| !py::is_unset(get(data, key)));
    let Some(raw_key) = raw_key else {
        if !py::is_unset(get(data, "symbol")) || !strict {
            return Vec::new();
        }
        return vec![ValidationError::new(
            format!("{base}/data/positionsJson"),
            "missing_alternative",
            "A Margin node needs positionsJson, positions, or a legacy symbol.",
        )
        .expected("a non-empty positionsJson, positions, or symbol")];
    };

    let raw = get(data, raw_key);
    let raw_base = format!("{base}/data/{raw_key}");
    let parsed;
    let positions = match raw {
        Value::String(text) => {
            let text = py::strip(text);
            if text.contains("{{") {
                return Vec::new();
            }
            // Python's json.loads, NaN and Infinity included, as the
            // executor reads it (lead decision 24).
            let Ok(value) = pyjson::loads(text) else {
                return vec![ValidationError::new(
                    raw_base,
                    "invalid_positions",
                    "Margin positions must be a JSON array of position objects.",
                )
                .expected("a JSON array of objects")
                .received(raw)];
            };
            parsed = value;
            &parsed
        }
        other => other,
    };

    let Some(legs) = positions.as_array().filter(|legs| !legs.is_empty()) else {
        return vec![ValidationError::new(
            raw_base,
            "invalid_positions",
            "Margin positions must be a non-empty array of position objects.",
        )
        .expected("a non-empty array of objects")
        .received(positions)];
    };

    let mut found = Vec::new();
    for (index, leg) in legs.iter().enumerate() {
        let leg_base = format!("{raw_base}/{index}");
        match leg.as_object() {
            Some(leg) => found.extend(static_order_leg_errors(&leg_base, leg, strict, MARGIN_LEG)),
            None => found.push(
                ValidationError::new(
                    leg_base,
                    "invalid_positions",
                    "Every Margin position must be an object.",
                )
                .expected("object")
                .received(leg),
            ),
        }
    }
    found
}

/// A generated strategy supports MARKET and LIMIT only; a custom one is
/// checked leg by leg, each leg inheriting the node's product, price type
/// and prices where it names none.
///
/// The strategy is read as the executor reads it: `strategy`, else the
/// legacy `strategyType`, else `custom`. The web's validator defaults to
/// `straddle` and ignores `strategyType`, so a node naming neither passed
/// with no legs and failed at run time, and a legacy custom node escaped
/// every leg check (parity V-059, a web defect).
pub(super) fn options_multi_errors(
    base: &str,
    data: &Map<String, Value>,
    strict: bool,
) -> Vec<ValidationError> {
    let default_strategy = Value::from("custom");
    let strategy = data
        .get("strategy")
        .or_else(|| data.get("strategyType"))
        .unwrap_or(&default_strategy);
    if py::is_template(strategy) {
        return Vec::new();
    }
    let canonical = strategy
        .as_str()
        .map(|s| py::strip(s).to_lowercase())
        .unwrap_or_default();
    if canonical != "custom" {
        return generated_strategy_errors(base, data);
    }

    let legs_key = if !data.contains_key("legs") && data.contains_key("orderLegs") {
        "orderLegs"
    } else {
        "legs"
    };
    let legs = get(data, legs_key);
    let legs_base = format!("{base}/data/{legs_key}");
    if py::is_unset(legs) {
        if !strict {
            return Vec::new();
        }
        return vec![ValidationError::new(
            legs_base,
            "missing_required_field",
            "A custom options strategy needs at least one leg.",
        )
        .expected("a non-empty legs array")
        .received(legs)];
    }
    if py::is_template(legs) {
        return Vec::new();
    }
    let Some(legs) = legs.as_array().filter(|l| !l.is_empty()) else {
        return vec![ValidationError::new(
            legs_base,
            "invalid_legs",
            "Custom options strategy legs must be a non-empty array.",
        )
        .expected("a non-empty array of objects")
        .received(legs)];
    };

    let mut found = Vec::new();
    for (index, leg) in legs.iter().enumerate() {
        let leg_base = format!("{legs_base}/{index}");
        let Some(leg) = leg.as_object() else {
            found.push(
                ValidationError::new(
                    leg_base,
                    "invalid_legs",
                    "Every custom options leg must be an object.",
                )
                .expected("object")
                .received(leg),
            );
            continue;
        };
        found.extend(custom_leg_errors(&leg_base, leg, data, strict));
        found.extend(strike_and_expiry_errors(&leg_base, leg, strict));
    }
    found
}

fn generated_strategy_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    let price_type = get(data, "priceType");
    match price_type.as_str() {
        Some(text)
            if !text.contains("{{")
                && !matches!(py::strip(text).to_uppercase().as_str(), "MARKET" | "LIMIT") =>
        {
            vec![ValidationError::new(
                format!("{base}/data/priceType"),
                "invalid_constant",
                "Generated option strategies support only MARKET and LIMIT price types.",
            )
            .expected(vec!["LIMIT", "MARKET"])
            .received(price_type)]
        }
        _ => Vec::new(),
    }
}

/// One custom leg's order fields, with the node's product, price type and
/// prices filled in where the leg names none, as the executor applies them.
fn custom_leg_errors(
    leg_base: &str,
    leg: &Map<String, Value>,
    node: &Map<String, Value>,
    strict: bool,
) -> Vec<ValidationError> {
    let mut effective = leg.clone();
    if !effective.contains_key("product") {
        if let Some(product) = node.get("product") {
            effective.insert("product".to_string(), product.clone());
        }
    }
    let price_type_key = if leg.contains_key("priceType") {
        "priceType"
    } else {
        "pricetype"
    };
    if !effective.contains_key(price_type_key) {
        if let Some(price_type) = node.get("priceType") {
            effective.insert(price_type_key.to_string(), price_type.clone());
        }
    }
    for field in ["price", "triggerPrice"] {
        if !effective.contains_key(field) {
            if let Some(value) = node.get(field) {
                effective.insert(field.to_string(), value.clone());
            }
        }
    }
    // A leg picks its strike one of two ways, so neither selector is
    // required on its own; the strike rules check that it names one.
    let shape = LegShape {
        required: &["optionType", "action", "quantity"],
        keys: PriceKeys {
            price_type: price_type_key,
            price: "price",
            trigger_price: "triggerPrice",
        },
        option_leg: true,
    };
    static_order_leg_errors(leg_base, &effective, strict, shape)
}

/// A parsed Margin position or custom options leg, checked without being
/// rewritten (`_static_order_leg_errors`).
fn static_order_leg_errors(
    base: &str,
    data: &Map<String, Value>,
    strict: bool,
    shape: LegShape,
) -> Vec<ValidationError> {
    let mut found = Vec::new();
    if strict {
        for field in shape.required {
            let value = get(data, field);
            if py::is_unset(value) {
                found.push(
                    ValidationError::new(
                        format!("{base}/{field}"),
                        "missing_required_field",
                        format!("A position needs {field} before it can execute."),
                    )
                    .expected(*field)
                    .received(value),
                );
            }
        }
    }

    let mut enum_fields = vec![
        ("exchange", Vocabulary::Exchange),
        ("action", Vocabulary::Action),
        ("product", Vocabulary::Product),
        (shape.keys.price_type, Vocabulary::PriceType),
    ];
    if shape.option_leg {
        enum_fields.push(("optionType", Vocabulary::OptionType));
        if shape.keys.price_type == "priceType" && data.contains_key("pricetype") {
            enum_fields.push(("pricetype", Vocabulary::PriceType));
        }
    }
    for (field, vocabulary) in enum_fields {
        let Some(value) = data.get(field) else {
            continue;
        };
        if value
            .as_str()
            .is_some_and(|s| s.contains("{{") || py::is_blank(s))
        {
            continue;
        }
        let known = value
            .as_str()
            .is_some_and(|s| vocabulary.contains(&py::strip(s).to_uppercase()));
        if !known {
            found.push(leg_constant_error(base, field, value, vocabulary));
        }
    }

    if let Some(quantity) = data.get("quantity") {
        let deferred = quantity
            .as_str()
            .is_some_and(|s| s.contains("{{") || py::is_blank(s));
        if !deferred && !valid_quantity(quantity, false) {
            found.push(
                ValidationError::new(
                    format!("{base}/quantity"),
                    "invalid_quantity",
                    format!(
                        "quantity must be a positive number; {} cannot execute.",
                        pyfmt::repr(quantity)
                    ),
                )
                .expected("a positive number")
                .received(quantity),
            );
        }
    }
    found.extend(priced_order_errors(base, data, strict, shape.keys));
    found
}

/// The two selectors a manually built leg adds: its strike (an offset from
/// the money or an absolute strike) and its expiry (an exact date or a
/// relative type, else the node's). Whatever the leg names must be usable,
/// and it must name a strike one way or the other.
fn strike_and_expiry_errors(
    base: &str,
    leg: &Map<String, Value>,
    strict: bool,
) -> Vec<ValidationError> {
    let mut found = Vec::new();
    let strike_mode = get(leg, "strikeMode");
    if !strike_mode.is_null() && !py::is_template(strike_mode) {
        let known = strike_mode
            .as_str()
            .is_some_and(|s| Vocabulary::StrikeMode.contains(&py::strip(s).to_uppercase()));
        if !known {
            found.push(leg_constant_error(
                base,
                "strikeMode",
                strike_mode,
                Vocabulary::StrikeMode,
            ));
        }
    }

    let strike = get(leg, "strike");
    let offset = get(leg, "offset");
    if !py::is_unset(strike) {
        if !py::is_template(strike) && strike_number(strike).is_none() {
            found.push(
                ValidationError::new(
                    format!("{base}/strike"),
                    "invalid_strike",
                    format!(
                        "strike must be a positive number; {} names no contract.",
                        pyfmt::repr(strike)
                    ),
                )
                .expected("a positive number")
                .received(strike),
            );
        }
    } else if !py::is_unset(offset) {
        let text = py::strip(&pyfmt::str(offset)).to_uppercase();
        if !py::is_template(offset) && !is_option_offset(&text) {
            found.push(
                ValidationError::new(
                    format!("{base}/offset"),
                    "invalid_constant",
                    format!("'{}' is not a valid offset.", pyfmt::str(offset)),
                )
                .expected("ATM, ITM1-ITM50 or OTM1-OTM50")
                .received(offset),
            );
        }
    } else if strict {
        found.push(
            ValidationError::new(
                format!("{base}/offset"),
                "missing_required_field",
                "A leg needs either an offset or an absolute strike before it can execute.",
            )
            .expected("offset or strike"),
        );
    }
    found.extend(expiry_errors(base, leg));
    found
}

/// A strike the web accepts: a number or numeric text that is not `<= 0`.
/// A boolean is not one (True would otherwise pass as strike 1). As on the
/// web (`numeric is None or numeric <= 0`), `"NaN"` and `"Infinity"` text
/// pass, since neither compares `<= 0`; the executor refuses them when it
/// resolves the strike (lead decision 36).
fn strike_number(strike: &Value) -> Option<f64> {
    let numeric = match strike {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => pyfmt::parse_float(s),
        _ => None,
    };
    // Not `<= 0`: above zero, or NaN, which compares false to everything.
    numeric.filter(|n| *n > 0.0 || n.is_nan())
}

fn expiry_errors(base: &str, leg: &Map<String, Value>) -> Vec<ValidationError> {
    let mut found = Vec::new();
    let expiry = get(leg, "expiry");
    if !expiry.is_null() && !py::is_template(expiry) {
        let text = py::strip(&pyfmt::str(expiry)).to_uppercase();
        if !text.is_empty() && !is_expiry_date(&text) {
            found.push(
                ValidationError::new(
                    format!("{base}/expiry"),
                    "invalid_expiry",
                    format!(
                        "expiry must be in DDMMMYY format such as 28OCT25; got {}.",
                        pyfmt::repr(expiry)
                    ),
                )
                .expected("DDMMMYY")
                .received(expiry),
            );
        }
    }
    let expiry_type = get(leg, "expiryType");
    if !expiry_type.is_null() && !py::is_template(expiry_type) {
        let text = py::strip(&pyfmt::str(expiry_type)).to_lowercase();
        if !text.is_empty() && !VALID_EXPIRY_TYPES.contains(&text.as_str()) {
            found.push(
                ValidationError::new(
                    format!("{base}/expiryType"),
                    "invalid_constant",
                    format!(
                        "'{}' is not a valid expiryType constant.",
                        pyfmt::str(expiry_type)
                    ),
                )
                .expected(VALID_EXPIRY_TYPES.to_vec())
                .received(expiry_type),
            );
        }
    }
    found
}
