//! The `market_data` frame: one update rendered for a subscriber at one
//! mode, in the web's key order, with the broker's web names beside the
//! canonical ones (`feed::dialect`). A field the update does not know is
//! left out, never sent as 0 (web openalgo#2376, audit C03).

use super::to_json;
use crate::brokers::common::streaming::{Number, NumberForm, TimeText};
use crate::feed::dialect::{dialect, DataMode, Dialect, Extra, Fold, Key, Source, Unit};
use crate::feed::source::{
    DepthBook, DepthLevel, InstrumentKey, MarketUpdate, Mode, QuoteFields, DEFAULT_DEPTH,
};
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

/// `market_data.data`, in the web's key order.
#[derive(Serialize)]
struct DataOut<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exchange: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<ModeOut>,
    ltp: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    ltt: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_quantity: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_price: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_buy_quantity: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_sell_quantity: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    high: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    low: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change_percent: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oi: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_interest: Option<Value>,
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
/// the order-flow fields of an index whose canonical keys the broker leaves
/// out at `content` (most web adapters' index dicts have none; mStock's
/// fixed 123-byte packet carries them, mstock_adapter.py:362-375). A web
/// name reads the same values, so it goes and stays with them; a field
/// the packet only filled with 0 is marked absent by the feed and never
/// reaches here.
fn shown_quote(update: &MarketUpdate, dia: &Dialect, content: Mode) -> QuoteFields {
    let mut q = update.quote.clone().unwrap_or_default();
    if update.key.is_index() {
        let off = |k: Key| !dia.keys.has(k, true, content);
        if off(Key::LastQuantity) {
            q.last_quantity = None;
        }
        if off(Key::AveragePrice) {
            q.average_price = None;
        }
        if off(Key::TotalBuyQuantity) {
            q.total_buy_quantity = None;
        }
        if off(Key::TotalSellQuantity) {
            q.total_sell_quantity = None;
        }
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

/// A trade time as text in `form`: `YYYY-MM-DD HH:MM:SS` in IST is what web
/// Python's `str(datetime.fromtimestamp(seconds))` writes on a host on IST
/// (the desktop keeps IST everywhere, so CI and every computer agree),
/// `HH:MM:SS` the time of day the Noren family and Definedge pass through,
/// and epoch seconds as a string Definedge's other form.
fn time_text(ms: i64, form: TimeText) -> Option<Value> {
    use chrono::TimeZone;
    let secs = ms.div_euclid(1000);
    let t = crate::clock::ist_offset().timestamp_opt(secs, 0).single()?;
    let text = match form {
        TimeText::DateTime => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        TimeText::TimeOfDay => t.format("%H:%M:%S").to_string(),
        TimeText::EpochSeconds => secs.to_string(),
    };
    Some(text.into())
}

/// The packet value `source` reads, for the number form the packet marked.
fn number_of(source: Source) -> Option<Number> {
    use Number as N;
    Some(match source {
        Source::Ltp => N::Ltp,
        Source::Open => N::Open,
        Source::High => N::High,
        Source::Low => N::Low,
        Source::Close => N::Close,
        Source::Volume => N::Volume,
        Source::LastQuantity => N::LastQuantity,
        Source::AveragePrice => N::AveragePrice,
        Source::TotalBuyQuantity => N::TotalBuyQuantity,
        Source::TotalSellQuantity => N::TotalSellQuantity,
        Source::Oi => N::OpenInterest,
        Source::Change => N::Change,
        Source::ChangePercent => N::ChangePercent,
        Source::TradeTime(_) => N::LastTradeTime,
        Source::Extra(e) => match e {
            Extra::UpperCircuit => N::UpperCircuit,
            Extra::LowerCircuit => N::LowerCircuit,
            Extra::High52Week => N::High52Week,
            Extra::Low52Week => N::Low52Week,
            Extra::BidPrice => N::BidPrice,
            Extra::BidQuantity => N::BidQuantity,
            Extra::AskPrice => N::AskPrice,
            Extra::AskQuantity => N::AskQuantity,
            Extra::PreviousOi => N::PreviousOi,
            Extra::OiChange => N::OiChange,
            Extra::TotalOi => N::TotalOi,
            Extra::DayClose => N::DayClose,
            Extra::ExchangeTime(_) => return None,
        },
        Source::FiftyLevels(inner) => return number_of(*inner),
        _ => return None,
    })
}

/// `v` in the JSON number form the packet marked for its value, else in
/// the broker's (`Dialect::floats` names the keys its web adapter writes
/// as doubles), else as it is: an integer for a count, a double for a
/// price.
fn shaped(v: Value, number: Option<Number>, key: &str, q: &QuoteFields, dia: &Dialect) -> Value {
    match number.and_then(|n| q.numbers.form(n)) {
        Some(NumberForm::Integer) => as_integer(v),
        Some(NumberForm::Double) => as_double(v),
        None if dia.floats.contains(&key) => as_double(v),
        None => v,
    }
}

/// An integer as a double (`4.0`).
fn as_double(v: Value) -> Value {
    match v.as_i64() {
        Some(i) => Value::from(i as f64),
        None => v,
    }
}

/// A whole double as an integer (445); a fraction stays a double.
fn as_integer(v: Value) -> Value {
    match v.as_f64() {
        Some(x) if v.is_f64() && x.fract() == 0.0 && x.abs() < 9.0e15 => Value::from(x as i64),
        _ => v,
    }
}

/// The levels this frame's book was streamed at: the holder's depth, or the
/// main feed's where its deep subscription fell back or has not brought its
/// first book yet (web Dhan names 5 then, dhan_adapter.py:930, 1042; web
/// Fyers sends no 50-level frame).
///
/// A lower holder of a one-dict broker's packet (`Fold::Richer`) is shown
/// the packet's whole main-feed book (a deep book never reaches it), so it
/// names that book's levels: the main feed's unless the book says (web
/// Dhan sends the full packet's `depth_level` 5 to its Quote client too).
fn served(fr: &Frame<'_>) -> u8 {
    let levels = book_for(fr.update, fr.depth).and_then(|b| b.levels);
    if fr.mode < Mode::Depth {
        return levels.unwrap_or(DEFAULT_DEPTH);
    }
    levels.map_or(fr.depth, |l| l.min(fr.depth))
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
        Source::TradeTimeText => q
            .trade_time
            .and_then(|ms| time_text(ms, q.time_text.unwrap_or(TimeText::DateTime))),
        Source::TradeTimeOfDay => q
            .trade_time
            .and_then(|ms| time_text(ms, q.time_text.unwrap_or(TimeText::TimeOfDay))),
        Source::TradeTimeMarked => {
            let form = q.time_text?;
            q.trade_time.and_then(|ms| time_text(ms, form))
        }
        Source::Extra(e) => extra(e, q),
        Source::DepthLevels => {
            // A lower holder names levels only of a book its frame shows.
            let shown = fr.mode == Mode::Depth || fr.update.depth.is_some();
            let book = fr.content == Mode::Depth && !fr.update.key.is_index() && shown;
            book.then(|| Value::from(served(fr)))
        }
        Source::BookLevels => {
            let b = book_for(fr.update, fr.depth).filter(|_| fr.content == Mode::Depth)?;
            let present = b.buy.len().max(b.sell.len()).min(usize::from(fr.depth));
            Some(Value::from(present))
        }
        Source::BookTime(unit) => book_for(fr.update, fr.depth)
            .and_then(|b| b.time_ms)
            .map(|ms| time(ms, unit)),
        Source::ExchangeToken => {
            let token = fr
                .update
                .token
                .as_deref()
                .filter(|_| !fr.update.key.is_index())?;
            Some(Value::from(token.get(10..).unwrap_or(token)))
        }
        Source::BrokerSymbol => fr.update.broker_symbol.as_deref().map(Value::from),
        Source::FiftyOr(fifty, other) => {
            let at_fifty = fr.content == Mode::Depth && served(fr) == 50;
            value(if at_fifty { *fifty } else { *other }, fr)
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
        Source::True => Some(Value::Bool(true)),
        Source::FiftyLevels(inner) => {
            let fifty = fr.content == Mode::Depth && served(fr) == 50;
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
    let dia = dialect(fr.broker);
    for name in dia.names {
        if fr.content < name.from || fr.content > name.to || taken(&out, name.key) {
            continue;
        }
        if let Some(v) = value(name.source, fr) {
            let v = shaped(v, number_of(name.source), name.key, fr.q, &dia);
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

/// Whether a holder at `mode` gets a frame of `update`: not above the
/// update's mode, only its own mode for an exact one (a book on its own),
/// and at Depth without a book only where the broker's web adapter
/// publishes such a packet on its Depth topic (`Dialect::depth_needs_book`).
pub(crate) fn reaches(update: &MarketUpdate, mode: Mode, depth: u8, broker: &str) -> bool {
    if mode > update.mode || (update.exact_mode && mode != update.mode) {
        return false;
    }
    if mode != Mode::Depth {
        return true;
    }
    // A main-feed book is not for a holder that has its own book from the
    // deeper socket (its frames carry that book).
    if update.exact_mode && update.depth.is_some() && own_deep_book(update, depth).is_some() {
        return false;
    }
    if book_for(update, depth).is_some() {
        return true;
    }
    // No book for this holder: a book event for other holders reaches it
    // not at all; a tick reaches it without a book unless its broker's web
    // adapter waits for the book.
    !update.exact_mode && !dialect(broker).depth_needs_book
}

/// The deeper socket's book for a holder at `depth`: the instrument's latest
/// on this connection of that socket, while its subscription streams from
/// there. A reconnect of the socket forgets it (`snapshot::forget_session`).
fn own_deep_book(update: &MarketUpdate, depth: u8) -> Option<&DepthBook> {
    update
        .deep_book
        .as_ref()
        .filter(|_| update.deep_live == Some(depth))
}

/// The book a Depth holder at `depth` gets from `update`: its own from the
/// deeper socket once that has come (a Dhan 20 or Fyers 50 holder then never
/// gets the main feed's 5 levels in between, which would show its book
/// shrinking and growing; web defect: the web's listener sends every Depth
/// publish of a symbol to every Depth client), else the main feed's, cut to
/// `depth`. Before its own first book that is the shallower book, named by
/// the levels it really has (`served`), as after a fallback: the web sends a
/// book in that frame, and a client reads `data['depth']` on every one.
fn book_for(update: &MarketUpdate, depth: u8) -> Option<&DepthBook> {
    own_deep_book(update, depth).or(update.depth.as_ref())
}

/// Render `update` for a subscriber at `mode` (which must not exceed
/// `update.mode`), with each depth side truncated to `depth` levels.
pub fn market_data(update: &MarketUpdate, mode: Mode, depth: u8, broker: &str) -> String {
    let key: &InstrumentKey = &update.key;
    let dia = dialect(broker);
    let content = match dia.fold {
        Fold::PerMode => mode,
        Fold::Richer => update.mode.max(mode),
    };
    let q = shown_quote(update, &dia, content);
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
    let held = if mode == Mode::Depth {
        book_for(update, depth)
    } else {
        update.depth.as_ref()
    };
    match held {
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
    use Number as N;
    let key = &update.key;
    let num =
        |name: &str, n: Number, v: Option<Value>| v.map(|v| shaped(v, Some(n), name, qf, dia));
    let i = |v: Option<i64>| v.map(Value::from);
    let f = |v: Option<f64>| v.map(Value::from);
    let ltp_copy = mode == Mode::Ltp && update.mode > Mode::Ltp;
    // `data.mode`: the dict's own mode, or the holder's where the web
    // adapter writes each subscription's mode into the dict it publishes.
    let written = if dia.mode_of_holder { mode } else { content };
    let on = |k: Key| dia.keys.has(k, key.is_index(), content);
    let ltt = update
        .ltt
        .filter(|_| on(Key::Ltt) && (dia.ltt_on_ltp_copies || !ltp_copy));
    DataOut {
        symbol: on(Key::Symbol).then_some(key.symbol.as_str()),
        exchange: on(Key::Exchange).then_some(key.exchange.as_str()),
        mode: on(Key::Mode)
            .then_some(match dia.data_mode {
                DataMode::Label => Some(ModeOut::Label(written.data_label())),
                DataMode::Number => Some(ModeOut::Number(written.as_u8())),
                DataMode::Absent => None,
            })
            .flatten(),
        ltp: shaped(Value::from(update.ltp), Some(N::Ltp), "ltp", qf, dia),
        ltt: num("ltt", N::LastTradeTime, i(ltt)),
        timestamp: on(Key::Timestamp).then_some(update.timestamp),
        volume: num(
            "volume",
            N::Volume,
            i(qf.volume.filter(|_| on(Key::Volume))),
        ),
        last_quantity: num(
            "last_quantity",
            N::LastQuantity,
            i(qf.last_quantity.filter(|_| on(Key::LastQuantity))),
        ),
        average_price: num(
            "average_price",
            N::AveragePrice,
            f(qf.average_price.filter(|_| on(Key::AveragePrice))),
        ),
        total_buy_quantity: num(
            "total_buy_quantity",
            N::TotalBuyQuantity,
            i(qf.total_buy_quantity.filter(|_| on(Key::TotalBuyQuantity))),
        ),
        total_sell_quantity: num(
            "total_sell_quantity",
            N::TotalSellQuantity,
            i(qf.total_sell_quantity
                .filter(|_| on(Key::TotalSellQuantity))),
        ),
        open: num("open", N::Open, f(qf.open.filter(|_| on(Key::Open)))),
        high: num("high", N::High, f(qf.high.filter(|_| on(Key::High)))),
        low: num("low", N::Low, f(qf.low.filter(|_| on(Key::Low)))),
        close: num("close", N::Close, f(qf.close.filter(|_| on(Key::Close)))),
        price_change: num(
            "price_change",
            N::Change,
            f(qf.price_change.filter(|_| on(Key::PriceChange))),
        ),
        price_change_percent: num(
            "price_change_percent",
            N::ChangePercent,
            f(qf.price_change_percent
                .filter(|_| on(Key::PriceChangePercent))),
        ),
        oi: num("oi", N::OpenInterest, i(qf.oi.filter(|_| on(Key::Oi)))),
        open_interest: num(
            "open_interest",
            N::OpenInterest,
            i(qf.oi.filter(|_| on(Key::OpenInterest))),
        ),
        depth: None,
        web: WebKeys::default(),
    }
}

#[cfg(test)]
mod tests;
