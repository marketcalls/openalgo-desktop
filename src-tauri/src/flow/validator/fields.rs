//! Rules on a node's own fields: the web's `_enum_and_range_errors` and
//! `_priced_order_errors` (checked at every level, since a supplied value
//! that is wrong is wrong however unfinished the graph is), and the strict
//! block of `validate_workflow` that asks for the fields a node needs.
//!
//! A value carrying a `{{variable}}` is skipped: only the run can resolve it,
//! and the executor checks it before it calls the broker.

use super::catalog::{
    is_one_of, required_fields, Vocabulary, EITHER_REQUIRED_FIELDS, ENUM_FIELDS, HTTP_METHODS,
    HTTP_TIMEOUT_MAX_MS, HTTP_TIMEOUT_MIN_MS, POSITIVE_NUMBER_FIELDS, PRICED_ORDER_NODE_TYPES,
    PRICE_ALERT_CONDITIONS, VALID_VARIABLE_OPERATIONS,
};
use super::structure::get;
use super::{legs, py, ValidationError};
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::{
    normalize_condition, normalize_status, parse_underlying_symbol, VALID_STATUSES,
};
use crate::flow::nodes::pyjson;
use serde_json::{Map, Value};

/// The value as a positive finite number, or None (`_positive_number`).
pub(super) fn positive_number(value: &Value) -> Option<f64> {
    if value.is_boolean() {
        return None;
    }
    pyfmt::to_float(value).filter(|n| n.is_finite() && *n > 0.0)
}

/// Whether an order quantity is positive, or non-negative when zero is
/// allowed (`_valid_quantity`).
pub(super) fn valid_quantity(value: &Value, allow_zero: bool) -> bool {
    if value.is_boolean() {
        return false;
    }
    match pyfmt::to_float(value) {
        Some(n) if n.is_finite() => {
            if allow_zero {
                n >= 0.0
            } else {
                n > 0.0
            }
        }
        _ => false,
    }
}

/// Every value rule for one node, Margin's basket and custom option legs
/// included, in the web's order.
pub(super) fn value_errors(
    base: &str,
    node_type: &str,
    data: &Map<String, Value>,
    strict: bool,
) -> Vec<ValidationError> {
    let mut found = enum_errors(base, data);
    found.extend(positive_number_errors(base, node_type, data));
    if is_one_of(&PRICED_ORDER_NODE_TYPES, node_type) {
        found.extend(priced_order_errors(
            &format!("{base}/data"),
            data,
            strict,
            PriceKeys::NODE,
        ));
    }
    match node_type {
        "indicator" => found.extend(indicator_params_errors(base, data)),
        "orderUpdateTrigger" => found.extend(order_update_value_errors(base, data)),
        "httpRequest" => found.extend(http_request_errors(base, data)),
        _ => {}
    }
    match node_type {
        "margin" => found.extend(legs::margin_errors(base, data, strict)),
        "optionsMultiOrder" => found.extend(legs::options_multi_errors(base, data, strict)),
        _ => {}
    }
    found
}

/// Known constants: an unrecognised exchange, side, product or price type
/// is silently replaced by a default in several broker mappers.
fn enum_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    let mut found = Vec::new();
    for (field, vocabulary) in ENUM_FIELDS {
        let Some(value) = data.get(field) else {
            continue;
        };
        if py::is_template(value) {
            continue;
        }
        let Some(text) = value.as_str() else {
            // A number or boolean here is not merely unknown, it is the wrong
            // type: the broker mapper stringifies it and looks up nothing.
            found.push(
                ValidationError::new(
                    format!("{base}/data/{field}"),
                    "invalid_constant",
                    format!(
                        "{field} must be one of the {field} constants written as a string; {} is a {}.",
                        pyfmt::repr(value),
                        pyfmt::type_name(value)
                    ),
                )
                .expected(vocabulary.sorted())
                .received(value),
            );
            continue;
        };
        let operation = field == "operation";
        if py::is_blank(text) && !operation {
            continue;
        }
        let canonical = if operation {
            text.to_string()
        } else {
            py::strip(text).to_uppercase()
        };
        if !vocabulary.contains(&canonical) {
            found.push(
                ValidationError::new(
                    format!("{base}/data/{field}"),
                    "invalid_constant",
                    format!(
                        "'{text}' is not a valid {field}. Brokers may silently substitute a default \
                         for an unrecognised value rather than reject the order."
                    ),
                )
                .expected(vocabulary.sorted())
                .received(value),
            );
        }
    }
    found
}

