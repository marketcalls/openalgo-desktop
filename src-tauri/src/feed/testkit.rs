//! The 8765 frames a broker feed's events make, for the brokers' feed
//! tests: built as the bridge builds them (each tick merged into the
//! instrument's snapshot, a depth on that snapshot, a full tick and the book
//! right behind it as one update) and delivered as the registry delivers
//! them.

use super::bridge::pairing;
use super::bridge::snapshot::{self, LastBook, Snapshot};
use super::bridge::{depth_update, snapshot_update};
use super::protocol::{market_data, reaches};
use super::{InstrumentKey, MarketUpdate, Mode};
use crate::brokers::common::streaming::FeedEvent;
use serde_json::Value;
use std::collections::HashMap;

/// One broker's frames, with the snapshot per instrument carried from one
/// call to the next, as the bridge carries it within one connection.
pub(crate) struct Frames {
    broker: &'static str,
    depth: u8,
    last: HashMap<InstrumentKey, Snapshot>,
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
        let mut events = events.iter().peekable();
        while let Some(e) = events.next() {
            // `bridge::pairing`: a full tick and the book that follows it.
            let book = pairing::awaits_book(e).and_then(|t| {
                events.next_if(|next| {
                    matches!(next, FeedEvent::Depth(d) if d.symbol == t.symbol && d.exchange == t.exchange)
                })
            });
            if let (FeedEvent::Tick(t), Some(FeedEvent::Depth(d))) = (e, book) {
                let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
                let merged = snapshot::merge(self.last.get(&key), t, 0);
                let both = depth_update(key.clone(), d, Some(merged.tick()), merged.book_lacks());
                self.last.insert(key.clone(), merged);
                if let Some(book) = LastBook::of(&both) {
                    snapshot::keep_book(&mut self.last, &key, book, 0);
                }
                self.push(&mut out, both_modes(both), mode);
                continue;
            }
            let update = match e {
                FeedEvent::Tick(t) => {
                    let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
                    let merged = snapshot::merge(self.last.get(&key), t, 0);
                    let update = snapshot_update(key.clone(), &merged);
                    self.last.insert(key, merged);
                    match update {
                        Some(u) => u,
                        None => continue,
                    }
                }
                FeedEvent::Depth(d) => {
                    let key = InstrumentKey::new(d.symbol.clone(), d.exchange.clone());
                    let last = self.last.get(&key);
                    let lacks = last.map(Snapshot::book_lacks).unwrap_or_default();
                    let u = depth_update(key.clone(), d, last.map(Snapshot::tick), lacks);
                    if let Some(book) = LastBook::of(&u) {
                        snapshot::keep_book(&mut self.last, &key, book, 0);
                    }
                    u
                }
                _ => continue,
            };
            self.push(&mut out, update, mode);
        }
        out
    }

    /// `update`'s frame for a subscriber at `mode`, if `feed::registry`
    /// serves it one (`protocol::reaches`).
    fn push(&self, out: &mut Vec<Value>, update: MarketUpdate, mode: Mode) {
        if !reaches(&update, mode, self.broker) {
            return;
        }
        let frame = market_data(&update, mode, self.depth, self.broker);
        let v: Value = serde_json::from_str(&frame).unwrap_or_default();
        out.push(v["data"].clone());
    }
}

/// A paired update reaches every mode (`bridge::pairing`).
fn both_modes(u: MarketUpdate) -> MarketUpdate {
    MarketUpdate {
        exact_mode: false,
        ..u
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
