//! What one SFeed packet says, as the feed's events: a tick with exactly
//! the fields the packet carries (the bridge merges it into what it last
//! knew of the instrument, `feed::bridge::snapshot`), and for a Depth
//! subscription the instrument's book.
//!
//! A market picture (levels 2, 4, 8, 16) carries the whole quote and OI,
//! and its values are the exchange's now, a 0 included: an empty bid side
//! has a total buy quantity of 0 (web `kotak_adapter._on_data_received`,
//! where a complete packet's fields win). A mini touch line carries the
//! price, the previous close and the last trade; what it lacks stays as the
//! bridge last knew it, or is left out before the first market picture
//! (web openalgo#2376). An index packet has no order flow or OI. Nothing
//! has a trade time before the first trade. Every price packet carries the
//! exchange's change, which the web publishes as `percent_change` (the
//! packet's `nc`, `sfeed_websocket._trade_fields`): the tick sends that
//! figure, not one worked out again from the price.

use super::{Sfeed, SfeedRow, SubInfo};
use crate::brokers::common::streaming::{
    AbsentFields, FeedMode, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::brokers::types::DepthLevel;

/// What a mini touch line does not carry.
const NOT_IN_A_MINI_TICK: AbsentFields = AbsentFields::of(&[
    QuoteField::Open,
    QuoteField::High,
    QuoteField::Low,
    QuoteField::Volume,
    QuoteField::AveragePrice,
    QuoteField::TotalBuyQuantity,
    QuoteField::TotalSellQuantity,
]);

/// What an index packet never carries.
const NOT_IN_AN_INDEX_PACKET: AbsentFields = AbsentFields::of(&[
    QuoteField::Volume,
    QuoteField::AveragePrice,
    QuoteField::LastQuantity,
    QuoteField::TotalBuyQuantity,
    QuoteField::TotalSellQuantity,
    QuoteField::OpenInterest,
]);

const BOOK_TOTALS: AbsentFields =
    AbsentFields::of(&[QuoteField::TotalBuyQuantity, QuoteField::TotalSellQuantity]);

/// An instrument's book, from its last depth packet (level 8 or 16): Kotak
/// sends it apart from the price, and each Depth frame carries it.
#[derive(Debug, Clone, Default)]
pub(super) struct Book {
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
}

impl Book {
    /// A depth packet's book: the whole book as the exchange sees it now,
    /// so an empty row is an empty level, never the last packet's (web
    /// openalgo#2376).
    pub(super) fn of(buy: &[SfeedRow], sell: &[SfeedRow]) -> Book {
        Book {
            buy: side(buy),
            sell: side(sell),
        }
    }
}

/// One side of a depth packet: its real levels, best first, at most five.
/// An empty row (no price, no quantity) is no level: a side is never padded
/// with zero-priced rows (lead decision 9 Nov; the web pads each side to
/// five, `sfeed_websocket._depth_rows`).
fn side(rows: &[SfeedRow]) -> Vec<DepthLevel> {
    rows.iter()
        .filter(|r| r.price != 0.0 || r.quantity != 0)
        .take(5)
        .map(|r| DepthLevel {
            price: r.price,
            quantity: r.quantity,
            orders: r.orders,
        })
        .collect()
}

/// The exchange's time in epoch ms (SFeed sends seconds; 0 before the
/// first trade).
fn epoch_ms(t: i64) -> i64 {
    if t <= 0 {
        0
    } else if t < 100_000_000_000 {
        t * 1000
    } else {
        t
    }
}

/// The tick a price packet makes for `sub`, or `None` for one that is not
/// a price packet.
pub(super) fn packet_tick(sub: &SubInfo, pkt: &Sfeed, now: i64) -> Option<NormalizedTick> {
    let mut t = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        timestamp_ms: now,
        ..Default::default()
    };
    let traded_at = match pkt {
        Sfeed::Scrip { .. } => market_picture(&mut t, pkt),
        Sfeed::Lite { .. } => mini_touch_line(&mut t, pkt),
        Sfeed::Index { .. } => index(&mut t, pkt),
        Sfeed::MarketStatus { .. } | Sfeed::Cas { .. } => return None,
    };
    t.mark_carried(QuoteField::Change);
    t.last_trade_time_ms = epoch_ms(traded_at);
    if t.last_trade_time_ms <= 0 {
        t.mark_absent(QuoteField::LastTradeTime);
    }
    if t.ltp <= 0.0 {
        // No trade price: the bridge keeps the rest for the next priced
        // packet and publishes nothing for this one.
        t.mark_absent(QuoteField::Ltp);
    }
    Some(t)
}

