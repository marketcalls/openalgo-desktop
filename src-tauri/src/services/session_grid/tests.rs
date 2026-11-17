//! The session grid (web nuvama `_session_align`) on bars worked by hand:
//! 2026-11-16, NSE open 09:15 IST (epoch 1794800700).

use super::{bucket_day, plan};
use crate::brokers::types::Candle;
use chrono::NaiveDate;
use std::collections::HashMap;

const DAY: i64 = 1_794_787_200 - 19_800; // 2026-11-16 00:00 IST
const OPEN_0915: i64 = 555;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
}

/// A bar at `hh:mm` IST.
fn bar(hh: i64, mm: i64, o: f64, h: f64, l: f64, c: f64, v: i64) -> Candle {
    Candle {
        timestamp: DAY + hh * 3600 + mm * 60,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        oi: 0,
    }
}

fn opens() -> HashMap<NaiveDate, i64> {
    HashMap::from([(day(), OPEN_0915)])
}

/// 1h and 30m bars on a 09:15 open are rebuilt from 15m; 15m bars are on
/// the grid and kept unless a bar comes before the open; a day the calendar
/// has no session for is left alone.
#[test]
fn each_day_is_rebuilt_from_the_coarsest_interval_that_fits() {
    let hourly = [
        bar(9, 0, 1.0, 1.0, 1.0, 1.0, 1),
        bar(10, 0, 1.0, 1.0, 1.0, 1.0, 1),
    ];
    assert_eq!(
        plan(&hourly, 60, &opens()).get(&15),
        Some(&vec![(day(), OPEN_0915)])
    );
    assert_eq!(
        plan(&hourly, 30, &opens()).get(&15),
        Some(&vec![(day(), OPEN_0915)])
    );
    let quarter = [
        bar(9, 15, 1.0, 1.0, 1.0, 1.0, 1),
        bar(9, 30, 1.0, 1.0, 1.0, 1.0, 1),
    ];
    assert!(plan(&quarter, 15, &opens()).is_empty());
    let pre_open = [
        bar(9, 8, 1.0, 1.0, 1.0, 1.0, 1),
        bar(9, 15, 1.0, 1.0, 1.0, 1.0, 1),
    ];
    assert_eq!(
        plan(&pre_open, 15, &opens()).get(&0),
        Some(&vec![(day(), OPEN_0915)])
    );
    assert!(plan(&hourly, 60, &HashMap::new()).is_empty());
    // Muhurat: a 14:20 open takes 5m bars for 15m, 1m for 3m.
    let muhurat = HashMap::from([(day(), 14 * 60 + 20)]);
    assert_eq!(
        plan(&quarter, 15, &muhurat)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [5]
    );
    assert_eq!(
        plan(&quarter, 3, &muhurat)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [1]
    );
}

/// 15m bars grouped into hours counted from 09:15: first open, highest high,
/// lowest low, last close, summed volume; a pre-open bar joins the first.
#[test]
fn bars_group_into_buckets_from_the_open() {
    let bars = [
        bar(9, 8, 99.0, 99.5, 98.0, 99.0, 5),
        bar(9, 15, 100.0, 101.0, 99.0, 100.5, 10),
        bar(9, 30, 100.5, 103.0, 100.0, 102.0, 20),
        bar(10, 0, 102.0, 102.5, 97.0, 98.0, 30),
        bar(10, 15, 98.0, 99.0, 96.0, 97.5, 40),
    ];
    let out = bucket_day(&bars, day(), OPEN_0915, 60);
    assert_eq!(out.len(), 2);
    let first = &out[0];
    assert_eq!(first.timestamp, DAY + 9 * 3600 + 15 * 60);
    assert_eq!(
        (first.open, first.high, first.low, first.close, first.volume),
        (99.0, 103.0, 97.0, 98.0, 65)
    );
    let second = &out[1];
    assert_eq!(second.timestamp, DAY + 10 * 3600 + 15 * 60);
    assert_eq!((second.open, second.close, second.volume), (98.0, 97.5, 40));
}

