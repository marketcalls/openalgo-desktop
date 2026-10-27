//! The core of `scan_symbols`: the condition parser and the per-instrument
//! evaluation, ported from the web's `services/agent/tools/indicators.py`.
//! Fetching each instrument's candles is the tool's job.
//!
//! The condition is never evaluated as code. It is either a comparison of
//! one named output against a number or a cross between two named outputs,
//! read by a fixed parser that accepts exactly the web's two regular
//! expressions, and anything else is refused: the text comes from the model,
//! which is untrusted input inside a live trading account.

use serde_json::{Map, Value};

use super::compute::{compute, Cell, Computed, IndicatorError};
use super::discovery::{unknown_indicator, ArgError};
use super::frame::CandleFrame;
use super::json::Json;
use super::pyfmt::py_repr;
use super::registry::{self, IndicatorSpec};

/// What a condition may look like (`_CONDITION_HELP`).
pub(crate) const CONDITION_HELP: &str = "Use a comparison against a number, such as \"rsi < 30\" \
     or \"adx > 25\", or a cross between two of the indicator's own outputs, such as \
     \"crossover(macd_line, signal_line)\". Nothing else is accepted: this is a fixed parser, \
     not an expression evaluator.";

/// Values per output a scan computes (`last_n=5`).
pub(crate) const SCAN_LAST_N: i64 = 5;

/// A comparison operator of the condition grammar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

/// Which cross the condition asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CrossKind {
    Over,
    Under,
    Either,
}

/// A parsed condition (`parse_condition`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Condition {
    Compare {
        left: String,
        op: Op,
        right: f64,
    },
    Cross {
        kind: CrossKind,
        a: String,
        b: String,
    },
}

/// A tiny cursor over the condition text.
struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn spaces(&mut self) {
        self.rest = self.rest.trim_start();
    }
    fn eat(&mut self, token: &str) -> bool {
        match self.rest.strip_prefix(token) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }
    /// `[a-zA-Z_][a-zA-Z0-9_]*`
    fn ident(&mut self) -> Option<String> {
        let mut chars = self.rest.char_indices();
        match chars.next() {
            Some((_, c)) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return None,
        }
        let end = chars
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map(|(i, _)| i)
            .unwrap_or(self.rest.len());
        let (word, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(word.to_string())
    }
    /// `-?\d+(\.\d+)?`
    fn number(&mut self) -> Option<f64> {
        let bytes = self.rest.as_bytes();
        let mut i = usize::from(bytes.first() == Some(&b'-'));
        let digits = |from: usize| {
            bytes[from..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
        };
        let whole = digits(i);
        if whole == 0 {
            return None;
        }
        i += whole;
        if bytes.get(i) == Some(&b'.') {
            let fraction = digits(i + 1);
            if fraction > 0 {
                i += 1 + fraction;
            }
        }
        let (text, rest) = self.rest.split_at(i);
        self.rest = rest;
        text.parse().ok()
    }
    fn done(&mut self) -> bool {
        self.spaces();
        self.rest.is_empty()
    }
}

fn parse_compare(text: &str) -> Option<Condition> {
    let mut cur = Cursor { rest: text };
    cur.spaces();
    let left = cur.ident()?;
    cur.spaces();
    let op = [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("<", Op::Lt),
        (">", Op::Gt),
        ("==", Op::Eq),
    ]
    .into_iter()
    .find(|(token, _)| cur.eat(token))
    .map(|(_, op)| op)?;
    cur.spaces();
    let right = cur.number()?;
    cur.done().then_some(Condition::Compare { left, op, right })
}

fn parse_cross(text: &str) -> Option<Condition> {
    let mut cur = Cursor { rest: text };
    cur.spaces();
    let kind = [
        ("crossover", CrossKind::Over),
        ("crossunder", CrossKind::Under),
        ("cross", CrossKind::Either),
    ]
    .into_iter()
    .find(|(token, _)| cur.eat(token))
    .map(|(_, kind)| kind)?;
    cur.spaces();
    if !cur.eat("(") {
        return None;
    }
    cur.spaces();
    let a = cur.ident()?;
    cur.spaces();
    if !cur.eat(",") {
        return None;
    }
    cur.spaces();
    let b = cur.ident()?;
    cur.spaces();
    if !cur.eat(")") {
        return None;
    }
    cur.done().then_some(Condition::Cross { kind, a, b })
}

/// `parse_condition`: one of the two shapes, or None.
pub(crate) fn parse_condition(condition: &str) -> Option<Condition> {
    parse_compare(condition).or_else(|| parse_cross(condition))
}

/// The parts of a computed result a condition reads.
pub(crate) struct ResultView<'a> {
    pub indicator: &'a str,
    pub outputs: Option<&'a [String]>,
    pub values: &'a [(String, Vec<Cell>)],
}

impl<'a> From<&'a Computed> for ResultView<'a> {
    fn from(result: &'a Computed) -> Self {
        Self {
            indicator: result.indicator,
            outputs: Some(&result.outputs),
            values: &result.values,
        }
    }
}

