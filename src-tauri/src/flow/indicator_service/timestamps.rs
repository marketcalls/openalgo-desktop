//! `pd.to_datetime(df["timestamp"])` in the web's `compute_indicator`.
//! The index it builds never changes a value, but a column pandas cannot
//! read raises, and the Indicator node reports that sentence. This answers
//! only whether pandas 2.3 raises, and with what, for the columns a history
//! source sends: epoch numbers (the desktop's own history), ISO text with
//! or without an offset, slash dates, empty or missing cells, and the
//! mistakes around them (text that is no date, epoch seconds written as
//! text, a column mixing dates and date-times, offsets and none, booleans,
//! lists). Other date spellings pandas guesses (`Feb 11 2025`, `11-02-2025`)
//! read as unparseable here.
//!
//! pandas guesses a format from the first non-empty text and holds every
//! other cell to it; with no guess, or a column that starts with something
//! else, it reads each cell on its own.

use serde_json::Value;

use super::IndicatorError;
use crate::flow::contract::pyfmt;

/// The hint pandas appends when a cell breaks the guessed format.
const HINT: &str = " You might want to try:\n    - passing `format` if your strings have a \
consistent format;\n    - passing `format='ISO8601'` if your strings are all ISO8601 but not \
necessarily in exactly the same format;\n    - passing `format='mixed'`, and the format will be \
inferred for each element individually. You might want to use `dayfirst` alongside this.";

/// The text pandas reads as a missing time.
const NAT_TEXT: [&str; 7] = ["", "NaT", "nat", "NAT", "nan", "NaN", "NAN"];

fn error(kind: &'static str, message: String) -> IndicatorError {
    IndicatorError { kind, message }
}

/// Whether the records' timestamp column reads as pandas reads it.
pub(crate) fn check(records: &[Value]) -> Result<(), IndicatorError> {
    let carried = records
        .iter()
        .any(|r| r.as_object().is_some_and(|o| o.contains_key("timestamp")));
    if !carried {
        return Ok(());
    }
    let cells: Vec<Option<&Value>> = records
        .iter()
        .map(|r| r.get("timestamp").filter(|v| !v.is_null()))
        .collect();
    let present: Vec<&Value> = cells.iter().flatten().copied().collect();
    if present.iter().all(|v| v.is_number()) {
        return numbers(&cells);
    }
    if present.len() == cells.len() && present.iter().all(|v| v.is_boolean()) {
        return Err(error(
            "TypeError",
            "dtype bool cannot be converted to datetime64[ns]".into(),
        ));
    }
    let first_text = cells.iter().flatten().find_map(|v| match v {
        Value::String(s) if !NAT_TEXT[1..].contains(&s.as_str()) => Some(s.as_str()),
        _ => None,
    });
    let first = cells.iter().flatten().find(|v| match v {
        Value::String(s) => !NAT_TEXT[1..].contains(&s.as_str()),
        _ => true,
    });
    match (first, first_text.and_then(guess_format)) {
        (Some(Value::String(_)), Some(format)) => strict(&cells, &format),
        _ => each(&cells),
    }
}

/// An int past 64 bits has no nanosecond time.
fn numbers(cells: &[Option<&Value>]) -> Result<(), IndicatorError> {
    for (i, cell) in cells.iter().enumerate() {
        if let Some(v) = cell.filter(|v| v.is_u64() && !v.is_i64()) {
            return Err(out_of_bounds(v, i));
        }
    }
    Ok(())
}

fn out_of_bounds(value: &Value, at: usize) -> IndicatorError {
    error(
        "OutOfBoundsDatetime",
        format!("cannot convert input {value} with the unit 'ns', at position {at}"),
    )
}

/// Every cell held to the format guessed from the first text.
fn strict(cells: &[Option<&Value>], format: &str) -> Result<(), IndicatorError> {
    for (i, cell) in cells.iter().enumerate() {
        let Some(value) = cell else { continue };
        let text = match value {
            Value::String(s) if NAT_TEXT.contains(&s.as_str()) => continue,
            Value::String(s) => s.clone(),
            other => pyfmt::str(other),
        };
        let problem = match parse(format, &text) {
            Parsed::Valid(_) => continue,
            Parsed::Mismatch => format!("time data \"{text}\" doesn't match format \"{format}\""),
            Parsed::Unconverted(rest) => format!(
                "unconverted data remains when parsing with format \"{format}\": \"{rest}\""
            ),
            Parsed::OutOfRange(what) => what.to_string(),
        };
        return Err(error(
            "ValueError",
            format!("{problem}, at position {i}.{HINT}"),
        ));
    }
    Ok(())
}

