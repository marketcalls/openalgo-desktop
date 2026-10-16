//! The research tools (`mcpserver.py` "RESEARCH TOOLS"): history through
//! `/api/v1/history` shaped as the SDK's DataFrame, indicators from
//! [`super::ta`], and the web's summaries (`_load_history`, `_last`,
//! `_bundle`, `_df_records`).
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `history` | the history frame, intervals, windows and records |
//! | `args` | reading and checking the arguments, indicator inputs, summaries |
//! | `limits` | the size, range, concurrency and budget limits, and admission |
//! | `tools` | `run` and each research tool |

mod args;
mod history;
mod limits;
mod tools;

pub use args::*;
pub use history::*;
pub use limits::*;
pub use tools::*;

#[cfg(test)]
mod tests {
    use super::args::strict_int;
    use super::history::{frame_from, tail_start};
    use super::*;
    use chrono::NaiveDate;
    use serde_json::{json, Value};

    #[test]
    fn frame_sorts_dedupes_and_formats_like_pandas() {
        let data = vec![
            json!({"timestamp": 1759290300, "close": 2.5, "volume": 20}),
            json!({"timestamp": 1759290000, "close": 100.123456789123, "volume": 10}),
            json!({"timestamp": 1759290000, "close": 1.0, "volume": 1}),
        ];
        let f = frame_from(&data, true);
        assert_eq!(f.ts, vec![1759290000, 1759290300]);
        assert_eq!(f.iso(0), "2025-10-01T09:10:00+05:30");
        let r = records(&f.ts, f.intraday, &f.cols, 0);
        assert_eq!(r[0]["timestamp"], "2025-10-01T03:40:00.000Z");
        assert_eq!(r[0]["close"], json!(100.1234567891));
        assert_eq!(r[0]["volume"], json!(10));
        let d = frame_from(&data, false);
        assert_eq!(d.iso(0), "2025-10-01T03:40:00");
        assert_eq!(
            records(&d.ts, false, &d.cols, 1)[0]["timestamp"],
            "2025-10-01T03:45:00.000"
        );
    }

    /// An admitted call that runs past RESEARCH_DEADLINE is stopped with
    /// the timeout error (the research tools and get_instruments share it).
    #[tokio::test(start_paused = true)]
    async fn an_admitted_call_stops_at_its_deadline() {
        let started = tokio::time::Instant::now();
        let v = within_deadline(std::future::pending::<Value>(), "Stopped.").await;
        assert_eq!(v["error"]["error_type"], "timeout");
        assert_eq!(v["error"]["message"], "Stopped.");
        assert_eq!(started.elapsed(), RESEARCH_DEADLINE);
    }

    #[test]
    fn python_tail_slices() {
        assert_eq!(tail_start(5, 2), 3);
        assert_eq!(tail_start(5, 0), 0);
        assert_eq!(tail_start(5, 9), 0);
        assert_eq!(tail_start(5, -2), 2);
        assert_eq!(tail_start(5, i64::MIN), 5);
        assert_eq!(tail_start(5, i64::MAX), 0);
    }

    #[test]
    fn intervals_have_one_strict_spelling() {
        for ok in [
            "1m", "5m", "15m", "1h", "4h", "1s", "D", "W", "M", "Q", "Y", "2W", "3M", "999m",
        ] {
            assert!(Interval::parse(ok).is_some(), "{}", ok);
        }
        for bad in [
            "", "m", "h", "s", "d", "w", "1d", "0m", "1000m", "-1m", "+1m", "1 m", " 1m", "1m ",
            "1e3m", "1min", "1.5m", "１m", "minute",
        ] {
            assert!(Interval::parse(bad).is_none(), "{:?}", bad);
        }
        // Minutes and months differ by case, and are counted that way.
        assert_eq!(Interval::parse("1m").unwrap().true_bars_per_day(), 375.0);
        assert!(Interval::parse("M").unwrap().true_bars_per_day() < 0.1);
        assert_eq!(Interval::parse("1s").unwrap().true_bars_per_day(), 22_500.0);
    }

    #[test]
    fn dates_have_one_strict_spelling() {
        assert!(strict_date("2026-01-31").is_some());
        for bad in [
            "2026-1-31",
            "2026/01/31",
            "20260131",
            " 2026-01-31",
            "2026-01-31 ",
            "+2026-01-31",
            "12026-01-31",
            "0001-01-01",
            "9999-12-31",
            "2026-02-30",
            "31-01-2026",
            "2026-01-31T00:00",
        ] {
            assert!(strict_date(bad).is_none(), "{:?}", bad);
        }
    }

    #[test]
    fn numbers_parse_once_and_extremes_are_refused() {
        assert_eq!(strict_int(&json!(5)), Some(5));
        assert_eq!(strict_int(&json!(5.0)), Some(5));
        assert_eq!(strict_int(&json!("5")), Some(5));
        assert_eq!(strict_int(&json!("-5")), Some(-5));
        for bad in [
            json!(5.5),
            json!("5.0"),
            json!(" 5"),
            json!("1e3"),
            json!("+5"),
            json!("-"),
            json!(1e300),
            json!("99999999999999999999"),
            json!(true),
            json!([5]),
        ] {
            assert_eq!(strict_int(&bad), None, "{}", bad);
        }
        let n = |v: Value| normalize(json!({"bars": v}).as_object().unwrap());
        assert!(n(json!(u64::MAX)).is_err());
        assert!(n(json!(i64::MIN)).is_err());
        assert!(n(json!(i64::MAX)).is_err());
        assert!(n(json!(-5001)).is_err());
        assert_eq!(n(json!("5000")).unwrap()["bars"], json!(5000));
        let d = |v: Value| normalize(json!({"lookback_days": v}).as_object().unwrap());
        assert!(d(json!(i64::MIN)).is_err());
        assert!(d(json!(-3661)).is_err());
        assert_eq!(
            normalize(json!({"start_date": ""}).as_object().unwrap())
                .unwrap()
                .get("start_date"),
            None,
            "an empty date is no date, as on the web"
        );
    }

    #[test]
    fn windows_use_checked_arithmetic_and_count_true_bars() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        let one_min = Interval::parse("1m").unwrap();
        let day = Interval::parse("D").unwrap();
        let win = |a: Value, itv: &Interval, lb: i64, t: NaiveDate| {
            resolve(a.as_object().unwrap(), itv, lb, t)
        };
        // Defaults: 252 daily bars, a tail of 252.
        let w = win(json!({}), &day, 252, today).unwrap();
        assert_eq!(w.tail, Some(252));
        assert_eq!(w.end, today);
        // Extreme lookbacks never panic: clamped, then refused past the range cap.
        assert!(win(json!({}), &day, i64::MIN, today).is_err());
        assert!(win(json!({}), &day, i64::MAX, today).is_err());
        assert!(win(json!({"lookback_days": -3000}), &one_min, 252, today).is_err());
        assert!(win(json!({"lookback_days": 3000}), &one_min, 252, today).is_err());
        assert!(win(json!({"lookback_days": 3000}), &day, 252, today).is_ok());
        // The far past and future are refused, not wrapped.
        assert!(win(
            json!({"start_date": "1900-01-01", "end_date": "2100-12-31"}),
            &day,
            252,
            today
        )
        .is_err());
        assert!(win(json!({"start_date": "1900-01-01"}), &day, 252, today).is_err());
        // Seconds are counted as seconds, whatever the web's sizing table says.
        assert!(win(json!({}), &Interval::parse("1s").unwrap(), 252, today).is_err());
    }
}
