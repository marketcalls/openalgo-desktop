//! One broker packet, one update (audit C08). A packet that carries a full
//! quote and its book reaches the bridge as a `Tick` and a `Depth`, back to
//! back: the manager sends a packet's events together, with nothing in
//! between. When the bridge finds the book already waiting behind its tick,
//! it sends the two on as one Depth update for every mode a client holds,
//! as the web's adapter publishes such a packet once. Each frame is then
//! shaped by the broker's dialect: trimmed to the holder's mode (Zerodha, as
//! recorded) or carrying the whole packet (`Fold::Richer`: Arrow, Paytm, the
//! XTS family, ...).
//!
//! A book in a packet of its own goes out on its own, to Depth holders
//! only, so a packet is never sent twice to the same holder. The manager
//! sends a broker message's events as one packet (`websocket::Packet`), so
//! which events pair never depends on what happens to be queued when the
//! bridge looks, nor on another connection's events in between (audit
//! R107-17).

use crate::brokers::common::streaming::{FeedEvent, NormalizedTick, QuoteField};
use crate::feed::source::InstrumentKey;

/// Whether `ev` is a full tick of an instrument with a book (an index has
/// none), whose book may follow it. A tick without a traded price is not
/// one: it only updates the snapshot, and the book behind it reaches Depth
/// holders alone (Groww's book message, web groww_adapter.py:966-967).
pub(in crate::feed) fn awaits_book(ev: &FeedEvent) -> Option<&NormalizedTick> {
    match ev {
        FeedEvent::Tick(t) if t.mode >= 3 && t.knows(QuoteField::Ltp) => {
            let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
            (!key.is_index()).then_some(t)
        }
        _ => None,
    }
}

/// One step of a broker packet: an event on its own, or a full tick and
/// its book.
pub(in crate::feed) enum Step<'a> {
    One(&'a FeedEvent),
    Pair(&'a FeedEvent, &'a FeedEvent),
}

/// A packet's events in order, each full tick paired with the book of its
/// instrument right behind it in the same packet.
pub(in crate::feed) fn steps<'a>(events: &[&'a FeedEvent]) -> Vec<Step<'a>> {
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        let ev = events[i];
        let book = awaits_book(ev).and_then(|t| {
            events.get(i + 1).copied().filter(|next| {
                matches!(next, FeedEvent::Depth(d) if d.symbol == t.symbol && d.exchange == t.exchange)
            })
        });
        match book {
            Some(book) => {
                out.push(Step::Pair(ev, book));
                i += 2;
            }
            None => {
                out.push(Step::One(ev));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
