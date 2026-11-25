//! The rate card: one row per segment, the two markets, and which segment an
//! order belongs to (web `sandbox/charges.py` `SEGMENTS`, `classify_segment`).
//!
//! Statutory rates are code, the brokerage plan is the trader's data: a rate
//! change edits [`SEGMENTS`], moves [`STATUTORY_SCHEDULE_AS_OF`] and adds a
//! test, with no migration, and trades already made keep what they paid.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;

/// The date the statutory schedule was last checked against the published
/// rates, shown on the settings page.
pub const STATUTORY_SCHEDULE_AS_OF: &str = "2026-11-23";

/// The two markets the rate card is split into. A trader is on one or the
/// other and a page shows the logged-in broker's alone: a schedule of
/// statutory charges on one side, a venue fee and nothing else on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Market {
    Indian,
    Crypto,
}

impl Market {
    pub fn as_str(&self) -> &'static str {
        match self {
            Market::Indian => "indian",
            Market::Crypto => "crypto",
        }
    }
}

/// The market a broker of this `broker_type` trades (web
/// `market_for_broker_type`). Only `crypto` is the crypto market; anything
/// else, an unknown or missing type included, is the Indian one.
pub fn market_for_broker_type(broker_type: Option<&str>) -> Market {
    if broker_type
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("crypto")
    {
        Market::Crypto
    } else {
        Market::Indian
    }
}

/// One row of the rate card. Every rate is a percentage of the trade value as
/// published (0.025 means 0.025 percent).
#[derive(Debug, PartialEq)]
pub struct Segment {
    /// Stable identifier, used in config keys and payloads.
    pub key: &'static str,
    pub label: &'static str,
    /// The heading the settings page files it under.
    pub group: &'static str,
    /// Which orders fall in the segment, in a trader's words.
    pub applies_to: &'static str,
    pub default_flat: Decimal,
    pub default_percent: Decimal,
    pub stt_buy: Decimal,
    pub stt_sell: Decimal,
    /// `STT`, `CTT`, or empty when neither applies.
    pub stt_label: &'static str,
    /// Stamp duty on a buy; a sell pays none.
    pub stamp_buy: Decimal,
    pub sebi: Decimal,
    /// GST on brokerage, exchange transaction charges and SEBI fees.
    pub gst: Decimal,
    /// Exchange transaction charge by exchange code, the main exchange first,
    /// any Investor Protection Fund Trust levy included.
    pub exchange_txn: &'static [(&'static str, Decimal)],
    /// The part of `exchange_txn` that is the levy, by exchange code. Not
    /// charged again: listed so a breakdown can show it on its own line.
    pub ipft: &'static [(&'static str, Decimal)],
    pub market: Market,
}

impl Segment {
    /// The exchanges the segment trades on, the main one first (web
    /// `exchanges_of`). Sent as a list because an object's keys arrive
    /// sorted, which once opened every calculator card on BSE and BFO. A
    /// crypto segment has no exchange rate and trades on CRYPTO.
    pub fn exchanges(&self) -> Vec<&'static str> {
        if self.exchange_txn.is_empty() {
            return vec!["CRYPTO"];
        }
        self.exchange_txn.iter().map(|(e, _)| *e).collect()
    }

    /// The transaction charge rate on `exchange` (zero when it has none).
    pub(crate) fn exchange_rate(&self, exchange: &str) -> Decimal {
        rate_on(self.exchange_txn, exchange)
    }

    /// The levy part of [`Segment::exchange_rate`] on `exchange`.
    pub(crate) fn levy_rate(&self, exchange: &str) -> Decimal {
        rate_on(self.ipft, exchange)
    }

    /// Whether the segment's commodity transaction tax is CTT (kept to the
    /// paisa) rather than STT (to the rupee).
    pub(crate) fn is_ctt(&self) -> bool {
        self.stt_label == "CTT"
    }
}

