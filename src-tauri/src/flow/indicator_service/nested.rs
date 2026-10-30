//! Nested mode's input: how the web's executor (`execute_indicator`) turns
//! a resolved `sourceSeries` into the numbers `compute_indicator` runs on.
//! The series is another Indicator node's `series` (rows of `value` or
//! `out0`), a History node's `data` (rows with `close`), or plain numbers.

use serde_json::Value;

use crate::flow::contract::pyfmt;

/// Why `sourceSeries` could not be read: the node's error `message`, and
/// the line the executor logs at error level.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSeriesError {
    pub message: String,
    pub log: String,
}

/// The numbers a resolved `sourceSeries` holds. `resolved` is what the
/// variable store returned for the field (the stored list itself when the
/// field is one whole `{{...}}` reference, otherwise the interpolated text,
/// which is read as JSON). `source_field` picks the key read from each row;
/// empty reads `value`, then `out0`, then `close`. Rows with no value are
/// skipped.
pub fn source_series_numbers(
    resolved: &Value,
    source_field: &str,
) -> Result<Vec<f64>, SourceSeriesError> {
    let parsed;
    let mut list = resolved;
    if let Value::String(text) = resolved {
        parsed = serde_json::from_str::<Value>(text).map_err(|_| SourceSeriesError {
            message: "sourceSeries did not resolve to a JSON array".into(),
            log: format!(
                "sourceSeries did not resolve to an array: {}",
                pyfmt::repr(resolved)
            ),
        })?;
        list = &parsed;
    }
    let Value::Array(items) = list else {
        return Err(SourceSeriesError {
            message: "sourceSeries must resolve to an array".into(),
            log: format!(
                "sourceSeries must resolve to an array, got: {}",
                pyfmt::repr(list)
            ),
        });
    };
    let mut numbers = Vec::with_capacity(items.len());
    for item in items {
        let value = match item {
            Value::Object(row) if !source_field.is_empty() => row.get(source_field),
            Value::Object(row) => ["value", "out0", "close"]
                .iter()
                .find_map(|key| row.get(*key).filter(|v| !v.is_null())),
            other => Some(other),
        };
        let Some(value) = value.filter(|v| !v.is_null()) else {
            continue;
        };
        match pyfmt::to_float(value) {
            Some(number) => numbers.push(number),
            None => {
                let message = format!(
                    "sourceSeries contains a non-numeric value: {}",
                    pyfmt::repr(value)
                );
                return Err(SourceSeriesError {
                    log: message.clone(),
                    message,
                });
            }
        }
    }
    Ok(numbers)
}