/// Each cell read on its own (no format guessed, or the column does not
/// start with text).
fn each(cells: &[Option<&Value>]) -> Result<(), IndicatorError> {
    let (mut aware, mut naive) = (false, false);
    for (i, cell) in cells.iter().enumerate() {
        let Some(value) = cell else { continue };
        match value {
            Value::Number(_) if value.is_u64() && !value.is_i64() => {
                return Err(out_of_bounds(value, i))
            }
            Value::Number(_) => naive = true,
            Value::String(s) if NAT_TEXT.contains(&s.as_str()) => {}
            Value::String(s) => match read_alone(s, i)? {
                true => aware = true,
                false => naive = true,
            },
            other => {
                let class = match other {
                    Value::Bool(_) => "bool",
                    Value::Array(_) => "list",
                    _ => "dict",
                };
                return Err(error(
                    "TypeError",
                    format!("<class '{class}'> is not convertible to datetime, at position {i}"),
                ));
            }
        }
        if aware && naive {
            return Err(error(
                "ValueError",
                "Cannot mix tz-aware with tz-naive values".into(),
            ));
        }
    }
    Ok(())
}

/// One text cell read without a format (dateutil's parser): whether it
/// carries an offset, or why it is no time.
fn read_alone(text: &str, at: usize) -> Result<bool, IndicatorError> {
    let parse_error = |what: String| error("DateParseError", format!("{what}, at position {at}"));
    if text.trim().is_empty() {
        return Err(parse_error(format!(
            "Unable to parse datetime string: {text}"
        )));
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return match text.len() {
            0..=3 => Err(error(
                "ValueError",
                format!("Given date string \"{text}\" not likely a datetime, at position {at}"),
            )),
            4 | 8 => Ok(false),
            _ => Err(parse_error(format!(
                "year {} is out of range: {text}",
                text.trim_start_matches('0')
            ))),
        };
    }
    match layout(text) {
        Some(format) => match parse(&format, text) {
            Parsed::Valid(aware) => Ok(aware),
            Parsed::OutOfRange(what) => Err(parse_error(format!("{what}: {text}"))),
            _ => Err(parse_error(format!(
                "Unknown datetime string format, unable to parse: {text}"
            ))),
        },
        None => Err(parse_error(format!(
            "Unknown datetime string format, unable to parse: {text}"
        ))),
    }
}

/// `guess_datetime_format`: the format of an ISO or slash date and time,
/// when every part of it is a real date.
fn guess_format(text: &str) -> Option<String> {
    let format = layout(text)?;
    matches!(parse(&format, text), Parsed::Valid(_)).then_some(format)
}

/// The format a date, date-time or year is written in (spaces around kept,
/// as pandas keeps them), whatever its values.
fn layout(text: &str) -> Option<String> {
    let core = text.trim_start_matches(' ');
    let lead = &text[..text.len() - core.len()];
    let body = core.trim_end_matches(' ');
    let trail = &core[body.len()..];
    let digits = |s: &str, lo: usize, hi: usize| {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        (lo..=hi).contains(&n).then_some(n)
    };
    let mut format = String::from(lead);
    let mut rest = body;
    if body.len() == 4 && body.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("{lead}%Y{trail}"));
    }
    if digits(body, 8, 8).is_some() && body.as_bytes().get(8).is_none_or(|b| *b == b'T') {
        format.push_str("%Y%m%d");
        rest = &body[8..];
        if let Some(time) = rest.strip_prefix('T') {
            if digits(time, 6, 6).is_some() && time.len() == 6 {
                format.push_str("T%H%M%S");
                rest = "";
            } else {
                return None;
            }
        }
    } else {
        digits(rest, 4, 4)?;
        let sep = *rest.as_bytes().get(4)?;
        if sep != b'-' && sep != b'/' {
            return None;
        }
        let sep = sep as char;
        rest = &rest[5..];
        let month = digits(rest, 1, 2)?;
        rest = rest[month..].strip_prefix(sep)?;
        let day = digits(rest, 1, 2)?;
        rest = &rest[day..];
        format.push_str(&format!("%Y{sep}%m{sep}%d"));
    }
    if !rest.is_empty() {
        format.push_str(&time_layout(rest)?);
    }
    format.push_str(trail);
    Some(format)
}

