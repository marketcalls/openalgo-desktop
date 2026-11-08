//! One XTS market-data message (the JSON shape of a `NNNN-json-*` event,
//! a 1105 text event or a decoded binary packet) as normalised events: a
//! tick for its code's mode, and for 1502 the book.

use super::super::mapping;
use crate::brokers::common::streaming::{
    now_ms, FeedEvent, FeedSubscription, NormalizedDepth, NormalizedTick,
};
use crate::brokers::types::DepthLevel;
use serde_json::Value;

/// XTS times are seconds since 1980-01-01 IST.
pub const XTS_EPOCH_OFFSET_SECS: i64 = 315_532_800 - 19_800;

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

fn depth_side(v: Option<&Value>) -> Vec<DepthLevel> {
    let mut out: Vec<DepthLevel> = v
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|l| l.is_object())
                .take(20)
                .map(|l| DepthLevel {
                    price: mapping::f(l, "Price"),
                    quantity: mapping::i(l, "Size"),
                    orders: mapping::i(l, "TotalOrders"),
                })
                .collect()
        })
        .unwrap_or_default();
    if out.len() < 5 {
        out.resize(5, DepthLevel::default());
    }
    out
}

/// web `_normalize_market_data` (`adapter.py:970-1104`). A message without
/// a `MessageCode` (1105 text) is a touchline.
pub fn normalise_message(v: &Value, sub: &FeedSubscription) -> Vec<FeedEvent> {
    let mode = match v.get("MessageCode") {
        None => 2,
        Some(_) => match mode_for_code(mapping::i(v, "MessageCode")) {
            Some(m) => m,
            None => return Vec::new(),
        },
    };
    let t = v.get("Touchline").filter(|t| t.is_object()).unwrap_or(v);
    let ltq = mapping::num(
        t.get("LastTradedQunatity")
            .or_else(|| t.get("LastTradedQuantity")),
    ) as i64;
    let avg = mapping::num(
        t.get("AverageTradedPrice")
            .or_else(|| t.get("AveragePrice")),
    );
    let now = now_ms();
    let mut tick = NormalizedTick {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        mode,
        ltp: mapping::f(t, "LastTradedPrice"),
        last_quantity: ltq,
        last_trade_time_ms: xts_time_ms(mapping::i(t, "LastTradedTime")),
        timestamp_ms: now,
        ..Default::default()
    };
    if mode >= 2 {
        tick.open = mapping::f(t, "Open");
        tick.high = mapping::f(t, "High");
        tick.low = mapping::f(t, "Low");
        tick.close = mapping::f(t, "Close");
        tick.volume = mapping::i(t, "TotalTradedQuantity");
        tick.average_price = avg;
        tick.total_buy_quantity = mapping::i(t, "TotalBuyQuantity");
        tick.total_sell_quantity = mapping::i(t, "TotalSellQuantity");
        tick.derive_change();
    }
    if mode == 3 {
        tick.oi = mapping::i(v, "OpenInterest");
    }
    let mut out = Vec::with_capacity(2);
    let depth = (mode == 3 && v.get("Bids").is_some() && v.get("Asks").is_some()).then(|| {
        NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: tick.ltp,
            buy: depth_side(v.get("Bids")),
            sell: depth_side(v.get("Asks")),
            total_buy_quantity: tick.total_buy_quantity,
            total_sell_quantity: tick.total_sell_quantity,
            timestamp_ms: now,
            ..Default::default()
        }
    });
    out.push(FeedEvent::Tick(tick));
    if let Some(d) = depth {
        out.push(FeedEvent::Depth(d));
    }
    out
}