/// Through the history service (`market_data_service::broker_candles`), on
/// the test calendar: a broker that asks has its 1h bars rebuilt from 15m
/// bars on Monday's 09:15 NSE open, Sunday (no NSE session) keeps the
/// broker's bars, and a broker that does not ask keeps all of them.
#[tokio::test]
async fn history_moves_a_clock_grid_broker_onto_the_session_grid() {
    use crate::brokers::common::symbols::tests::row;
    use crate::brokers::mock::{MockBroker, MockCall};
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::services::market_data_service::history;
    use std::sync::Arc;
    let mock = Arc::new(MockBroker::new("nuvama"));
    let t = crate::state::testing::build(
        BrokerRegistry::with(vec![mock.clone() as Arc<dyn Broker>]),
        chrono::Utc::now(),
    );
    let ctx = &t.ctx;
    ctx.load_symbol_cache(vec![row("RELIANCE", "RELIANCE", "NSE", "2885")]);
    ctx.set_broker_session(Some(crate::state::BrokerSession {
        broker_id: "nuvama".into(),
        auth_token: crate::security::Secret::new("t"),
        feed_token: None,
        user_id: "AB1".into(),
        user_name: None,
        authenticated_at: ctx.now(),
    }));
    let sunday = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let sunday_bar = bar(10 - 24, 0, 5.0, 5.0, 5.0, 5.0, 5);
    *mock.history_by_interval.lock() = HashMap::from([
        (
            "1h".to_string(),
            vec![
                sunday_bar,
                bar(9, 0, 1.0, 1.0, 1.0, 1.0, 1),
                bar(10, 0, 1.0, 1.0, 1.0, 1.0, 1),
            ],
        ),
        (
            "15m".to_string(),
            vec![
                bar(9, 15, 100.0, 101.0, 99.0, 100.5, 10),
                bar(9, 30, 100.5, 103.0, 100.0, 102.0, 20),
                bar(10, 0, 102.0, 102.5, 97.0, 98.0, 30),
                bar(10, 15, 98.0, 99.0, 96.0, 97.5, 40),
            ],
        ),
    ]);
    let bars = |r: &crate::services::core::Reply| -> Vec<(i64, f64, f64, f64, f64, i64)> {
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["timestamp"].as_i64().unwrap(),
                    c["open"].as_f64().unwrap(),
                    c["high"].as_f64().unwrap(),
                    c["low"].as_f64().unwrap(),
                    c["close"].as_f64().unwrap(),
                    c["volume"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    let at = |hh: i64, mm: i64| DAY + hh * 3600 + mm * 60;
    let fifteen = || -> Vec<(NaiveDate, NaiveDate)> {
        mock.calls
            .lock()
            .iter()
            .filter_map(|c| match c {
                MockCall::History(r) if r.interval == "15m" => Some((r.start, r.end)),
                _ => None,
            })
            .collect()
    };

    *mock.session_grid.lock() = true;
    let r = history(ctx, "RELIANCE", "NSE", "1h", sunday, day(), "api").await;
    assert_eq!(
        bars(&r),
        [
            (sunday_bar.timestamp, 5.0, 5.0, 5.0, 5.0, 5),
            (at(9, 15), 100.0, 103.0, 97.0, 98.0, 60),
            (at(10, 15), 98.0, 99.0, 96.0, 97.5, 40),
        ]
    );
    assert_eq!(
        fifteen(),
        [(day(), day())],
        "only the day the calendar opens"
    );

    *mock.session_grid.lock() = false;
    let r = history(ctx, "RELIANCE", "NSE", "1h", sunday, day(), "api").await;
    assert_eq!(
        bars(&r).iter().map(|b| b.0).collect::<Vec<_>>(),
        [sunday_bar.timestamp, at(9, 0), at(10, 0)]
    );
    assert_eq!(fifteen().len(), 1, "no sub-request without the switch");
}
