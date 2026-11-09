//! The keys each broker's web adapter puts in `market_data.data` beyond
//! the desktop's canonical ones, and where each key's value comes from.
//!
//! The desktop sends one canonical set for every broker (the names the
//! recorded Zerodha transcript carries) and, beside it, every other key the
//! broker's web adapter publishes for the same value, so a script written
//! against the web with that broker finds the key it reads. Read from the
//! web adapters (`broker/<name>/streaming/*_adapter.py`, `*_mapping.py`) at
//! a1b9e3250; the per-broker matrix is in `docs/port/market-data-frames.md`.
//!
//! Rules (deliberate, recorded in that document):
//! * A key goes out only when its value is known for this frame: never a
//!   default the web fills in (`.get(x, 0)`), never a placeholder.
//! * A key the web sends from some mode up is sent in that mode and every
//!   mode above it (the web switches some names between Quote and Depth;
//!   a script then never meets a KeyError on the richer frame).
//! * A web alias follows its adapter's unit, type and format: seconds for
//!   Fyers' and Dhan's `last_trade_time`, text for Tradejini's and for the
//!   raw strings of the Noren family and Definedge. The docs' keys `ltt`
//!   and `timestamp` are always epoch milliseconds, whatever the adapter
//!   sends under those names. No key is ever sent as `null`: a value not
//!   known leaves its key out.

use super::super::source::Mode;
use super::keys::{Key, Keys, Modes};

/// The unit a broker's web adapter sends a time in: a web name follows its
/// adapter's unit, type and format, while `ltt` and `timestamp` are epoch
/// milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unit {
    Millis,
    Seconds,
}

/// An optional shared field (`QuoteExtras`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Extra {
    UpperCircuit,
    LowerCircuit,
    High52Week,
    Low52Week,
    BidPrice,
    BidQuantity,
    AskPrice,
    AskQuantity,
    PreviousOi,
    OiChange,
    TotalOi,
    DayClose,
    ExchangeTime(Unit),
}

/// Where a web key's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Ltp,
    Open,
    High,
    Low,
    /// Previous close.
    Close,
    Volume,
    LastQuantity,
    AveragePrice,
    TotalBuyQuantity,
    TotalSellQuantity,
    Oi,
    Change,
    ChangePercent,
    /// The exchange's trade time (`ltt`).
    TradeTime(Unit),
    /// The exchange's trade time as text, `YYYY-MM-DD HH:MM:SS` in IST, only
    /// when the broker sent one (Tradejini's `last_trade_time`: the web
    /// writes `str(datetime.fromtimestamp(seconds))`), or in the form the
    /// packet marks (`NormalizedTick::time_text`: Definedge).
    TradeTimeText,
    /// The exchange's trade time as the time of day, `HH:MM:SS` in IST, only
    /// when the broker sent one (the Noren family's `last_trade_time`, the
    /// broker's `ltt` passed through), or in the form the packet marks.
    TradeTimeOfDay,
    Extra(Extra),
    /// The book depth the frame streams (Dhan, the Noren family:
    /// `depth_level`).
    DepthLevels,
    /// The frame's mode as a number (Groww, Fyers: `subscription_mode`).
    SubscriptionMode,
    /// The web's topic, `<EXCHANGE>_<SYMBOL>_<LTP|QUOTE|DEPTH>` (Groww).
    Topic,
    /// The broker's id (Groww: `broker`).
    BrokerId,
    /// A fixed text per mode: LTP, Quote, Depth (Fyers `data_type`).
    PerMode([&'static str; 3]),
    /// The broker's instrument token, as text (`token`).
    Token,
    /// The frame's `timestamp` in the unit the web name uses (Fyers 50-level
    /// `feed_time`).
    Timestamp(Unit),
    /// `true` (Fyers' 50-level frames: `is_50_depth`).
    True,
    /// The inner source, only in a Depth frame streaming 50 levels (Fyers'
    /// TBT book frame keys).
    FiftyLevels(&'static Source),
}

/// One web key: its name, its value, and the modes it is sent in (`from`
/// to `to`, by the mode of the dict the frame carries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebName {
    pub key: &'static str,
    pub source: Source,
    pub from: Mode,
    pub to: Mode,
}

/// How a broker's web adapter fills `data.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataMode {
    /// `"ltp"`, `"quote"`, `"full"` (Zerodha, Sahi, IIFL Capital, Nubra).
    Label,
    /// 1, 2, 3 (most adapters).
    Number,
    /// No `mode` key (Arrow, both HDFC, Kotak, Rupeezy, Upstox, Fyers,
    /// AliceBlue).
    Absent,
}

