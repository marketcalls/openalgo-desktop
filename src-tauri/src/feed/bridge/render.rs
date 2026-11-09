//! How the bridge renders broker events as feed updates: a tick for the
//! LTP and Quote holders, a depth snapshot (with the quote fields of the
//! instrument's last tick) for the Depth holders. A field reaches an update
//! only when the packet (or, through the bridge's snapshot, an earlier one
//! of the same connection) carried it (audit C03).

use super::snapshot::Snapshot;
use crate::brokers::common::streaming::{
    AbsentFields, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::feed::source::{DepthBook, DepthLevel, InstrumentKey, MarketUpdate, Mode, QuoteFields};

/// The quote fields `t` knows. An index level of 0 is none: an index
/// outside market hours has no open, high, low or close, and its value is
/// never 0 (web Zerodha `_transform_index_tick` adds OHLC only when sent).
fn quote_fields(key: &InstrumentKey, t: &NormalizedTick, known: AbsentFields) -> QuoteFields {
    use QuoteField as F;
    let index = key.is_index();
    let has = |f: QuoteField| known.contains(f);
    let int = |f: QuoteField, v: i64| has(f).then_some(v);
    let price = |f: QuoteField, v: f64| (has(f) && !(index && v == 0.0)).then_some(v);
    QuoteFields {
        volume: int(F::Volume, t.volume),
        last_quantity: int(F::LastQuantity, t.last_quantity),
        average_price: has(F::AveragePrice).then_some(t.average_price),
        total_buy_quantity: int(F::TotalBuyQuantity, t.total_buy_quantity),
        total_sell_quantity: int(F::TotalSellQuantity, t.total_sell_quantity),
        open: price(F::Open, t.open),
        high: price(F::High, t.high),
        low: price(F::Low, t.low),
        close: price(F::Close, t.close),
        oi: int(F::OpenInterest, t.oi),
        price_change: has(F::Change).then_some(t.change),
        price_change_percent: has(F::Change).then_some(t.change_percent),
        trade_time: has(F::LastTradeTime).then_some(t.last_trade_time_ms),
        extras: t.extras,
        broker_fields: t.broker_fields.clone(),
        numbers: t.numbers,
        time_text: t.time_text,
    }
}

/// The exchange's trade time, else when the tick was made; none for a
/// stream that carries no trade time (an index on Sahi), rather than a
/// receive time passed off as one.
fn ltt(t: &NormalizedTick, known: AbsentFields) -> Option<i64> {
    if t.absent.contains(QuoteField::LastTradeTime) {
        None
    } else if known.contains(QuoteField::LastTradeTime) {
        Some(t.last_trade_time_ms)
    } else {
        Some(t.timestamp_ms)
    }
}

/// A tick, or nothing when no traded price is known for it (a packet
/// without a trade, before the instrument's first price). It reaches every
/// mode up to its own: a full tick reaches Depth holders too, with no
/// `depth` key when its book does not come with it (lead decision
/// 2026-11-09: one frame per held mode per tick; the web publishes such a
/// packet on its Depth topic without a book). A book right behind its tick
/// goes out with it instead (`bridge::pairing`).
pub(crate) fn tick_update(key: InstrumentKey, t: &NormalizedTick) -> Option<MarketUpdate> {
    let known = t.known();
    if !known.contains(QuoteField::Ltp) {
        return None;
    }
    let mode = Mode::from_u8(t.mode).unwrap_or(Mode::Ltp);
    Some(MarketUpdate {
        quote: Some(quote_fields(&key, t, known)),
        key,
        mode,
        ltp: t.ltp,
        ltt: ltt(t, known),
        timestamp: t.timestamp_ms,
        depth: None,
        exact_mode: false,
        token: None,
        broker_symbol: None,
        deep_live: None,
        deep_book: None,
    })
}

/// A tick on its instrument's snapshot. Once a book has come in this
/// connection, a full tick carries that book, with the book's totals where
/// the tick has none, so a Depth holder hears every tick (lead decision
/// 2026-11-09, the Samco ruling). Before the first book it is a tick
/// without one (`tick_update`).
pub(in crate::feed) fn snapshot_update(key: InstrumentKey, s: &Snapshot) -> Option<MarketUpdate> {
    let mut u = tick_update(key, s.tick())?;
    // A packet that says it has no book shows none, the last one not either.
    if u.mode != Mode::Depth || s.tick().book_absent {
        return Some(u);
    }
    if let Some(last) = s.book() {
        if let Some(q) = u.quote.as_mut() {
            q.total_buy_quantity = q.total_buy_quantity.or(last.total_buy);
            q.total_sell_quantity = q.total_sell_quantity.or(last.total_sell);
        }
        u.depth = Some(last.book.clone());
    }
    u.deep_book = s.deep_book().map(|last| last.book.clone());
    Some(u)
}

/// What a depth frame knows of the quote before its instrument's first
/// tick: nothing. It carries the book, its totals, `ltp` and `timestamp`
/// (all the SDK's Depth callback reads) and leaves the rest out, as web
/// `docs/api/websocket-streaming/depth.md` allows, instead of sending 0
/// (a deep book from its own socket, Fyers TBT or Dhan 20, often comes
/// first).
fn no_quote_yet() -> NormalizedTick {
    NormalizedTick {
        mode: 3,
        absent: AbsentFields::ALL,
        ..Default::default()
    }
}

/// A depth snapshot for Depth holders, with the quote fields of the latest
/// tick for the instrument. The book's own totals win unless the book, or
/// the instrument's ticks (`book_lacks`), say the stream lacks them; a book
/// without totals takes the tick's, one without order counts sends none.
pub(crate) fn depth_update(
    key: InstrumentKey,
    d: &NormalizedDepth,
    last: Option<&NormalizedTick>,
    book_lacks: AbsentFields,
) -> MarketUpdate {
    use QuoteField as F;
    let base = last.cloned().unwrap_or_else(no_quote_yet);
    let known = base.known();
    let mut q = quote_fields(&key, &base, known);
    for (field, value) in d.broker_fields.iter() {
        q.broker_fields.set(field, value);
    }
    let lacks = d.absent.union(book_lacks);
    if !lacks.contains(F::TotalBuyQuantity) {
        q.total_buy_quantity = Some(d.total_buy_quantity);
    }
    if !lacks.contains(F::TotalSellQuantity) {
        q.total_sell_quantity = Some(d.total_sell_quantity);
    }
    let orders = !d.absent.contains(F::Orders);
    let side = |v: &[crate::brokers::types::DepthLevel]| -> Vec<DepthLevel> {
        v.iter()
            .map(|l| DepthLevel {
                price: l.price,
                quantity: l.quantity,
                orders: orders.then_some(l.orders),
            })
            .collect()
    };
    let own_ltp = !d.absent.contains(F::Ltp) && d.ltp > 0.0;
    MarketUpdate {
        key,
        mode: Mode::Depth,
        ltp: if own_ltp { d.ltp } else { base.ltp },
        ltt: ltt(&base, known),
        timestamp: d.timestamp_ms,
        quote: Some(q),
        depth: Some(DepthBook {
            buy: side(&d.buy),
            sell: side(&d.sell),
            levels: None,
            time_ms: d.feed_time_ms,
        }),
        exact_mode: true,
        token: None,
        broker_symbol: None,
        deep_live: None,
        deep_book: None,
    }
}

#[cfg(test)]
pub(super) mod tests;
