//! `compute_indicator`: one `ta` indicator over a symbol's history records,
//! or, in nested mode, over another Indicator node's output series. Returns
//! the web's result: the latest and previous values, a fixed-length tail for
//! `{{ind.series[N]}}`, and the value `offset_bars` closed bars back.

use serde_json::{json, Value};

use super::catalogue::{self, input_columns, normalise, takes_one_series};
use super::params::{bind, library_params, IndicatorParams};
use super::records::Columns;
use super::window::clamp_bars;
use super::{IndicatorError, INDICATOR_WARMUP_BARS, MAX_HISTORY_BARS};
use crate::agent::indicators::ta::{self, Series as LibrarySeries, TaError};
use crate::agent::indicators::IndicatorSpec;
use crate::agent::tools::numbers::py_round;

/// Decimal places kept on every returned value.
const ROUND_DP: usize = 6;

/// What the Indicator node computes over.
#[derive(Debug, Clone, Copy)]
pub enum IndicatorSource<'a> {
    /// The `data` records of a history response, ascending, and the node's
    /// `lookbackBars`. Up to `lookbackBars` plus a 50-bar warm-up of the
    /// latest records are used, never more than `MAX_HISTORY_BARS`.
    Candles {
        records: &'a [Value],
        lookback_bars: i64,
    },
    /// Nested mode: another indicator's series, already read into numbers
    /// ([`super::source_series_numbers`]).
    Nested(&'a [f64]),
}

/// One Indicator node run: `compute_indicator`'s arguments.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorRequest<'a> {
    /// The node's `indicatorName`, as written (trimmed and lower-cased here).
    pub indicator_name: &'a str,
    pub params: &'a IndicatorParams,
    pub source: IndicatorSource<'a>,
    /// The node's `tailBars`: the length of `series` (web default 5).
    pub tail_bars: i64,
    /// The node's `offsetBars`: which bar `at_offset` reads (web default 0).
    pub offset_bars: i64,
}

/// One bar's values, by output: `value` for an indicator with one output,
/// `out0`, `out1`, ... in return order for one with several. `None` where
/// the output has no value that far back.
pub type Row = Vec<(String, Option<f64>)>;

/// The web's success result.
#[derive(Debug, Clone, PartialEq)]
pub struct Computed {
    pub indicator: String,
    pub nested: bool,
    pub offset_bars: i64,
    pub at_offset: Row,
    /// The history columns used (`["data (nested)"]` in nested mode).
    pub inputs: Vec<String>,
    /// The node's parameters with `REQUIRED_PARAM_DEFAULTS` filled in.
    pub params: IndicatorParams,
    pub outputs: Vec<String>,
    pub latest: Row,
    pub previous: Row,
    /// `tail_bars` rows, oldest first, the latest last.
    pub series: Vec<Row>,
    pub bars_used: usize,
}

/// The web's result when every output of the latest bar is null: reported
/// as `status: error` rather than a value a condition would branch on.
#[derive(Debug, Clone, PartialEq)]
pub struct NoValue {
    pub indicator: String,
    pub message: String,
    pub inputs: Vec<String>,
    pub params: IndicatorParams,
    pub outputs: Vec<String>,
    pub bars_used: usize,
}

/// What `compute_indicator` returns: the node stores either as its output.
#[derive(Debug, Clone, PartialEq)]
pub enum IndicatorOutput {
    Success(Computed),
    NoValue(NoValue),
}

impl IndicatorOutput {
    /// The result dict as the web returns it.
    pub fn to_value(&self) -> Value {
        match self {
            IndicatorOutput::Success(c) => json!({
                "status": "success",
                "indicator": c.indicator,
                "nested": c.nested,
                "offset_bars": c.offset_bars,
                "at_offset": row_value(&c.at_offset),
                "inputs": c.inputs,
                "params": c.params.to_value(),
                "outputs": c.outputs,
                "latest": row_value(&c.latest),
                "previous": row_value(&c.previous),
                "series": c.series.iter().map(row_value).collect::<Vec<_>>(),
                "bars_used": c.bars_used,
            }),
            IndicatorOutput::NoValue(n) => json!({
                "status": "error",
                "indicator": n.indicator,
                "message": n.message,
                "inputs": n.inputs,
                "params": n.params.to_value(),
                "outputs": n.outputs,
                "bars_used": n.bars_used,
            }),
        }
    }
}

/// A row as a JSON object; a non-finite value (which JSON cannot carry)
/// becomes null.
fn row_value(row: &Row) -> Value {
    Value::Object(
        row.iter()
            .map(|(name, value)| (name.clone(), value.map_or(Value::Null, Value::from)))
            .collect(),
    )
}