/// Quantities, split sizes and lots are positive; a SmartOrder quantity may
/// be zero, which squares the position off.
fn positive_number_errors(
    base: &str,
    node_type: &str,
    data: &Map<String, Value>,
) -> Vec<ValidationError> {
    let mut found = Vec::new();
    for field in POSITIVE_NUMBER_FIELDS {
        let Some(value) = data.get(field) else {
            continue;
        };
        if value
            .as_str()
            .is_some_and(|s| s.contains("{{") || py::is_blank(s))
        {
            continue;
        }
        let allow_zero = node_type == "smartOrder" && field == "quantity";
        if valid_quantity(value, allow_zero) {
            continue;
        }
        let shown = pyfmt::repr(value);
        let (expectation, message) = if allow_zero {
            (
                "a non-negative number",
                format!(
                    "SmartOrder quantity must be a non-negative number; {shown} cannot describe a \
                     target position."
                ),
            )
        } else {
            (
                "a positive number",
                format!("{field} must be a positive number; {shown} reaches the broker as a malformed order."),
            )
        };
        found.push(
            ValidationError::new(format!("{base}/data/{field}"), "invalid_quantity", message)
                .expected(expectation)
                .received(value),
        );
    }
    found
}

/// The keys a priced order reads its price type and prices from.
#[derive(Debug, Clone, Copy)]
pub(super) struct PriceKeys {
    pub(super) price_type: &'static str,
    pub(super) price: &'static str,
    pub(super) trigger_price: &'static str,
}

impl PriceKeys {
    /// An order node's own fields.
    pub(super) const NODE: PriceKeys = PriceKeys {
        price_type: "priceType",
        price: "price",
        trigger_price: "triggerPrice",
    };
}

/// A static LIMIT or SL needs a positive price, and an SL or SL-M a positive
/// trigger price (`_priced_order_errors`). A blank one is an unfinished
/// draft, reported only at the strict level.
pub(super) fn priced_order_errors(
    base: &str,
    data: &Map<String, Value>,
    strict: bool,
    keys: PriceKeys,
) -> Vec<ValidationError> {
    let Some(price_type) = get(data, keys.price_type).as_str() else {
        return Vec::new();
    };
    if price_type.contains("{{") {
        return Vec::new();
    }
    let canonical = py::strip(price_type).to_uppercase();
    let mut found = Vec::new();
    for (field, applies_to) in [
        (keys.price, ["LIMIT", "SL"]),
        (keys.trigger_price, ["SL", "SL-M"]),
    ] {
        if !applies_to.contains(&canonical.as_str()) {
            continue;
        }
        let value = get(data, field);
        if py::is_unset(value) {
            if strict {
                found.push(
                    ValidationError::new(
                        format!("{base}/{field}"),
                        "missing_price",
                        format!("A {canonical} order needs {field} before it can execute."),
                    )
                    .expected(field)
                    .received(value),
                );
            }
            continue;
        }
        if py::is_template(value) {
            continue;
        }
        if positive_number(value).is_none() {
            found.push(
                ValidationError::new(
                    format!("{base}/{field}"),
                    "invalid_price",
                    format!(
                        "A {canonical} order needs a positive {field}; {} cannot be priced.",
                        pyfmt::repr(value)
                    ),
                )
                .expected("a positive number")
                .received(value),
            );
        }
    }
    found
}

