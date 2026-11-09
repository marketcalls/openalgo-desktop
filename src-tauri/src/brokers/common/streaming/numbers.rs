//! The JSON number form of a packet's values, where the broker's own JSON
//! decides it and the web passes it through: 5paisa sends a whole price as
//! an integer (445) and a fractional one as a double (445.5); Jainam's and
//! RMoney's 1105 text event is parsed with `float()`, so its integers are
//! doubles. An adapter marks the packet; a value it does not mark goes out
//! in the broker's usual form (`feed::dialect`'s list of doubles, else an
//! integer for a count and a double for a price).

/// The text a broker's web adapter writes a trade time as, where it passes
/// the broker's own text through: Definedge's `ltt` is sometimes epoch
/// seconds as a string and sometimes the time of day. An adapter marks the
/// packet (`NormalizedTick::time_text`); unmarked, each text source has its
/// own form (`feed::dialect::Source::TradeTimeText`, `TradeTimeOfDay`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeText {
    /// `YYYY-MM-DD HH:MM:SS` in IST.
    DateTime,
    /// `HH:MM:SS` in IST.
    TimeOfDay,
    /// Epoch seconds, as a string (`"1791000000"`).
    EpochSeconds,
}

/// A numeric value of a packet, whichever key carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Number {
    Ltp,
    Open,
    High,
    Low,
    Close,
    AveragePrice,
    Change,
    ChangePercent,
    Volume,
    LastQuantity,
    TotalBuyQuantity,
    TotalSellQuantity,
    OpenInterest,
    LastTradeTime,
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
}

/// How one value is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberForm {
    /// An integer when the value is whole (5paisa's 445), else a double.
    Integer,
    /// A double (`4.0`).
    Double,
}

/// The forms a packet marked, by value; unmarked values take the broker's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NumberForms {
    integers: u32,
    doubles: u32,
}

impl NumberForms {
    /// Write `number` in `form` for this packet.
    pub fn set(&mut self, number: Number, form: NumberForm) {
        let bit = 1u32 << number as u8;
        let (on, off) = match form {
            NumberForm::Integer => (&mut self.integers, &mut self.doubles),
            NumberForm::Double => (&mut self.doubles, &mut self.integers),
        };
        *on |= bit;
        *off &= !bit;
    }

    /// The form the packet marked for `number`, if any.
    pub fn form(self, number: Number) -> Option<NumberForm> {
        let bit = 1u32 << number as u8;
        if self.integers & bit != 0 {
            Some(NumberForm::Integer)
        } else if self.doubles & bit != 0 {
            Some(NumberForm::Double)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_takes_the_last_form_marked_for_it() {
        let mut n = NumberForms::default();
        assert_eq!(n.form(Number::Ltp), None);
        n.set(Number::Ltp, NumberForm::Integer);
        n.set(Number::DayClose, NumberForm::Double);
        assert_eq!(n.form(Number::Ltp), Some(NumberForm::Integer));
        assert_eq!(n.form(Number::DayClose), Some(NumberForm::Double));
        n.set(Number::Ltp, NumberForm::Double);
        assert_eq!(n.form(Number::Ltp), Some(NumberForm::Double));
        assert_eq!(n.form(Number::Open), None);
    }
}
