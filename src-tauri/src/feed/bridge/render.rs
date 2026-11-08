//! How the bridge renders broker events as feed updates: a tick for the
//! LTP and Quote holders, a depth snapshot (with the quote fields of the
//! instrument's last tick) for the Depth holders.

use crate::brokers::common::streaming::{
    AbsentFields, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::feed::source::{DepthBook, DepthLevel, InstrumentKey, MarketUpdate, Mode, QuoteFields};

fn quote_fields(key: &InstrumentKey, t: &NormalizedTick, with_oi: bool) -> QuoteFields {
    let index = key.is_index();
    let opt = |v: f64| if index && v == 0.0 { None } else { Some(v) };
    QuoteFields {
        volume: t.volume,
        last_quantity: t.last_quantity,
        average_price: t.average_price,
        total_buy_quantity: t.total_buy_quantity,
        total_sell_quantity: t.total_sell_quantity,
        open: opt(t.open),
        high: opt(t.high),
        low: opt(t.low),
        close: opt(t.close),
        oi: with_oi.then_some(t.oi),
        price_change: index.then_some(t.change),
        price_change_percent: index.then_some(t.change_percent),
        absent: t.absent,
    }
}

/// The exchange's trade time, else when the tick was made; none for a
/// stream that carries no trade time (an index on Sahi), rather than a
/// receive time passed off as one.
fn ltt(t: &NormalizedTick) -> Option<i64> {
    if t.absent.contains(QuoteField::LastTradeTime) {
        return None;
    }
    Some(if t.last_trade_time_ms > 0 {
        t.last_trade_time_ms
    } else {
        t.timestamp_ms
    })
}

/// A tick. A non-index full tick is served at Quote: its book follows as a
/// separate `Depth` event.
pub(crate) fn tick_update(key: InstrumentKey, t: &NormalizedTick) -> MarketUpdate {
    let raw = Mode::from_u8(t.mode).unwrap_or(Mode::Ltp);
    let mode = if raw == Mode::Depth && !key.is_index() {
        Mode::Quote
    } else {
        raw
    };
    MarketUpdate {
        quote: (mode >= Mode::Quote).then(|| quote_fields(&key, t, raw == Mode::Depth)),
        key,
        mode,
        ltp: t.ltp,
        ltt: ltt(t),
        timestamp: t.timestamp_ms,
        depth: None,
        exact_mode: false,
    }
}

/// What a depth frame knows of the quote before its instrument's first
/// tick: nothing. It carries the book, its totals, `ltp` and `timestamp`
/// (all the SDK's Depth callback reads) and leaves the rest out, as web
/// `docs/api/websocket-streaming/depth.md` allows, instead of sending 0
/// (a deep book from its own socket, Fyers TBT or Dhan 20, often comes
/// first).
const NO_QUOTE_YET: AbsentFields = AbsentFields::of(&[
    QuoteField::Open,
    QuoteField::High,
    QuoteField::Low,
    QuoteField::Close,
    QuoteField::Volume,
    QuoteField::AveragePrice,
    QuoteField::LastQuantity,
    QuoteField::OpenInterest,
    QuoteField::Change,
    QuoteField::LastTradeTime,
]);

/// A depth snapshot for Depth holders, with the quote fields of the latest
/// tick for the instrument (and what that tick said its stream lacks).
pub(crate) fn depth_update(
    key: InstrumentKey,
    d: &NormalizedDepth,
    last: Option<&NormalizedTick>,
) -> MarketUpdate {
    let base = last.cloned().unwrap_or_else(|| NormalizedTick {
        absent: NO_QUOTE_YET,
        ..Default::default()
    });
    let mut q = quote_fields(&key, &base, true);
    q.total_buy_quantity = d.total_buy_quantity;
    q.total_sell_quantity = d.total_sell_quantity;
    let side = |v: &[crate::brokers::types::DepthLevel]| -> Vec<DepthLevel> {
        v.iter()
            .map(|l| DepthLevel {
                price: l.price,
                quantity: l.quantity,
                orders: l.orders,
            })
            .collect()
    };
    MarketUpdate {
        key,
        mode: Mode::Depth,
        ltp: if d.ltp > 0.0 { d.ltp } else { base.ltp },
        ltt: ltt(&base),
        timestamp: d.timestamp_ms,
        quote: Some(q),
        depth: Some(DepthBook {
            buy: side(&d.buy),
            sell: side(&d.sell),
        }),
        exact_mode: true,
    }
}

#[cfg(test)]
pub(super) mod tests;
