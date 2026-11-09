//! An instrument's last known quote: every field one broker connection has
//! carried for it, merged packet by packet by what each packet says it
//! carries (audit C03), never by testing a value for zero.
//!
//! An LTPC-only, OI-only or circuit-only packet therefore keeps the quote
//! it did not repeat, as the web's merging adapters do (Upstox
//! `_merge_feed`, HDFC's snapshot, the Noren cache), and a Depth frame
//! built on the snapshot carries that quote. A new connection starts every
//! instrument afresh, so nothing from before an outage is sent as current.
//!
//! Two fields follow others: the trade time belongs to the price it came
//! with (a packet with a new price and no trade time drops the old one),
//! and the change is worked out again from the price and the previous
//! close when a packet moves either without sending a change.
//!
//! The snapshot also keeps the instrument's last book, so a full tick after
//! it reaches Depth holders with that book (lead decision 2026-11-09, the
//! Samco ruling: once a book exists a Depth holder hears every tick).

use crate::brokers::common::streaming::{AbsentFields, NormalizedTick, QuoteField};
use crate::feed::source::{DepthBook, InstrumentKey, MarketUpdate};
use std::collections::HashMap;
use std::sync::Arc;

/// An instrument's broker token and trading symbol (`None`: not in the
/// symbol master).
pub(in crate::feed) type BrokerIds = (Option<Arc<str>>, Option<Arc<str>>);

/// An instrument's last book as its frame sent it, with the book's totals.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::feed) struct LastBook {
    pub(in crate::feed) book: DepthBook,
    pub(in crate::feed) total_buy: Option<i64>,
    pub(in crate::feed) total_sell: Option<i64>,
}

impl LastBook {
    /// The book of a depth update, if it carries one.
    pub(in crate::feed) fn of(update: &MarketUpdate) -> Option<Self> {
        let q = update.quote.as_ref();
        Some(Self {
            book: update.depth.clone()?,
            total_buy: q.and_then(|q| q.total_buy_quantity),
            total_sell: q.and_then(|q| q.total_sell_quantity),
        })
    }
}

/// The merged quote of one instrument, and the broker connection it
/// belongs to.
#[derive(Debug, Clone)]
pub(in crate::feed) struct Snapshot {
    /// Every field explicit: `carried` is what is known, `absent` the rest.
    tick: NormalizedTick,
    epoch: u64,
    /// The book totals the latest tick marked absent: its stream lacks
    /// them, and so do its depth messages (the 1.0.6 contract: an adapter
    /// marks a total only when its book lacks it too).
    book_lacks: AbsentFields,
    /// The instrument's broker token and trading symbol, looked up once per
    /// snapshot.
    token: Option<Arc<str>>,
    broker_symbol: Option<Arc<str>>,
    /// The instrument's last book in this connection (shared: every tick
    /// makes a new snapshot), and the deeper socket's apart.
    book: Option<Arc<LastBook>>,
    deep_book: Option<Arc<LastBook>>,
}

impl Snapshot {
    pub(in crate::feed) fn tick(&self) -> &NormalizedTick {
        &self.tick
    }

