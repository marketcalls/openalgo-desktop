//! Each session's history opening at the pre-open discovered price, for a
//! broker whose intraday bars include the pre-open
//! (`Broker::history_pre_open_minutes`; web flattrade
//! `_open_at_discovered_price`, openalgo#2482).
//!
//! Flattrade's TPSeries returns bars from before the session opens: a flat
//! 09:14 bar on a normal day, and on Muhurat 2026 (session 14:20) 14:05 to
//! 14:19, the previous close as a placeholder and then the discovered
//! price, some stamped with seconds (14:14:05). None is a trade in the
//! session, and the `/trading` chart refuses a whole page over one bar
//! before the open ("Could not load older history"). The discovered price
//! is the exchange's opening price, where the broker's daily candle opens,
//! so each day's first session bar opens at the last pre-open close, its
//! high or low widened to hold it, and the pre-open bars are left out
//! (their volume too: the matched quantity cannot be told from the
//! placeholders'). A bar that starts before the open but runs past it holds
//! session trades, so it moves to the open. The open comes from the market
//! calendar, special sessions included; a day the calendar has no session
//! for, or a calendar that cannot be read, keeps every bar as sent.

use super::session_grid::{session_open_epoch, session_opens};
use crate::brokers::common::history::{ist_day, session_exchange};
use crate::brokers::types::{Candle, HistoryRequest};
use crate::state::AppState;
use chrono::NaiveDate;
use std::collections::{BTreeSet, HashMap, HashSet};

/// `candles` (the broker's bars for `req`, `bar_minutes` long, oldest
/// first) with each session opening at its discovered price, the session
/// opens read from the market calendar.
pub(crate) async fn open_sessions(
    ctx: &AppState,
    req: &HistoryRequest,
    bar_minutes: i64,
    candles: Vec<Candle>,
) -> Vec<Candle> {
    let days: BTreeSet<NaiveDate> = candles
        .iter()
        .filter_map(|c| ist_day(c.timestamp))
        .collect();
    if days.is_empty() {
        return candles;
    }
    let opens: HashMap<NaiveDate, i64> =
        session_opens(ctx, session_exchange(&req.key.exchange), days)
            .await
            .into_iter()
            .map(|(day, minute)| (day, session_open_epoch(day, minute)))
            .collect();
    open_at_discovered_price(candles, bar_minutes, &opens)
}

/// `candles` (`bar_minutes` long, oldest first) with each day in `opens`
/// (its session open, epoch seconds) opening at the last pre-open close and
/// without its pre-open bars; other days as sent.
fn open_at_discovered_price(
    candles: Vec<Candle>,
    bar_minutes: i64,
    opens: &HashMap<NaiveDate, i64>,
) -> Vec<Candle> {
    let span = bar_minutes.saturating_mul(60);
    // The last pre-open bar of each day: its time and its close.
    let mut discovered: HashMap<NaiveDate, (i64, f64)> = HashMap::new();
    // Session bars with the time the broker gave them.
    let mut session: Vec<(i64, Candle)> = Vec::with_capacity(candles.len());
    for c in candles {
        let day_open = ist_day(c.timestamp).and_then(|d| opens.get(&d).map(|&o| (d, o)));
        match day_open {
            Some((day, open)) if c.timestamp < open => {
                if c.timestamp.saturating_add(span) <= open {
                    let last = discovered.entry(day).or_insert((c.timestamp, c.close));
                    if c.timestamp > last.0 {
                        *last = (c.timestamp, c.close);
                    }
                } else {
                    session.push((
                        c.timestamp,
                        Candle {
                            timestamp: open,
                            ..c
                        },
                    ));
                }
            }
            _ => session.push((c.timestamp, c)),
        }
    }
    session.sort_by_key(|(sent, c)| (c.timestamp, *sent));
    let mut out = merge_same_time(session.into_iter().map(|(_, c)| c));
    open_at(&mut out, &discovered);
    out
}

/// Bars that share a time folded into one, in the order they traded: a bar
/// moved to the open can meet the one the broker already put there.
fn merge_same_time(bars: impl Iterator<Item = Candle>) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for b in bars {
        match out.last_mut() {
            Some(m) if m.timestamp == b.timestamp => {
                m.high = m.high.max(b.high);
                m.low = m.low.min(b.low);
                m.close = b.close;
                m.volume = m.volume.saturating_add(b.volume);
                m.oi = b.oi;
            }
            _ => out.push(b),
        }
    }
    out
}

/// Each day's first bar (of `bars`, oldest first) opens at the day's
/// discovered price, its high or low widened to hold it.
fn open_at(bars: &mut [Candle], discovered: &HashMap<NaiveDate, (i64, f64)>) {
    let mut first_seen = HashSet::new();
    for b in bars.iter_mut() {
        let Some(day) = ist_day(b.timestamp) else {
            continue;
        };
        if !first_seen.insert(day) {
            continue;
        }
        if let Some(&(_, price)) = discovered.get(&day) {
            b.open = price;
            b.high = b.high.max(price);
            b.low = b.low.min(price);
        }
    }
}

#[cfg(test)]
mod tests;