fn rate_on(rates: &[(&str, Decimal)], exchange: &str) -> Decimal {
    rates
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(exchange))
        .map(|(_, r)| *r)
        .unwrap_or(Decimal::ZERO)
}

/// Rs 10 per crore of turnover.
const SEBI: Decimal = dec!(0.0001);
const GST: Decimal = dec!(18);
const ZERO: Decimal = Decimal::ZERO;

/// The rate card, in the order pages list it.
pub static SEGMENTS: [Segment; 10] = [
    Segment {
        key: "equity_delivery",
        label: "Equity Delivery",
        group: "Equity",
        applies_to: "CNC orders on NSE and BSE",
        default_flat: dec!(20),
        default_percent: dec!(2.5),
        stt_buy: dec!(0.1),
        stt_sell: dec!(0.1),
        stt_label: "STT",
        stamp_buy: dec!(0.015),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[("NSE", dec!(0.00307)), ("BSE", dec!(0.00375))],
        ipft: &[("NSE", dec!(0.0001))],
        market: Market::Indian,
    },
    Segment {
        key: "equity_intraday",
        label: "Equity Intraday",
        group: "Equity",
        applies_to: "MIS orders on NSE and BSE",
        default_flat: dec!(20),
        default_percent: dec!(0.1),
        stt_buy: ZERO,
        stt_sell: dec!(0.025),
        stt_label: "STT",
        stamp_buy: dec!(0.003),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[("NSE", dec!(0.00307)), ("BSE", dec!(0.00375))],
        ipft: &[("NSE", dec!(0.0001))],
        market: Market::Indian,
    },
    Segment {
        key: "fno_futures",
        label: "F&O Futures",
        group: "Equity F&O",
        applies_to: "Futures on NFO and BFO",
        default_flat: dec!(20),
        default_percent: dec!(0.03),
        stt_buy: ZERO,
        stt_sell: dec!(0.05),
        stt_label: "STT",
        stamp_buy: dec!(0.002),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[("NFO", dec!(0.00183)), ("BFO", dec!(0))],
        ipft: &[("NFO", dec!(0.0001))],
        market: Market::Indian,
    },
    Segment {
        key: "fno_options",
        label: "F&O Options",
        group: "Equity F&O",
        applies_to: "Options on NFO and BFO, charged on the premium",
        default_flat: dec!(20),
        default_percent: dec!(0),
        stt_buy: ZERO,
        stt_sell: dec!(0.15),
        stt_label: "STT",
        stamp_buy: dec!(0.003),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[("NFO", dec!(0.03553)), ("BFO", dec!(0.0325))],
        ipft: &[("NFO", dec!(0.0005))],
        market: Market::Indian,
    },
    Segment {
        key: "currency_futures",
        label: "Currency Futures",
        group: "Currency",
        applies_to: "Futures on CDS and BCD",
        default_flat: dec!(20),
        default_percent: dec!(0.03),
        stt_buy: ZERO,
        stt_sell: ZERO,
        stt_label: "",
        stamp_buy: dec!(0.0001),
        sebi: SEBI,
        gst: GST,
        // CDS: 0.00035 plus the 0.00005 Investor Protection Fund Trust levy.
        exchange_txn: &[("CDS", dec!(0.0004)), ("BCD", dec!(0.00045))],
        ipft: &[("CDS", dec!(0.00005))],
        market: Market::Indian,
    },
    Segment {
        key: "currency_options",
        label: "Currency Options",
        group: "Currency",
        applies_to: "Options on CDS and BCD, charged on the premium",
        default_flat: dec!(20),
        default_percent: dec!(0),
        stt_buy: ZERO,
        stt_sell: ZERO,
        stt_label: "",
        stamp_buy: dec!(0.0001),
        sebi: SEBI,
        gst: GST,
        // CDS: 0.0311 plus the 0.002 Investor Protection Fund Trust levy.
        exchange_txn: &[("CDS", dec!(0.0331)), ("BCD", dec!(0.001))],
        ipft: &[("CDS", dec!(0.002))],
        market: Market::Indian,
    },
    Segment {
        key: "commodity_futures",
        label: "Commodity Futures",
        group: "Commodity",
        applies_to: "Futures on MCX, NCDEX and NCO",
        default_flat: dec!(20),
        default_percent: dec!(0.03),
        stt_buy: ZERO,
        stt_sell: dec!(0.01),
        stt_label: "CTT",
        stamp_buy: dec!(0.002),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[
            ("MCX", dec!(0.0021)),
            ("NCDEX", dec!(0.0021)),
            ("NCO", dec!(0.0001)),
        ],
        ipft: &[],
        market: Market::Indian,
    },
    Segment {
        key: "commodity_options",
        label: "Commodity Options",
        group: "Commodity",
        applies_to: "Options on MCX, NCDEX and NCO, charged on the premium",
        default_flat: dec!(20),
        default_percent: dec!(0),
        stt_buy: ZERO,
        stt_sell: dec!(0.05),
        stt_label: "CTT",
        stamp_buy: dec!(0.003),
        sebi: SEBI,
        gst: GST,
        exchange_txn: &[
            ("MCX", dec!(0.0418)),
            ("NCDEX", dec!(0.0418)),
            ("NCO", dec!(0.001)),
        ],
        ipft: &[],
        market: Market::Indian,
    },
    Segment {
        key: "crypto_futures",
        label: "Crypto Futures and Spot",
        group: "Crypto",
        applies_to: "Perpetuals, dated futures and spot on CRYPTO",
        default_flat: dec!(0),
        default_percent: dec!(0),
        stt_buy: ZERO,
        stt_sell: ZERO,
        stt_label: "",
        stamp_buy: ZERO,
        sebi: ZERO,
        gst: ZERO,
        exchange_txn: &[],
        ipft: &[],
        market: Market::Crypto,
    },
    Segment {
        key: "crypto_options",
        label: "Crypto Options",
        group: "Crypto",
        applies_to: "Options on CRYPTO, charged on the premium",
        default_flat: dec!(0),
        default_percent: dec!(0),
        stt_buy: ZERO,
        stt_sell: ZERO,
        stt_label: "",
        stamp_buy: ZERO,
        sebi: ZERO,
        gst: ZERO,
        exchange_txn: &[],
        ipft: &[],
        market: Market::Crypto,
    },
];

