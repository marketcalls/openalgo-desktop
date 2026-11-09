//! The web's price model for the simulated feed
//! (`dhan_sandbox_adapter.py`), formula for formula: where an instrument's
//! price starts, how it moves each second, and what the rest of a frame
//! is drawn from. Pure apart from the random draws, which come from the
//! generator the caller passes (seeded in tests).

use crate::brokers::common::streaming::{
    round2, FeedEvent, FeedMode, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::brokers::common::symbols::{ContractQuery, SymbolGeneration, SymbolResolver};
use crate::brokers::types::DepthLevel;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const MONTHS: [&[u8]; 12] = [
    b"JAN", b"FEB", b"MAR", b"APR", b"MAY", b"JUN", b"JUL", b"AUG", b"SEP", b"OCT", b"NOV", b"DEC",
];

/// Exchanges searched for an underlying's calls, in the web's order.
const OPTION_EXCHANGES: [&str; 4] = ["NFO", "BFO", "MCX", "CDS"];
/// Calls read per exchange (web `limit=300`), and the count that ends the
/// search (web `len(strikes) >= 30`).
const CALLS_PER_EXCHANGE: usize = 300;
const ENOUGH_STRIKES: usize = 30;
/// Underlyings remembered per master; past it the memory starts again.
const MAX_SPOT_HINTS: usize = 4096;
/// Price levels a side of the simulated book has (web `range(1, 6)`).
const BOOK_LEVELS: u32 = 5;

/// An option contract read from an OpenAlgo symbol (web
/// `_parse_option_contract`: `^([A-Z]+)\d{2}MON\d{2}([\d.]+)(CE|PE)$`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OptionContract {
    pub(crate) underlying: String,
    pub(crate) strike: f64,
    pub(crate) call: bool,
}

/// The option contract `symbol` names, if it names one. Upper case first,
/// as the web reads it.
pub(crate) fn option_contract(symbol: &str) -> Option<OptionContract> {
    let upper = symbol.to_uppercase();
    let s = upper.as_bytes();
    let letters = s.iter().take_while(|b| b.is_ascii_uppercase()).count();
    // letters, DD, MON, YY, at least one strike character, CE or PE
    if letters == 0 || s.len() < letters + 10 {
        return None;
    }
    let rest = &s[letters..];
    let digits = |b: &[u8]| b.iter().all(u8::is_ascii_digit);
    let (strike, kind) = rest[7..].split_at(rest.len() - 9);
    let call = match kind {
        b"CE" => true,
        b"PE" => false,
        _ => return None,
    };
    if !digits(&rest[..2])
        || !MONTHS.contains(&&rest[2..5])
        || !digits(&rest[5..7])
        || !strike.iter().all(|b| b.is_ascii_digit() || *b == b'.')
    {
        return None;
    }
    let strike = std::str::from_utf8(strike).ok()?.parse::<f64>().ok()?;
    Some(OptionContract {
        underlying: upper[..letters].to_string(),
        strike,
        call,
    })
}

/// web `_stable_noise`: a value in `[low, high]` fixed by `seed_key`, from
/// the first four bytes of its SHA-256.
pub(crate) fn stable_noise(seed_key: &str, low: f64, high: f64) -> f64 {
    let d = Sha256::digest(seed_key.as_bytes());
    let head = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
    low + (high - low) * (f64::from(head) / f64::from(u32::MAX))
}

/// The premium an option drifts towards around `spot`: intrinsic value
/// plus a time value that shrinks with the distance to the strike (web
/// `fair_price` in `_mock_streaming_loop`, the premium in
/// `_derive_reference_price`).
pub(crate) fn fair_premium(spot: f64, c: &OptionContract) -> f64 {
    let intrinsic = if c.call {
        (spot - c.strike).max(0.0)
    } else {
        (c.strike - spot).max(0.0)
    };
    let distance = (spot - c.strike).abs() / spot.max(1.0);
    let time_value = (spot * 0.006 * (1.0 - distance * 8.0).max(0.08)).max(0.5);
    intrinsic + time_value
}

/// Where an instrument's simulated price starts (web
/// `_derive_reference_price`): an option's fair premium with a fixed
/// nudge; anything else the spot its listed calls suggest, else a fixed
/// value between 80 and 1,200 drawn from the symbol.
pub(crate) fn reference_price(symbol: &str, spot: &mut impl FnMut(&str) -> Option<f64>) -> f64 {
    let upper = symbol.to_uppercase();
    if let Some(c) = option_contract(&upper) {
        let base = spot(&c.underlying).unwrap_or(c.strike);
        let premium = fair_premium(base, &c);
        let nudged =
            premium + stable_noise(&format!("{}|init", upper), -0.03, 0.03) * premium.max(1.0);
        return nudged.max(0.05);
    }
    match spot(&upper) {
        Some(s) if s > 0.0 => s,
        _ => stable_noise(&format!("{}|fallback", upper), 80.0, 1200.0).max(20.0),
    }
}

