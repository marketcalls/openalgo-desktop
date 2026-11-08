//! The `market_data` frame: one update rendered for a subscriber at one
//! mode, in the web's key order, with the broker's web names beside the
//! canonical ones (`feed::dialect`).

use super::to_json;
use crate::brokers::common::streaming::{AbsentFields, QuoteField};
use crate::feed::dialect::{aliases, Aliases, Canonical};
use crate::feed::source::{DepthLevel, InstrumentKey, MarketUpdate, Mode};
use serde::Serialize;

#[derive(Serialize)]
struct LevelOut {
    price: f64,
    quantity: i64,
    orders: i64,
}

impl From<&DepthLevel> for LevelOut {
    fn from(l: &DepthLevel) -> Self {
        Self {
            price: l.price,
            quantity: l.quantity,
            orders: l.orders,
        }
    }
}

#[derive(Serialize)]
struct BookOut {
    buy: Vec<LevelOut>,
    sell: Vec<LevelOut>,
}

/// `market_data.data`, in the web's key order.
#[derive(Serialize)]
struct DataOut<'a> {
    symbol: &'a str,
    exchange: &'a str,
    mode: &'static str,
    ltp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ltt: Option<i64>,
    timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_buy_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_sell_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oi: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_interest: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<BookOut>,
    /// The broker's web names for the same values (WS-05).
    #[serde(flatten)]
    aliases: Aliases,
}

#[derive(Serialize)]
struct MarketDataOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    symbol: &'a str,
    exchange: &'a str,
    mode: u8,
    data: DataOut<'a>,
    broker: &'a str,
}

/// Leave out what the broker's stream did not carry rather than send it as
/// 0 (web openalgo#2376); an alias goes with its canonical key.
fn leave_out(d: &mut DataOut<'_>, absent: AbsentFields) {
    fn drop_if<T>(slot: &mut Option<T>, absent: AbsentFields, field: QuoteField) {
        if absent.contains(field) {
            *slot = None;
        }
    }
    use QuoteField as F;
    drop_if(&mut d.open, absent, F::Open);
    drop_if(&mut d.high, absent, F::High);
    drop_if(&mut d.low, absent, F::Low);
    drop_if(&mut d.close, absent, F::Close);
    drop_if(&mut d.volume, absent, F::Volume);
    drop_if(&mut d.average_price, absent, F::AveragePrice);
    drop_if(&mut d.last_quantity, absent, F::LastQuantity);
    drop_if(&mut d.total_buy_quantity, absent, F::TotalBuyQuantity);
    drop_if(&mut d.total_sell_quantity, absent, F::TotalSellQuantity);
    drop_if(&mut d.oi, absent, F::OpenInterest);
    drop_if(&mut d.open_interest, absent, F::OpenInterest);
    drop_if(&mut d.price_change, absent, F::Change);
    drop_if(&mut d.price_change_percent, absent, F::Change);
}

/// Render `update` for a subscriber at `mode` (which must not exceed
/// `update.mode`), with each depth side truncated to `depth` levels.
pub fn market_data(update: &MarketUpdate, mode: Mode, depth: u8, broker: &str) -> String {
    let key: &InstrumentKey = &update.key;
    let index = key.is_index();
    let mut d = DataOut {
        symbol: &key.symbol,
        exchange: &key.exchange,
        mode: mode.data_label(),
        ltp: update.ltp,
        ltt: update.ltt,
        timestamp: update.timestamp,
        volume: None,
        last_quantity: None,
        average_price: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        open: None,
        high: None,
        low: None,
        close: None,
        price_change: None,
        price_change_percent: None,
        oi: None,
        open_interest: None,
        depth: None,
        aliases: Aliases::default(),
    };
    if mode >= Mode::Quote {
        let q = update.quote.clone().unwrap_or_default();
        d.volume = Some(q.volume);
        if index {
            d.open = q.open;
            d.high = q.high;
            d.low = q.low;
            d.close = q.close;
            d.price_change = Some(q.price_change.unwrap_or(0.0));
            d.price_change_percent = Some(q.price_change_percent.unwrap_or(0.0));
        } else {
            d.last_quantity = Some(q.last_quantity);
            d.average_price = Some(q.average_price);
            d.total_buy_quantity = Some(q.total_buy_quantity);
            d.total_sell_quantity = Some(q.total_sell_quantity);
            d.open = Some(q.open.unwrap_or(0.0));
            d.high = Some(q.high.unwrap_or(0.0));
            d.low = Some(q.low.unwrap_or(0.0));
            d.close = Some(q.close.unwrap_or(0.0));
            d.price_change = q.price_change;
            d.price_change_percent = q.price_change_percent;
        }
        if let Some(oi) = q.oi {
            d.oi = Some(oi);
            d.open_interest = Some(oi);
        } else if mode == Mode::Depth && !index {
            d.oi = Some(0);
            d.open_interest = Some(0);
        }
        leave_out(&mut d, q.absent);
    }
    d.aliases = aliases(
        broker,
        Canonical {
            last_quantity: d.last_quantity,
            average_price: d.average_price,
            price_change: d.price_change,
            price_change_percent: d.price_change_percent,
            ltt: d.ltt,
        },
    );
    if mode == Mode::Depth && !index {
        let n = depth as usize;
        let book = update.depth.as_ref();
        d.depth = Some(BookOut {
            buy: book
                .map(|b| b.buy.iter().take(n).map(LevelOut::from).collect())
                .unwrap_or_default(),
            sell: book
                .map(|b| b.sell.iter().take(n).map(LevelOut::from).collect())
                .unwrap_or_default(),
        });
    }
    to_json(&MarketDataOut {
        kind: "market_data",
        symbol: &key.symbol,
        exchange: &key.exchange,
        mode: mode.as_u8(),
        data: d,
        broker,
    })
}
