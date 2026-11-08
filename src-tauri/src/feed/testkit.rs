//! The 8765 frames a broker feed's events make, for the brokers' feed
//! tests: built as the bridge builds them (a depth on the instrument's last
//! tick) and delivered as the registry delivers them.

use super::bridge::{depth_update, tick_update};
use super::protocol::market_data;
use super::{InstrumentKey, Mode};
use crate::brokers::common::streaming::{FeedEvent, NormalizedTick};
use serde_json::Value;
use std::collections::HashMap;

/// One broker's frames, with the last tick per instrument carried from
/// one call to the next, as the bridge carries it.
pub(crate) struct Frames {
    broker: &'static str,
    depth: u8,
    last: HashMap<InstrumentKey, NormalizedTick>,
}

impl Frames {
    pub(crate) fn new(broker: &'static str) -> Self {
        Self {
            broker,
            depth: 5,
            last: HashMap::new(),
        }
    }

    /// The `data` of every frame a subscriber at `mode` receives for
    /// `events`, in order.
    pub(crate) fn at(&mut self, events: &[FeedEvent], mode: Mode) -> Vec<Value> {
        let mut out = Vec::new();
        for e in events {
            let update = match e {
                FeedEvent::Tick(t) => {
                    let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
                    self.last.insert(key.clone(), t.clone());
                    tick_update(key, t)
                }
                FeedEvent::Depth(d) => {
                    let key = InstrumentKey::new(d.symbol.clone(), d.exchange.clone());
                    let last = self.last.get(&key);
                    depth_update(key, d, last)
                }
                _ => continue,
            };
            // `feed::registry`: a mode above the update's, or another mode
            // than an exact one's, is not served.
            if mode > update.mode || (update.exact_mode && mode != update.mode) {
                continue;
            }
            let frame = market_data(&update, mode, self.depth, self.broker);
            let v: Value = serde_json::from_str(&frame).unwrap_or_default();
            out.push(v["data"].clone());
        }
        out
    }
}

/// The keys of a frame's `data`, sorted.
pub(crate) fn keys(data: &Value) -> Vec<String> {
    let mut k: Vec<String> = data
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    k.sort();
    k
}
