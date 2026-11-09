//! What a packet carries (audit C03) and the fields beyond the canonical
//! quote (C06). The adapter API is described in the parent module's doc.
//!
//! # What each broker's adapter fills
//!
//! Read from the web adapters at a1b9e3250 (every key each one publishes
//! per mode is in `docs/port/market-data-frames.md`; the names the frames
//! use are in `feed/dialect/names.rs`). Beside the canonical quote, the
//! web publishes, and the desktop adapter fills (`carried` / `absent`
//! marks, [`QuoteExtras`], [`BrokerField`]):
//!
//! * aliceblue: OI (as `total_oi`); marks for the `tf` fields it lacks.
//! * angel: Depth circuits (SNAP packet) (done).
//! * arrow: close and change in the 17-byte LTPC packet (`carried`), Depth
//!   circuits (241-byte packet).
//! * XTS family (compositedge, fivepaisaxts, ibulls, iifl, jainamxts,
//!   rmoney, wisdom): last quantity in 1512 LTP packets (`carried`), Depth
//!   OI and circuits from 1502.
//! * definedge: previous OI, total OI, best bid and ask, circuits, 52-week
//!   range.
//! * deltaexchange: best bid and ask (ticker `q`), the last book's totals on
//!   a ticker, `absent(Orders)` (done).
//! * dhan: previous close as `close`, the packet's close as `day_close`,
//!   previous OI, `OiHigh` / `OiLow`; no totals on a 20-level book (done).
//! * dhan_sandbox: the simulated feed's fields, drawn by the web's model;
//!   OI in Quote (`carried`), no change (`absent`) (done).
//! * firstock: OI in Quote (`carried`), circuits.
//! * fivepaisa: best bid and ask.
//! * Noren family (flattrade, shoonya, tradesmart, zebu): the feed time
//!   `ft` as `exchange_time_ms`, circuits, 52-week range; TradeSmart OI in
//!   Quote.
//! * fyers: best bid and ask, circuits, exchange time, `UpdateType`; the
//!   quote in LTP-mode packets (`carried`) (done).
//! * groww: `TradedValue`, best bid and ask from the book.
//! * hdfcsecurities, hdfcsky: circuits (full and CIRC packets), OI on derivative
//!   quote packets, index order flow absent.
//! * iiflcapital, nubra, samco: best bid and ask (nubra done).
//! * indmoney: best bid and ask, OI change, OI in Quote.
//! * kotak: OI in Quote (`carried`, market pictures); the packet's own
//!   change (`carried`).
//! * motilal: volume in LTP packets (`carried`), circuits.
//! * mstock: exchange time, circuits.
//! * paytm: 52-week range, OI change.
//! * pocketful: change and OI in compact (LTP) packets, best bid and ask.
//! * rupeezy, tradejini: circuits; Tradejini OI in Quote.
//! * sahi: circuits, OI in Quote.
//! * upstox: last quantity and close in LTPC packets (`carried`), the
//!   auction fields, `absent(Orders)` (done).
//! * zerodha: the index packet's change (`carried`) and exchange time
//!   (done).

use super::FeedMode;
use serde::ser::{Serialize, SerializeMap, Serializer};

/// A field of a `market_data` frame whose presence a packet decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QuoteField {
    Open = 1 << 0,
    High = 1 << 1,
    Low = 1 << 2,
    /// Previous close.
    Close = 1 << 3,
    Volume = 1 << 4,
    AveragePrice = 1 << 5,
    LastQuantity = 1 << 6,
    TotalBuyQuantity = 1 << 7,
    TotalSellQuantity = 1 << 8,
    OpenInterest = 1 << 9,
    /// The change since the previous close, absolute and in percent.
    Change = 1 << 10,
    /// The exchange's last trade time (an index tick may have none): `ltt`
    /// is then left out in every mode, LTP included.
    LastTradeTime = 1 << 11,
    /// The last traded price. A packet without a trade (an OI, circuit or
    /// book-only packet) marks it absent and leaves `ltp` at 0.
    Ltp = 1 << 12,
    /// The order count of each book level (`NormalizedDepth::absent`;
    /// Upstox's book has none).
    Orders = 1 << 13,
}

/// A set of [`QuoteField`]s. Named for its first use, the fields a packet
/// does not carry (`NormalizedTick::absent`); the same set names what a
/// packet carries beyond its mode (`NormalizedTick::carried`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AbsentFields(u32);

impl AbsentFields {
    /// Every field.
    pub const ALL: Self = Self((1 << 14) - 1);