    /// Whether it belongs to the broker connection numbered `epoch`.
    pub(in crate::feed) fn of_epoch(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    pub(in crate::feed) fn token(&self) -> Option<Arc<str>> {
        self.token.clone()
    }

    pub(in crate::feed) fn broker_symbol(&self) -> Option<Arc<str>> {
        self.broker_symbol.clone()
    }

    pub(in crate::feed) fn set_ids(&mut self, (token, broker_symbol): BrokerIds) {
        self.token = token;
        self.broker_symbol = broker_symbol;
    }

    /// What a depth frame on this snapshot leaves out of the book's own
    /// fields (`depth_update`).
    pub(in crate::feed) fn book_lacks(&self) -> AbsentFields {
        self.book_lacks
    }

    pub(in crate::feed) fn book(&self) -> Option<&LastBook> {
        self.book.as_deref()
    }

    pub(in crate::feed) fn deep_book(&self) -> Option<&LastBook> {
        self.deep_book.as_deref()
    }
}

/// Keep `book` as `key`'s last book (`deep`: from the deeper socket) in
/// connection `epoch`: on its snapshot, or on a snapshot with no quote yet
/// when the book came first.
pub(in crate::feed) fn keep_book(
    snapshots: &mut HashMap<InstrumentKey, Snapshot>,
    key: &InstrumentKey,
    book: LastBook,
    epoch: u64,
    deep: bool,
) {
    let book = Some(Arc::new(book));
    if let Some(s) = snapshots.get_mut(key).filter(|s| s.of_epoch(epoch)) {
        if deep {
            s.deep_book = book;
        } else {
            s.book = book;
        }
        return;
    }
    let (book, deep_book) = if deep { (None, book) } else { (book, None) };
    let tick = NormalizedTick {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        absent: AbsentFields::ALL,
        ..Default::default()
    };
    snapshots.insert(
        key.clone(),
        Snapshot {
            tick,
            epoch,
            book_lacks: AbsentFields::default(),
            token: None,
            broker_symbol: None,
            book,
            deep_book,
        },
    );
}

const BOOK_TOTALS: AbsentFields =
    AbsentFields::of(&[QuoteField::TotalBuyQuantity, QuoteField::TotalSellQuantity]);

/// `t` merged into `prev`, unless `prev` is missing or from an earlier
/// connection (then `t` alone).
pub(in crate::feed) fn merge(prev: Option<&Snapshot>, t: &NormalizedTick, epoch: u64) -> Snapshot {
    use QuoteField as F;
    let prev = prev.filter(|p| p.of_epoch(epoch));
    let token = prev.and_then(Snapshot::token);
    let broker_symbol = prev.and_then(Snapshot::broker_symbol);
    let book = prev.and_then(|p| p.book.clone());
    let deep_book = prev.and_then(|p| p.deep_book.clone());
    let (mut s, mut known) = match prev {
        Some(p) => (p.tick.clone(), p.tick.known()),
        None => (
            NormalizedTick {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                ..Default::default()
            },
            AbsentFields::default(),
        ),
    };
    take_values(&mut s, &mut known, t);
    known = known.minus(t.cleared);
    let trade_time_absent = take_trade_time(&mut s, t);
    known = take_change(&mut s, known, t);
    s.mode = t.mode;
    s.timestamp_ms = t.timestamp_ms;
    s.extras = s.extras.overlay(t.extras);
    s.broker_fields = t.broker_fields.clone();
    s.numbers = t.numbers;
    s.book_absent = t.book_absent;
    let known = known.without(F::LastTradeTime).without(F::Orders);
    s.carried = known;
    s.absent = AbsentFields::ALL
        .minus(known)
        .without(F::LastTradeTime)
        .without(F::Orders);
    if trade_time_absent {
        s.absent = s.absent.with(F::LastTradeTime);
    }
    Snapshot {
        tick: s,
        epoch,
        book_lacks: t.absent.minus(AbsentFields::ALL.minus(BOOK_TOTALS)),
        token,
        broker_symbol,
        book,
        deep_book,
    }
}

/// Every field `t` carries, over the snapshot's.
fn take_values(s: &mut NormalizedTick, known: &mut AbsentFields, t: &NormalizedTick) {
    use QuoteField as F;
    let incoming = if t.knows(F::Ltp) {
        t.known()
    } else {
        sent_without_price(t)
    };
    take(incoming, known, F::Ltp, &mut s.ltp, t.ltp);
    take(incoming, known, F::Open, &mut s.open, t.open);
    take(incoming, known, F::High, &mut s.high, t.high);
    take(incoming, known, F::Low, &mut s.low, t.low);
    take(incoming, known, F::Close, &mut s.close, t.close);
    take(incoming, known, F::Volume, &mut s.volume, t.volume);
    take(
        incoming,
        known,
        F::AveragePrice,
        &mut s.average_price,
        t.average_price,
    );
    take(
        incoming,
        known,
        F::LastQuantity,
        &mut s.last_quantity,
        t.last_quantity,
    );
    let (buy, sell) = (t.total_buy_quantity, t.total_sell_quantity);
    take(
        incoming,
        known,
        F::TotalBuyQuantity,
        &mut s.total_buy_quantity,
        buy,
    );
    take(
        incoming,
        known,
        F::TotalSellQuantity,
        &mut s.total_sell_quantity,
        sell,
    );
    take(incoming, known, F::OpenInterest, &mut s.oi, t.oi);
}

/// What a packet without a price brings (an OI-only packet, HDFC before the
/// first trade, bk-b's no_trade_yet): what it marks as carried and the
/// fields of its mode it sent a value for. A zero there is the default the
/// packet was left with (proto3 writes 0 for a field not sent), not a
/// quote, so it never replaces nor stands for one.
fn sent_without_price(t: &NormalizedTick) -> AbsentFields {
    use QuoteField as F;
    let valued = [
        (F::Open, t.open != 0.0),
        (F::High, t.high != 0.0),
        (F::Low, t.low != 0.0),
        (F::Close, t.close != 0.0),
        (F::Volume, t.volume != 0),
        (F::AveragePrice, t.average_price != 0.0),
        (F::LastQuantity, t.last_quantity != 0),
        (F::TotalBuyQuantity, t.total_buy_quantity != 0),
        (F::TotalSellQuantity, t.total_sell_quantity != 0),
        (F::OpenInterest, t.oi != 0),
    ];
    let sent = valued
        .into_iter()
        .filter(|(_, v)| *v)
        .fold(t.carried, |k, (f, _)| k.with(f));
    t.known().minus(AbsentFields::ALL.minus(sent))
}

/// The trade time goes with the price: a new price without one has no
/// known trade time (the frame falls back as for any packet without one).
/// Whether the stream marks it absent: a packet with neither a price nor a
/// trade time keeps its mark unless an earlier packet of the connection
/// brought a real trade time (then that one stays).
fn take_trade_time(s: &mut NormalizedTick, t: &NormalizedTick) -> bool {
    use QuoteField as F;
    let incoming = t.known();
    if !(incoming.contains(F::Ltp) || incoming.contains(F::LastTradeTime)) {
        let none_before = s.last_trade_time_ms <= 0;
        return s.absent.contains(F::LastTradeTime)
            || (none_before && t.absent.contains(F::LastTradeTime));
    }
    s.last_trade_time_ms = if incoming.contains(F::LastTradeTime) {
        t.last_trade_time_ms
    } else {
        0
    };
    s.time_text = t.time_text;
    t.absent.contains(F::LastTradeTime)
}

/// The change: the packet's own, none where its stream has none, else
/// worked out again when the price or the close moved (the old change is
/// then wrong).
fn take_change(s: &mut NormalizedTick, known: AbsentFields, t: &NormalizedTick) -> AbsentFields {
    use QuoteField as F;
    let incoming = t.known();
    if incoming.contains(F::Change) {
        s.change = t.change;
        s.change_percent = t.change_percent;
        known.with(F::Change)
    } else if t.absent.contains(F::Change) {
        known.without(F::Change)
    } else if incoming.contains(F::Ltp) || incoming.contains(F::Close) {
        if known.contains(F::Close) && s.close > 0.0 && s.ltp > 0.0 {
            s.derive_change();
            known.with(F::Change)
        } else {
            known.without(F::Change)
        }
    } else {
        known
    }
}

/// `src` into `dst` when the packet carries `field`.
fn take<T>(
    incoming: AbsentFields,
    known: &mut AbsentFields,
    field: QuoteField,
    dst: &mut T,
    src: T,
) {
    if incoming.contains(field) {
        *dst = src;
        *known = known.with(field);
    }
}

#[cfg(test)]
mod tests;