/// The series the library is called with.
struct Input {
    columns: Vec<String>,
    series: Vec<Vec<f64>>,
    bars: usize,
}

/// `compute_indicator`. A `ValueError` is the web node's error; anything
/// else stops the web's run (see [`IndicatorError::is_value_error`]).
///
/// One deliberate difference (IND-049): rows are read by bar, not by value.
/// See [`Table::at_bar`].
pub fn compute_indicator(
    request: &IndicatorRequest<'_>,
) -> Result<IndicatorOutput, IndicatorError> {
    compute_with(request, Table::at_bar)
}

/// `compute_indicator` reading rows as the web does (each output's NaN
/// dropped first), so tests can show the web's numbers next to ours.
#[cfg(test)]
pub(super) fn compute_indicator_as_web(
    request: &IndicatorRequest<'_>,
) -> Result<IndicatorOutput, IndicatorError> {
    compute_with(request, Table::skipping_missing)
}

/// Reads one row, `back` bars (or values) from the end.
type Reader = fn(&Table, usize) -> Row;

fn compute_with(
    request: &IndicatorRequest<'_>,
    read: Reader,
) -> Result<IndicatorOutput, IndicatorError> {
    let name = normalise(request.indicator_name);
    let spec = runnable(&name, request.indicator_name)?;
    let params = request.params.resolved(&name);
    let input = match request.source {
        IndicatorSource::Nested(values) => nested_input(spec, values)?,
        IndicatorSource::Candles {
            records,
            lookback_bars,
        } => candle_input(spec, records, lookback_bars)?,
    };
    let parts = run(spec, &input, &params)?;
    let table = Table::new(parts, input.bars)?;
    Ok(shape(request, name, input, params, &table, read))
}

/// The refusals before anything is read: two-series, unsupported, unknown.
fn runnable(name: &str, as_written: &str) -> Result<&'static IndicatorSpec, IndicatorError> {
    if catalogue::TWO_SERIES_INDICATORS.contains(&name) {
        return Err(IndicatorError::value(format!(
            "'{name}' needs two independent series (a second symbol or two already-computed \
             signals) and cannot run from a single symbol's history. Use two Indicator nodes \
             plus a Math Expression/Var Condition node instead."
        )));
    }
    if catalogue::UNSUPPORTED_INDICATORS.contains(&name) {
        return Err(IndicatorError::value(format!(
            "'{name}' is not available: the installed openalgo build does not return usable \
             values for it. Pick a different indicator."
        )));
    }
    catalogue::spec(name)
        .ok_or_else(|| IndicatorError::value(format!("unknown indicator '{as_written}'")))
}

fn nested_input(spec: &IndicatorSpec, values: &[f64]) -> Result<Input, IndicatorError> {
    if !takes_one_series(spec) {
        return Err(IndicatorError::value(format!(
            "'{}' takes more than one independent input series (e.g. high/low/close), so it \
             cannot be nested on top of another indicator's single output series. Nest only \
             single-series indicators (SMA, EMA, RSI, WMA, stdev, highest/lowest, ...).",
            spec.name
        )));
    }
    if values.len() < 2 {
        return Err(IndicatorError::value(
            "nested indicator needs at least 2 upstream values",
        ));
    }
    Ok(Input {
        columns: vec!["data (nested)".into()],
        series: vec![values.to_vec()],
        bars: values.len(),
    })
}

fn candle_input(
    spec: &IndicatorSpec,
    records: &[Value],
    lookback_bars: i64,
) -> Result<Input, IndicatorError> {
    if records.is_empty() {
        return Err(IndicatorError::value(
            "no historical data available for this symbol/date range",
        ));
    }
    let frame = Columns::read(records);
    // The warm-up rows are already in the fetched response, so keeping them
    // costs no extra call and lets recursive indicators settle.
    let keep = (clamp_bars(lookback_bars, "bars") + INDICATOR_WARMUP_BARS).min(MAX_HISTORY_BARS);
    let start = records.len().saturating_sub(keep as usize);
    let columns = input_columns(spec);
    let missing: Vec<&str> = columns
        .iter()
        .copied()
        .filter(|c| frame.get(c).is_none())
        .collect();
    if !missing.is_empty() {
        let quoted: Vec<String> = missing.iter().map(|c| format!("'{c}'")).collect();
        return Err(IndicatorError::value(format!(
            "history data is missing required column(s): [{}]",
            quoted.join(", ")
        )));
    }
    let series = columns
        .iter()
        .filter_map(|c| frame.get(c))
        .map(|values| values[start..].to_vec())
        .collect();
    Ok(Input {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        series,
        bars: records.len() - start,
    })
}