/// A market picture's fields into `t`; its trade time.
fn market_picture(t: &mut NormalizedTick, pkt: &Sfeed) -> i64 {
    let Sfeed::Scrip {
        ltp,
        open,
        high,
        low,
        close,
        average_price,
        last_trade_time,
        last_trade_qty,
        total_buy,
        total_sell,
        volume,
        oi,
        change,
        change_percent,
        ..
    } = pkt
    else {
        return 0;
    };
    t.mode = FeedMode::Quote.code();
    (t.change, t.change_percent) = (*change, *change_percent);
    t.ltp = *ltp;
    t.open = *open;
    t.high = *high;
    t.low = *low;
    t.close = *close;
    t.average_price = *average_price;
    t.last_quantity = *last_trade_qty;
    t.total_buy_quantity = *total_buy;
    t.total_sell_quantity = *total_sell;
    t.volume = *volume;
    t.oi = *oi;
    // A market picture carries OI, and the web's Quote frame sends it
    // (`_quote_extras`).
    t.mark_carried(QuoteField::OpenInterest);
    *last_trade_time
}

/// A mini touch line's fields into `t`; its trade time.
fn mini_touch_line(t: &mut NormalizedTick, pkt: &Sfeed) -> i64 {
    let Sfeed::Lite {
        ltp,
        close,
        last_trade_time,
        last_trade_qty,
        change,
        change_percent,
        ..
    } = pkt
    else {
        return 0;
    };
    t.mode = FeedMode::Quote.code();
    (t.change, t.change_percent) = (*change, *change_percent);
    t.ltp = *ltp;
    t.close = *close;
    t.last_quantity = *last_trade_qty;
    t.absent = NOT_IN_A_MINI_TICK;
    if *close <= 0.0 {
        // A partial packet without a previous close says nothing about
        // it: the last known one stays.
        t.mark_absent(QuoteField::Close);
    }
    *last_trade_time
}

/// An index packet's fields into `t`; its trade time.
fn index(t: &mut NormalizedTick, pkt: &Sfeed) -> i64 {
    let Sfeed::Index {
        value,
        open,
        high,
        low,
        close,
        last_trade_time,
        change,
        change_percent,
        ..
    } = pkt
    else {
        return 0;
    };
    // An index's packet is all it has: Depth holders read it too.
    t.mode = FeedMode::Depth.code();
    (t.change, t.change_percent) = (*change, *change_percent);
    t.ltp = *value;
    t.open = *open;
    t.high = *high;
    t.low = *low;
    t.close = *close;
    t.absent = NOT_IN_AN_INDEX_PACKET;
    *last_trade_time
}

/// The Depth event a packet makes for `sub` over its `book`: the packet's
/// price, and its book totals where it carries them (a mini touch line
/// does not: the frame takes the instrument's last known ones).
pub(super) fn depth_event(sub: &SubInfo, book: &Book, pkt: &Sfeed, now: i64) -> NormalizedDepth {
    let mut d = NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        buy: book.buy.clone(),
        sell: book.sell.clone(),
        timestamp_ms: now,
        ..Default::default()
    };
    match pkt {
        Sfeed::Scrip {
            ltp,
            total_buy,
            total_sell,
            ..
        } => {
            d.ltp = *ltp;
            d.total_buy_quantity = *total_buy;
            d.total_sell_quantity = *total_sell;
        }
        Sfeed::Lite { ltp, .. } => {
            d.ltp = *ltp;
            d.absent = BOOK_TOTALS;
        }
        _ => d.absent = BOOK_TOTALS,
    }
    if d.ltp <= 0.0 {
        d.absent = d.absent.with(QuoteField::Ltp);
    }
    d
}
