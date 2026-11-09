//! A Noren scrip's merged snapshot as a tick and a book (audit FB-07): each
//! member's web adapter reads the same keys, and they differ only where
//! the member does (`NorenConfig::feed_oi`).
//!
//! * The tick's mode is the packet's: a touchline (`tk`, `tf`) carries the
//!   quote, a depth packet (`dk`, `df`) the quote and open interest.
//! * A key the scrip's snapshot has never held is not carried: the frame
//!   leaves it out where the web sends its `safe_float` default of 0.
//! * `pc` is Noren's own change percent and is published when known; with
//!   none known on the connection there is no change, never one worked out
//!   from the previous close (the web only forwards `pc`: `percent_change =
//!   safe_float(pc)`, which is 0 without one; lead rule no-derived-change).
//! * `ft` is the feed time (the web's `<member>_timestamp`), not the trade
//!   time: the trade time is Noren's `ltt`, a time of day in IST.
//! * Circuits and the 52-week range (`uc`, `lc`, `52h`, `52l`) are carried
//!   as sent.

use super::super::mapping::{f, i, text};
use super::super::{FeedOi, NorenConfig};
use crate::brokers::common::streaming::{
    round2, AbsentFields, NormalizedDepth, NormalizedTick, QuoteExtras, QuoteField,
};
use crate::brokers::types::DepthLevel;
use serde_json::{Map, Value};

/// What a frame is about: the OpenAlgo symbol and exchange it is reported
/// under.
pub(super) struct Instrument<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
}

/// The touchline keys whose presence a tick reports.
const QUOTE_KEYS: [(&str, QuoteField); 9] = [
    ("o", QuoteField::Open),
    ("h", QuoteField::High),
    ("l", QuoteField::Low),
    ("c", QuoteField::Close),
    ("v", QuoteField::Volume),
    ("ap", QuoteField::AveragePrice),
    ("ltq", QuoteField::LastQuantity),
    ("tbq", QuoteField::TotalBuyQuantity),
    ("tsq", QuoteField::TotalSellQuantity),
];

/// IST is five and a half hours ahead of UTC.
const IST_OFFSET_SECS: i64 = 19_800;
const DAY_SECS: i64 = 86_400;

/// The tick for a scrip's merged snapshot `d`, from a depth packet when
/// `depth_packet`, received at `now_ms`.
pub(super) fn tick(
    cfg: &NorenConfig,
    sub: Instrument<'_>,
    d: &Map<String, Value>,
    depth_packet: bool,
    now_ms: i64,
) -> NormalizedTick {
    let v = Value::Object(d.clone());
    let mut t = NormalizedTick {
        symbol: sub.symbol.to_string(),
        exchange: sub.exchange.to_string(),
        mode: if depth_packet { 3 } else { 2 },
        ltp: f(&v, "lp"),
        open: f(&v, "o"),
        high: f(&v, "h"),
        low: f(&v, "l"),
        close: f(&v, "c"),
        volume: i(&v, "v"),
        average_price: f(&v, "ap"),
        last_quantity: i(&v, "ltq"),
        total_buy_quantity: i(&v, "tbq"),
        total_sell_quantity: i(&v, "tsq"),
        timestamp_ms: now_ms,
        ..Default::default()
    };
    if !d.contains_key("lp") {
        t.mark_absent(QuoteField::Ltp);
    }
    for (key, field) in QUOTE_KEYS {
        if !d.contains_key(key) {
            t.mark_absent(field);
        }
    }
    open_interest(cfg, &mut t, d, &v, depth_packet);
    change(&mut t, d, &v);
    let feed_secs = d.contains_key("ft").then(|| i(&v, "ft"));
    t.last_trade_time_ms = trade_time_ms(&text(&v, "ltt"), feed_secs, now_ms);
    if t.last_trade_time_ms <= 0 {
        t.mark_absent(QuoteField::LastTradeTime);
    }
    t.extras = QuoteExtras {
        exchange_time_ms: feed_secs.filter(|s| *s > 0).map(|s| s * 1000),
        upper_circuit: d.contains_key("uc").then(|| f(&v, "uc")),
        lower_circuit: d.contains_key("lc").then(|| f(&v, "lc")),
        high_52_week: d.contains_key("52h").then(|| f(&v, "52h")),
        low_52_week: d.contains_key("52l").then(|| f(&v, "52l")),
        ..Default::default()
    };
    t
}

