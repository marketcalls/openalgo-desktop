//! The `market_data` frame: one update rendered for a subscriber at one
//! mode, in the web's key order, with the broker's web names beside the
//! canonical ones (`feed::dialect`). A field the update does not know is
//! left out, never sent as 0 (web openalgo#2376, audit C03).

use super::to_json;
use crate::feed::dialect::{dialect, DataMode, Dialect, Extra, Fold, Key, Source, Unit};
use crate::feed::source::{DepthLevel, InstrumentKey, MarketUpdate, Mode, QuoteFields};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct LevelOut {
    price: f64,
    quantity: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    orders: Option<i64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<i64>,
}

/// `data.mode` as the broker's web adapter writes it.
#[derive(Serialize)]
#[serde(untagged)]
enum ModeOut {
    Label(&'static str),
    Number(u8),
}

/// The broker's own keys (`dialect::names`), each once, after the
/// canonical ones.
#[derive(Default)]
struct WebKeys(Vec<(&'static str, Value)>);

impl Serialize for WebKeys {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// An integer as the broker's web adapter types it: an int, or a double
/// where its packet carries one (`Dialect::floats`).
#[derive(Serialize)]
#[serde(untagged)]
enum Num {
    Int(i64),
    Float(f64),
}

/// `market_data.data`, in the web's key order.
#[derive(Serialize)]
struct DataOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exchange: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<ModeOut>,
    ltp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ltt: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_quantity: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_buy_quantity: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_sell_quantity: Option<Num>,
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
    oi: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_interest: Option<Num>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<BookOut>,
    #[serde(flatten)]
    web: WebKeys,
}

impl DataOut<'_> {
    /// Whether the canonical part already carries `key`.
    fn has(&self, key: &str) -> bool {
        match key {
            "symbol" => self.symbol.is_some(),
            "exchange" => self.exchange.is_some(),
            "ltp" => true,
            "timestamp" => self.timestamp.is_some(),
            "mode" => self.mode.is_some(),
            "ltt" => self.ltt.is_some(),
            "volume" => self.volume.is_some(),
            "last_quantity" => self.last_quantity.is_some(),
            "average_price" => self.average_price.is_some(),
            "total_buy_quantity" => self.total_buy_quantity.is_some(),
            "total_sell_quantity" => self.total_sell_quantity.is_some(),
            "open" => self.open.is_some(),
            "high" => self.high.is_some(),
            "low" => self.low.is_some(),
            "close" => self.close.is_some(),
            "price_change" => self.price_change.is_some(),
            "price_change_percent" => self.price_change_percent.is_some(),
            "oi" => self.oi.is_some(),
            "open_interest" => self.open_interest.is_some(),
            "depth" => self.depth.is_some(),
            _ => false,
        }
    }
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

/// The values a frame may show, under any name: the update's quote without
/// an index's order-flow fields (no web adapter's index dict has them).
/// Which canonical keys carry them is the broker's `Keys`; a web name reads
/// the value whatever its canonical key does.
fn shown_quote(update: &MarketUpdate) -> QuoteFields {
    let mut q = update.quote.clone().unwrap_or_default();
    if update.key.is_index() {
        q.last_quantity = None;
        q.average_price = None;
        q.total_buy_quantity = None;
        q.total_sell_quantity = None;
    }
    q
}

/// What a frame knows besides the quote, for the broker's own keys.
struct Frame<'a> {
    update: &'a MarketUpdate,
    q: &'a QuoteFields,
    /// The holder's mode (the envelope's).
    mode: Mode,
    /// The mode whose keys the frame carries: the holder's, or the
    /// packet's for a broker whose web adapter sends lower holders the
    /// whole packet (`Fold::Richer`).
    content: Mode,
    depth: u8,
    broker: &'a str,
}

fn extra(e: Extra, q: &QuoteFields) -> Option<Value> {
    let x = &q.extras;
    let f = |v: Option<f64>| v.map(Value::from);
    let i = |v: Option<i64>| v.map(Value::from);
    match e {
        Extra::UpperCircuit => f(x.upper_circuit),
        Extra::LowerCircuit => f(x.lower_circuit),
        Extra::High52Week => f(x.high_52_week),
        Extra::Low52Week => f(x.low_52_week),
        Extra::BidPrice => f(x.bid_price),
        Extra::BidQuantity => i(x.bid_quantity),
        Extra::AskPrice => f(x.ask_price),
        Extra::AskQuantity => i(x.ask_quantity),
        Extra::PreviousOi => i(x.previous_oi),
        Extra::OiChange => i(x.oi_change),
        Extra::TotalOi => i(x.total_oi),
        Extra::DayClose => f(x.day_close),
        Extra::ExchangeTime(unit) => x.exchange_time_ms.map(|ms| time(ms, unit)),
    }
}

/// An epoch-millisecond time in the unit a broker's web name uses.
fn time(ms: i64, unit: Unit) -> Value {
    match unit {
        Unit::Millis => ms.into(),
        Unit::Seconds => (ms / 1000).into(),
    }
}

/// A trade time as text, `YYYY-MM-DD HH:MM:SS` in IST: what web Python's
/// `str(datetime.fromtimestamp(seconds))` writes on a host on IST (the
/// desktop keeps IST everywhere, so CI and every computer agree).
fn time_text(ms: i64) -> Option<Value> {
    use chrono::TimeZone;
    let t = crate::clock::ist_offset()
        .timestamp_opt(ms.div_euclid(1000), 0)
        .single()?;
    Some(t.format("%Y-%m-%d %H:%M:%S").to_string().into())
}

/// The value of `source` in this frame, if known.
fn value(source: Source, fr: &Frame<'_>) -> Option<Value> {
    let q = fr.q;
    let f = |v: Option<f64>| v.map(Value::from);
    let i = |v: Option<i64>| v.map(Value::from);
    match source {
        Source::Ltp => Some(Value::from(fr.update.ltp)),
        Source::Open => f(q.open),
        Source::High => f(q.high),
        Source::Low => f(q.low),
        Source::Close => f(q.close),
        Source::Volume => i(q.volume),
        Source::LastQuantity => i(q.last_quantity),
        Source::AveragePrice => f(q.average_price),
        Source::TotalBuyQuantity => i(q.total_buy_quantity),
        Source::TotalSellQuantity => i(q.total_sell_quantity),
        Source::Oi => i(q.oi),
        Source::Change => f(q.price_change),
        Source::ChangePercent => f(q.price_change_percent),
        Source::TradeTime(unit) => fr.update.ltt.map(|ms| time(ms, unit)),
        Source::TradeTimeText => q.trade_time.and_then(time_text),
        Source::Extra(e) => extra(e, q),
        Source::DepthLevels => {
            let book = fr.content == Mode::Depth && !fr.update.key.is_index();
            book.then(|| Value::from(fr.depth))
        }
        Source::SubscriptionMode => Some(Value::from(fr.mode.as_u8())),
        Source::Topic => Some(Value::from(format!(
            "{}_{}_{}",
            fr.update.key.exchange,
            fr.update.key.symbol,
            ["LTP", "QUOTE", "DEPTH"][fr.mode.index()]
        ))),
        Source::BrokerId => Some(Value::from(fr.broker)),
        Source::PerMode(texts) => Some(Value::from(texts[fr.mode.index()])),
        Source::Token => fr.update.token.as_deref().map(Value::from),
        Source::Timestamp(unit) => Some(time(fr.update.timestamp, unit)),
        Source::True => Some(Value::Bool(true)),
        Source::FiftyLevels(inner) => {
            let fifty = fr.content == Mode::Depth && fr.depth == 50;
            fifty.then(|| value(*inner, fr)).flatten()
        }
    }
}

/// The broker's own keys for this frame: its table's names from their
/// mode up, then the broker-only fields the packet carried, each only when
/// known and never over a key already in the frame.
fn web_keys(d: &DataOut<'_>, fr: &Frame<'_>) -> WebKeys {
    let mut out: Vec<(&'static str, Value)> = Vec::new();
    let taken =
        |out: &[(&'static str, Value)], key: &str| d.has(key) || out.iter().any(|(k, _)| *k == key);
    for name in dialect(fr.broker).names {
        if fr.content < name.from || fr.content > name.to || taken(&out, name.key) {
            continue;
        }
        if let Some(v) = value(name.source, fr) {
            out.push((name.key, v));
        }
    }
    for (field, v) in fr.q.broker_fields.iter() {
        if fr.content.as_u8() < field.from_mode().code() || taken(&out, field.name()) {
            continue;
        }
        if let Ok(v) = serde_json::to_value(v) {
            out.push((field.name(), v));
        }
    }
    WebKeys(out)
}

/// Render `update` for a subscriber at `mode` (which must not exceed
/// `update.mode`), with each depth side truncated to `depth` levels.
pub fn market_data(update: &MarketUpdate, mode: Mode, depth: u8, broker: &str) -> String {
    let key: &InstrumentKey = &update.key;
    let dia = dialect(broker);
    let q = shown_quote(update);
    let content = match dia.fold {
        Fold::PerMode => mode,
        Fold::Richer => update.mode.max(mode),
    };
    let mut d = canonical(update, &q, mode, content, &dia);
    if dia.keys.has(Key::Depth, key.is_index(), content) {
        d.depth = book(update, mode, depth).map(|b| BookOut {
            timestamp: dia.book_time.then_some(update.timestamp),
            ..b
        });
    }
    let frame = Frame {
        update,
        q: &q,
        mode,
        content,
        depth,
        broker,
    };
    d.web = web_keys(&d, &frame);
    to_json(&MarketDataOut {
        kind: "market_data",
        symbol: &key.symbol,
        exchange: &key.exchange,
        mode: mode.as_u8(),
        data: d,
        broker,
    })
}

/// The book a frame carries: the levels the broker sent, up to the depth
/// the holder streams (a lower holder of a richer packet gets them all);
/// none for a full tick without its book; an empty one on an index whose
/// broker's web adapter sends one. Never padded.
fn book(update: &MarketUpdate, mode: Mode, depth: u8) -> Option<BookOut> {
    let n = if mode == Mode::Depth {
        depth as usize
    } else {
        usize::MAX
    };
    match &update.depth {
        Some(b) => Some(BookOut {
            buy: b.buy.iter().take(n).map(LevelOut::from).collect(),
            sell: b.sell.iter().take(n).map(LevelOut::from).collect(),
            timestamp: None,
        }),
        None if update.key.is_index() => Some(BookOut {
            buy: Vec::new(),
            sell: Vec::new(),
            timestamp: None,
        }),
        None => None,
    }
}

/// The canonical keys of `data` for a holder at `mode` of a frame carrying
/// `content`'s keys, shaped as the broker's web adapter shapes them: which
/// identity, time and OI keys it writes, how it writes `data.mode` (the
/// dict's own mode, as the web forwards it), which integers it sends as
/// doubles.
fn canonical<'a>(
    update: &'a MarketUpdate,
    qf: &QuoteFields,
    mode: Mode,
    content: Mode,
    dia: &Dialect,
) -> DataOut<'a> {
    let key = &update.key;
    let num = |name: &str, v: Option<i64>| {
        v.map(|v| {
            if dia.floats.contains(&name) {
                Num::Float(v as f64)
            } else {
                Num::Int(v)
            }
        })
    };
    let ltp_copy = mode == Mode::Ltp && update.mode > Mode::Ltp;
    let on = |k: Key| dia.keys.has(k, key.is_index(), content);
    let ltt = update
        .ltt
        .filter(|_| on(Key::Ltt) && (dia.ltt_on_ltp_copies || !ltp_copy));
    DataOut {
        symbol: on(Key::Symbol).then_some(key.symbol.as_str()),
        exchange: on(Key::Exchange).then_some(key.exchange.as_str()),
        mode: on(Key::Mode)
            .then_some(match dia.data_mode {
                DataMode::Label => Some(ModeOut::Label(content.data_label())),
                DataMode::Number => Some(ModeOut::Number(content.as_u8())),
                DataMode::Absent => None,
            })
            .flatten(),
        ltp: update.ltp,
        ltt: num("ltt", ltt),
        timestamp: on(Key::Timestamp).then_some(update.timestamp),
        volume: num("volume", qf.volume.filter(|_| on(Key::Volume))),
        last_quantity: num(
            "last_quantity",
            qf.last_quantity.filter(|_| on(Key::LastQuantity)),
        ),
        average_price: qf.average_price.filter(|_| on(Key::AveragePrice)),
        total_buy_quantity: num(
            "total_buy_quantity",
            qf.total_buy_quantity.filter(|_| on(Key::TotalBuyQuantity)),
        ),
        total_sell_quantity: num(
            "total_sell_quantity",
            qf.total_sell_quantity
                .filter(|_| on(Key::TotalSellQuantity)),
        ),
        open: qf.open.filter(|_| on(Key::Open)),
        high: qf.high.filter(|_| on(Key::High)),
        low: qf.low.filter(|_| on(Key::Low)),
        close: qf.close.filter(|_| on(Key::Close)),
        price_change: qf.price_change.filter(|_| on(Key::PriceChange)),
        price_change_percent: qf
            .price_change_percent
            .filter(|_| on(Key::PriceChangePercent)),
        oi: num("oi", qf.oi.filter(|_| on(Key::Oi))),
        open_interest: num("open_interest", qf.oi.filter(|_| on(Key::OpenInterest))),
        depth: None,
        web: WebKeys::default(),
    }
}

#[cfg(test)]
mod tests;
