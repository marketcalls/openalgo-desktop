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

use crate::brokers::common::streaming::{AbsentFields, NormalizedTick, QuoteField};
use std::sync::Arc;

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
    /// The instrument's broker token, looked up once per snapshot.
    token: Option<Arc<str>>,
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

    pub(in crate::feed) fn set_token(&mut self, token: Option<Arc<str>>) {
        self.token = token;
    }

    /// What a depth frame on this snapshot leaves out of the book's own
    /// fields (`depth_update`).
    pub(in crate::feed) fn book_lacks(&self) -> AbsentFields {
        self.book_lacks
    }
}

const BOOK_TOTALS: AbsentFields =
    AbsentFields::of(&[QuoteField::TotalBuyQuantity, QuoteField::TotalSellQuantity]);

/// `t` merged into `prev`, unless `prev` is missing or from an earlier
/// connection (then `t` alone).
pub(in crate::feed) fn merge(prev: Option<&Snapshot>, t: &NormalizedTick, epoch: u64) -> Snapshot {
    use QuoteField as F;
    let incoming = t.known();
    let prev = prev.filter(|p| p.of_epoch(epoch));
    let token = prev.and_then(Snapshot::token);
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
    take(incoming, &mut known, F::Ltp, &mut s.ltp, t.ltp);
    take(incoming, &mut known, F::Open, &mut s.open, t.open);
    take(incoming, &mut known, F::High, &mut s.high, t.high);
    take(incoming, &mut known, F::Low, &mut s.low, t.low);
    take(incoming, &mut known, F::Close, &mut s.close, t.close);
    take(incoming, &mut known, F::Volume, &mut s.volume, t.volume);
    let (avg, ltq) = (t.average_price, t.last_quantity);
    take(
        incoming,
        &mut known,
        F::AveragePrice,
        &mut s.average_price,
        avg,
    );
    take(
        incoming,
        &mut known,
        F::LastQuantity,
        &mut s.last_quantity,
        ltq,
    );
    let (buy, sell) = (t.total_buy_quantity, t.total_sell_quantity);
    take(
        incoming,
        &mut known,
        F::TotalBuyQuantity,
        &mut s.total_buy_quantity,
        buy,
    );
    take(
        incoming,
        &mut known,
        F::TotalSellQuantity,
        &mut s.total_sell_quantity,
        sell,
    );
    take(incoming, &mut known, F::OpenInterest, &mut s.oi, t.oi);

    // The trade time goes with the price: a new price without one has no
    // known trade time (the frame falls back as for any packet without).
    let mut trade_time_absent = s.absent.contains(F::LastTradeTime);
    if incoming.contains(F::Ltp) || incoming.contains(F::LastTradeTime) {
        s.last_trade_time_ms = if incoming.contains(F::LastTradeTime) {
            t.last_trade_time_ms
        } else {
            0
        };
        trade_time_absent = t.absent.contains(F::LastTradeTime);
    }

    if incoming.contains(F::Change) {
        s.change = t.change;
        s.change_percent = t.change_percent;
        known = known.with(F::Change);
    } else if incoming.contains(F::Ltp) || incoming.contains(F::Close) {
        // A moved price or close makes the old change wrong.
        if known.contains(F::Close) && s.close > 0.0 && s.ltp > 0.0 {
            s.derive_change();
            known = known.with(F::Change);
        } else {
            known = known.without(F::Change);
        }
    }

    s.mode = t.mode;
    s.timestamp_ms = t.timestamp_ms;
    s.extras = s.extras.overlay(t.extras);
    s.broker_fields = t.broker_fields.clone();
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
