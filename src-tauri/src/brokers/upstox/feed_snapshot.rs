//! The quote Upstox has sent for one instrument (web `upstox_adapter.py`
//! `_merge_feed` and `_build_payload`, openalgo#2377).
//!
//! Upstox V3 ticks are incremental: a tick carries only what changed (the
//! trade, the book, the day candle, the totals), and a value equal to its
//! protobuf default is not on the wire at all (the web reads ticks through
//! `MessageToDict`, which drops them; here a 0 is "not sent"). Every tick
//! is merged into its instrument's snapshot and each tick published is
//! built from the merged values, so a field a tick did not carry keeps its
//! last value and a field never sent is left out, never sent as 0.
//!
//! * `close` is `ltpc.cp`, the previous day's close, never today's candle
//!   close (about the LTP intraday).
//! * OHLC comes from the day candle (`1d`) only; an intraday candle (`I1`)
//!   never stands in for it.
//! * Volume is `vtt`, the volume traded today, else the day candle's.
//! * The book is the five best bids and asks Upstox sent, without padding.

use super::proto::{feed::FeedUnion, full_feed::FullFeedUnion, Ltpc, MarketFullFeed, MarketOhlc};
use crate::brokers::common::streaming::{
    AbsentFields, BrokerField, BrokerFields, BrokerValue, FeedEvent, FeedMode, NormalizedDepth,
    NormalizedTick, QuoteField,
};
use crate::brokers::types::DepthLevel;

/// Book levels Upstox's `full` mode streams.
const LEVELS: usize = 5;

#[derive(Debug, Clone, Default)]
pub(super) struct Snapshot {
    ltp: Option<f64>,
    ltq: Option<i64>,
    ltt: Option<i64>,
    /// `ltpc.cp`, the previous day's close.
    prev_close: Option<f64>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    /// `vtt`, the volume traded today.
    volume_today: Option<i64>,
    /// The day candle's volume, while no `vtt` has come.
    day_candle_volume: Option<i64>,
    average_price: Option<f64>,
    total_buy_quantity: Option<i64>,
    total_sell_quantity: Option<i64>,
    oi: Option<f64>,
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    /// The closing-auction fields of the latest tick only (web
    /// `_extract_cas_fields`: valid for the tick that carried them, never
    /// cached).
    auction: BrokerFields,
}

/// A value Upstox sent: proto3 leaves a default off the wire.
fn sent<T: PartialEq + Default>(v: T) -> Option<T> {
    (v != T::default()).then_some(v)
}

/// Overwrite `slot` with what a tick carried; keep it otherwise.
fn keep<T>(slot: &mut Option<T>, v: Option<T>) {
    if v.is_some() {
        *slot = v;
    }
}

/// A tick field from the snapshot, or the field marked absent.
fn fill<T: Copy>(slot: &mut T, v: Option<T>, field: QuoteField, absent: &mut AbsentFields) {
    match v {
        Some(v) => *slot = v,
        None => *absent = absent.with(field),
    }
}

/// The LTPC a feed carries, whichever branch it came in.
fn tick_ltpc(f: Option<&FeedUnion>) -> Option<&Ltpc> {
    match f? {
        FeedUnion::Ltpc(l) => Some(l),
        FeedUnion::FullFeed(ff) => match ff.full_feed_union.as_ref()? {
            FullFeedUnion::MarketFf(m) => m.ltpc.as_ref(),
            FullFeedUnion::IndexFf(i) => i.ltpc.as_ref(),
        },
        FeedUnion::FirstLevelWithGreeks(g) => g.ltpc.as_ref(),
    }
}

/// The five best bids (highest first) and asks (lowest first) of a book.
fn book(quotes: &[super::proto::Quote]) -> (Vec<DepthLevel>, Vec<DepthLevel>) {
    let level = |price: f64, quantity: i64| DepthLevel {
        price,
        quantity,
        orders: 0,
    };
    let mut buy: Vec<DepthLevel> = quotes
        .iter()
        .filter(|q| q.bid_p > 0.0)
        .map(|q| level(q.bid_p, q.bid_q))
        .collect();
    let mut sell: Vec<DepthLevel> = quotes
        .iter()
        .filter(|q| q.ask_p > 0.0)
        .map(|q| level(q.ask_p, q.ask_q))
        .collect();
    buy.sort_by(|a, b| b.price.total_cmp(&a.price));
    sell.sort_by(|a, b| a.price.total_cmp(&b.price));
    buy.truncate(LEVELS);
    sell.truncate(LEVELS);
    (buy, sell)
}

impl Snapshot {
    /// Merge one tick (web `_merge_feed`).
    pub(super) fn merge(&mut self, feed: Option<&FeedUnion>) {
        self.auction = auction_fields(feed);
        if let Some(l) = tick_ltpc(feed) {
            keep(&mut self.ltp, sent(l.ltp));
            keep(&mut self.ltq, sent(l.ltq));
            keep(&mut self.ltt, sent(l.ltt));
            keep(&mut self.prev_close, sent(l.cp));
        }
        let full = match feed {
            Some(FeedUnion::FullFeed(ff)) => ff.full_feed_union.as_ref(),
            _ => None,
        };
        match full {
            Some(FullFeedUnion::MarketFf(m)) => {
                self.merge_day_candle(m.market_ohlc.as_ref());
                self.merge_market(m);
            }
            Some(FullFeedUnion::IndexFf(i)) => self.merge_day_candle(i.market_ohlc.as_ref()),
            None => {}
        }
    }