/// An indicator's params must be the JSON string the executor parses: the
/// object form imports cleanly and then fails when the node runs.
fn indicator_params_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    match data.get("params") {
        Some(params) if !params.is_null() && !params.is_string() => vec![ValidationError::new(
            format!("{base}/data/params"),
            "invalid_params",
            "params must be a JSON object written as a string, for example \"{\"period\": 14}\". \
             An object here is accepted by import and then fails when the node runs.",
        )
        .expected("a JSON object string")
        .received(params)],
        _ => Vec::new(),
    }
}

/// The order-update monitor matches literal string filters and a known
/// status.
fn order_update_value_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    let mut found = Vec::new();
    for field in ["orderId", "symbol"] {
        let value = get(data, field);
        if !value.is_null() && !value.is_string() {
            found.push(
                ValidationError::new(
                    format!("{base}/data/{field}"),
                    "invalid_type",
                    format!(
                        "orderUpdateTrigger {field} must be a string filter; {} cannot match an \
                         order update.",
                        pyfmt::type_name(value)
                    ),
                )
                .expected("string")
                .received(value),
            );
        }
    }
    let default_status = Value::from("complete");
    let status = data.get("status").unwrap_or(&default_status);
    let text = if pyfmt::truthy(status) {
        pyfmt::str(status)
    } else {
        String::new()
    };
    if !VALID_STATUSES.contains(&normalize_status(&text).as_str()) {
        found.push(
            ValidationError::new(
                format!("{base}/data/status"),
                "invalid_status",
                format!(
                    "orderUpdateTrigger status must be one of {}, got {}",
                    pyfmt::repr(&Value::from(VALID_STATUSES.to_vec())),
                    pyfmt::repr(status)
                ),
            )
            .expected(VALID_STATUSES.to_vec())
            .received(status),
        );
    }
    found
}

/// The httpRequest fields whose shape only failed at run time before.
fn http_request_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    let mut found = Vec::new();
    let method = get(data, "method");
    if let Some(text) = method.as_str() {
        let upper = py::strip(text).to_uppercase();
        if !py::is_blank(text) && !text.contains("{{") && !HTTP_METHODS.contains(&upper.as_str()) {
            found.push(
                ValidationError::new(
                    format!("{base}/data/method"),
                    "invalid_method",
                    format!(
                        "'{text}' is not a method this node can send. Only {} are implemented.",
                        HTTP_METHODS.join(", ")
                    ),
                )
                .expected(HTTP_METHODS.to_vec())
                .received(method),
            );
        }
    }
    let headers = get(data, "headers");
    if let Some(text) = headers.as_str() {
        if !py::is_blank(text) && !text.contains("{{") {
            // Read as Python's json.loads reads it, NaN and Infinity
            // included, as the executor does (lead decision 24).
            let parsed: Option<Value> = pyjson::loads(text).ok();
            if !parsed.is_some_and(|p| p.is_object()) {
                found.push(
                    ValidationError::new(
                        format!("{base}/data/headers"),
                        "invalid_headers",
                        "headers must be a JSON object written as a string, for example \
                         \"{\"Authorization\": \"Bearer x\"}\". An unparseable value is dropped \
                         and the request goes out unauthenticated.",
                    )
                    .expected("a JSON object string")
                    .received(headers),
                );
            }
        }
    }
    let timeout = get(data, "timeout");
    if !timeout.is_null() && !py::is_template(timeout) {
        let in_range = pyfmt::to_float(timeout).is_some_and(|ms| {
            ms >= f64::from(HTTP_TIMEOUT_MIN_MS) && ms <= f64::from(HTTP_TIMEOUT_MAX_MS)
        });
        if !in_range {
            // A 1 ms timeout is not a fast request, it is a request that can
            // only ever fail, and it reads as a deliberate setting.
            found.push(
                ValidationError::new(
                    format!("{base}/data/timeout"),
                    "invalid_timeout",
                    format!(
                        "timeout is in milliseconds and must be between {HTTP_TIMEOUT_MIN_MS} and \
                         {HTTP_TIMEOUT_MAX_MS}. The request blocks the workflow for its whole \
                         duration."
                    ),
                )
                .expected(format!("{HTTP_TIMEOUT_MIN_MS}..{HTTP_TIMEOUT_MAX_MS}"))
                .received(timeout),
            );
        }
    }
    found
}