/// The segment with this key.
pub fn segment(key: &str) -> Option<&'static Segment> {
    SEGMENTS.iter().find(|s| s.key == key)
}

/// The segments of one market in rate-card order; every segment when none is
/// named.
pub fn segments_for(market: Option<Market>) -> impl Iterator<Item = &'static Segment> {
    SEGMENTS
        .iter()
        .filter(move |s| market.is_none_or(|m| s.market == m))
}

/// The segment an order belongs to (web `classify_segment`), decided by the
/// exchange first so an equity symbol that ends in CE or PE (RELIANCE) is
/// never read as an option. `None` for an exchange nothing is charged on
/// (the index exchanges, which cannot be traded).
pub fn classify_segment(exchange: &str, product: &str, symbol: &str) -> Option<&'static Segment> {
    let exchange = exchange.to_ascii_uppercase();
    let symbol = symbol.to_ascii_uppercase();
    let is_option = symbol.ends_with("CE") || symbol.ends_with("PE");
    let pick = |option: &'static str, other: &'static str| if is_option { option } else { other };
    let key = match exchange.as_str() {
        "NSE" | "BSE" => {
            if product.eq_ignore_ascii_case("MIS") {
                "equity_intraday"
            } else {
                "equity_delivery"
            }
        }
        "NFO" | "BFO" => pick("fno_options", "fno_futures"),
        "CDS" | "BCD" => pick("currency_options", "currency_futures"),
        "MCX" | "NCDEX" | "NCO" => pick("commodity_options", "commodity_futures"),
        "CRYPTO" => pick("crypto_options", "crypto_futures"),
        _ => return None,
    };
    segment(key)
}
