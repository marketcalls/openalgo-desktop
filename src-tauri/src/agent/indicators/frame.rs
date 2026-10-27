//! The candles an indicator runs over: the cleaned frame the web's
//! `candle_frame` (`services/agent/tools/market.py`) builds. Oldest first,
//! one row per timestamp, and no NaN anywhere: the tool that fetched the
//! history cleans it before it gets here.

use chrono::DateTime;
use chrono_tz::Asia::Kolkata;

/// Candles in columns, as a DataFrame holds them.
#[derive(Debug, Clone, Default)]
pub struct CandleFrame {
    /// UTC epoch seconds, one per bar.
    timestamps: Vec<i64>,
    /// `open`, `high`, `low`, `close`, `volume`, those the broker supplied,
    /// in that order; each as long as `timestamps`.
    columns: Vec<(String, Vec<f64>)>,
}

impl CandleFrame {
    /// A frame from cleaned columns. A column of another length is cut or
    /// padded with NaN to the bar count, so the frame stays rectangular.
    pub fn new(timestamps: Vec<i64>, columns: Vec<(String, Vec<f64>)>) -> Self {
        let n = timestamps.len();
        let columns = columns
            .into_iter()
            .map(|(name, mut values)| {
                values.resize(n, f64::NAN);
                (name, values)
            })
            .collect();
        Self {
            timestamps,
            columns,
        }
    }

    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }

    pub fn timestamps(&self) -> &[i64] {
        &self.timestamps
    }

    pub fn column(&self, name: &str) -> Option<&[f64]> {
        self.columns
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, values)| values.as_slice())
    }

    pub fn column_names(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().map(|(name, _)| name.as_str())
    }

    /// The columns, in frame order.
    pub fn columns(&self) -> &[(String, Vec<f64>)] {
        &self.columns
    }
}

/// A pandas `Timestamp.isoformat()` of an IST index entry:
/// `2026-02-11T09:15:00+05:30`.
pub fn ist_iso(epoch_seconds: i64) -> String {
    match DateTime::from_timestamp(epoch_seconds, 0) {
        Some(utc) => utc
            .with_timezone(&Kolkata)
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string(),
        None => epoch_seconds.to_string(),
    }
}