/// The strict-only rules: the fields a node needs before it can run, in the
/// web's order (filter alternatives, alert conditions, either-or fields,
/// then each missing field).
pub(super) fn completeness_errors(
    base: &str,
    node_type: &str,
    data: &Map<String, Value>,
) -> Vec<ValidationError> {
    let mut required: Vec<&str> = required_fields(node_type).to_vec();
    required.extend(conditional_required(node_type, data));
    let mut found = Vec::new();
    if node_type == "orderUpdateTrigger" {
        found.extend(order_update_filter_errors(base, data));
    }
    if node_type == "priceAlert" {
        let (needed, errors) = price_alert_errors(base, data);
        required.extend(needed);
        found.extend(errors);
    }
    for (either_type, groups) in EITHER_REQUIRED_FIELDS {
        if either_type != node_type {
            continue;
        }
        for group in groups {
            if group.iter().all(|f| py::is_unset(get(data, f))) {
                found.push(
                    ValidationError::new(
                        format!("{base}/data/{}", group[0]),
                        "missing_required_field",
                        format!(
                            "A {node_type} node needs {}. Without it the check passes \
                             unconditionally, so whatever it guards runs every time.",
                            group[0]
                        ),
                    )
                    .expected(group[0]),
                );
            }
        }
    }
    for field in required {
        let value = get(data, field);
        if py::is_unset(value) {
            found.push(
                ValidationError::new(
                    format!("{base}/data/{field}"),
                    "missing_required_field",
                    format!(
                        "A {node_type} node needs {field}. Without it the node fails at run time, \
                         not at import."
                    ),
                )
                .expected(field)
                .received(value),
            );
        }
    }
    found
}