    /// Only the day candle carries the session's OHLC.
    fn merge_day_candle(&mut self, m: Option<&MarketOhlc>) {
        let Some(day) = m.and_then(|m| m.ohlc.iter().find(|o| o.interval == "1d")) else {
            return;
        };
        keep(&mut self.open, sent(day.open));
        keep(&mut self.high, sent(day.high));
        keep(&mut self.low, sent(day.low));
        keep(&mut self.day_candle_volume, sent(day.vol));
    }

    fn merge_market(&mut self, m: &MarketFullFeed) {
        keep(&mut self.volume_today, sent(m.vtt));
        keep(&mut self.average_price, sent(m.atp));
        keep(&mut self.total_buy_quantity, sent(m.tbq).map(|v| v as i64));
        keep(&mut self.total_sell_quantity, sent(m.tsq).map(|v| v as i64));
        keep(&mut self.oi, sent(m.oi));
        if let Some(level) = &m.market_level {
            if !level.bid_ask_quote.is_empty() {
                (self.buy, self.sell) = book(&level.bid_ask_quote);
            }
        }
    }

    /// What one subscription is published (web `_build_payload`): a tick
    /// of the merged quote, and for Depth the book after it. Nothing until
    /// Upstox has sent a price: a frame's `ltp` cannot be left out, and a 0
    /// would be one invented.
    pub(super) fn events(
        &self,
        symbol: &str,
        exchange: &str,
        mode: FeedMode,
        now: i64,
    ) -> Vec<FeedEvent> {
        let Some(t) = self.tick(symbol, exchange, mode, now) else {
            return Vec::new();
        };
        if mode != FeedMode::Depth {
            return vec![FeedEvent::Tick(t)];
        }
        let depth = NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy: self.buy.clone(),
            sell: self.sell.clone(),
            total_buy_quantity: t.total_buy_quantity,
            total_sell_quantity: t.total_sell_quantity,
            timestamp_ms: now,
            // Upstox's book levels carry no order count (web
            // `upstox_adapter.py`: levels are price and quantity).
            absent: AbsentFields::of(&[QuoteField::Orders]),
        };
        vec![FeedEvent::Tick(t), FeedEvent::Depth(depth)]
    }

    fn tick(
        &self,
        symbol: &str,
        exchange: &str,
        mode: FeedMode,
        now: i64,
    ) -> Option<NormalizedTick> {
        use QuoteField as F;
        let mut t = NormalizedTick {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            mode: mode.code(),
            ltp: self.ltp?,
            timestamp_ms: now,
            ..Default::default()
        };
        let mut absent = AbsentFields::default();
        let a = &mut absent;
        fill(&mut t.last_quantity, self.ltq, F::LastQuantity, a);
        fill(&mut t.last_trade_time_ms, self.ltt, F::LastTradeTime, a);
        fill(&mut t.close, self.prev_close, F::Close, a);
        if self.prev_close.is_none() {
            *a = a.with(F::Change);
        }
        fill(&mut t.open, self.open, F::Open, a);
        fill(&mut t.high, self.high, F::High, a);
        fill(&mut t.low, self.low, F::Low, a);
        let volume = self.volume_today.or(self.day_candle_volume);
        fill(&mut t.volume, volume, F::Volume, a);
        fill(&mut t.average_price, self.average_price, F::AveragePrice, a);
        fill(
            &mut t.total_buy_quantity,
            self.total_buy_quantity,
            F::TotalBuyQuantity,
            a,
        );
        fill(
            &mut t.total_sell_quantity,
            self.total_sell_quantity,
            F::TotalSellQuantity,
            a,
        );
        fill(&mut t.oi, self.oi.map(|v| v as i64), F::OpenInterest, a);
        t.absent = absent;
        // web `_build_payload`: the Quote carries OI, the LTP payload the
        // LTPC's last quantity and previous close.
        if self.oi.is_some() {
            t.mark_carried(F::OpenInterest);
        }
        if mode == FeedMode::Ltp {
            if self.ltq.is_some() {
                t.mark_carried(F::LastQuantity);
            }
            if self.prev_close.is_some() {
                t.mark_carried(F::Close);
            }
        }
        t.broker_fields = self.auction.clone();
        t.derive_change();
        Some(t)
    }
}

/// web `_extract_cas_fields`: what a full packet carries of the closing
/// auction. proto3 leaves a default off the wire, so a 0 is "not sent";
/// the LTPC's `iep` is a wrapper whose explicit 0 is kept.
fn auction_fields(f: Option<&FeedUnion>) -> BrokerFields {
    let mut out = BrokerFields::default();
    let Some(FeedUnion::FullFeed(ff)) = f else {
        return out;
    };
    let Some(FullFeedUnion::MarketFf(m)) = ff.full_feed_union.as_ref() else {
        return out;
    };
    let iep = sent(m.iep).or_else(|| {
        m.ltpc
            .as_ref()
            .and_then(|l| l.iep.as_ref())
            .map(|w| w.value)
    });
    if let Some(v) = iep {
        out.set(
            BrokerField::IndicativeEquilibriumPrice,
            BrokerValue::Float(v),
        );
    }
    if let Some(v) = sent(m.rp) {
        out.set(BrokerField::ReferencePrice, BrokerValue::Float(v));
    }
    if let Some(v) = sent(m.ieq) {
        out.set(
            BrokerField::IndicativeEquilibriumQuantity,
            BrokerValue::Int(v),
        );
    }
    if let Some(v) = sent(m.iiq_total) {
        out.set(
            BrokerField::IndicativeImbalanceQuantityTotal,
            BrokerValue::Int(v),
        );
    }
    if let Some(v) = sent(m.iiq_m) {
        out.set(
            BrokerField::IndicativeImbalanceQuantityMarket,
            BrokerValue::Int(v),
        );
    }
    if m.cas_eligible {
        out.set(BrokerField::CasEligible, BrokerValue::Bool(true));
    }
    out
}
