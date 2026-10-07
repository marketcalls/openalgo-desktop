//! CSV and Parquet import (web `import_from_csv` / `import_from_parquet`).
//!
//! DuckDB reads the file (`read_csv` with every column as text, or
//! `read_parquet`); rows are converted here with pandas' rules: column names
//! are case- and space-insensitive; the time comes from `timestamp` (epoch
//! seconds, or milliseconds when the first value is above 1e12),
//! `datetime`, or `date` plus an optional `time`; open, high, low and close
//! must be numbers (rows where one is not are skipped and counted); volume
//! and oi default to 0.
//!
//! Text dates without a zone are IST wall-clock times, so a file exported
//! here imports back unchanged.

use super::db::Bar;
use super::export::sql_str;
use super::time::from_ist;
use crate::error::Result;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use duckdb::Connection;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Csv,
    Parquet,
}

impl FileKind {
    fn label(self) -> &'static str {
        match self {
            FileKind::Csv => "CSV",
            FileKind::Parquet => "Parquet",
        }
    }
}

/// Parsed rows and how many were dropped for bad prices.
#[derive(Debug)]
pub struct Parsed {
    pub bars: Vec<Bar>,
    pub dropped: usize,
}

const NAIVE_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    "%Y/%m/%d %H:%M:%S",
    "%Y/%m/%d %H:%M",
    "%m/%d/%Y %H:%M:%S",
    "%m/%d/%Y %H:%M",
    "%d/%m/%Y %H:%M:%S",
    "%d/%m/%Y %H:%M",
    "%m-%d-%Y %H:%M:%S",
    "%m-%d-%Y %H:%M",
    "%d-%m-%Y %H:%M:%S",
    "%d-%m-%Y %H:%M",
    "%d-%b-%Y %H:%M:%S",
    "%d-%b-%Y %H:%M",
];

const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d", "%Y/%m/%d", "%m/%d/%Y", "%d/%m/%Y", "%m-%d-%Y", "%d-%m-%Y", "%d-%b-%Y", "%d-%b-%y",
    "%Y%m%d",
];

/// A text date-time to epoch seconds; IST when it carries no zone.
pub fn parse_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp());
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S%.f%z",
    ] {
        if let Ok(t) = DateTime::parse_from_str(s, f) {
            return Some(t.timestamp());
        }
    }
    for f in NAIVE_FORMATS {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(from_ist(n).timestamp());
        }
    }
    for f in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Some(from_ist(d.and_time(NaiveTime::MIN)).timestamp());
        }
    }
    None
}

fn is_numeric_type(t: &str) -> bool {
    let t = t.to_ascii_uppercase();
    [
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "FLOAT",
        "DOUBLE",
        "REAL",
    ]
    .contains(&t.as_str())
        || t.starts_with("DECIMAL")
}

