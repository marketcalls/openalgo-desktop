//! Each trading day's last intraday bar rebuilt from 1m bars (web shoonya
//! `_day_end_repair_window` and `_rebuild_day_last_bars`,
//! marketcalls/openalgo#2343, #2360, 0e4a488cc).
//!
//! When one `TPSeries` request spans more than one trading day, Shoonya
//! folds the next trading day's first 1m bar into the last bar of each
//! day. On NSE that is the 09:14 pre-open bar: SBIN 5m 30-10-2026 15:25
//! came back with high = close = 965.00 (the 02-11 09:14 price) against
//! its own close of 961.85. Every interval from 3m to 4h does it, on NSE,
//! NSE_INDEX, BSE and MCX; 1m and daily bars are clean, and so is a
//! single-day request.
//!
//! Every other bar is Shoonya's own, so its bucketing and session times
//! are kept, special sessions included. The last bar of each date is
//! replaced by the 1m bars from its own timestamp to the end of that same
//! date (IST), which leaves out anything of the next day without assuming
//! a session open, close or trading calendar. Volume is the raw 1m sum,
//! which equals TPSeries' own bar volume. A day the 1m bars do not cover
//! keeps the bar Shoonya sent: no 1m bar at or after its last bar, or a
//! failed 1m chunk overlapping its span (a partial rebuild would be wrong).
//! A member opts in with `NorenConfig::day_end_repair` (Shoonya; the web
//! repairs no other Noren member).

use super::{fetch_chart, noren_interval, parse_history, sort_dedupe_last, Chart};
use crate::brokers::families::noren::transport::Session;
use crate::brokers::families::noren::NorenBroker;
use crate::brokers::types::{Candle, QuoteKey};
use chrono::{NaiveDate, TimeZone};
use chrono_tz::Asia::Kolkata;

/// The trading date (IST) of an epoch-second timestamp.
fn ist_date(ts: i64) -> Option<NaiveDate> {
    Kolkata
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.date_naive())
}

/// The 1m span the repair reads, or `None` when nothing needs it. Only a
/// day followed by another day in the same reply can carry that day's
/// first 1m bar, so the last date is never repaired and a reply covering
/// one date needs no 1m bars. The span runs from the first date's last
/// bar to just before the last date's first bar: bounded by the bars
/// Shoonya returned, not by the range asked for (five years asked against
/// about a year of intraday history reads a year of 1m bars).
fn repair_window(bars: &[Candle]) -> Option<(i64, i64)> {
    let dated: Vec<(NaiveDate, i64)> = bars
        .iter()
        .filter_map(|c| ist_date(c.timestamp).map(|d| (d, c.timestamp)))
        .collect();
    let first_day = dated.iter().map(|(d, _)| *d).min()?;
    let last_day = dated.iter().map(|(d, _)| *d).max()?;
    if first_day == last_day {
        return None;
    }
    let start = dated
        .iter()
        .filter(|(d, _)| *d == first_day)
        .map(|(_, t)| *t)
        .max()?;
    let end = dated
        .iter()
        .filter(|(d, _)| *d == last_day)
        .map(|(_, t)| *t)
        .min()?;
    Some((start, end - 1))
}

/// What [`rebuild_day_last_bars`] did: day-end bars whose prices changed,
/// and bars kept as sent because a failed 1m chunk overlaps them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Rebuilt {
    changed: usize,
    kept: usize,
}

/// Rebuild the last bar of each date in `bars` (sorted, oldest first) from
/// `minute` (sorted 1m bars), skipping any whose span overlaps a range in
/// `failed`.
fn rebuild_day_last_bars(bars: &mut [Candle], minute: &[Candle], failed: &[(i64, i64)]) -> Rebuilt {
    let mut done = Rebuilt::default();
    if bars.is_empty() || minute.is_empty() {
        return done;
    }
    for idx in day_last_indices(bars) {
        let last_ts = bars[idx].timestamp;
        // Its span ends just before the next bar Shoonya returned.
        let span_end = bars.get(idx + 1).map(|next| next.timestamp - 1);
        let hole = failed.iter().any(|&(start, end)| {
            end >= last_ts && span_end.is_none_or(|span_end| start <= span_end)
        });
        if hole {
            done.kept += 1;
            continue;
        }
        let day = ist_date(last_ts);
        let window: Vec<&Candle> = minute
            .iter()
            .filter(|m| m.timestamp >= last_ts && ist_date(m.timestamp) == day)
            .collect();
        let (Some(first), Some(last)) = (window.first(), window.last()) else {
            continue;
        };
        let rebuilt = Candle {
            timestamp: last_ts,
            open: first.open,
            high: window.iter().map(|m| m.high).fold(f64::MIN, f64::max),
            low: window.iter().map(|m| m.low).fold(f64::MAX, f64::min),
            close: last.close,
            volume: window.iter().map(|m| m.volume).sum(),
            oi: last.oi,
        };
        let bar = &mut bars[idx];
        if (bar.open, bar.high, bar.low, bar.close, bar.oi)
            != (
                rebuilt.open,
                rebuilt.high,
                rebuilt.low,
                rebuilt.close,
                rebuilt.oi,
            )
        {
            done.changed += 1;
        }
        *bar = rebuilt;
    }
    done
}

/// The index of each date's last bar in `bars` (sorted, oldest first).
fn day_last_indices(bars: &[Candle]) -> Vec<usize> {
    (0..bars.len())
        .filter(|&i| {
            let day = ist_date(bars[i].timestamp);
            day.is_some() && bars.get(i + 1).is_none_or(|n| ist_date(n.timestamp) != day)
        })
        .collect()
}

/// Fetch the 1m bars the repair reads and rebuild `bars` (sorted). Best
/// effort: when the 1m fetch fails the bars Shoonya sent are returned
/// unchanged, and the failure is logged.
pub(super) async fn repair(
    b: &NorenBroker,
    s: &Session,
    chart: &Chart<'_>,
    key: &QuoteKey,
    bars: &mut [Candle],
) {
    let Some(window) = repair_window(bars) else {
        return;
    };
    let Ok(minute_intrv) = noren_interval(b, "1m") else {
        return;
    };
    let minute_chart = Chart {
        intrv: minute_intrv,
        ..*chart
    };
    let read = match fetch_chart(b, s, &minute_chart, "1m", window).await {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Could not fetch 1m bars to repair the last bar of each day for {}:{}: {}",
                key.exchange,
                key.symbol,
                e.code()
            );
            return;
        }
    };
    let minute = sort_dedupe_last(parse_history(b.cfg, &read.rows, false, chart.exch));
    let done = rebuild_day_last_bars(bars, &minute, &read.failed);
    if done.changed > 0 {
        tracing::info!(
            broker = b.cfg.id,
            "Rebuilt {} day-end candles from 1m bars for {}:{}",
            done.changed,
            key.exchange,
            key.symbol
        );
    }
    if done.kept > 0 {
        tracing::warn!(
            broker = b.cfg.id,
            "Kept {} day-end candles as the broker sent them for {}:{}: the 1m bars covering them did not arrive",
            done.kept,
            key.exchange,
            key.symbol
        );
    }
}

#[cfg(test)]
mod tests;