/// `_pick`: an output by name, case-insensitively, and a single-output
/// indicator by its own name.
fn pick<'a>(result: &ResultView<'a>, label: &str) -> Option<&'a [Cell]> {
    if let Some((_, cells)) = result.values.iter().find(|(key, _)| key == label) {
        return Some(cells);
    }
    let folded = label.to_lowercase();
    if let Some((_, cells)) = result
        .values
        .iter()
        .find(|(key, _)| key.to_lowercase() == folded)
    {
        return Some(cells);
    }
    if folded == result.indicator.to_lowercase() && result.values.len() == 1 {
        return result.values.first().map(|(_, cells)| cells.as_slice());
    }
    None
}

fn number(cell: &Cell) -> Option<f64> {
    match cell {
        Cell::Num(v) => Some(*v),
        _ => None,
    }
}

fn outputs_json(result: &ResultView) -> Json {
    match result.outputs {
        Some(outputs) => Json::Arr(outputs.iter().map(|o| Json::str(o.clone())).collect()),
        None => Json::Null,
    }
}

/// `evaluate_condition`: whether the result meets the condition, and the
/// values that decided it.
pub(crate) fn evaluate_condition(parsed: &Condition, result: &ResultView) -> (bool, Json) {
    match parsed {
        Condition::Compare { left, op, right } => {
            let Some(series) = pick(result, left) else {
                return (
                    false,
                    Json::obj([
                        (
                            "error",
                            Json::str(format!(
                                "no output named {}",
                                py_repr(&Value::String(left.clone()))
                            )),
                        ),
                        ("outputs", outputs_json(result)),
                    ]),
                );
            };
            let Some(latest) = series.iter().rev().find_map(number) else {
                return (
                    false,
                    Json::obj([(
                        "error",
                        Json::str("no finite value yet; the warm-up has not completed"),
                    )]),
                );
            };
            let hit = match op {
                Op::Lt => latest < *right,
                Op::Le => latest <= *right,
                Op::Gt => latest > *right,
                Op::Ge => latest >= *right,
                Op::Eq => latest == *right,
            };
            let mut detail = Json::obj([(left.clone(), Json::Num(latest))]);
            detail.set("condition_met", Json::Bool(hit));
            (hit, detail)
        }
        Condition::Cross { kind, a, b } => evaluate_cross(*kind, a, b, result),
    }
}

fn evaluate_cross(kind: CrossKind, a: &str, b: &str, result: &ResultView) -> (bool, Json) {
    let (Some(first), Some(second)) = (pick(result, a), pick(result, b)) else {
        return (
            false,
            Json::obj([
                ("error", Json::str(format!("needs outputs {} and {}", a, b))),
                ("outputs", outputs_json(result)),
            ]),
        );
    };
    let pairs: Vec<(f64, f64)> = first
        .iter()
        .zip(second)
        .filter_map(|(x, y)| Some((number(x)?, number(y)?)))
        .collect();
    if pairs.len() < 2 {
        return (
            false,
            Json::obj([(
                "error",
                Json::str("not enough finite values to detect a cross"),
            )]),
        );
    }
    let (prev_a, prev_b) = pairs[pairs.len() - 2];
    let (last_a, last_b) = pairs[pairs.len() - 1];
    let up = prev_a <= prev_b && last_a > last_b;
    let down = prev_a >= prev_b && last_a < last_b;
    let hit = match kind {
        CrossKind::Over => up,
        CrossKind::Under => down,
        CrossKind::Either => up || down,
    };
    let mut detail = Json::obj([(a, Json::Num(last_a))]);
    detail.set(b, Json::Num(last_b));
    detail.set("condition_met", Json::Bool(hit));
    (hit, detail)
}

/// A scan the tool may run: the indicator and the parsed condition.
#[derive(Debug, Clone)]
pub struct ScanPlan {
    pub(crate) spec: &'static IndicatorSpec,
    pub(crate) condition: Condition,
}

/// `scan_symbols`' checks before any instrument is fetched: a known
/// indicator over one instrument, and a condition the parser reads.
pub fn plan_scan(indicator: &str, condition: &str) -> Result<ScanPlan, ArgError> {
    let name = indicator.trim().to_lowercase();
    let Some(spec) = registry::get_spec(&name) else {
        return Err(unknown_indicator("indicator", indicator));
    };
    if spec.needs_second_series {
        return Err(ArgError::new(
            "indicator",
            format!(
                "{} compares two instruments and cannot be scanned across a list",
                name
            ),
            "Use compute_indicator with compare_symbol instead, once per instrument.",
        ));
    }
    let Some(parsed) = parse_condition(condition) else {
        return Err(ArgError::new(
            "condition",
            format!(
                "{} is not one of the two accepted shapes",
                py_repr(&Value::String(condition.into()))
            ),
            format!(
                "{} Valid output names for {}: {}.",
                CONDITION_HELP,
                name,
                spec.outputs.join(", ")
            ),
        ));
    };
    Ok(ScanPlan {
        spec,
        condition: parsed,
    })
}

/// One instrument of a scan: the indicator over its candles, then the
/// condition. The tool adds the symbol and exchange to the detail.
pub fn scan_symbol(
    plan: &ScanPlan,
    frame: &CandleFrame,
    params: Option<&Map<String, Value>>,
) -> Result<(bool, Json), IndicatorError> {
    let result = compute(plan.spec.name, frame, params, None, SCAN_LAST_N)?;
    Ok(evaluate_condition(
        &plan.condition,
        &ResultView::from(&result),
    ))
}
