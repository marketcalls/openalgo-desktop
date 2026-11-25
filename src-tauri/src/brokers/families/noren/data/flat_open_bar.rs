//! The pre-open bar TPSeries opens a session with (web tradesmart
//! `_drop_preopen_bars`, marketcalls/openalgo#2475, 9a12bd0f7).
//!
//! On NSE and BSE cash and indices, 1m TPSeries opens each date with a bar
//! holding the pre-open discovered price: flat OHLC and zero volume
//! (RELIANCE 18-11-2026 09:14 at 1370.0, vol 0, before the 09:15 bar that
//! opens at 1367.0). It is not a trade, and the session's first candle is
//! the one after it (QA HS-07). F&O and MCX send no such bar.
//!
//! It is recognised by its shape, not a session time, so special sessions
//! are covered too: the first bar of an IST date that is flat, has zero
//! volume and is followed by more bars that date. Index bars always carry
//! zero volume, but their first trading bar is not flat, so it is kept.
//! Flattrade's pre-open bars follow its own web rule, not this one.

use super::day_end::ist_date;
use crate::brokers::families::noren::NorenConfig;
use crate::brokers::types::Candle;

/// One price all bar long, and nothing traded.
fn flat_without_volume(c: &Candle) -> bool {
    c.open == c.high && c.high == c.low && c.low == c.close && c.volume == 0
}

/// `bars` (sorted by time) without the pre-open bar opening each IST date.
pub(super) fn without_flat_open_bars(cfg: &NorenConfig, bars: Vec<Candle>) -> Vec<Candle> {
    let mut kept = Vec::with_capacity(bars.len());
    let mut previous_day = None;
    for (n, bar) in bars.iter().enumerate() {
        let day = ist_date(bar.timestamp);
        let opens_day = n == 0 || day != previous_day;
        previous_day = day;
        let more_that_day = bars
            .get(n + 1)
            .is_some_and(|next| ist_date(next.timestamp) == day);
        let pre_open = opens_day && more_that_day && flat_without_volume(bar);
        if !pre_open {
            kept.push(*bar);
        }
    }
    let dropped = bars.len() - kept.len();
    if dropped > 0 {
        tracing::debug!(
            broker = cfg.id,
            "Dropped {} pre-open bar(s) from TPSeries history",
            dropped
        );
    }
    kept
}

#[cfg(test)]
mod tests;