/// Fields a node needs only in some configurations.
fn conditional_required(node_type: &str, data: &Map<String, Value>) -> Vec<&'static str> {
    match node_type {
        // An indicator over an upstream series needs no history of its own.
        "indicator" if py::is_unset(get(data, "sourceSeries")) => vec!["symbol", "exchange"],
        "optionSymbol" | "optionChain" => {
            let underlying = get(data, "underlying");
            let templated = py::is_template(underlying);
            let embedded_expiry = underlying.as_str().is_some_and(|u| {
                !templated && !py::is_blank(u) && parse_underlying_symbol(u).1.is_some()
            });
            if embedded_expiry || templated {
                Vec::new()
            } else {
                vec!["expiryDate"]
            }
        }
        "syntheticFuture" => vec!["expiryDate"],
        "variable" => {
            let default_operation = Value::from("set");
            let operation = data.get("operation").unwrap_or(&default_operation);
            match operation.as_str() {
                Some(op) if op.contains("{{") || !VALID_VARIABLE_OPERATIONS.contains(&op) => {
                    Vec::new()
                }
                Some("get" | "stringify") => vec!["sourceVariable"],
                Some("add" | "subtract" | "multiply" | "divide" | "parse_json") => vec!["value"],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// An order-update trigger watches a literal order id or a symbol.
fn order_update_filter_errors(base: &str, data: &Map<String, Value>) -> Vec<ValidationError> {
    let order_id = get(data, "orderId");
    let has_order_id = order_id.as_str().is_some_and(|s| !py::is_blank(s));
    let has_symbol = get(data, "symbol")
        .as_str()
        .is_some_and(|s| !py::is_blank(s));
    if !has_order_id && !has_symbol {
        return vec![ValidationError::new(
            format!("{base}/data/orderId"),
            "missing_alternative",
            "An orderUpdateTrigger needs an Order ID or a Symbol to watch.",
        )
        .expected("a non-empty orderId or symbol")];
    }
    if py::is_template(order_id) {
        return vec![ValidationError::new(
            format!("{base}/data/orderId"),
            "invalid_trigger_filter",
            "orderUpdateTrigger Order ID must be a literal order id, not a {{variable}} reference.",
        )
        .expected("a literal order id")
        .received(order_id)];
    }
    Vec::new()
}

/// A price alert's condition must be one the monitor evaluates, and the
/// levels it compares against must be reachable. Returns the fields the
/// condition needs, and the errors found.
fn price_alert_errors(
    base: &str,
    data: &Map<String, Value>,
) -> (Vec<&'static str>, Vec<ValidationError>) {
    let empty = Value::from("");
    let selector = data.get("condition").unwrap_or(&empty);
    let chosen = normalize_condition(&pyfmt::str(selector));
    let mut found = Vec::new();
    let needed = PRICE_ALERT_CONDITIONS
        .iter()
        .find(|(condition, _)| *condition == chosen)
        .map_or(&[][..], |(_, fields)| *fields);
    if !chosen.is_empty() && !PRICE_ALERT_CONDITIONS.iter().any(|(c, _)| *c == chosen) {
        // An unrecognised condition can never evaluate true, so the trigger
        // would sit registered and silently never fire.
        let known: Vec<&str> = PRICE_ALERT_CONDITIONS.iter().map(|(c, _)| *c).collect();
        found.push(
            ValidationError::new(
                format!("{base}/data/condition"),
                "unknown_condition",
                format!(
                    "'{}' is not a condition the price monitor can evaluate, so this alert could \
                     never fire.",
                    pyfmt::str(selector)
                ),
            )
            .expected(known)
            .received(selector),
        );
    }
    found.extend(alert_threshold_errors(base, data, &chosen, needed));
    (needed.to_vec(), found)
}

/// A zero or negative level is never reached by a real price, and an
/// inverted channel is empty, so either leaves the alert unable to fire
/// (`_alert_threshold_errors`).
fn alert_threshold_errors(
    base: &str,
    data: &Map<String, Value>,
    condition: &str,
    required: &[&str],
) -> Vec<ValidationError> {
    let mut found = Vec::new();
    for field in required {
        let Some(value) = data.get(*field) else {
            continue; // absence is reported as a missing required field
        };
        if positive_number(value).is_none() {
            found.push(
                ValidationError::new(
                    format!("{base}/data/{field}"),
                    "invalid_threshold",
                    format!(
                        "{field} must be a positive number for a '{condition}' alert; {} can never \
                         be reached.",
                        pyfmt::repr(value)
                    ),
                )
                .expected("a positive number")
                .received(value),
            );
        }
    }
    let lower = positive_number(get(data, "priceLower"));
    let upper = positive_number(get(data, "priceUpper"));
    if let (true, Some(lower), Some(upper)) = (required.contains(&"priceLower"), lower, upper) {
        if lower >= upper {
            let (shown_lower, shown_upper) = (
                pyfmt::repr(&Value::from(lower)),
                pyfmt::repr(&Value::from(upper)),
            );
            found.push(
                ValidationError::new(
                    format!("{base}/data/priceLower"),
                    "invalid_threshold",
                    format!(
                        "priceLower ({shown_lower}) must be below priceUpper ({shown_upper}); an \
                         inverted channel contains no prices."
                    ),
                )
                .expected("priceLower < priceUpper")
                .received(&Value::from(vec![lower, upper])),
            );
        }
    }
    found
}

/// A vocabulary check on one leg field, shared with the leg rules: `'X' is
/// not a valid field constant.`
pub(super) fn leg_constant_error(
    base: &str,
    field: &str,
    value: &Value,
    vocabulary: Vocabulary,
) -> ValidationError {
    ValidationError::new(
        format!("{base}/{field}"),
        "invalid_constant",
        format!("'{}' is not a valid {field} constant.", pyfmt::str(value)),
    )
    .expected(vocabulary.sorted())
    .received(value)
}