/// Open interest as the member's web adapter reads it: Shoonya, Flattrade
/// and Zebu `toi` in Depth, TradeSmart the native `oi` in Quote and Depth.
fn open_interest(
    cfg: &NorenConfig,
    t: &mut NormalizedTick,
    d: &Map<String, Value>,
    v: &Value,
    depth_packet: bool,
) {
    let key = match cfg.feed_oi {
        FeedOi::TotalInDepth => "toi",
        FeedOi::NativeFromQuote => "oi",
    };
    if !d.contains_key(key) {
        t.mark_absent(QuoteField::OpenInterest);
        return;
    }
    t.oi = i(v, key);
    if cfg.feed_oi == FeedOi::NativeFromQuote && !depth_packet {
        t.mark_carried(QuoteField::OpenInterest);
    }
}

/// Noren's `pc` when known (a 0 included), else no change at all: the web
/// only forwards `pc`, so the desktop works none out.
fn change(t: &mut NormalizedTick, d: &Map<String, Value>, v: &Value) {
    if !d.contains_key("pc") {
        t.mark_absent(QuoteField::Change);
        return;
    }
    t.change_percent = f(v, "pc");
    if d.contains_key("c") && t.close > 0.0 && t.ltp > 0.0 {
        t.change = round2(t.ltp - t.close);
    }
    t.mark_carried(QuoteField::Change);
}

/// Noren's trade time `ltt` (`HH:MM:SS`, IST) on the trading day of the
/// frame's feed time `ft` (epoch seconds; the receipt time when the frame
/// has none), in epoch milliseconds; 0 when the frame has no readable one.
/// A time later than the feed time is the evening before (a frame read
/// just after midnight).
fn trade_time_ms(ltt: &str, feed_secs: Option<i64>, now_ms: i64) -> i64 {
    let mut parts = ltt.trim().split(':').map(|p| p.parse::<i64>().ok());
    let (Some(Some(h)), Some(Some(m)), Some(Some(s)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return 0;
    };
    if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..60).contains(&s) {
        return 0;
    }
    let reference = feed_secs.filter(|s| *s > 0).unwrap_or(now_ms / 1000);
    let midnight = reference - (reference + IST_OFFSET_SECS).rem_euclid(DAY_SECS);
    let mut at = midnight + h * 3600 + m * 60 + s;
    if at > reference + 60 {
        at -= DAY_SECS;
    }
    at * 1000
}

/// The book of a scrip's merged snapshot `d`, received at `now_ms`: the
/// levels of Noren's five fixed slots (`bp1`..`bp5`, `sp1`..`sp5`) the
/// broker has sent, a slot sent as 0 included; a slot it never sent is not
/// a level (the web pads it with `safe_float`'s 0). It says which of its
/// totals and LTP the snapshot never held.
pub(super) fn depth(sub: Instrument<'_>, d: &Map<String, Value>, now_ms: i64) -> NormalizedDepth {
    let v = Value::Object(d.clone());
    let side = |p: &str, q: &str, o: &str| -> Vec<DepthLevel> {
        (1..=5)
            .filter(|n| d.contains_key(&format!("{}{}", p, n)))
            .map(|n| DepthLevel {
                price: f(&v, &format!("{}{}", p, n)),
                quantity: i(&v, &format!("{}{}", q, n)),
                orders: i(&v, &format!("{}{}", o, n)),
            })
            .collect()
    };
    let mut absent = AbsentFields::default();
    for (key, field) in [
        ("tbq", QuoteField::TotalBuyQuantity),
        ("tsq", QuoteField::TotalSellQuantity),
        ("lp", QuoteField::Ltp),
    ] {
        if !d.contains_key(key) {
            absent = absent.with(field);
        }
    }
    NormalizedDepth {
        symbol: sub.symbol.to_string(),
        exchange: sub.exchange.to_string(),
        ltp: f(&v, "lp"),
        buy: side("bp", "bq", "bo"),
        sell: side("sp", "sq", "so"),
        total_buy_quantity: i(&v, "tbq"),
        total_sell_quantity: i(&v, "tsq"),
        timestamp_ms: now_ms,
        absent,
        ..Default::default()
    }
}