/// What a holder of a lower mode receives from a richer packet. The web's
/// listener sends each published message once per client, under the
/// highest mode the client holds at or below the message's
/// (`server.py` around line 2102 at a1b9e3250), with the adapter's dict
/// unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fold {
    /// The adapter publishes a dict per mode (Zerodha, as recorded in
    /// tests/fixtures/web/websocket/01 and 05; Sahi, Rupeezy, Upstox, ...):
    /// each frame is trimmed to its holder's mode.
    PerMode,
    /// The adapter publishes one dict per packet (Arrow, Paytm, mStock, the
    /// XTS family, Delta Exchange, Angel, Dhan, AliceBlue, IndMoney,
    /// Pocketful): a lower mode's frame carries the whole packet, its book
    /// and circuits included. The desktop still sends one frame per mode a
    /// client holds (lead decision 2026-11-09): the web's single frame
    /// leaves a client's lower-mode callbacks unrun, a web defect.
    Richer,
}

/// What the renderer needs to know about a broker's web frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dialect {
    /// How `data.mode` is written where `keys` sends it.
    pub data_mode: DataMode,
    /// Which canonical keys go out, in which modes, per kind of instrument.
    pub keys: Keys,
    /// Whether `ltt` goes in an LTP frame cut from a richer packet: not on
    /// Zerodha (its LTP copy of a quote or full tick is `ltp` and
    /// `timestamp` only, tests/fixtures/web/websocket/01).
    pub ltt_on_ltp_copies: bool,
    /// Canonical integer keys the web adapter sends as JSON doubles (Angel's
    /// book totals, binary doubles in its packet).
    pub floats: &'static [&'static str],
    pub fold: Fold,
    /// Whether the book carries the frame's `timestamp` inside it (Upstox:
    /// `depth: {buy, sell, timestamp}`).
    pub book_time: bool,
    /// Whether a Depth holder waits for the instrument's first book: before
    /// it, the broker's web adapter publishes a quote packet on its Quote
    /// topic alone, so a Depth-only client hears nothing (Definedge's
    /// touchline, 5paisa's feed frame, Tradejini's L1 tick). Otherwise a
    /// full packet without a book reaches Depth holders without `depth`.
    /// After the first book every full tick carries it, either way (lead
    /// decision 2026-11-09).
    pub depth_needs_book: bool,
    /// Whether `data.mode` is the holder's mode rather than the mode of
    /// the dict a `Fold::Richer` broker sends lower holders: its web
    /// adapter publishes per subscription and writes that subscription's
    /// mode (mStock, Delta Exchange, Jainam XTS), or writes the mode of the
    /// one subscription it keeps per token (Angel, Pocketful).
    pub mode_of_holder: bool,
    pub names: &'static [WebName],
}

impl Dialect {
    /// `key` in `modes`, for an instrument and an index alike.
    const fn key(self, key: Key, modes: Modes) -> Self {
        Self {
            keys: self.keys.set(key, modes),
            ..self
        }
    }

    /// `key` in `modes` on an instrument that is not an index.
    const fn instrument_key(self, key: Key, modes: Modes) -> Self {
        Self {
            keys: self.keys.instrument(key, modes),
            ..self
        }
    }

