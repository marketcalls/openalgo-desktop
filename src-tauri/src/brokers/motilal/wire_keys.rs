//! Which instrument holds each wire key of Motilal's broadcast feed.
//!
//! A broadcast packet names only its exchange character and scrip code
//! (bytes 0-4 of the 30-byte packet, web `motilal_websocket.py:456-484`), so
//! NSE cash and NSE F&O share `('N', scrip)`: the segment travels only in
//! the register packet (`=cHcciB`, byte 4), and the web says the two
//! "cannot be told apart" (`:105-114`, `:952-960`). A cash and an F&O
//! contract with the same code would then show each other's prices. One
//! instrument holds a wire key; a second one is refused when it is
//! subscribed, in words a trader can act on (the web registers both and
//! mixes their prices).
//!
//! The subscribe step ([`WireKeys::claim`], `Broker::feed_presubscribe`)
//! checks and reserves in one hold of the lock (CLAUDE.md: claim under the
//! same lock that checks), so two colliding subscribes arriving together,
//! in one request or from two clients, cannot both pass. The feed marks a
//! reservation registered when it registers the instrument and frees it
//! when it lets go (or hands it to the instrument still sharing it). A key
//! follows who holds its instrument: a release no socket carries (the feed
//! is not connected) never reaches the feed, so the manager says so
//! ([`WireKeys::let_go`], `Broker::feed_let_go`) and the key is free at
//! once. The map is bounded by the registered instruments plus
//! reservations younger than [`RESERVATION_TTL`]: a reservation that never
//! became a registration (its subscribe was refused for room afterwards)
//! is dropped by the next claim once it is older than that, and claiming
//! an instrument again replaces its own reservation.

use crate::brokers::common::streaming::{FeedLetGo, FeedSubscription, Presubscribe};
use futures_util::FutureExt;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a reservation waits for its registration. The manager places
/// a claimed instrument at once; a feed that is reconnecting registers it
/// when its socket is back.
pub(crate) const RESERVATION_TTL: Duration = Duration::from_secs(60);

/// A wire key: the exchange character and the scrip code.
pub(crate) type WireKey = (u8, i32);

/// An OpenAlgo instrument: symbol and exchange.
type Instrument = (String, String);

#[derive(Debug, Clone)]
struct Holder {
    instrument: Instrument,
    /// `None` once the feed registered it; else when it was reserved.
    reserved_at: Option<Instant>,
}

/// The wire keys of one broker session, shared by its subscribe step and
/// its feeds.
#[derive(Debug, Clone, Default)]
pub(crate) struct WireKeys(Arc<Mutex<HashMap<WireKey, Holder>>>);

/// The wire key a subscription registers on, or none when its scrip code
/// is not numeric (the feed skips it).
fn wire_key(s: &FeedSubscription) -> Option<WireKey> {
    let scrip = s.token.trim().parse::<i32>().ok()?;
    let exchange = super::streaming::feed_exchange(&s.exchange);
    Some((super::streaming::exchange_char(exchange), scrip))
}

fn instrument(s: &FeedSubscription) -> Instrument {
    (s.symbol.clone(), s.exchange.clone())
}

impl WireKeys {
    /// The session's subscribe step (`Broker::feed_presubscribe`).
    pub(crate) fn step(&self) -> Presubscribe {
        let keys = self.clone();
        Arc::new(move |subs: Vec<FeedSubscription>| {
            futures_util::future::ready(keys.claim(subs)).boxed()
        })
    }

    /// What the manager calls once no consumer holds an instrument
    /// (`Broker::feed_let_go`).
    pub(crate) fn let_go_step(&self) -> FeedLetGo {
        let keys = self.clone();
        Arc::new(move |subs: &[FeedSubscription]| keys.let_go(subs))
    }

    /// No consumer holds these any more: each one's key is free at once,
    /// its reservation too. A key another instrument holds or reserved
    /// stays with it.
    pub(crate) fn let_go(&self, subs: &[FeedSubscription]) {
        for s in subs {
            if let Some(key) = wire_key(s) {
                self.released(key, &s.symbol, &s.exchange);
            }
        }
    }

    /// The subscribe step: each new instrument's wire key checked and
    /// reserved, or refused when another instrument holds it.
    pub(crate) fn claim(
        &self,
        subs: Vec<FeedSubscription>,
    ) -> Vec<Result<FeedSubscription, String>> {
        self.claim_at(subs, Instant::now())
    }

    pub(crate) fn claim_at(
        &self,
        subs: Vec<FeedSubscription>,
        now: Instant,
    ) -> Vec<Result<FeedSubscription, String>> {
        let mut map = self.0.lock();
        map.retain(|_, h| {
            h.reserved_at
                .is_none_or(|at| now.saturating_duration_since(at) < RESERVATION_TTL)
        });
        subs.into_iter()
            .map(|s| {
                let Some(key) = wire_key(&s) else {
                    return Ok(s);
                };
                let mine = instrument(&s);
                match map.get(&key) {
                    Some(h) if h.instrument != mine => Err(refusal(&s, &h.instrument)),
                    Some(h) if h.reserved_at.is_none() => Ok(s),
                    _ => {
                        map.insert(
                            key,
                            Holder {
                                instrument: mine,
                                reserved_at: Some(now),
                            },
                        );
                        Ok(s)
                    }
                }
            })
            .collect()
    }

    /// The feed registered `symbol` on `exchange` on `key`: the key is its
    /// own until it lets go, unless another instrument registered on it
    /// first (its packets go to that one, `FeedState::register`).
    pub(crate) fn registered(&self, key: WireKey, symbol: &str, exchange: &str) {
        let mine = (symbol.to_string(), exchange.to_string());
        let mut map = self.0.lock();
        if map
            .get(&key)
            .is_some_and(|h| h.reserved_at.is_none() && h.instrument != mine)
        {
            return;
        }
        map.insert(
            key,
            Holder {
                instrument: mine,
                reserved_at: None,
            },
        );
    }

    /// The feed let go of `symbol` on `exchange`: `key` is free, unless
    /// another instrument holds it.
    pub(crate) fn released(&self, key: WireKey, symbol: &str, exchange: &str) {
        let mut map = self.0.lock();
        if map
            .get(&key)
            .is_some_and(|h| h.instrument.0 == symbol && h.instrument.1 == exchange)
        {
            map.remove(&key);
        }
    }

    /// Wire keys held or reserved.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock().len()
    }
}

/// Why `s` cannot stream beside `holder`, for the trader.
fn refusal(s: &FeedSubscription, holder: &Instrument) -> String {
    format!(
        "{} on {} cannot be streamed at the same time as {} on {}: Motilal sends both under one code",
        s.symbol, s.exchange, holder.0, holder.1
    )
}
