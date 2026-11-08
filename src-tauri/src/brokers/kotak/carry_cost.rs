//! A delivery position carried overnight valued at what it cost (web
//! `_backfill_carry_cost` and `_holding_costs`, marketcalls/openalgo#2365,
//! #2367, eb4cce0a1).
//!
//! Kotak values every overnight leg at the previous day's settlement
//! (`cfBuyAmt` / `cfSellAmt`), so without its cost the average and P&L of
//! a delivery position carried overnight measure from yesterday's close.
//! The cost is in Holdings: Kotak's Positions docs say the Neo app takes it
//! from Holdings' `averagePrice`, matched on `positions.tok ==
//! holdings.exchangeIdentifier`, and its SDK's P&L code does the same.
//! Holdings covers the cash segments only; a carried F&O leg has no cost
//! anywhere in Kotak's API and keeps the carry-forward valuation (#2271).
//!
//! Holdings is read only when a carried cash row needs it, at most every
//! five minutes per session (the positions book is polled by the page and
//! by every smart order, and a carried leg's cost does not move intraday),
//! and best effort: a failed read leaves the rows on the carry-forward
//! valuation, flagged as such, and is not remembered.

use super::mapping::{n, s, HOLDING_AVERAGE};
use super::orders::raw_holdings;
use super::{KotakBroker, KotakSession};
use crate::brokers::types::AuthToken;
use crate::cache::TtlCache;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The cash segments, the only ones Holdings covers.
const HOLDING_SEGMENTS: &[&str] = &["nse_cm", "bse_cm"];

/// Holdings averagePrice by exchange token, per session (its `sid`): at
/// most a few sessions, five minutes each, cleared at logout. Adapter
/// state, like the feed URL (`docs/port/no-keychain.md`, caches).
pub(crate) struct HoldingCosts(TtlCache<String, HashMap<String, f64>>);

impl Default for HoldingCosts {
    fn default() -> Self {
        Self(TtlCache::new(8, Duration::from_secs(300)))
    }
}

impl HoldingCosts {
    pub(crate) fn clear(&self) {
        self.0.clear();
    }
}

/// Whether a position row is a carried cash-segment leg that Kotak sent
/// without an uploaded price (`upldPrc`, which wins when present).
fn needs_cost(p: &Value) -> bool {
    HOLDING_SEGMENTS.contains(&s(p, "exSeg").as_str())
        && !s(p, "tok").is_empty()
        && (n(p, "cfBuyQty") != 0.0 || n(p, "cfSellQty") != 0.0)
        && n(p, "upldPrc") <= 0.0
}

/// Holdings averagePrice keyed by `exchangeIdentifier`, from the rows of
/// a holdings read (a zero or unreadable price is left out).
fn costs_of(holdings: &[Value]) -> HashMap<String, f64> {
    holdings
        .iter()
        .filter_map(|h| {
            let token = s(h, "exchangeIdentifier");
            let price = n(h, "averagePrice");
            (!token.is_empty() && price > 0.0).then_some((token, price))
        })
        .collect()
}

/// Stamp each carried cash-segment row of `rows` with its Holdings average
/// price (`mapping::HOLDING_AVERAGE`), which the position mapping prefers
/// to Kotak's carry-forward valuation.
pub(crate) async fn backfill(b: &KotakBroker, auth: &AuthToken, rows: &mut [Value]) {
    if !rows.iter().any(needs_cost) {
        return;
    }
    let Ok(session) = KotakSession::parse(auth) else {
        return;
    };
    let Some(costs) = holding_costs(b, auth, &session).await else {
        return;
    };
    for p in rows.iter_mut().filter(|p| needs_cost(p)) {
        if let Some(cost) = costs.get(&s(p, "tok")) {
            p[HOLDING_AVERAGE] = Value::from(*cost);
        }
    }
}

/// This session's Holdings costs, read once per five minutes; `None` when
/// Holdings cannot be read (nothing is remembered then).
async fn holding_costs(
    b: &KotakBroker,
    auth: &AuthToken,
    session: &KotakSession,
) -> Option<HashMap<String, f64>> {
    let (cached, generation) = b
        .holding_costs
        .0
        .get_with_generation(&session.sid, Instant::now());
    if cached.is_some() {
        return cached;
    }
    match raw_holdings(b, auth).await {
        Ok(rows) => {
            let costs = costs_of(&rows);
            // A logout that cleared the cache meanwhile wins: the late
            // answer is not kept (`TtlCache::fill`).
            b.holding_costs.0.fill(
                session.sid.clone(),
                costs.clone(),
                generation,
                Instant::now(),
            );
            Some(costs)
        }
        Err(e) => {
            tracing::warn!(
                "Could not read holdings cost for carried Kotak positions: {}",
                e.code()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