    pub const fn of(fields: &[QuoteField]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < fields.len() {
            bits |= fields[i] as u32;
            i += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, field: QuoteField) -> bool {
        self.0 & field as u32 != 0
    }

    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// This set and `field`: an adapter that learns field by field what a
    /// message lacks.
    pub const fn with(self, field: QuoteField) -> Self {
        Self(self.0 | field as u32)
    }

    pub const fn without(self, field: QuoteField) -> Self {
        Self(self.0 & !(field as u32))
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// What a packet of `mode` carries unless it marks a field absent: the
    /// last traded price; in Quote the day's quote, the previous close and
    /// the change; in Depth open interest too. The trade time goes by its
    /// value (`NormalizedTick::known`).
    pub const fn of_mode(mode: u8) -> Self {
        const LTP: AbsentFields = AbsentFields::of(&[QuoteField::Ltp]);
        const QUOTE: AbsentFields = LTP.union(AbsentFields::of(&[
            QuoteField::Open,
            QuoteField::High,
            QuoteField::Low,
            QuoteField::Close,
            QuoteField::Volume,
            QuoteField::AveragePrice,
            QuoteField::LastQuantity,
            QuoteField::TotalBuyQuantity,
            QuoteField::TotalSellQuantity,
            QuoteField::Change,
        ]));
        match mode {
            2 => QUOTE,
            m if m >= 3 => QUOTE.with(QuoteField::OpenInterest),
            _ => LTP,
        }
    }
}

/// The optional fields several brokers' web adapters publish with the
/// same meaning (C06): `None` when this packet did not carry the value,
/// never a default. The bridge keeps an instrument's last value of each
/// within one connection, as it keeps its quote. `feed::dialect` renders
/// each under the names the broker's web adapter uses, and only those.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct QuoteExtras {
    /// Today's price band (circuit limits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_circuit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_circuit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_52_week: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_52_week: Option<f64>,
    /// The best bid and ask of the packet (its own top-of-book fields, or
    /// level 1 where the web adapter takes it from the book).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_quantity: Option<i64>,
    /// The previous session's open interest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_oi: Option<i64>,
    /// Open interest against the previous session's, as the broker sends it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oi_change: Option<i64>,
    /// The broker's total open interest where it sends one beside `oi`
    /// (Definedge `total_oi`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_oi: Option<i64>,
    /// Today's close as the exchange sends it, beside the previous close
    /// (`close`; Dhan's `day_close`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day_close: Option<f64>,
    /// The packet's own exchange or feed time, epoch milliseconds (Zerodha
    /// `exchange_timestamp`, the Noren family's `<broker>_timestamp`). It
    /// belongs to this packet alone: the next packet's frames carry the
    /// next packet's time or none, never this one sent again as current
    /// (web mStock sends no `exchange_timestamp` for a packet without one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange_time_ms: Option<i64>,
    /// What this packet says is gone (`clear`): not kept from earlier
    /// packets, where a `None` keeps the last value.
    #[serde(skip)]
    pub cleared: ClearedExtras,
}

/// One of the [`QuoteExtras`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ExtraField {
    UpperCircuit = 1 << 0,
    LowerCircuit = 1 << 1,
    High52Week = 1 << 2,
    Low52Week = 1 << 3,
    BidPrice = 1 << 4,
    BidQuantity = 1 << 5,
    AskPrice = 1 << 6,
    AskQuantity = 1 << 7,
    PreviousOi = 1 << 8,
    OiChange = 1 << 9,
    TotalOi = 1 << 10,
    DayClose = 1 << 11,
}

/// The [`ExtraField`]s a packet clears.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearedExtras(u16);

impl ClearedExtras {
    pub const fn contains(self, field: ExtraField) -> bool {
        self.0 & field as u16 != 0
    }
}

impl QuoteExtras {
    /// No value: what it clears is not a value.
    pub fn is_empty(&self) -> bool {
        Self {
            cleared: ClearedExtras::default(),
            ..*self
        } == Self::default()
    }

    /// `field` is gone (a book that emptied both sides has no best bid or
    /// ask): left out of this packet's frames and not kept from earlier
    /// ones, unlike a `None`, which keeps the last value.
    pub fn clear(&mut self, field: ExtraField) {
        use ExtraField as X;
        match field {
            X::UpperCircuit => self.upper_circuit = None,
            X::LowerCircuit => self.lower_circuit = None,
            X::High52Week => self.high_52_week = None,
            X::Low52Week => self.low_52_week = None,
            X::BidPrice => self.bid_price = None,
            X::BidQuantity => self.bid_quantity = None,
            X::AskPrice => self.ask_price = None,
            X::AskQuantity => self.ask_quantity = None,
            X::PreviousOi => self.previous_oi = None,
            X::OiChange => self.oi_change = None,
            X::TotalOi => self.total_oi = None,
            X::DayClose => self.day_close = None,
        }
        self.cleared.0 |= field as u16;
    }

