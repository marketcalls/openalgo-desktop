//! What a parameter value does in the web's `openalgo.ta` method, step by
//! step. The web hands each value of the node's params JSON to the method
//! untouched, and the method's first operations on it decide whether it
//! computes and, if not, the exception and its sentence: `int(value)`,
//! `validate_period(value)`, `value <= 0`, `value + 1`, `range(value)`,
//! a slice, `float(value)`, a NumPy assignment or multiplication,
//! `value.upper()`. Each [`Step`] is one of those, with CPython 3.12 and
//! NumPy's own wording; `rules` says which steps each parameter goes
//! through, as measured on openalgo 2.0.5 (`flow_indicators/types.json`).

use serde_json::Value;

use super::super::IndicatorError;
use crate::flow::contract::pyfmt;

/// Which side of the operator the value is on (`value <= 0` is `Left`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Side {
    Left,
    Right,
}

/// One operation the method performs on the value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Step {
    /// `int(value)`: numbers truncate, integer text parses.
    IntCall,
    /// `BaseIndicator.validate_period`: anything but an int is refused.
    Period,
    /// A comparison with an int (`value <= 0`).
    Compare(&'static str, Side),
    /// Arithmetic with an int (`value + 1`, `value - 1`, `value / 100`).
    Arith(&'static str, Side),
    /// `range(value)`: only an int.
    Index,
    /// A slice bound: an int or None.
    Slice,
    /// `float(value)`.
    FloatCall,
    /// A NumPy float array element set to the value (`arr[0] = value`).
    ArrayAssign,
    /// A NumPy float array multiplied by the value.
    ArrayMultiply,
    /// `value.upper()`.
    Upper,
    /// Compared with the method's accepted words: anything that is not text
    /// raises the method's `ValueError` (`{}` is the value's `str()`).
    TextOrRefuse(&'static str),
    /// Compared with the method's accepted words, anything else meaning
    /// the default: a value that is not text reads as the default.
    TextOrDefault,
    /// Not read with the method's other arguments as given.
    Unused,
}

/// A parameter value as Python holds it after `json.loads`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Py {
    Int(i64),
    /// An int past 64 bits, as written.
    Big(Value),
    Float(f64),
    Bool(bool),
    Str(String),
    None,
    List(Vec<Value>),
    Dict(Value),
    /// The library's own default stands (the method does not read it).
    Default,
}

impl Py {
    pub(crate) fn from_json(value: &Value) -> Self {
        match value {
            Value::Null => Py::None,
            Value::Bool(b) => Py::Bool(*b),
            Value::Number(n) => match (n.as_i64(), n.is_f64()) {
                (Some(i), _) => Py::Int(i),
                (None, true) => Py::Float(n.as_f64().unwrap_or(f64::NAN)),
                (None, false) => Py::Big(value.clone()),
            },
            Value::String(s) => Py::Str(s.clone()),
            Value::Array(items) => Py::List(items.clone()),
            Value::Object(_) => Py::Dict(value.clone()),
        }
    }

    /// `type(value).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            Py::Int(_) | Py::Big(_) | Py::Default => "int",
            Py::Float(_) => "float",
            Py::Bool(_) => "bool",
            Py::Str(_) => "str",
            Py::None => "NoneType",
            Py::List(_) => "list",
            Py::Dict(_) => "dict",
        }
    }

    /// A Python number (bool included: it is an int).
    fn is_number(&self) -> bool {
        matches!(self, Py::Int(_) | Py::Big(_) | Py::Float(_) | Py::Bool(_))
    }

    /// An int (bool included).
    fn is_int(&self) -> bool {
        matches!(self, Py::Int(_) | Py::Big(_) | Py::Bool(_))
    }
}

fn type_error(message: String) -> IndicatorError {
    IndicatorError::type_error(message)
}

/// Run one step on the value: the value the method goes on with, or the
/// exception it raises. `bars` is the length of the series a NumPy
/// operation broadcasts against.
pub(crate) fn apply(step: Step, value: Py, bars: usize) -> Result<Py, IndicatorError> {
    let t = value.type_name();
    match step {
        Step::IntCall => int_call(value),
        Step::Period if value.is_int() => Ok(value),
        Step::Period => Err(type_error(format!(
            "Period must be an integer, got <class '{t}'>"
        ))),
        Step::Compare(_, _) | Step::Arith(_, _) if value.is_number() => Ok(value),
        Step::Compare(op, side) => Err(type_error(match side {
            Side::Left => format!("'{op}' not supported between instances of '{t}' and 'int'"),
            Side::Right => format!("'{op}' not supported between instances of 'int' and '{t}'"),
        })),
        Step::Arith(op, side) => Err(type_error(arith_message(op, side, t))),
        Step::Index if value.is_int() => Ok(value),
        Step::Index => Err(type_error(format!(
            "'{t}' object cannot be interpreted as an integer"
        ))),
        Step::Slice if value.is_int() || value == Py::None => Ok(value),
        Step::Slice => Err(type_error(
            "slice indices must be integers or None or have an __index__ method".into(),
        )),
        Step::FloatCall => float_call(value, false),
        Step::ArrayAssign => float_call(value, true),
        Step::ArrayMultiply => array_multiply(value, bars),
        Step::Upper | Step::TextOrRefuse(_) | Step::TextOrDefault
            if matches!(value, Py::Str(_)) =>
        {
            Ok(value)
        }
        Step::Upper => Err(IndicatorError {
            kind: "AttributeError",
            message: format!("'{t}' object has no attribute 'upper'"),
        }),
        Step::TextOrRefuse(template) => Err(IndicatorError::value(
            template.replace("{}", &python_str(&value)),
        )),
        Step::TextOrDefault | Step::Unused => Ok(Py::Default),
    }
}