    /// `key` in `modes` on an index.
    const fn index_key(self, key: Key, modes: Modes) -> Self {
        Self {
            keys: self.keys.index(key, modes),
            ..self
        }
    }

    /// `open_interest` beside `oi` (Zerodha, Sahi, Rupeezy, Arrow, both
    /// HDFC, IIFL Capital send both).
    const fn open_interest(self) -> Self {
        self.key(Key::OpenInterest, Modes::QUOTE_UP)
    }

    /// No `ltt` (Fyers, Motilal, Nubra, the Noren family, Delta Exchange).
    const fn no_ltt(self) -> Self {
        self.key(Key::Ltt, Modes::NEVER)
    }

    const fn no_ltt_on_ltp_copies(self) -> Self {
        Self {
            ltt_on_ltp_copies: false,
            ..self
        }
    }

    /// No canonical change on an index (Firstock, AliceBlue, Samco,
    /// Tradejini, Kotak, Pocketful); the broker's own names still go.
    const fn no_index_change(self) -> Self {
        self.index_key(Key::PriceChange, Modes::NEVER)
            .index_key(Key::PriceChangePercent, Modes::NEVER)
    }

    /// An index's Depth frame carries an empty book (Kotak, Sahi).
    const fn index_book(self) -> Self {
        self.index_key(Key::Depth, Modes::DEPTH)
    }

    const fn floats(self, floats: &'static [&'static str]) -> Self {
        Self { floats, ..self }
    }

    const fn book_time(self) -> Self {
        Self {
            book_time: true,
            ..self
        }
    }

    const fn depth_needs_book(self) -> Self {
        Self {
            depth_needs_book: true,
            ..self
        }
    }

    const fn mode_of_holder(self) -> Self {
        Self {
            mode_of_holder: true,
            ..self
        }
    }

    const fn richer(self) -> Self {
        Self {
            fold: Fold::Richer,
            ..self
        }
    }

    /// `data` without `symbol` and `exchange`, and `timestamp` only from
    /// `from` up.
    const fn bare(self, from: Mode) -> Self {
        self.key(Key::Symbol, Modes::NEVER)
            .key(Key::Exchange, Modes::NEVER)
            .key(Key::Timestamp, Modes::range(from, Mode::Depth))
    }
}

const fn n(key: &'static str, source: Source, from: Mode) -> WebName {
    WebName {
        key,
        source,
        from,
        to: Mode::Depth,
    }
}

/// A web key sent from `from` to `to` only.
const fn nt(key: &'static str, source: Source, from: Mode, to: Mode) -> WebName {
    WebName {
        key,
        source,
        from,
        to,
    }
}

use Extra as E;
use Mode::{Depth as D, Ltp as L, Quote as Q};
use Source as S;

const CIRCUITS_IN_DEPTH: [WebName; 2] = [
    n("upper_circuit", S::Extra(E::UpperCircuit), D),
    n("lower_circuit", S::Extra(E::LowerCircuit), D),
];