/// `fn(*args, **resolved_params)` on the library.
fn run(
    spec: &'static IndicatorSpec,
    input: &Input,
    params: &IndicatorParams,
) -> Result<Vec<LibrarySeries>, IndicatorError> {
    let bound = bind(spec, params)?;
    let series: Vec<&[f64]> = input.series.iter().map(Vec::as_slice).collect();
    match ta::call(spec.name, &series, &library_params(spec, &bound)) {
        Some(result) => {
            result.map_err(|TaError { kind, message }| IndicatorError { kind, message })
        }
        None => Err(IndicatorError {
            kind: "AttributeError",
            message: format!(
                "'TechnicalAnalysis' object has no attribute '{}'",
                spec.name
            ),
        }),
    }
}

/// The library's outputs as the web's `out` frame: one column per output,
/// as long as the bars, flags read as 1.0 and 0.0.
struct Table {
    names: Vec<String>,
    columns: Vec<Vec<f64>>,
}

impl Table {
    fn new(parts: Vec<LibrarySeries>, bars: usize) -> Result<Self, IndicatorError> {
        let names: Vec<String> = if parts.len() == 1 {
            vec!["value".into()]
        } else {
            (0..parts.len()).map(|i| format!("out{i}")).collect()
        };
        let mut columns = Vec::with_capacity(parts.len());
        for part in parts {
            let values: Vec<f64> = match part {
                LibrarySeries::Float(values) => values,
                LibrarySeries::Bool(flags) => {
                    flags.into_iter().map(|b| f64::from(u8::from(b))).collect()
                }
            };
            if values.len() != bars {
                return Err(IndicatorError::value(format!(
                    "Length of values ({}) does not match length of index ({})",
                    values.len(),
                    bars
                )));
            }
            columns.push(values);
        }
        Ok(Self { names, columns })
    }

    /// Each output's value `back` bars from the end (1 is the latest bar),
    /// rounded to six places; `None` where the output has no value on that
    /// bar.
    ///
    /// The web reads `out[col].dropna().iloc[-back]`, so it counts values,
    /// not bars: an output with trailing gaps (ichimoku's lagging span is
    /// empty on its last 25 bars) reports, in the same row, a value from an
    /// older bar than its neighbours, and an output with no value on the
    /// latest bar silently reports an earlier one as `latest`, which a
    /// condition can trade on. Fixed here (IND-049, lead decision 10); the
    /// web's reading is [`Table::skipping_missing`], pinned by the tests.
    fn at_bar(&self, back: usize) -> Row {
        self.read(back, |values| values.to_vec())
    }

    /// The web's `_at(-back)`: each output with its NaN dropped first.
    #[cfg(test)]
    fn skipping_missing(&self, back: usize) -> Row {
        self.read(back, |values| {
            values.iter().copied().filter(|v| !v.is_nan()).collect()
        })
    }

    fn read(&self, back: usize, column: impl Fn(&[f64]) -> Vec<f64>) -> Row {
        self.names
            .iter()
            .zip(&self.columns)
            .map(|(name, values)| {
                let values = column(values);
                let value = values
                    .len()
                    .checked_sub(back)
                    .map(|i| values[i])
                    .filter(|v| !v.is_nan())
                    .map(|v| py_round(v, ROUND_DP));
                (name.clone(), value)
            })
            .collect()
    }
}

fn shape(
    request: &IndicatorRequest<'_>,
    indicator: String,
    input: Input,
    params: IndicatorParams,
    table: &Table,
    read: Reader,
) -> IndicatorOutput {
    let tail = clamp_bars(request.tail_bars, "tail bars") as usize;
    let series = (1..=tail).rev().map(|back| read(table, back)).collect();
    // A direct "N bars back" reader: unlike `series[N]`, its meaning does
    // not shift when `tail_bars` changes.
    let offset = clamp_bars(request.offset_bars.saturating_add(1), "offset bars") - 1;
    let at_offset = read(table, offset as usize + 1);
    let latest = read(table, 1);
    if latest.iter().all(|(_, value)| value.is_none()) {
        return IndicatorOutput::NoValue(NoValue {
            message: format!(
                "Indicator '{indicator}' produced no value over {} bars - every output was \
                 null. Increase the lookback if the indicator needs a longer warm-up, or check \
                 that this indicator is usable with the selected inputs.",
                input.bars
            ),
            indicator,
            inputs: input.columns,
            params,
            outputs: table.names.clone(),
            bars_used: input.bars,
        });
    }
    IndicatorOutput::Success(Computed {
        indicator,
        nested: matches!(request.source, IndicatorSource::Nested(_)),
        offset_bars: offset,
        at_offset,
        inputs: input.columns,
        params,
        outputs: table.names.clone(),
        latest,
        previous: read(table, 2),
        series,
        bars_used: input.bars,
    })
}
