//! The arithmetic: rounding, brokerage, the lines of one contract note, and
//! a round trip as a brokerage calculator shows it (web `sandbox/charges.py`
//! `compute_charges`, `_contract_note`, `estimate_round_trip`).

use super::rates::{segment, Segment};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

const HUNDRED: Decimal = dec!(100);

/// What one fill or one round trip costs, each line as a contract note
/// prints it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Charges {
    pub brokerage: Decimal,
    /// STT, or CTT on commodities.
    pub stt: Decimal,
    pub exchange_charges: Decimal,
    pub sebi_charges: Decimal,
    pub stamp_duty: Decimal,
    pub gst: Decimal,
}

/// The charge columns of `sandbox_trades`, in the order reports list them.
pub const CHARGE_COLUMNS: [&str; 7] = [
    "brokerage",
    "stt",
    "exchange_charges",
    "sebi_charges",
    "stamp_duty",
    "gst",
    "total_charges",
];

impl Charges {
    /// The sum of the lines, so it never disagrees with them.
    pub fn total(&self) -> Decimal {
        self.brokerage
            + self.stt
            + self.exchange_charges
            + self.sebi_charges
            + self.stamp_duty
            + self.gst
    }

    /// The values in [`CHARGE_COLUMNS`] order, the total last.
    pub fn columns(&self) -> [Decimal; 7] {
        [
            self.brokerage,
            self.stt,
            self.exchange_charges,
            self.sebi_charges,
            self.stamp_duty,
            self.gst,
            self.total(),
        ]
    }

    /// The lines as floats under their column names, the total included
    /// (web `_charges_dict`).
    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (name, value) in CHARGE_COLUMNS.iter().zip(self.columns()) {
            m.insert((*name).to_string(), serde_json::json!(f(value)));
        }
        serde_json::Value::Object(m)
    }
}

impl std::ops::Add for Charges {
    type Output = Charges;
    fn add(self, o: Charges) -> Charges {
        Charges {
            brokerage: self.brokerage + o.brokerage,
            stt: self.stt + o.stt,
            exchange_charges: self.exchange_charges + o.exchange_charges,
            sebi_charges: self.sebi_charges + o.sebi_charges,
            stamp_duty: self.stamp_duty + o.stamp_duty,
            gst: self.gst + o.gst,
        }
    }
}

