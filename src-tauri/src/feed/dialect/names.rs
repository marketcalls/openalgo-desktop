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
//! * A text copy of a time (Definedge, Tradejini, the Noren `ltt`) is not
//!   sent: `ltt` carries it in epoch milliseconds.

use super::super::source::Mode;

/// The unit a broker's web adapter sends a time in.
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
}

/// One web key: its name, its value, and the lowest mode it is sent in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebName {
    pub key: &'static str,
    pub source: Source,
    pub from: Mode,
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

/// Where a broker's web adapter puts `ltt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ltt {
    /// In every frame that has a trade time.
    Always,
    /// Not in an LTP frame cut from a richer packet (Zerodha's LTP copy of
    /// a quote or full tick is `ltp` and `timestamp` only, as recorded in
    /// tests/fixtures/web/websocket/01).
    NotOnLtpCopies,
    /// Never (Fyers, Motilal, Nubra, the Noren family, Delta Exchange).
    Never,
}

/// What the renderer needs to know about a broker's web frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dialect {
    pub data_mode: DataMode,
    /// Whether the canonical `price_change` / `price_change_percent` go on
    /// an instrument that is not an index: only where the broker's web
    /// adapter publishes a change for one (C04). Zerodha's recorded frames
    /// carry the change for indices only.
    pub instrument_change: bool,
    /// Whether `open_interest` goes beside `oi` (Zerodha, Sahi, Rupeezy,
    /// Arrow, both HDFC, IIFL Capital send both).
    pub open_interest: bool,
    pub ltt: Ltt,
    /// Canonical integer keys the web adapter sends as JSON doubles (Angel's
    /// book totals, binary doubles in its packet).
    pub floats: &'static [&'static str],
    /// Whether `data` repeats the frame's `symbol` and `exchange` (the
    /// envelope always carries them, as the web's `server.py` does).
    pub data_identity: bool,
    /// The lowest mode whose `data` carries `timestamp`.
    pub timestamp_from: Mode,
    pub names: &'static [WebName],
}

impl Dialect {
    const fn open_interest(self) -> Self {
        Self {
            open_interest: true,
            ..self
        }
    }

    const fn ltt(self, ltt: Ltt) -> Self {
        Self { ltt, ..self }
    }

    const fn floats(self, floats: &'static [&'static str]) -> Self {
        Self { floats, ..self }
    }

    /// `data` without `symbol` and `exchange`, and `timestamp` only from
    /// `from` up.
    const fn bare(self, from: Mode) -> Self {
        Self {
            data_identity: false,
            timestamp_from: from,
            ..self
        }
    }
}