/// `THH[:MM[:SS[.f]]][offset]` after a date.
fn time_layout(text: &str) -> Option<String> {
    let sep = text.chars().next().filter(|c| *c == 'T' || *c == ' ')?;
    let mut rest = &text[1..];
    let mut format = format!("{sep}");
    for (i, directive) in ["%H", ":%M", ":%S"].iter().enumerate() {
        if i > 0 {
            match rest.strip_prefix(':') {
                Some(r) if r.bytes().next().is_some_and(|b| b.is_ascii_digit()) => rest = r,
                _ => break,
            }
        }
        let n = rest.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=2).contains(&n) {
            return None;
        }
        rest = &rest[n..];
        format.push_str(directive);
    }
    if format.ends_with("%S") {
        if let Some(r) = rest.strip_prefix('.') {
            let n = r.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return None;
            }
            rest = &r[n..];
            format.push_str(".%f");
        }
    }
    let (space, offset) = match rest.strip_prefix(' ') {
        Some(r) => (" ", r),
        None => ("", rest),
    };
    if offset.is_empty() && space.is_empty() {
        return Some(format);
    }
    offset_length(offset).filter(|n| *n == offset.len())?;
    format.push_str(space);
    format.push_str("%z");
    Some(format)
}

/// The length of a `Z`, `+HH`, `+HHMM` or `+HH:MM` offset at the start.
fn offset_length(text: &str) -> Option<usize> {
    if text.starts_with('Z') {
        return Some(1);
    }
    let sign = text.chars().next().filter(|c| *c == '+' || *c == '-')?;
    let body = &text[sign.len_utf8()..];
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    match digits(body) {
        2 if body.as_bytes().get(2) == Some(&b':') && digits(&body[3..]) >= 2 => Some(6),
        2 => Some(3),
        n if n >= 4 => Some(5),
        _ => None,
    }
}

/// What reading `text` with `format` gives.
enum Parsed {
    /// A real time; true when it carries an offset.
    Valid(bool),
    Mismatch,
    /// Text left after the format: the rest.
    Unconverted(String),
    /// A part outside its range: pandas' sentence for it.
    OutOfRange(&'static str),
}

/// Read `text` with a `format` of `%Y %m %d %H %M %S %f %z` and literals.
fn parse(format: &str, text: &str) -> Parsed {
    let mut values = [0u32; 6];
    let mut aware = false;
    let mut rest = text;
    let mut directives = format.chars();
    while let Some(c) = directives.next() {
        if c != '%' {
            match rest.strip_prefix(c) {
                Some(r) => rest = r,
                None => return Parsed::Mismatch,
            }
            continue;
        }
        let Some(d) = directives.next() else {
            return Parsed::Mismatch;
        };
        let max = match d {
            'Y' => 4,
            'f' => 9,
            'z' => 0,
            _ => 2,
        };
        if d == 'z' {
            match offset_length(rest) {
                Some(n) => {
                    rest = &rest[n..];
                    aware = true;
                }
                None => return Parsed::Mismatch,
            }
            continue;
        }
        let n = rest
            .bytes()
            .take(max)
            .take_while(u8::is_ascii_digit)
            .count();
        if n == 0 || (d == 'Y' && n != 4) {
            return Parsed::Mismatch;
        }
        let number: u32 = rest[..n].parse().unwrap_or(0);
        rest = &rest[n..];
        let slot = match d {
            'Y' => 0,
            'm' => 1,
            'd' => 2,
            'H' => 3,
            'M' => 4,
            'S' => 5,
            _ => continue,
        };
        values[slot] = number;
    }
    if !rest.is_empty() {
        return Parsed::Unconverted(rest.to_string());
    }
    let [year, month, day, hour, minute, second] = values;
    let has = |d: &str| format.contains(d);
    if has("%m") && !(1..=12).contains(&month) {
        return Parsed::OutOfRange("month must be in 1..12");
    }
    if has("%d") && chrono::NaiveDate::from_ymd_opt(year as i32, month, day).is_none() {
        return Parsed::OutOfRange("day is out of range for month");
    }
    if hour > 23 {
        return Parsed::OutOfRange("hour must be in 0..23");
    }
    if minute > 59 {
        return Parsed::OutOfRange("minute must be in 0..59");
    }
    if second > 59 {
        return Parsed::OutOfRange("second must be in 0..59");
    }
    Parsed::Valid(aware)
}