    /// The best bid and ask, with their quantities, are gone.
    pub fn clear_best_bid_ask(&mut self) {
        for f in [
            ExtraField::BidPrice,
            ExtraField::BidQuantity,
            ExtraField::AskPrice,
            ExtraField::AskQuantity,
        ] {
            self.clear(f);
        }
    }

    /// These fields, each replaced by `newer`'s where it carries one and
    /// dropped where `newer` clears it; the packet's own time is `newer`'s,
    /// none included.
    pub fn overlay(self, newer: Self) -> Self {
        use ExtraField as X;
        let gone = newer.cleared;
        fn pick<T>(gone: bool, newer: Option<T>, older: Option<T>) -> Option<T> {
            if gone {
                None
            } else {
                newer.or(older)
            }
        }
        Self {
            upper_circuit: pick(
                gone.contains(X::UpperCircuit),
                newer.upper_circuit,
                self.upper_circuit,
            ),
            lower_circuit: pick(
                gone.contains(X::LowerCircuit),
                newer.lower_circuit,
                self.lower_circuit,
            ),
            high_52_week: pick(
                gone.contains(X::High52Week),
                newer.high_52_week,
                self.high_52_week,
            ),
            low_52_week: pick(
                gone.contains(X::Low52Week),
                newer.low_52_week,
                self.low_52_week,
            ),
            bid_price: pick(gone.contains(X::BidPrice), newer.bid_price, self.bid_price),
            bid_quantity: pick(
                gone.contains(X::BidQuantity),
                newer.bid_quantity,
                self.bid_quantity,
            ),
            ask_price: pick(gone.contains(X::AskPrice), newer.ask_price, self.ask_price),
            ask_quantity: pick(
                gone.contains(X::AskQuantity),
                newer.ask_quantity,
                self.ask_quantity,
            ),
            previous_oi: pick(
                gone.contains(X::PreviousOi),
                newer.previous_oi,
                self.previous_oi,
            ),
            oi_change: pick(gone.contains(X::OiChange), newer.oi_change, self.oi_change),
            total_oi: pick(gone.contains(X::TotalOi), newer.total_oi, self.total_oi),
            day_close: pick(gone.contains(X::DayClose), newer.day_close, self.day_close),
            exchange_time_ms: newer.exchange_time_ms,
            cleared: ClearedExtras::default(),
        }
    }
}

/// A field one broker's web adapter publishes and no other does with the
/// same meaning. The set is fixed here, so an adapter can never write an
/// identity, mode or canonical key (each name is checked against them by
/// `feed::dialect`'s tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerField {
    /// Upstox: the call auction's indicative equilibrium price (an explicit
    /// 0 included), web `upstox_adapter.py` `_auction_fields`.
    IndicativeEquilibriumPrice,
    /// Upstox: the auction's reference price.
    ReferencePrice,
    /// Upstox: the indicative equilibrium quantity.
    IndicativeEquilibriumQuantity,
    /// Upstox: the signed indicative imbalance, total and at market.
    IndicativeImbalanceQuantityTotal,
    IndicativeImbalanceQuantityMarket,
    /// Upstox: whether the instrument trades in the closing auction.
    CasEligible,
    /// Dhan: today's highest and lowest open interest (`oi_high`, `oi_low`).
    OiHigh,
    OiLow,
    /// Groww: the day's traded value (`value`).
    TradedValue,
    /// Fyers: whether the message is the first full one or an update
    /// (`update_type`: `snapshot` or `live`).
    UpdateType,
}

impl BrokerField {
    /// Whether the field is market state that holds until the broker sends
    /// a new value or clears it (Dhan's day OI range, Groww's day traded
    /// value): kept from earlier packets as the quote is. The others
    /// describe their own packet (Upstox's auction figures, Fyers' update
    /// type) and last that packet (audit R107-12).
    pub const fn persists(self) -> bool {
        matches!(
            self,
            BrokerField::OiHigh | BrokerField::OiLow | BrokerField::TradedValue
        )
    }

