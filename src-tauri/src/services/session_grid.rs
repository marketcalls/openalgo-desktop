//! Intraday bars moved from a broker's clock grid onto the exchange
//! session's (web nuvama `_session_align`), for a broker whose
//! `Broker::history_on_session_grid` says its bars need it.
//!
//! Nuvama's bars are clock-aligned: its 1h bars start 09:00, 10:00, and its
//! 15m bars on Muhurat day (session 14:20) start 14:00, 14:15. The
//! `/trading` chart refuses a whole history when one source bar is off its
//! session grid ("Unknown source candle boundaries") while deriving 45m,
//! 2h, 3h and 4h. Each day whose session open (from the market calendar, so
//! no session time is assumed) is not a multiple of the bar length is
//! rebuilt from the coarsest broker interval that fits both the bar length
//! and the open (15m for 30m and 1h on a 09:15 open, 5m or 1m on a special
//! session), in buckets counted from the open. Bars before the open (the
//! pre-open match on special sessions) fold into the first bucket on every
//! interval, 1m included. Days already on the grid with nothing before the
//! open, and days the calendar does not cover, keep the broker's bars.

use crate::brokers::common::history::{intraday_minutes, ist_day, session_exchange, sort_dedupe};
use crate::brokers::types::{Candle, HistoryRequest};
use crate::db::sqlite::market_calendar as cal;
use crate::error::{AppError, Result};
use crate::services::core::BrokerHandle;
use crate::state::AppState;
use chrono::{DateTime, NaiveDate, Timelike};
use chrono_tz::Asia::Kolkata;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// `day`'s IST midnight plus `open_minute`, epoch seconds.
pub(super) fn session_open_epoch(day: NaiveDate, open_minute: i64) -> i64 {
    cal::ist_midnight_ms(day) / 1000 + open_minute * 60
}

/// Where each day is rebuilt from: `0` for the bars in hand (only pre-open
/// bars to fold), else the source interval in minutes (web `rebuild`).
pub fn plan(
    candles: &[Candle],
    step: i64,
    opens: &HashMap<NaiveDate, i64>,
) -> BTreeMap<i64, Vec<(NaiveDate, i64)>> {
    let mut first: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for c in candles {
        if let Some(d) = ist_day(c.timestamp) {
            let e = first.entry(d).or_insert(c.timestamp);
            *e = (*e).min(c.timestamp);
        }
    }
    let mut out: BTreeMap<i64, Vec<(NaiveDate, i64)>> = BTreeMap::new();
    for (day, day_first) in first {
        let Some(&open) = opens.get(&day) else {
            continue;
        };
        if open % step == 0 {
            if day_first < session_open_epoch(day, open) {
                out.entry(0).or_default().push((day, open));
            }
            continue;
        }
        let source = [15, 5, 3, 1]
            .into_iter()
            .find(|m| *m < step && step % m == 0 && open % m == 0)
            .unwrap_or(1);
        out.entry(source).or_default().push((day, open));
    }
    out
}

/// One day's `bars` grouped into `step`-minute buckets counted from the
/// session open; a bar before the open falls in the first.
pub fn bucket_day(bars: &[Candle], day: NaiveDate, open: i64, step: i64) -> Vec<Candle> {
    let start = session_open_epoch(day, open);
    let span = step * 60;
    let mut buckets: BTreeMap<i64, Candle> = BTreeMap::new();
    for b in bars {
        let key = start + (b.timestamp.max(start) - start).div_euclid(span) * span;
        buckets
            .entry(key)
            .and_modify(|c| {
                c.high = c.high.max(b.high);
                c.low = c.low.min(b.low);
                c.close = b.close;
                c.volume += b.volume;
            })
            .or_insert(Candle {
                timestamp: key,
                oi: 0,
                ..*b
            });
    }
    buckets.into_values().collect()
}

/// The minute of the IST day `exchange` opens on each of `days`, from the
/// market calendar (read off the runtime). A day the calendar has no
/// session for is left out, and a calendar that cannot be read leaves out
/// every day, so the bars stay as the broker sent them.
pub(super) async fn session_opens(
    ctx: &AppState,
    exchange: &str,
    days: BTreeSet<NaiveDate>,
) -> HashMap<NaiveDate, i64> {
    let db = ctx.sqlite.clone();
    let exchange = exchange.to_string();
    let read = tokio::task::spawn_blocking(move || -> Result<HashMap<NaiveDate, i64>> {
        let conn = db.conn()?;
        let mut out = HashMap::new();
        for day in days {
            let start = cal::timings_for_date(&conn, day)?
                .into_iter()
                .filter(|w| w.exchange == exchange)
                .map(|w| w.start_time)
                .min();
            let Some(ms) = start else { continue };
            if let Some(t) = DateTime::from_timestamp_millis(ms) {
                let t = t.with_timezone(&Kolkata);
                out.insert(day, i64::from(t.hour()) * 60 + i64::from(t.minute()));
            }
        }
        Ok(out)
    })
    .await;
    match read {
        Ok(Ok(opens)) => opens,
        Ok(Err(e)) => {
            tracing::warn!(
                "Market calendar could not be read; history bars kept as the broker sent them: {}",
                e.code()
            );
            HashMap::new()
        }
        Err(_) => HashMap::new(),
    }
}

/// `candles` (the broker's bars for `req`) on the session grid.
pub async fn align(
    ctx: &AppState,
    h: &BrokerHandle,
    req: &HistoryRequest,
    candles: Vec<Candle>,
) -> Result<Vec<Candle>> {
    let Some(step) = intraday_minutes(&req.interval) else {
        return Ok(candles);
    };
    let days: BTreeSet<NaiveDate> = candles
        .iter()
        .filter_map(|c| ist_day(c.timestamp))
        .collect();
    let opens = session_opens(ctx, session_exchange(&req.key.exchange), days).await;
    let plan = plan(&candles, step, &opens);
    if plan.is_empty() {
        return Ok(candles);
    }
    let mut rebuilt = Vec::new();
    let mut rebuilt_days = HashSet::new();
    for (source, day_opens) in plan {
        let (Some(first), Some(last)) = (day_opens.first(), day_opens.last()) else {
            continue;
        };
        let src = if source == 0 {
            candles.clone()
        } else {
            let sub = HistoryRequest {
                interval: format!("{}m", source),
                start: first.0,
                end: last.0,
                key: req.key.clone(),
            };
            h.broker
                .get_history(&h.auth, &sub)
                .await
                .map_err(|e| match e {
                    AppError::Validation(m) => AppError::Broker(m),
                    other => other,
                })?
        };
        for (day, open) in day_opens {
            let bars: Vec<Candle> = src
                .iter()
                .filter(|c| ist_day(c.timestamp) == Some(day))
                .cloned()
                .collect();
            if bars.is_empty() {
                continue;
            }
            rebuilt.extend(bucket_day(&bars, day, open, step));
            rebuilt_days.insert(day);
        }
    }
    let kept = candles
        .into_iter()
        .filter(|c| ist_day(c.timestamp).is_none_or(|d| !rebuilt_days.contains(&d)));
    Ok(sort_dedupe(kept.chain(rebuilt).collect()))
}

#[cfg(test)]
mod tests;
