//! Which canonical keys a broker's web adapter writes into
//! `market_data.data`, in which modes, for an index and for any other
//! instrument. A canonical key turned off never removes a web name of the
//! same value (`names`): the names read the values, not the keys.

use crate::feed::source::Mode;

/// A canonical key of `data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Key {
    Symbol,
    Exchange,
    Mode,
    Ltt,
    Timestamp,
    Volume,
    LastQuantity,
    AveragePrice,
    TotalBuyQuantity,
    TotalSellQuantity,
    Open,
    High,
    Low,
    Close,
    PriceChange,
    PriceChangePercent,
    Oi,
    OpenInterest,
    /// The book. On an index it goes out empty (`{buy: [], sell: []}`)
    /// where the broker's web adapter sends one (Kotak, Sahi).
    Depth,
}

const COUNT: usize = Key::Depth as usize + 1;

/// A set of modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Modes(u8);

const fn bit(m: Mode) -> u8 {
    1 << (m as u8 - 1)
}

impl Modes {
    pub(crate) const NEVER: Modes = Modes(0);
    pub(crate) const ALL: Modes = Modes::range(Mode::Ltp, Mode::Depth);
    /// Quote and Depth.
    pub(crate) const QUOTE_UP: Modes = Modes::range(Mode::Quote, Mode::Depth);
    pub(crate) const DEPTH: Modes = Modes::range(Mode::Depth, Mode::Depth);

    /// `from` to `to`, both included (none when `from` is above `to`).
    pub(crate) const fn range(from: Mode, to: Mode) -> Modes {
        let mut bits = 0;
        let mut m = from as u8;
        while m <= to as u8 {
            bits |= 1 << (m - 1);
            m += 1;
        }
        Modes(bits)
    }

    pub(crate) const fn has(self, m: Mode) -> bool {
        self.0 & bit(m) != 0
    }
}

/// For each canonical key, the modes it goes out in, for an instrument and
/// for an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Keys {
    instrument: [Modes; COUNT],
    index: [Modes; COUNT],
}

impl Keys {
    /// The frame of 1.0.6 (the recorded Zerodha transcript's keys): identity,
    /// mode, trade time and timestamp in every mode; the quote, OI and
    /// `open_interest` from Quote up; the order-flow fields and the book on
    /// an instrument only, the change on an index only.
    pub(crate) const V106: Keys = {
        let mut instrument = [Modes::QUOTE_UP; COUNT];
        let mut index = [Modes::QUOTE_UP; COUNT];
        let all = [
            Key::Symbol,
            Key::Exchange,
            Key::Mode,
            Key::Ltt,
            Key::Timestamp,
        ];
        let mut i = 0;
        while i < all.len() {
            instrument[all[i] as usize] = Modes::ALL;
            index[all[i] as usize] = Modes::ALL;
            i += 1;
        }
        let flow = [
            Key::LastQuantity,
            Key::AveragePrice,
            Key::TotalBuyQuantity,
            Key::TotalSellQuantity,
            Key::Depth,
        ];
        let mut i = 0;
        while i < flow.len() {
            index[flow[i] as usize] = Modes::NEVER;
            i += 1;
        }
        instrument[Key::Depth as usize] = Modes::DEPTH;
        instrument[Key::PriceChange as usize] = Modes::NEVER;
        instrument[Key::PriceChangePercent as usize] = Modes::NEVER;
        Keys { instrument, index }
    };

    /// `key` in `modes`, for an instrument and for an index.
    pub(crate) const fn set(self, key: Key, modes: Modes) -> Keys {
        self.instrument(key, modes).index(key, modes)
    }

    /// `key` in `modes` for an instrument that is not an index.
    pub(crate) const fn instrument(mut self, key: Key, modes: Modes) -> Keys {
        self.instrument[key as usize] = modes;
        self
    }

    /// `key` in `modes` for an index.
    pub(crate) const fn index(mut self, key: Key, modes: Modes) -> Keys {
        self.index[key as usize] = modes;
        self
    }

    /// Whether `key` goes out in a frame carrying `mode`'s keys.
    pub(crate) fn has(&self, key: Key, index: bool, mode: Mode) -> bool {
        let table = if index { &self.index } else { &self.instrument };
        table[key as usize].has(mode)
    }
}
