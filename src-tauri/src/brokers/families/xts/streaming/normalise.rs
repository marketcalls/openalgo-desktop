//! One XTS market-data message (the JSON shape of a `NNNN-json-*` event,
//! a 1105 text event or a decoded binary packet) as normalised events: a
//! tick for its code's mode, and for 1502 the book.
//!
//! A tick carries what its message carries (audit C03, appendix item 1): a
//! partial event, a 1105 text packet or a 1501 at the root names only some
//! fields, and every field it does not name is marked absent, so the
//! bridge keeps the instrument's last value instead of a 0 (the web sends
//! `.get(x, 0)`). An event without a trade time sends no `ltt` rather than
//! the time it was received. The book is the levels XTS sent, never padded
//! with zero levels (the web pads each side to five).

use super::super::mapping;
use crate::brokers::common::streaming::QuoteField as F;
use crate::brokers::common::streaming::{
    now_ms, AbsentFields, FeedEvent, FeedSubscription, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::brokers::types::DepthLevel;
use serde_json::Value;

/// XTS times are seconds since 1980-01-01 IST.
pub const XTS_EPOCH_OFFSET_SECS: i64 = 315_532_800 - 19_800;

/// The most levels a book side carries (web `_extract_depth_data`
/// `depth[:20]`).
const MAX_LEVELS: usize = 20;

/// The quote fields of a 1501 or 1502 message and the keys XTS sends them
/// under (web `_normalize_market_data`).
const QUOTE_KEYS: &[(QuoteField, &[&str])] = &[
    (F::Open, &["Open"]),
    (F::High, &["High"]),
    (F::Low, &["Low"]),
    (F::Close, &["Close"]),
    (F::Volume, &["TotalTradedQuantity"]),
    (F::AveragePrice, &["AverageTradedPrice", "AveragePrice"]),
    (
        F::LastQuantity,
        &["LastTradedQunatity", "LastTradedQuantity"],
    ),
    (F::TotalBuyQuantity, &["TotalBuyQuantity"]),
    (F::TotalSellQuantity, &["TotalSellQuantity"]),
];

/// The book totals a book message takes from its tick.
const BOOK_TOTALS: AbsentFields = AbsentFields::of(&[F::TotalBuyQuantity, F::TotalSellQuantity]);

fn mode_for_code(code: i64) -> Option<u8> {
    match code {
        1512 => Some(1),
        1501 => Some(2),
        1502 => Some(3),
        _ => None,
    }
}

/// XTS `LastTradedTime` (seconds since 1980-01-01 IST) -> epoch ms.
pub fn xts_time_ms(t: i64) -> i64 {
    if t <= 0 {
        0
    } else {
        (t + XTS_EPOCH_OFFSET_SECS) * 1000
    }
}

/// The first of `keys` that `v` carries as a number (or a numeric
/// string); `None` when it carries none of them.
pub(crate) fn number(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| match v.get(*k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// The levels of one book side as XTS sent them, at most twenty.
fn levels(v: Option<&Value>) -> Vec<DepthLevel> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|l| l.is_object())
                .take(MAX_LEVELS)
                .map(|l| DepthLevel {
                    price: mapping::f(l, "Price"),
                    quantity: mapping::i(l, "Size"),
                    orders: mapping::i(l, "TotalOrders"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The price, its trade time and the last quantity, which every code
/// carries; the last quantity counts beyond an LTP message's mode (the
/// web's XTS LTP data is `{ltp, ltt, ltq}`).
fn trade(tick: &mut NormalizedTick, t: &Value) {
    match number(t, &["LastTradedPrice"]) {
        Some(p) => tick.ltp = p,
        None => tick.mark_absent(F::Ltp),
    }
    match number(t, &["LastTradedTime"]).filter(|s| *s > 0.0) {
        Some(s) => tick.last_trade_time_ms = xts_time_ms(s as i64),
        None => tick.mark_absent(F::LastTradeTime),
    }
    if tick.mode == 1 {
        if let Some(q) = number(t, &["LastTradedQunatity", "LastTradedQuantity"]) {
            tick.last_quantity = q as i64;
            tick.mark_carried(F::LastQuantity);
        }
    }
}

/// The quote fields of a Quote or Depth message; each one it lacks is
/// marked absent.
fn quote(tick: &mut NormalizedTick, t: &Value) {
    for (field, keys) in QUOTE_KEYS {
        let Some(v) = number(t, keys) else {
            tick.mark_absent(*field);
            continue;
        };
        match field {
            F::Open => tick.open = v,
            F::High => tick.high = v,
            F::Low => tick.low = v,
            F::Close => tick.close = v,
            F::Volume => tick.volume = v as i64,
            F::AveragePrice => tick.average_price = v,
            F::LastQuantity => tick.last_quantity = v as i64,
            F::TotalBuyQuantity => tick.total_buy_quantity = v as i64,
            F::TotalSellQuantity => tick.total_sell_quantity = v as i64,
            _ => {}
        }
    }
    tick.derive_change();
}

/// web `_normalize_market_data` (`adapter.py:970-1104`). A message without
/// a `MessageCode` (1105 text on the template members) is a touchline.
/// The quote is read from `Touchline` when the message has one, else from
/// its root; open interest and the circuit limits from the root.
pub fn normalise_message(v: &Value, sub: &FeedSubscription) -> Vec<FeedEvent> {
    let mode = match v.get("MessageCode") {
        None => 2,
        Some(_) => match mode_for_code(mapping::i(v, "MessageCode")) {
            Some(m) => m,
            None => return Vec::new(),
        },
    };
    let t = v.get("Touchline").filter(|t| t.is_object()).unwrap_or(v);
    let now = now_ms();
    let mut tick = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode,
        timestamp_ms: now,
        ..Default::default()
    };
    trade(&mut tick, t);
    if mode >= 2 {
        quote(&mut tick, t);
    }
    if mode == 3 {
        match number(v, &["OpenInterest"]) {
            Some(oi) => tick.oi = oi as i64,
            None => tick.mark_absent(F::OpenInterest),
        }
    }
    tick.extras.upper_circuit = number(v, &["UpperCircuitLimit"]);
    tick.extras.lower_circuit = number(v, &["LowerCircuitLimit"]);
    let book = mode == 3 && v.get("Bids").is_some() && v.get("Asks").is_some();
    let depth = book.then(|| NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp: tick.ltp,
        buy: levels(v.get("Bids")),
        sell: levels(v.get("Asks")),
        total_buy_quantity: tick.total_buy_quantity,
        total_sell_quantity: tick.total_sell_quantity,
        timestamp_ms: now,
        absent: book_lacks(&tick),
    });
    let mut out = vec![FeedEvent::Tick(tick)];
    out.extend(depth.map(FeedEvent::Depth));
    out
}

/// What a 1502's book lacks with its touchline: the totals and the price
/// the touchline did not carry.
fn book_lacks(tick: &NormalizedTick) -> AbsentFields {
    let mut lacks = tick.absent.minus(AbsentFields::ALL.minus(BOOK_TOTALS));
    if tick.absent.contains(F::Ltp) {
        lacks = lacks.with(F::Ltp);
    }
    lacks
}