    /// The web adapter's key.
    pub const fn name(self) -> &'static str {
        match self {
            BrokerField::IndicativeEquilibriumPrice => "indicative_equilibrium_price",
            BrokerField::ReferencePrice => "reference_price",
            BrokerField::IndicativeEquilibriumQuantity => "indicative_equilibrium_quantity",
            BrokerField::IndicativeImbalanceQuantityTotal => "indicative_imbalance_quantity_total",
            BrokerField::IndicativeImbalanceQuantityMarket => {
                "indicative_imbalance_quantity_market"
            }
            BrokerField::CasEligible => "cas_eligible",
            BrokerField::OiHigh => "oi_high",
            BrokerField::OiLow => "oi_low",
            BrokerField::TradedValue => "value",
            BrokerField::UpdateType => "update_type",
        }
    }

    /// The lowest mode whose frames carry it (and every mode above).
    pub const fn from_mode(self) -> FeedMode {
        match self {
            BrokerField::IndicativeEquilibriumPrice
            | BrokerField::TradedValue
            | BrokerField::UpdateType => FeedMode::Ltp,
            BrokerField::OiHigh | BrokerField::OiLow => FeedMode::Depth,
            _ => FeedMode::Quote,
        }
    }

    pub const ALL: [BrokerField; 10] = [
        BrokerField::IndicativeEquilibriumPrice,
        BrokerField::ReferencePrice,
        BrokerField::IndicativeEquilibriumQuantity,
        BrokerField::IndicativeImbalanceQuantityTotal,
        BrokerField::IndicativeImbalanceQuantityMarket,
        BrokerField::CasEligible,
        BrokerField::OiHigh,
        BrokerField::OiLow,
        BrokerField::TradedValue,
        BrokerField::UpdateType,
    ];
}

/// A [`BrokerField`]'s value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrokerValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A fixed text the adapter names (Fyers' `snapshot` / `live`).
    Text(&'static str),
}

impl Serialize for BrokerValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match *self {
            BrokerValue::Int(v) => s.serialize_i64(v),
            BrokerValue::Float(v) => s.serialize_f64(v),
            BrokerValue::Bool(v) => s.serialize_bool(v),
            BrokerValue::Text(v) => s.serialize_str(v),
        }
    }
}

/// The [`BrokerField`]s one packet carried, each at most once. Not kept
/// from one packet to the next: a value that should last (an auction's
/// price lasts one tick) stays only as long as the adapter sends it.
#[derive(Debug, Clone, Default, PartialEq)]
/// A packet's broker-only fields: a value per field, or `None` for a field
/// the packet clears (one list, so a tick stays the size it was).
pub struct BrokerFields(Vec<(BrokerField, Option<BrokerValue>)>);

impl BrokerFields {
    fn put(&mut self, field: BrokerField, value: Option<BrokerValue>) {
        match self.0.iter_mut().find(|(f, _)| *f == field) {
            Some(slot) => slot.1 = value,
            None => self.0.push((field, value)),
        }
    }

    /// Record `field`, replacing an earlier value of it.
    pub fn set(&mut self, field: BrokerField, value: BrokerValue) {
        self.put(field, Some(value));
    }

    /// `field` is gone: left out of this packet's frames and not kept from
    /// earlier ones (an absent field keeps a persistent one's last value).
    pub fn clear(&mut self, field: BrokerField) {
        self.put(field, None);
    }

    /// These fields with `newer`'s over them: a persistent field `newer`
    /// does not carry stays unless `newer` clears it; a per-packet one
    /// lasts its packet.
    pub fn overlay(&self, newer: &Self) -> Self {
        let mut out = BrokerFields(
            self.0
                .iter()
                .copied()
                .filter(|(f, v)| v.is_some() && f.persists())
                .collect(),
        );
        for (f, v) in &newer.0 {
            match v {
                Some(v) => out.set(*f, *v),
                None => out.0.retain(|(g, _)| g != f),
            }
        }
        out
    }

    pub fn get(&self, field: BrokerField) -> Option<BrokerValue> {
        self.0
            .iter()
            .find(|(f, _)| *f == field)
            .and_then(|(_, v)| *v)
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|(_, v)| v.is_none())
    }

    pub fn iter(&self) -> impl Iterator<Item = (BrokerField, BrokerValue)> + '_ {
        self.0.iter().filter_map(|(f, v)| v.map(|v| (*f, v)))
    }
}

impl Serialize for BrokerFields {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        for (f, v) in self.iter() {
            m.serialize_entry(f.name(), &v)?;
        }
        m.end()
    }
}