/// A decimal as the float the web's `float(Decimal)` sends.
pub(crate) fn f(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

/// Two places, halves away from zero, as a contract note prints.
pub fn paisa(value: Decimal) -> Decimal {
    let mut r = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    r.rescale(2);
    r
}

/// The nearest rupee, as a contract note prints STT and stamp duty: to the
/// paisa first and then to the rupee, halves up both times, so 0.4995 is
/// 0.50 and then 1 (the order a published calculator rounds in).
pub fn rupee(value: Decimal) -> Decimal {
    let mut r = paisa(value).round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
    r.rescale(2);
    r
}

fn of(value: Decimal, rate: Decimal) -> Decimal {
    value * rate / HUNDRED
}

/// Brokerage on one executed order, unrounded: the lower of the flat amount
/// and the percentage when both are set, the one that is set when only one
/// is, zero when neither is (zero or less means not set).
pub fn brokerage_for(trade_value: Decimal, flat: Decimal, percent: Decimal) -> Decimal {
    if trade_value <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let by_percent = of(trade_value, percent);
    match (flat > Decimal::ZERO, percent > Decimal::ZERO) {
        (true, true) => flat.min(by_percent),
        (true, false) => flat,
        (false, true) => by_percent,
        (false, false) => Decimal::ZERO,
    }
}

/// Everything one fill is charged, as a contract note for that one trade
/// prints it. `None` or an empty trade charges nothing.
pub fn compute_charges(
    segment: Option<&Segment>,
    exchange: &str,
    action: &str,
    trade_value: Decimal,
    flat: Decimal,
    percent: Decimal,
) -> Charges {
    let Some(segment) = segment else {
        return Charges::default();
    };
    if trade_value <= Decimal::ZERO {
        return Charges::default();
    }
    let is_buy = action.eq_ignore_ascii_case("BUY");
    let brokerage = paisa(brokerage_for(trade_value, flat, percent));
    let (buy, sell) = if is_buy {
        (trade_value, Decimal::ZERO)
    } else {
        (Decimal::ZERO, trade_value)
    };
    contract_note(segment, exchange, buy, sell, brokerage)
}

/// The lines of one contract note for a bought and a sold value; either may
/// be zero (one fill). Every line is worked on what it applies to and
/// rounded once.
fn contract_note(
    segment: &Segment,
    exchange: &str,
    buy_value: Decimal,
    sell_value: Decimal,
    brokerage: Decimal,
) -> Charges {
    let turnover = buy_value + sell_value;
    // The exchange's own charge and its levy are two amounts on a note, each
    // rounded on its own: rounding the combined rate once is a paisa out
    // about one time in five (6,670 of NSE equity: 0.20 plus 0.01, not 0.20).
    let rate = segment.exchange_rate(exchange);
    let levy = segment.levy_rate(exchange);
    let exchange_charges = paisa(of(turnover, rate - levy)) + paisa(of(turnover, levy));
    let sebi_charges = paisa(of(turnover, segment.sebi));
    // STT goes to the rupee; CTT, the same tax on commodities, to the paisa,
    // as a published calculator shows the two.
    let tax = of(buy_value, segment.stt_buy) + of(sell_value, segment.stt_sell);
    let stt = if segment.is_ctt() {
        paisa(tax)
    } else {
        rupee(tax)
    };
    Charges {
        brokerage,
        stt,
        exchange_charges,
        sebi_charges,
        stamp_duty: rupee(of(buy_value, segment.stamp_buy)),
        gst: paisa(of(brokerage + exchange_charges + sebi_charges, segment.gst)),
    }
}

/// How much of a fill's exchange charge is the Investor Protection Fund
/// Trust levy, for a breakdown that lists the two apart: `(transaction,
/// levy)`, adding back to the charge paid.
pub fn split_exchange_charges(
    segment: Option<&Segment>,
    exchange: &str,
    trade_value: Decimal,
    exchange_charges: Decimal,
) -> (Decimal, Decimal) {
    let Some(segment) = segment else {
        return (exchange_charges, paisa(Decimal::ZERO));
    };
    let levy = paisa(of(trade_value, segment.levy_rate(exchange))).min(exchange_charges);
    (exchange_charges - levy, levy)
}

/// A buy and a sell of the same quantity, as a brokerage calculator shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundTrip {
    pub turnover: Decimal,
    pub total: Charges,
    pub gross_pnl: Decimal,
    pub net_pnl: Decimal,
    pub points_to_breakeven: Decimal,
}

/// Price a round trip as one contract note for the two orders: brokerage on
/// each order, every other line on the combined value, rounded once. The two
/// fills charged one at a time can come to a paisa more or less on a line.
pub fn estimate_round_trip(
    segment_key: &str,
    exchange: &str,
    buy_price: Decimal,
    sell_price: Decimal,
    quantity: Decimal,
    flat: Decimal,
    percent: Decimal,
) -> RoundTrip {
    let buy_value = buy_price * quantity;
    let sell_value = sell_price * quantity;
    let total = match segment(segment_key) {
        Some(s) if buy_value > Decimal::ZERO && sell_value > Decimal::ZERO => {
            let brokerage = paisa(
                brokerage_for(buy_value, flat, percent) + brokerage_for(sell_value, flat, percent),
            );
            contract_note(s, exchange, buy_value, sell_value, brokerage)
        }
        _ => Charges::default(),
    };
    let gross = sell_value - buy_value;
    let breakeven = if quantity > Decimal::ZERO {
        paisa(total.total() / quantity)
    } else {
        paisa(Decimal::ZERO)
    };
    RoundTrip {
        turnover: paisa(buy_value + sell_value),
        total,
        gross_pnl: paisa(gross),
        net_pnl: paisa(gross - total.total()),
        points_to_breakeven: breakeven,
    }
}