/// One second's move (web `_mock_streaming_loop`): an option steps 18% of
/// the way to its fair premium plus up to 2% of it either way (at least
/// 0.2); anything else up to 0.25% either way (at least 0.05). Never below
/// 0.05.
pub(crate) fn next_price(current: f64, fair: Option<f64>, rng: &mut impl Rng) -> f64 {
    let step = match fair {
        Some(fair) => {
            let width = (fair * 0.02).max(0.2);
            (fair - current) * 0.18 + rng.gen_range(-width..=width)
        }
        None => {
            let width = (current * 0.0025).max(0.05);
            rng.gen_range(-width..=width)
        }
    };
    (current + step).max(0.05)
}

/// The events of one instrument's simulated second at `price` (web
/// `market_data` in `_mock_streaming_loop`): the LTP and trade time, from
/// Quote up the day's figures drawn around it, at Depth five levels a side
/// 0.05 apart. The draws go in the web's order.
pub(crate) fn events(
    symbol: &str,
    exchange: &str,
    mode: FeedMode,
    price: f64,
    now_ms: i64,
    rng: &mut impl Rng,
) -> Vec<FeedEvent> {
    let mut t = NormalizedTick {
        symbol: symbol.to_string(),
        exchange: exchange.to_string(),
        mode: mode.code(),
        ltp: round2(price),
        last_trade_time_ms: now_ms,
        timestamp_ms: now_ms,
        ..Default::default()
    };
    // The web's frame has no change keys: a change from a made-up close
    // would be a constant 2%.
    t.mark_absent(QuoteField::Change);
    if mode >= FeedMode::Quote {
        t.volume = rng.gen_range(1000..=50_000);
        t.oi = rng.gen_range(10_000..=500_000);
        t.open = round2(price * 0.95);
        t.high = round2(price * 1.05);
        t.low = round2(price * 0.90);
        t.close = round2(price * 0.98);
        t.last_quantity = rng.gen_range(1..=100);
        t.average_price = round2(price);
        t.total_buy_quantity = rng.gen_range(5000..=100_000);
        t.total_sell_quantity = rng.gen_range(5000..=100_000);
        t.mark_carried(QuoteField::OpenInterest);
    }
    if mode < FeedMode::Depth {
        return vec![FeedEvent::Tick(t)];
    }
    let buy = side(price, -1.0, rng);
    let sell = side(price, 1.0, rng);
    let book = NormalizedDepth {
        symbol: t.symbol.clone(),
        exchange: t.exchange.clone(),
        ltp: t.ltp,
        buy,
        sell,
        total_buy_quantity: t.total_buy_quantity,
        total_sell_quantity: t.total_sell_quantity,
        timestamp_ms: now_ms,
        ..Default::default()
    };
    vec![FeedEvent::Tick(t), FeedEvent::Depth(book)]
}

/// One side of the simulated book, best first. A level the walk took to a
/// price of 0 or below is left out: the web sends it, a price no book can
/// hold (a web defect, not copied).
fn side(price: f64, direction: f64, rng: &mut impl Rng) -> Vec<DepthLevel> {
    (1..=BOOK_LEVELS)
        .map(|i| DepthLevel {
            price: round2(price + direction * (0.05 * f64::from(i))),
            quantity: rng.gen_range(10..=500),
            orders: rng.gen_range(1..=10),
        })
        .filter(|l| l.price > 0.0)
        .collect()
}

/// web `_estimate_spot_from_contracts`: an underlying's spot taken as the
/// median strike of its listed calls, the first 300 by symbol on each of
/// NFO, BFO, MCX and CDS in turn until 30 strikes are found.
pub(crate) fn median_call_strike(master: &SymbolGeneration, underlying: &str) -> Option<f64> {
    let mut strikes = Vec::new();
    for exchange in OPTION_EXCHANGES {
        let mut calls = master.contracts(&ContractQuery {
            exchange,
            underlying,
            instrument_type: Some("CE"),
            ..Default::default()
        });
        calls.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        strikes.extend(
            calls
                .iter()
                .take(CALLS_PER_EXCHANGE)
                .map(|r| r.strike)
                .filter(|s| *s > 0.0),
        );
        if strikes.len() >= ENOUGH_STRIKES {
            break;
        }
    }
    strikes.sort_by(f64::total_cmp);
    strikes.get(strikes.len() / 2).copied()
}

/// The spot of each underlying, read once per master (web
/// `_spot_hint_cache`; an underlying with no calls is remembered too, and
/// a new master starts the memory again).
#[derive(Default)]
pub(crate) struct SpotHints {
    master: u64,
    known: HashMap<String, Option<f64>>,
}

impl SpotHints {
    pub(crate) fn spot(&mut self, symbols: &SymbolResolver, underlying: &str) -> Option<f64> {
        let master = symbols.snapshot();
        if master.id() != self.master || self.known.len() >= MAX_SPOT_HINTS {
            self.master = master.id();
            self.known.clear();
        }
        *self
            .known
            .entry(underlying.to_string())
            .or_insert_with(|| median_call_strike(&master, underlying))
    }
}