const fn n(key: &'static str, source: Source, from: Mode) -> WebName {
    WebName { key, source, from }
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

const DEFINEDGE: &[WebName] = &[
    n("ltq", S::LastQuantity, Q),
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

const FIVEPAISA: &[WebName] = &[
    n("last_trade_quantity", S::LastQuantity, Q),
    n("percent_change", S::ChangePercent, Q),
    n("last_trade_time", S::TradeTime(Unit::Millis), L),
    n("bid_price", S::Extra(E::BidPrice), Q),
    n("bid_quantity", S::Extra(E::BidQuantity), Q),
    n("ask_price", S::Extra(E::AskPrice), Q),
    n("ask_quantity", S::Extra(E::AskQuantity), Q),
];

/// The Noren family's depth keys (Shoonya, Flattrade, TradeSmart, Zebu).
const NOREN_DEPTH: [WebName; 5] = [
    n("depth_level", S::DepthLevels, D),
    n("upper_circuit", S::Extra(E::UpperCircuit), D),
    n("lower_circuit", S::Extra(E::LowerCircuit), D),
    n("52_week_high", S::Extra(E::High52Week), D),
    n("52_week_low", S::Extra(E::Low52Week), D),
];

const fn noren(timestamp: &'static str, oi_in_depth: &'static str) -> [WebName; 8] {
    [
        n(timestamp, S::Extra(E::ExchangeTime(Unit::Seconds)), L),
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
const SHOONYA: &[WebName] = &noren("shoonya_timestamp", "open_interest");
/// Flattrade and Zebu name the total OI `total_traded_value`.
const FLATTRADE: &[WebName] = &noren("flattrade_timestamp", "total_traded_value");
const ZEBU: &[WebName] = &noren("zebu_timestamp", "total_traded_value");
/// TradeSmart sends `oi` in Quote too (canonical).
const TRADESMART: &[WebName] = &noren("tradesmart_timestamp", "oi");

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
    n("last_trade_quantity", S::LastQuantity, Q),
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
    n("best_bid_price", S::Extra(E::BidPrice), Q),
    n("best_bid_quantity", S::Extra(E::BidQuantity), Q),
    n("best_ask_price", S::Extra(E::AskPrice), Q),
    n("best_ask_quantity", S::Extra(E::AskQuantity), Q),
];

const TRADEJINI: &[WebName] = &[
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

const fn d(data_mode: DataMode, instrument_change: bool, names: &'static [WebName]) -> Dialect {
    Dialect {
        data_mode,
        instrument_change,
        open_interest: false,
        ltt: Ltt::Always,
        floats: &[],
        data_identity: true,
        timestamp_from: Mode::Ltp,
        names,
    }
}

/// The renderer's view of `broker`'s web frames. A broker not listed gets
/// the canonical frame with a numeric `data.mode`, `oi` and
/// `open_interest`, and `ltt` everywhere.
pub(crate) fn dialect(broker: &str) -> Dialect {
    use DataMode::{Absent, Label, Number};
    match broker {
        "zerodha" => d(Label, false, ZERODHA)
            .open_interest()
            .ltt(Ltt::NotOnLtpCopies),
        "sahi" => d(Label, false, SAHI).open_interest(),
        "iiflcapital" => d(Label, false, IIFLCAPITAL).open_interest(),
        "nubra" => d(Label, false, NUBRA).ltt(Ltt::Never),
        "angel" => d(Number, false, ANGEL).floats(&["total_buy_quantity", "total_sell_quantity"]),
        "arrow" | "hdfcsecurities" | "hdfcsky" => d(Absent, true, ARROW_HDFC).open_interest(),
        "rupeezy" => d(Absent, true, RUPEEZY).open_interest(),
        // aliceblue_adapter.py:1243-1305: no symbol, exchange or mode in
        // `data`, and `timestamp` in the Depth dict only.
        "aliceblue" => d(Absent, true, ALICEBLUE).bare(Mode::Depth),
        "kotak" => d(Absent, true, KOTAK),
        "upstox" => d(Absent, false, UPSTOX),
        "fyers" => d(Absent, true, FYERS).ltt(Ltt::Never),
        "compositedge" | "fivepaisaxts" | "ibulls" | "iifl" | "jainamxts" | "rmoney" | "wisdom" => {
            d(Number, false, XTS)
        }
        "definedge" => d(Number, true, DEFINEDGE),
        "deltaexchange" => d(Number, false, DELTA).ltt(Ltt::Never),
        "dhan" => d(Number, false, DHAN),
        "firstock" => d(Number, false, FIRSTOCK),
        "fivepaisa" => d(Number, true, FIVEPAISA),
        "shoonya" => d(Number, true, SHOONYA).ltt(Ltt::Never),
        "flattrade" => d(Number, true, FLATTRADE).ltt(Ltt::Never),
        "zebu" => d(Number, true, ZEBU).ltt(Ltt::Never),
        "tradesmart" => d(Number, true, TRADESMART).ltt(Ltt::Never),
        "groww" => d(Number, false, GROWW),
        "indmoney" => d(Number, false, INDMONEY),
        "motilal" => d(Number, false, MOTILAL).ltt(Ltt::Never),
        "mstock" => d(Number, false, MSTOCK),
        "paytm" => d(Number, true, PAYTM),
        "pocketful" => d(Number, true, POCKETFUL),
        "samco" => d(Number, true, SAMCO),
        "tradejini" => d(Number, true, TRADEJINI),
        _ => d(Number, false, &[]).open_interest(),
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
