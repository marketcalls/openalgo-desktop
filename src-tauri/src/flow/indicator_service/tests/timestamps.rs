//! Decision 33 (IND-045): the timestamp column reads ISO text, offsets,
//! epoch seconds and slash dates as the web's `pd.to_datetime` does, and
//! refuses day-first dates (`11-02-2025`) and month names (`Feb 11 2025`),
//! which pandas would guess: history candles never carry those forms, and a
//! guess could put a bar on the wrong day. The refusal is the sentence pandas
//! gives for text it cannot read.

use serde_json::{json, Value};

use crate::flow::indicator_service::{
    compute_indicator, IndicatorError, IndicatorOutput, IndicatorParams, IndicatorRequest,
    IndicatorSource,
};

/// Bar `i`'s timestamp.
type Stamp = Box<dyn Fn(usize) -> Value>;

/// `sma(5)` over 30 bars whose timestamps are `stamp(i)`.
fn sma_over(stamp: impl Fn(usize) -> Value) -> Result<IndicatorOutput, IndicatorError> {
    let records: Vec<Value> = (0..30)
        .map(|i| {
            let close = 100.0 + i as f64;
            json!({"timestamp": stamp(i), "open": close, "high": close + 1.0,
                   "low": close - 1.0, "close": close, "volume": 1000 + i})
        })
        .collect();
    let params = IndicatorParams::from_pairs([("period".to_string(), json!(5))]);
    compute_indicator(&IndicatorRequest {
        indicator_name: "sma",
        params: &params,
        source: IndicatorSource::Candles {
            records: &records,
            lookback_bars: 100,
        },
        tail_bars: 5,
        offset_bars: 0,
    })
}

fn refusal(stamp: impl Fn(usize) -> Value) -> IndicatorError {
    match sma_over(stamp) {
        Err(e) => e,
        Ok(out) => panic!("expected a refusal, computed {}", out.to_value()),
    }
}

#[test]
fn day_first_dates_and_month_names_are_refused() {
    let day_first = refusal(|i| json!(format!("{:02}-02-2025", i % 28 + 1)));
    assert_eq!(day_first.kind, "DateParseError");
    assert_eq!(
        day_first.message,
        "Unknown datetime string format, unable to parse: 01-02-2025, at position 0"
    );
    assert!(day_first.is_value_error(), "a node error, as on the web");

    let day_first_later = refusal(|i| {
        json!(if i < 3 {
            format!("2025-02-{:02}", i + 1)
        } else {
            format!("{:02}-02-2025", i + 1)
        })
    });
    assert!(
        day_first_later.message.starts_with(
            "time data \"04-02-2025\" doesn't match format \"%Y-%m-%d\", at position 3."
        ),
        "{}",
        day_first_later.message
    );

    let month_names = refusal(|i| json!(format!("Feb {} 2025", i % 28 + 1)));
    assert_eq!(month_names.kind, "DateParseError");
    assert_eq!(
        month_names.message,
        "Unknown datetime string format, unable to parse: Feb 1 2025, at position 0"
    );
    let month_first = refusal(|i| json!(format!("11 Feb 2025 09:{:02}", i)));
    assert_eq!(
        month_first.message,
        "Unknown datetime string format, unable to parse: 11 Feb 2025 09:00, at position 0"
    );
}

#[test]
fn iso_offsets_epoch_seconds_and_slash_dates_read() {
    let epoch = |i: usize| 1_738_381_500 + i as i64 * 86_400;
    let reads: [(&str, Stamp); 7] = [
        ("epoch seconds", Box::new(move |i| json!(epoch(i)))),
        (
            "fractional epoch seconds",
            Box::new(move |i| json!(epoch(i) as f64 + 0.5)),
        ),
        (
            "ISO date",
            Box::new(|i| json!(format!("2025-02-{:02}", i % 28 + 1))),
        ),
        (
            "ISO date and time",
            Box::new(|i| json!(format!("2025-02-{:02} 09:15:00", i % 28 + 1))),
        ),
        (
            "ISO with an offset",
            Box::new(|i| json!(format!("2025-02-{:02}T09:15:00+05:30", i % 28 + 1))),
        ),
        (
            "ISO in UTC",
            Box::new(|i| json!(format!("2025-02-{:02}T03:45:00Z", i % 28 + 1))),
        ),
        (
            "slash dates",
            Box::new(|i| json!(format!("2025/02/{:02}", i % 28 + 1))),
        ),
    ];
    for (form, stamp) in reads {
        match sma_over(stamp) {
            Ok(IndicatorOutput::Success(out)) => assert_eq!(out.bars_used, 30, "{form}"),
            other => panic!("{form}: {other:?}"),
        }
    }
    // Epoch seconds written as text are refused, as pandas refuses them.
    let text = refusal(move |i| json!(epoch(i).to_string()));
    assert_eq!(
        text.message,
        "year 1738381500 is out of range: 1738381500, at position 0"
    );
}