/// `int + "a"`, `"a" + 1`, `[1] + 1`: CPython's sentences.
fn arith_message(op: &str, side: Side, t: &str) -> String {
    match (op, side, t) {
        ("+", Side::Left, "str") => "can only concatenate str (not \"int\") to str".into(),
        ("+", Side::Left, "list") => "can only concatenate list (not \"int\") to list".into(),
        (_, Side::Left, _) => format!("unsupported operand type(s) for {op}: '{t}' and 'int'"),
        (_, Side::Right, _) => format!("unsupported operand type(s) for {op}: 'int' and '{t}'"),
    }
}

/// `str(value)` for the method's own message.
fn python_str(value: &Py) -> String {
    match value {
        Py::Str(s) => s.clone(),
        Py::Int(i) => i.to_string(),
        Py::Float(f) => pyfmt::float(*f),
        Py::Bool(true) => "True".into(),
        Py::Bool(false) => "False".into(),
        Py::None => "None".into(),
        Py::List(items) => pyfmt::str(&Value::Array(items.clone())),
        Py::Dict(v) | Py::Big(v) => pyfmt::str(v),
        Py::Default => String::new(),
    }
}

/// `int(value)`.
fn int_call(value: Py) -> Result<Py, IndicatorError> {
    match value {
        Py::Int(_) | Py::Big(_) | Py::Bool(_) => Ok(value),
        Py::Float(f) if f.is_nan() => {
            Err(IndicatorError::value("cannot convert float NaN to integer"))
        }
        Py::Float(f) if f.is_infinite() => Err(IndicatorError {
            kind: "OverflowError",
            message: "cannot convert float infinity to integer".into(),
        }),
        Py::Float(f) if f.trunc().abs() < 9.2e18 => Ok(Py::Int(f.trunc() as i64)),
        Py::Float(f) => Ok(Py::Big(Value::from(f))),
        Py::Str(s) => match parse_int(&s) {
            Some(parsed) => Ok(parsed),
            None => Err(IndicatorError::value(format!(
                "invalid literal for int() with base 10: {}",
                pyfmt::quote(&s)
            ))),
        },
        other => Err(type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// Python's `int(text)`: whitespace around, a sign, digits with single
/// underscores between them.
fn parse_int(text: &str) -> Option<Py> {
    let body = text.trim();
    let digits = body.strip_prefix(['+', '-']).unwrap_or(body);
    let bytes = digits.as_bytes();
    let well_formed = !bytes.is_empty()
        && bytes.first().is_some_and(u8::is_ascii_digit)
        && bytes.last().is_some_and(u8::is_ascii_digit)
        && bytes.iter().all(|b| b.is_ascii_digit() || *b == b'_')
        && !digits.contains("__");
    if !well_formed {
        return None;
    }
    let clean: String = body.chars().filter(|c| *c != '_').collect();
    Some(
        clean
            .parse::<i64>()
            .map_or_else(|_| Py::Big(Value::from(clean.clone())), Py::Int),
    )
}

/// `float(value)`, or a NumPy element assignment (`assign`), which reads a
/// None as NaN and refuses a list.
fn float_call(value: Py, assign: bool) -> Result<Py, IndicatorError> {
    match value {
        Py::Int(i) => Ok(Py::Float(i as f64)),
        Py::Bool(b) => Ok(Py::Float(f64::from(u8::from(b)))),
        Py::Float(_) => Ok(value),
        Py::Big(v) => Ok(Py::Float(v.as_f64().unwrap_or(f64::NAN))),
        Py::Str(s) => pyfmt::parse_float(&s).map(Py::Float).ok_or_else(|| {
            IndicatorError::value(format!(
                "could not convert string to float: {}",
                pyfmt::quote(&s)
            ))
        }),
        Py::None if assign => Ok(Py::Float(f64::NAN)),
        Py::List(_) if assign => Err(IndicatorError::value(
            "setting an array element with a sequence.",
        )),
        other => Err(type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// A float64 array of `bars` values times the value: a number scales it, a
/// one-number list broadcasts as that number, text has no NumPy loop.
fn array_multiply(value: Py, bars: usize) -> Result<Py, IndicatorError> {
    let no_loop = |width: usize| IndicatorError {
        kind: "UFuncTypeError",
        message: format!(
            "ufunc 'multiply' did not contain a loop with signature matching types \
             (dtype('float64'), dtype('<U{}')) -> None",
            width.max(1)
        ),
    };
    match value {
        v if v.is_number() => Ok(v),
        Py::Str(s) => Err(no_loop(s.chars().count())),
        Py::List(items) if !items.is_empty() && items.iter().all(Value::is_string) => {
            let width = items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(1);
            Err(no_loop(width))
        }
        Py::List(items) => match items.as_slice() {
            [one] if one.is_number() => Ok(Py::from_json(one)),
            _ => Err(IndicatorError::value(format!(
                "operands could not be broadcast together with shapes ({bars},) ({},) ",
                items.len()
            ))),
        },
        other => Err(type_error(format!(
            "unsupported operand type(s) for *: 'float' and '{}'",
            other.type_name()
        ))),
    }
}