const ANGEL: &[WebName] = &[
    n("last_trade_quantity", S::LastQuantity, Q),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

/// Arrow, both HDFC adapters and Rupeezy publish the same price keys from
/// the LTP packet up.
const LTPC_PRICE_KEYS: [WebName; 6] = [
    n("token", S::Token, L),
    n("last_price", S::Ltp, L),
    n("close", S::Close, L),
    n("prev_close", S::Close, L),
    n("change", S::Change, L),
    n("change_percent", S::ChangePercent, L),
];

const ARROW_HDFC: &[WebName] = &[
    LTPC_PRICE_KEYS[0],
    LTPC_PRICE_KEYS[1],
    LTPC_PRICE_KEYS[2],
    LTPC_PRICE_KEYS[3],
    LTPC_PRICE_KEYS[4],
    LTPC_PRICE_KEYS[5],
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

const RUPEEZY: &[WebName] = &[
    LTPC_PRICE_KEYS[0],
    LTPC_PRICE_KEYS[1],
    LTPC_PRICE_KEYS[2],
    LTPC_PRICE_KEYS[3],
    LTPC_PRICE_KEYS[4],
    LTPC_PRICE_KEYS[5],
    n("open", S::Open, Q),
    n("high", S::High, Q),
    n("low", S::Low, Q),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

/// The XTS family (CompositEdge, 5paisa XTS, IBulls, IIFL, Jainam, RMoney,
/// Wisdom): `ltq` in LTP, circuits in Depth.
const XTS: &[WebName] = &[
    n("ltq", S::LastQuantity, L),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

const ALICEBLUE: &[WebName] = &[
    n("change_percent", S::ChangePercent, Q),
    n("total_oi", S::Oi, Q),
];

/// Definedge sends `ltq` in Depth only (`_DEPTH_EXTRA_FIELDS`); Quote's
/// last quantity is `last_quantity` alone.
const DEFINEDGE: &[WebName] = &[
    n("ltq", S::LastQuantity, D),
    n("change_percent", S::ChangePercent, Q),
    n("percent_change", S::ChangePercent, Q),
    n("prev_oi", S::Extra(E::PreviousOi), Q),
    n("total_oi", S::Extra(E::TotalOi), Q),
    n("bid", S::Extra(E::BidPrice), Q),
    n("bid_qty", S::Extra(E::BidQuantity), Q),
    n("ask", S::Extra(E::AskPrice), Q),
    n("ask_qty", S::Extra(E::AskQuantity), Q),
    n("total_buy_qty", S::TotalBuyQuantity, D),
    n("total_sell_qty", S::TotalSellQuantity, D),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
    n("52w_high", S::Extra(E::High52Week), D),
    n("52w_low", S::Extra(E::Low52Week), D),
];

/// Delta Exchange publishes one dict in every mode.
const DELTA: &[WebName] = &[
    n("open", S::Open, L),
    n("high", S::High, L),
    n("low", S::Low, L),
    n("close", S::Close, L),
    n("oi", S::Oi, L),
    n("bid_price", S::Extra(E::BidPrice), L),
    n("bid_qty", S::Extra(E::BidQuantity), L),
    n("ask_price", S::Extra(E::AskPrice), L),
    n("ask_qty", S::Extra(E::AskQuantity), L),
    n("totalbuyqty", S::TotalBuyQuantity, D),
    n("totalsellqty", S::TotalSellQuantity, D),
];

const DHAN: &[WebName] = &[
    n("last_trade_time", S::TradeTime(Unit::Seconds), L),
    n("day_close", S::Extra(E::DayClose), Q),
    n("prev_oi", S::Extra(E::PreviousOi), D),
    n("depth_level", S::DepthLevels, D),
];

const FIRSTOCK: &[WebName] = &[
    n("upper_circuit", S::Extra(E::UpperCircuit), Q),
    n("lower_circuit", S::Extra(E::LowerCircuit), Q),
];

/// fivepaisa_adapter.py `_normalize_market_data`: the best bid and ask in
/// the Quote dict only (its Depth dict is `_quote_fields` and the book).
const FIVEPAISA: &[WebName] = &[
    n("last_trade_quantity", S::LastQuantity, Q),
    n("percent_change", S::ChangePercent, Q),
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    nt("bid_price", S::Extra(E::BidPrice), Q, Q),
    nt("bid_quantity", S::Extra(E::BidQuantity), Q, Q),
    nt("ask_price", S::Extra(E::AskPrice), Q, Q),
    nt("ask_quantity", S::Extra(E::AskQuantity), Q, Q),
];

/// The Noren family's depth keys (Shoonya, Flattrade, TradeSmart, Zebu).
const NOREN_DEPTH: [WebName; 5] = [
    n("depth_level", S::DepthLevels, D),
    n("upper_circuit", S::Extra(E::UpperCircuit), D),
    n("lower_circuit", S::Extra(E::LowerCircuit), D),
    n("52_week_high", S::Extra(E::High52Week), D),
    n("52_week_low", S::Extra(E::Low52Week), D),
];

/// The Noren family's names. `last_trade_time` is the broker's `ltt` passed
/// through, the time of day (from LTP on Shoonya, from Quote on the rest).
const fn noren(
    timestamp: &'static str,
    oi_in_depth: &'static str,
    trade_time_from: Mode,
) -> [WebName; 9] {
    [
        n(timestamp, S::Extra(E::ExchangeTime(Unit::Seconds)), L),
        n("last_trade_time", S::TradeTimeOfDay, trade_time_from),
        n("percent_change", S::ChangePercent, Q),
        NOREN_DEPTH[0],
        NOREN_DEPTH[1],
        NOREN_DEPTH[2],
        NOREN_DEPTH[3],
        NOREN_DEPTH[4],
        n(oi_in_depth, S::Oi, D),
    ]
}

/// Shoonya publishes OI as `open_interest` in Depth (canonical already).
const SHOONYA: &[WebName] = &noren("shoonya_timestamp", "open_interest", L);
/// Flattrade and Zebu name the total OI `total_traded_value`.
const FLATTRADE: &[WebName] = &noren("flattrade_timestamp", "total_traded_value", Q);
const ZEBU: &[WebName] = &noren("zebu_timestamp", "total_traded_value", Q);
/// TradeSmart sends `oi` in Quote too (canonical).
const TRADESMART: &[WebName] = &noren("tradesmart_timestamp", "oi", Q);

/// Fyers sends its symbol feed's whole dict to LTP and Quote holders.
const FYERS: &[WebName] = &[
    n("token", S::Token, L),
    n("open", S::Open, L),
    n("high", S::High, L),
    n("low", S::Low, L),
    n("close", S::Close, L),
    n("volume", S::Volume, L),
    n("oi", S::Oi, L),
    n("bid_price", S::Extra(E::BidPrice), L),
    n("ask_price", S::Extra(E::AskPrice), L),
    n("bid_size", S::Extra(E::BidQuantity), L),
    n("ask_size", S::Extra(E::AskQuantity), L),
    n("upper_circuit", S::Extra(E::UpperCircuit), L),
    n("lower_circuit", S::Extra(E::LowerCircuit), L),
    n("last_traded_time", S::TradeTime(Unit::Seconds), L),
    n("last_trade_time", S::TradeTime(Unit::Seconds), L),
    n("exchange_time", S::Extra(E::ExchangeTime(Unit::Seconds)), L),
    n("avg_trade_price", S::AveragePrice, L),
    n("average_price", S::AveragePrice, L),
    n("last_trade_quantity", S::LastQuantity, L),
    n("last_quantity", S::LastQuantity, L),
    n("total_buy_quantity", S::TotalBuyQuantity, L),
    n("total_sell_quantity", S::TotalSellQuantity, L),
    n("change", S::Change, L),
    n("change_percent", S::ChangePercent, L),
    n("percent_change", S::ChangePercent, L),
    n("data_type", S::PerMode(["Quote", "Quote", "Depth"]), L),
    n("subscription_mode", S::SubscriptionMode, L),
    // `fyers_mapping.py` `map_to_openalgo_50_depth`: the TBT book's keys.
    n("total_buy_qty", S::FiftyLevels(&S::TotalBuyQuantity), D),
    n("total_sell_qty", S::FiftyLevels(&S::TotalSellQuantity), D),
    n("feed_time", S::FiftyLevels(&S::Timestamp(Unit::Seconds)), D),
    n("depth_levels", S::FiftyLevels(&S::DepthLevels), D),
    n("is_50_depth", S::FiftyLevels(&S::True), D),
];

const GROWW: &[WebName] = &[
    n("open", S::Open, L),
    n("high", S::High, L),
    n("low", S::Low, L),
    n("close", S::Close, L),
    n("volume", S::Volume, L),
    n("broker", S::BrokerId, L),
    n("topic", S::Topic, L),
    n("subscription_mode", S::SubscriptionMode, L),
    n("bid", S::Extra(E::BidPrice), D),
    n("bid_price", S::Extra(E::BidPrice), D),
    n("bid_qty", S::Extra(E::BidQuantity), D),
    n("bid_size", S::Extra(E::BidQuantity), D),
    n("bid_quantity", S::Extra(E::BidQuantity), D),
    n("ask", S::Extra(E::AskPrice), D),
    n("ask_price", S::Extra(E::AskPrice), D),
    n("offer_price", S::Extra(E::AskPrice), D),
    n("ask_qty", S::Extra(E::AskQuantity), D),
    n("ask_size", S::Extra(E::AskQuantity), D),
    n("ask_quantity", S::Extra(E::AskQuantity), D),
    n("offer_quantity", S::Extra(E::AskQuantity), D),
];

const IIFLCAPITAL: &[WebName] = &[
    n("bid", S::Extra(E::BidPrice), Q),
    n("ask", S::Extra(E::AskPrice), Q),
    n("bid_quantity", S::Extra(E::BidQuantity), Q),
    n("ask_quantity", S::Extra(E::AskQuantity), Q),
];

const INDMONEY: &[WebName] = &[
    n("bid_price", S::Extra(E::BidPrice), Q),
    n("bid_qty", S::Extra(E::BidQuantity), Q),
    n("ask_price", S::Extra(E::AskPrice), Q),
    n("ask_qty", S::Extra(E::AskQuantity), Q),
    n("oi_change", S::Extra(E::OiChange), Q),
];

const KOTAK: &[WebName] = &[
    n("percent_change", S::ChangePercent, Q),
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    n("totalbuyqty", S::TotalBuyQuantity, D),
    n("totalsellqty", S::TotalSellQuantity, D),
];

const MOTILAL: &[WebName] = &[
    n("volume", S::Volume, L),
    n("ltq", S::LastQuantity, Q),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

const MSTOCK: &[WebName] = &[
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    n(
        "exchange_timestamp",
        S::Extra(E::ExchangeTime(Unit::Millis)),
        L,
    ),
    n("prev_close", S::Close, Q),
    n("last_trade_quantity", S::LastQuantity, Q),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

const NUBRA: &[WebName] = &[
    n("prev_close", S::Close, Q),
    n("bid", S::Extra(E::BidPrice), Q),
    n("ask", S::Extra(E::AskPrice), Q),
];

const PAYTM: &[WebName] = &[
    n("close", S::Close, L),
    n("change_absolute", S::Change, L),
    n("change_percent", S::ChangePercent, L),
    // paytm_adapter.py:715 and :734: Quote names it `last_trade_quantity`,
    // Depth `last_quantity`.
    nt("last_trade_quantity", S::LastQuantity, Q, Q),
    n("52_week_high", S::Extra(E::High52Week), Q),
    n("52_week_low", S::Extra(E::Low52Week), Q),
    n("oi_change", S::Extra(E::OiChange), D),
];

const POCKETFUL: &[WebName] = &[
    n("change", S::Change, L),
    n("oi", S::Oi, L),
    n("bid_price", S::Extra(E::BidPrice), L),
    n("ask_price", S::Extra(E::AskPrice), L),
    n("last_trade_quantity", S::LastQuantity, Q),
];

const SAHI: &[WebName] = &[
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    n("upper_circuit", S::Extra(E::UpperCircuit), Q),
    n("lower_circuit", S::Extra(E::LowerCircuit), Q),
];

const SAMCO: &[WebName] = &[
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    n("last_trade_quantity", S::LastQuantity, Q),
    n("change", S::Change, Q),
    n("change_percentage", S::ChangePercent, Q),
    // samco_adapter.py sends the best bid and ask in its Quote dict only.
    nt("best_bid_price", S::Extra(E::BidPrice), Q, Q),
    nt("best_bid_quantity", S::Extra(E::BidQuantity), Q, Q),
    nt("best_ask_price", S::Extra(E::AskPrice), Q, Q),
    nt("best_ask_quantity", S::Extra(E::AskQuantity), Q, Q),
];

const TRADEJINI: &[WebName] = &[
    // web `_add_reference_keys`: the broker's trade time as text, only when
    // it sent one.
    n("last_trade_time", S::TradeTimeText, Q),
    n("change", S::Change, Q),
    n("change_percent", S::ChangePercent, Q),
    n("percent_change", S::ChangePercent, Q),
    CIRCUITS_IN_DEPTH[0],
    CIRCUITS_IN_DEPTH[1],
];

const UPSTOX: &[WebName] = &[
    n("token", S::Token, L),
    n("ltq", S::LastQuantity, L),
    n("cp", S::Close, L),
    n("last_trade_quantity", S::LastQuantity, Q),
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
];

const ZERODHA: &[WebName] = &[n(
    "exchange_timestamp",
    S::Extra(E::ExchangeTime(Unit::Seconds)),
    Q,
)];

/// A listed broker's frame: 1.0.6's keys, with `open_interest` only where
/// a broker sets it, and no canonical change on a non-index instrument
/// (no web adapter sends `price_change` there; the broker's own names carry
/// the change, C04).
const fn d(data_mode: DataMode, names: &'static [WebName]) -> Dialect {
    let keys = Keys::V106.set(Key::OpenInterest, Modes::NEVER);
    let keys = match data_mode {
        DataMode::Absent => keys.set(Key::Mode, Modes::NEVER),
        _ => keys,
    };
    Dialect {
        data_mode,
        keys,
        ltt_on_ltp_copies: true,
        floats: &[],
        fold: Fold::PerMode,
        book_time: false,
        depth_needs_book: false,
        mode_of_holder: false,
        names,
    }
}

/// The renderer's view of `broker`'s web frames. A broker not listed gets
/// the canonical frame with a numeric `data.mode`, `oi` and
/// `open_interest`, and `ltt` everywhere.
pub(crate) fn dialect(broker: &str) -> Dialect {
    use DataMode::{Absent, Label, Number};
    match broker {
        "zerodha" => d(Label, ZERODHA).open_interest().no_ltt_on_ltp_copies(),
        "sahi" => d(Label, SAHI).open_interest().index_book(),
        "iiflcapital" => d(Label, IIFLCAPITAL).open_interest(),
        "nubra" => d(Label, NUBRA).no_ltt(),
        // angel_adapter.py:716-736: `mode` is the token's subscription's.
        "angel" => d(Number, ANGEL)
            .richer()
            .mode_of_holder()
            .floats(&["total_buy_quantity", "total_sell_quantity"]),
        "arrow" => d(Absent, ARROW_HDFC).open_interest().richer(),
        "hdfcsecurities" | "hdfcsky" => d(Absent, ARROW_HDFC).open_interest(),
        "rupeezy" => d(Absent, RUPEEZY).open_interest(),
        // aliceblue_adapter.py:1243-1305: no symbol, exchange or mode in
        // `data`, and `timestamp` in the Depth dict only.
        "aliceblue" => d(Absent, ALICEBLUE)
            .bare(Mode::Depth)
            .richer()
            .no_index_change(),
        "kotak" => d(Absent, KOTAK).no_index_change().index_book(),
        "upstox" => d(Absent, UPSTOX).book_time(),
        "fyers" => d(Absent, FYERS).no_ltt(),
        // These name `mode` after the packet's MessageCode; Jainam publishes
        // once per subscribed mode with that mode (jainamxts_adapter.py:709-725).
        "compositedge" | "fivepaisaxts" | "ibulls" | "iifl" | "rmoney" | "wisdom" => {
            d(Number, XTS).richer()
        }
        "jainamxts" => d(Number, XTS).richer().mode_of_holder(),
        // definedge_adapter.py _QUOTE_FIELDS: the change only as `pc`
        // (change_percent, percent_change), on an index too.
        "definedge" => d(Number, DEFINEDGE).depth_needs_book().no_index_change(),
        // delta_adapter.py:470-484: one dict per subscription, with its mode.
        "deltaexchange" => d(Number, DELTA).no_ltt().richer().mode_of_holder(),
        "dhan" => d(Number, DHAN).richer(),
        "firstock" => d(Number, FIRSTOCK).no_index_change(),
        // fivepaisa_adapter.py `_quote_fields`: ChgPcnt as `percent_change`
        // only, on an index too (no `price_change`).
        "fivepaisa" => d(Number, FIVEPAISA).depth_needs_book().no_index_change(),
        "shoonya" => d(Number, SHOONYA).no_ltt(),
        "flattrade" => d(Number, FLATTRADE).no_ltt(),
        "zebu" => d(Number, ZEBU).no_ltt(),
        "tradesmart" => d(Number, TRADESMART).no_ltt(),
        "groww" => d(Number, GROWW).floats(&["volume"]),
        "indmoney" => d(Number, INDMONEY).richer(),
        // motilal_adapter.py: the last quantity as `ltq` only, no change.
        "motilal" => d(Number, MOTILAL)
            .no_ltt()
            .no_index_change()
            .instrument_key(Key::LastQuantity, Modes::NEVER),
        // mstock_adapter.py:186-202: one dict per subscription, with its
        // mode; :362-375: the fixed packet carries an index's order flow too;
        // :340-433: no change on any instrument.
        "mstock" => d(Number, MSTOCK)
            .richer()
            .mode_of_holder()
            .no_index_change()
            .index_key(Key::LastQuantity, Modes::QUOTE_UP)
            .index_key(Key::AveragePrice, Modes::QUOTE_UP)
            .index_key(Key::TotalBuyQuantity, Modes::QUOTE_UP)
            .index_key(Key::TotalSellQuantity, Modes::QUOTE_UP),
        "paytm" => d(Number, PAYTM)
            .richer()
            .instrument_key(Key::LastQuantity, Modes::DEPTH),
        // pocketful_adapter.py:565-600: `mode` is the token's subscription's.
        "pocketful" => d(Number, POCKETFUL)
            .richer()
            .mode_of_holder()
            .no_index_change()
            .instrument_key(Key::LastQuantity, Modes::NEVER),
        // An index's last quantity when Samco sends one (samco_adapter.py
        // sends last_quantity for every instrument at Quote and Depth).
        "samco" => d(Number, SAMCO)
            .no_index_change()
            .index_key(Key::LastQuantity, Modes::QUOTE_UP),
        "tradejini" => d(Number, TRADEJINI).no_index_change().depth_needs_book(),
        _ => Dialect {
            keys: Keys::V106,
            ..d(Number, &[])
        },
    }
}

/// Every broker the table names (for the tests).
#[cfg(test)]
pub(crate) const BROKERS: &[&str] = &[
    "aliceblue",
    "angel",
    "arrow",
    "compositedge",
    "definedge",
    "deltaexchange",
    "dhan",
    "firstock",
    "fivepaisa",
    "fivepaisaxts",
    "flattrade",
    "fyers",
    "groww",
    "hdfcsecurities",
    "hdfcsky",
    "ibulls",
    "iifl",
    "iiflcapital",
    "indmoney",
    "jainamxts",
    "kotak",
    "motilal",
    "mstock",
    "nubra",
    "paytm",
    "pocketful",
    "rmoney",
    "rupeezy",
    "sahi",
    "samco",
    "shoonya",
    "tradejini",
    "tradesmart",
    "upstox",
    "wisdom",
    "zebu",
    "zerodha",
];