fn is_time_type(t: &str) -> bool {
    t.to_ascii_uppercase().starts_with("TIMESTAMP")
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

enum TsSource {
    /// Epoch number (seconds, or ms when the first value is above 1e12).
    Number(String),
    /// A typed timestamp column (absolute instant).
    Typed(String),
    /// Text in one column.
    Text(String),
    /// Text date plus optional text time.
    DateTime(String, Option<String>),
}

/// Read the file into candles. `Ok(Err(message))` is a trader-facing
/// rejection of the file's content.
pub fn read_file(
    c: &Connection,
    path: &Path,
    kind: FileKind,
) -> Result<std::result::Result<Parsed, String>> {
    let src = match kind {
        FileKind::Csv => format!(
            "read_csv({}, header = true, all_varchar = true)",
            sql_str(&path.to_string_lossy())
        ),
        FileKind::Parquet => format!("read_parquet({})", sql_str(&path.to_string_lossy())),
    };
    let unreadable = format!(
        "The file could not be read. Check that it is a valid {} file with a header row.",
        kind.label()
    );
    // Column names and types.
    let cols: Vec<(String, String)> = match (|| -> Result<Vec<(String, String)>> {
        let mut st = c.prepare(&format!("DESCRIBE SELECT * FROM {}", src))?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    })() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Historify import could not read the file: {}", e);
            return Ok(Err(unreadable));
        }
    };
    let total: i64 = match c.query_row(&format!("SELECT COUNT(*) FROM {}", src), [], |r| r.get(0)) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("Historify import could not count rows: {}", e);
            return Ok(Err(unreadable));
        }
    };
    if total == 0 || cols.is_empty() {
        return Ok(Err(format!("{} file is empty", kind.label())));
    }
    let mut by_name: HashMap<String, (String, String)> = HashMap::new();
    for (name, ty) in &cols {
        by_name
            .entry(name.trim().to_lowercase())
            .or_insert((name.clone(), ty.clone()));
    }
    let col = |k: &str| by_name.get(k).cloned();

    let ts = if let Some((name, ty)) = col("timestamp") {
        let numeric = if kind == FileKind::Parquet {
            is_numeric_type(&ty)
        } else {
            let bad: i64 = c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {s} WHERE {c} IS NOT NULL AND TRY_CAST({c} AS DOUBLE) IS NULL",
                    s = src,
                    c = ident(&name)
                ),
                [],
                |r| r.get(0),
            )?;
            bad == 0
        };
        if numeric {
            TsSource::Number(name)
        } else if is_time_type(&ty) {
            TsSource::Typed(name)
        } else {
            TsSource::Text(name)
        }
    } else if let Some((name, ty)) = col("datetime") {
        if is_time_type(&ty) {
            TsSource::Typed(name)
        } else {
            TsSource::Text(name)
        }
    } else if let Some((name, _)) = col("date") {
        TsSource::DateTime(name, col("time").map(|t| t.0))
    } else {
        return Ok(Err(format!(
            "{} must have 'timestamp', 'datetime', or 'date' column",
            kind.label()
        )));
    };

    let required = ["open", "high", "low", "close", "volume"];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| col(k).is_none())
        .collect();
    if !missing.is_empty() {
        return Ok(Err(format!(
            "Missing required columns: {}",
            missing.join(", ")
        )));
    }
    let num = |k: &str| match col(k) {
        Some((n, _)) => format!("TRY_CAST({} AS DOUBLE)", ident(&n)),
        None => "CAST(0 AS DOUBLE)".into(),
    };
    let (ts_a, ts_b) = match &ts {
        TsSource::Number(n) => (format!("TRY_CAST({} AS DOUBLE)", ident(n)), "NULL".into()),
        TsSource::Typed(n) => (
            format!("CAST(epoch_ms({}) // 1000 AS DOUBLE)", ident(n)),
            "NULL".into(),
        ),
        TsSource::Text(n) => (format!("CAST({} AS VARCHAR)", ident(n)), "NULL".into()),
        TsSource::DateTime(d, t) => (
            format!("CAST({} AS VARCHAR)", ident(d)),
            t.as_ref()
                .map(|t| format!("CAST({} AS VARCHAR)", ident(t)))
                .unwrap_or_else(|| "NULL".into()),
        ),
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {} FROM {}",
        ts_a,
        ts_b,
        num("open"),
        num("high"),
        num("low"),
        num("close"),
        num("volume"),
        num("oi"),
        src
    );
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query([])?;
    let mut bars = Vec::with_capacity(total.clamp(0, 1 << 20) as usize);
    let mut dropped = 0usize;
    let mut ms: Option<bool> = None;
    let mut line = 1usize;
    let bad_time = |line: usize| {
        format!(
            "Could not read the date and time in row {}. Use YYYY-MM-DD and HH:MM:SS, or epoch seconds.",
            line
        )
    };
    while let Some(r) = rows.next()? {
        line += 1;
        let ts = match &ts {
            TsSource::Number(_) | TsSource::Typed(_) => {
                let Some(v) = r.get::<_, Option<f64>>(0)? else {
                    return Ok(Err(bad_time(line)));
                };
                let is_ms = *ms.get_or_insert(matches!(ts, TsSource::Number(_)) && v > 1e12);
                if is_ms {
                    (v / 1000.0).floor() as i64
                } else {
                    v as i64
                }
            }
            TsSource::Text(_) => {
                let s: Option<String> = r.get(0)?;
                match s.as_deref().and_then(parse_datetime) {
                    Some(t) => t,
                    None => return Ok(Err(bad_time(line))),
                }
            }
            TsSource::DateTime(..) => {
                let d: Option<String> = r.get(0)?;
                let t: Option<String> = r.get(1)?;
                let text = match (d, t) {
                    (Some(d), Some(t)) => format!("{} {}", d.trim(), t.trim()),
                    (Some(d), None) => d,
                    _ => return Ok(Err(bad_time(line))),
                };
                match parse_datetime(&text) {
                    Some(t) => t,
                    None => return Ok(Err(bad_time(line))),
                }
            }
        };
        let f = |i: usize| -> std::result::Result<Option<f64>, duckdb::Error> {
            Ok(r.get::<_, Option<f64>>(i)?.filter(|v| v.is_finite()))
        };
        let (Some(open), Some(high), Some(low), Some(close)) = (f(2)?, f(3)?, f(4)?, f(5)?) else {
            dropped += 1;
            continue;
        };
        bars.push(Bar {
            timestamp: ts,
            open,
            high,
            low,
            close,
            volume: f(6)?.map(|v| v as i64).unwrap_or(0),
            oi: f(7)?.map(|v| v as i64).unwrap_or(0),
        });
    }
    if bars.is_empty() {
        return Ok(Err("No valid data rows after parsing".into()));
    }
    Ok(Ok(Parsed { bars, dropped }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_times_are_ist_unless_zoned() {
        // 2024-01-01 09:15 IST = 03:45 UTC.
        assert_eq!(parse_datetime("2024-01-01 09:15:00"), Some(1_704_080_700));
        assert_eq!(parse_datetime("2024-01-01T09:15:00"), Some(1_704_080_700));
        assert_eq!(parse_datetime("2024-01-01 09:15"), Some(1_704_080_700));
        assert_eq!(parse_datetime("2024-01-01T03:45:00Z"), Some(1_704_080_700));
        assert_eq!(
            parse_datetime("2024-01-01T09:15:00+05:30"),
            Some(1_704_080_700)
        );
        assert_eq!(parse_datetime("2024-01-01"), Some(1_704_047_400));
        assert_eq!(parse_datetime("not a date"), None);
    }
}
