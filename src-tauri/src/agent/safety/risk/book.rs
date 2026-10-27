//! One Agent session's order count and duplicate registry (the web's
//! `RiskGuard` state: `_orders_claimed`, `_recent`, `_claims`), and the
//! claim made on them (rules 8 and 9).
//!
//! Pure: the caller loads the book, the check changes it, the caller saves
//! it, all inside one write transaction (`super::super::counters`), which
//! is the "one hold of one lock" the web takes: a second approval racing
//! the first cannot pass the same cap or slip through the same window.
//!
//! Desktop difference (D-003): the web keeps this in memory, so a restart
//! forgot the cap and the window; here it is persisted.

use super::intent::Intent;
use super::num::one_decimal;
use super::{blocked, RiskCode, Verdict};
use crate::agent::safety::limits::RiskLimits;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// How many fingerprints and pending claims a book keeps (the web's
/// `_MAX_TRACKED_FINGERPRINTS` and `_MAX_PENDING_CLAIMS`).
pub const MAX_TRACKED_FINGERPRINTS: usize = 512;
pub const MAX_PENDING_CLAIMS: usize = 256;

/// An order the guard allowed and has not yet seen dispatched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub fingerprints: Vec<String>,
    /// The stamp each fingerprint had before the claim, so a release
    /// restores it rather than erasing a real duplicate.
    pub previous: BTreeMap<String, DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// One session's counters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub orders_claimed: i64,
    /// Fingerprint to the time its order was allowed.
    pub recent: BTreeMap<String, DateTime<Utc>>,
    /// Claim token to its claim.
    pub claims: BTreeMap<String, Claim>,
}

/// What the claim step decided.
pub enum Claimed {
    /// Allowed and claimed under this token.
    Token(String),
    /// Allowed, nothing claimed (a dry run).
    DryRun,
}

impl Book {
    /// Rules 8 and 9 and the claim itself (the web's `_claim`). `token` is
    /// the claim's identity, or `None` for a dry run that claims nothing.
    pub fn claim(
        &mut self,
        intents: &[Intent],
        limits: &RiskLimits,
        now: DateTime<Utc>,
        token: Option<&str>,
    ) -> Result<Claimed, Verdict> {
        let window = limits.duplicate_order_window_seconds;
        let needed = intents.len() as i64;
        if self.orders_claimed + needed > limits.max_orders_per_session {
            return Err(blocked(
                RiskCode::SessionCapReached,
                format!(
                    "This session has already claimed {} of its {} permitted orders and this request needs {} more. Start a new conversation or raise max_orders_per_session.",
                    self.orders_claimed, limits.max_orders_per_session, needed
                ),
                json!({
                    "orders_claimed": self.orders_claimed,
                    "orders_allowed": limits.max_orders_per_session,
                    "requested": needed,
                }),
            ));
        }
        if window > 0 {
            self.check_duplicates(intents, window, now)?;
        }
        let Some(token) = token else {
            return Ok(Claimed::DryRun);
        };
        let fingerprints: Vec<String> = intents.iter().map(Intent::fingerprint).collect();
        let previous = fingerprints
            .iter()
            .filter_map(|f| self.recent.get(f).map(|t| (f.clone(), *t)))
            .collect();
        for f in &fingerprints {
            self.recent.insert(f.clone(), now);
        }
        self.orders_claimed += needed;
        self.claims.insert(
            token.to_string(),
            Claim {
                fingerprints,
                previous,
                created_at: now,
            },
        );
        self.prune(now, window);
        Ok(Claimed::Token(token.to_string()))
    }

    fn check_duplicates(
        &self,
        intents: &[Intent],
        window: i64,
        now: DateTime<Utc>,
    ) -> Result<(), Verdict> {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (i, intent) in intents.iter().enumerate() {
            let index = i + 1;
            let fingerprint = intent.fingerprint();
            if let Some(earlier) = seen.get(&fingerprint) {
                return Err(blocked(
                    RiskCode::DuplicateOrder,
                    format!(
                        "Leg {} repeats leg {} exactly ({}). Send it once with the combined quantity if that is what you meant.",
                        index,
                        earlier,
                        intent.describe()
                    ),
                    json!({"symbol": intent.symbol, "leg": index, "duplicate_of": earlier}),
                ));
            }
            seen.insert(fingerprint.clone(), index);
            if let Some(previous) = self.recent.get(&fingerprint) {
                let age = seconds_between(*previous, now);
                if age >= 0.0 && age < window as f64 {
                    let (text, rounded) = one_decimal(age);
                    return Err(blocked(
                        RiskCode::DuplicateOrder,
                        format!(
                            "An identical order ({}) was allowed {}s ago, inside the {}s duplicate window. Wait, or change the order.",
                            intent.describe(),
                            text,
                            window
                        ),
                        json!({
                            "symbol": intent.symbol,
                            "seconds_ago": rounded,
                            "window_seconds": window,
                        }),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Undo a claim whose order was never sent (the web's `_rollback`):
    /// the count goes back and each fingerprint gets its earlier stamp.
    /// False when there is nothing to undo (released, committed, a dry
    /// run, or no claim at all).
    pub fn rollback(&mut self, token: Option<&str>) -> bool {
        let Some(record) = token.and_then(|t| self.claims.remove(t)) else {
            return false;
        };
        self.orders_claimed = (self.orders_claimed - record.fingerprints.len() as i64).max(0);
        for fingerprint in record.fingerprints {
            match record.previous.get(&fingerprint) {
                Some(stamp) => {
                    self.recent.insert(fingerprint, *stamp);
                }
                None => {
                    self.recent.remove(&fingerprint);
                }
            }
        }
        true
    }

    /// The order was dispatched: keep the count and the fingerprint, drop
    /// the record that would undo them.
    pub fn commit(&mut self, token: Option<&str>) -> bool {
        token.and_then(|t| self.claims.remove(t)).is_some()
    }

    /// Bound the registry and the pending claims: entries leave on age
    /// first, then the oldest on size (the web's `_prune`).
    fn prune(&mut self, now: DateTime<Utc>, window: i64) {
        if window > 0 {
            self.recent
                .retain(|_, stamp| seconds_between(*stamp, now) < window as f64);
        }
        if self.recent.len() > MAX_TRACKED_FINGERPRINTS {
            let mut by_age: Vec<(String, DateTime<Utc>)> =
                self.recent.iter().map(|(k, v)| (k.clone(), *v)).collect();
            by_age.sort_by_key(|(_, stamp)| *stamp);
            for (fingerprint, _) in by_age
                .iter()
                .take(self.recent.len() - MAX_TRACKED_FINGERPRINTS)
            {
                self.recent.remove(fingerprint);
            }
        }
        if self.claims.len() > MAX_PENDING_CLAIMS {
            let mut by_age: Vec<(String, DateTime<Utc>)> = self
                .claims
                .iter()
                .map(|(k, v)| (k.clone(), v.created_at))
                .collect();
            by_age.sort_by_key(|(_, created)| *created);
            for (token, _) in by_age.iter().take(self.claims.len() - MAX_PENDING_CLAIMS) {
                self.claims.remove(token);
            }
        }
    }
}

/// `(later - earlier).total_seconds()`, to the microsecond.
fn seconds_between(earlier: DateTime<Utc>, later: DateTime<Utc>) -> f64 {
    let micros = (later - earlier).num_microseconds().unwrap_or(i64::MAX);
    micros as f64 / 1_000_000.0
}
