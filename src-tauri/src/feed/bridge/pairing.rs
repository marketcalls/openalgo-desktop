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
//! A book not found waiting goes out on its own, to Depth holders only, as
//! before, so a packet is never sent twice to the same holder.

use crate::brokers::common::streaming::{FeedEvent, MarketEvent, NormalizedTick};
use crate::feed::source::InstrumentKey;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::broadcast::Receiver;

/// What waits behind a full tick on the broker feed.
pub(in crate::feed) enum Next {
    /// The tick's own book.
    Book(MarketEvent),
    /// Another event, to be handled after the tick.
    Other(MarketEvent),
    Nothing,
    /// The relay fell this far behind.
    Lagged(u64),
    Closed,
}

/// Whether `ev` is a full tick of an instrument with a book (an index has
/// none), whose book may follow it.
pub(in crate::feed) fn awaits_book(ev: &FeedEvent) -> Option<&NormalizedTick> {
    match ev {
        FeedEvent::Tick(t) if t.mode >= 3 => {
            let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
            (!key.is_index()).then_some(t)
        }
        _ => None,
    }
}

/// The event already waiting behind `tick`, without waiting for one.
pub(in crate::feed) fn next_event(tick: &NormalizedTick, rx: &mut Receiver<MarketEvent>) -> Next {
    match rx.try_recv() {
        Ok(ev) => match &*ev {
            FeedEvent::Depth(d) if d.symbol == tick.symbol && d.exchange == tick.exchange => {
                Next::Book(ev)
            }
            _ => Next::Other(ev),
        },
        Err(TryRecvError::Empty) => Next::Nothing,
        Err(TryRecvError::Lagged(n)) => Next::Lagged(n),
        Err(TryRecvError::Closed) => Next::Closed,
    }
}
