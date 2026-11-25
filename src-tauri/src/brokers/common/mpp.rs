//! Market Price Protection (web `utils/mpp_slab.py`).
//!
//! Where a broker only accepts LIMIT (Kite GTT), a MARKET request is sent as
//! a LIMIT at LTP plus (BUY) or minus (SELL) a slab percentage, rounded to
//! the instrument's tick size. A price that is not finite matches no slab
//! and is refused (web #2052), so a NaN or infinite quote never becomes a
//! limit price.

use super::mapping::Action;
use crate::error::{AppError, Result};
use regex::Regex;
use std::sync::LazyLock;

/// Python `round()` (round half to even), so prices match the web exactly.
pub fn py_round(v: f64, ndigits: i32) -> f64 {
    let m = 10f64.powi(ndigits);
    let x = v * m;
    let r = x.round();
    let r = if (x - x.trunc()).abs() == 0.5 {
        // Tie: pick the even neighbour.
        let down = x.trunc();
        if (down as i64) % 2 == 0 {
            down
        } else {
            down + x.signum()
        }
    } else {
        r
    };
    r / m
}

/// OpenAlgo derivative symbols carry the expiry (`DDMMMYY`) before the
/// suffix: options `[Base][Expiry][Strike][CE/PE]` (NIFTY28MAR2420800CE,
/// VEDL25APR24292.5CE), futures `[Base][Expiry]FUT`. Equity symbols never
/// do, so a cash symbol that merely ends in CE or PE (RELIANCE,
/// BAJFINANCE, PRINCEPIPE, REDTAPE) is not an option (web #2388).
static OPTION_SYMBOL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^.+\d{2}[A-Z]{3}\d{2}\d+(?:\.\d+)?(CE|PE)$").ok());
static FUTURE_SYMBOL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^.+\d{2}[A-Z]{3}\d{2}FUT$").ok());

/// `EQ`, `FUT`, `CE` or `PE` of an OpenAlgo symbol (web
/// `get_instrument_type_from_symbol`): an option needs the full option
/// shape (expiry, strike, CE/PE) and a future the expiry before FUT;
/// anything else is equity. The suffix alone is not enough: a MARKET BUY
/// of RELIANCE at 1339.7 went to the broker at 1353.1 (the 1% option slab)
/// instead of 1346.4.
pub fn instrument_type_from_symbol(symbol: &str) -> &'static str {
    let s = symbol.trim().to_ascii_uppercase();
    let option = OPTION_SYMBOL
        .as_ref()
        .and_then(|re| re.captures(&s))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    match option {
        Some("CE") => "CE",
        Some("PE") => "PE",
        _ if FUTURE_SYMBOL.as_ref().is_some_and(|re| re.is_match(&s)) => "FUT",
        _ => "EQ",
    }
}

/// Whether an instrument type takes the options slabs (web
/// `is_option_instrument_type`): OpenAlgo's `CE`/`PE`, or a master contract
/// type some brokers store in `instrumenttype` (`OPTIDX`, `OPTSTK`,
/// `OPTFUT`, `OPTCUR`, ...), which the GTT paths pass.
fn is_option_instrument_type(instrument_type: &str) -> bool {
    let t = instrument_type.to_ascii_uppercase();
    matches!(t.as_str(), "CE" | "PE") || t.starts_with("OPT")
}

/// Protection percentage for a price (web `get_mpp_percentage`). A NaN or
/// infinite price is refused as a validation error: the order is not sent.
pub fn mpp_percentage(price: f64, instrument_type: &str) -> Result<f64> {
    if !price.is_finite() {
        tracing::warn!(
            "Market price protection refused a price that is not finite: {}",
            price
        );
        return Err(AppError::Validation(
            "The market price for this order is not a valid number, so price protection could not be applied and the order was not sent. Try again in a moment."
                .into(),
        ));
    }
    Ok(if is_option_instrument_type(instrument_type) {
        if price < 10.0 {
            5.0
        } else if price < 100.0 {
            3.0
        } else if price < 500.0 {
            2.0
        } else {
            1.0
        }
    } else if price < 100.0 {
        2.0
    } else if price < 500.0 {
        1.0
    } else {
        0.5
    })
}

pub fn round_to_tick(price: f64, tick_size: Option<f64>) -> f64 {
    match tick_size {
        Some(t) if t > 0.0 => py_round(py_round(price / t, 0) * t, 2),
        _ => py_round(price, 2),
    }
}

/// LIMIT price that protects a MARKET order (web
/// `calculate_protected_price`); refused like [`mpp_percentage`].
pub fn protected_price(
    price: f64,
    action: Action,
    instrument_type: &str,
    tick_size: Option<f64>,
) -> Result<f64> {
    let m = mpp_percentage(price, instrument_type)? / 100.0;
    let p = match action {
        Action::Buy => price * (1.0 + m),
        Action::Sell => price * (1.0 - m),
    };
    Ok(round_to_tick(p, tick_size))
}

/// Decimal places a tick size carries (0.05 -> 2, 0.0025 -> 4, 1 -> 0), as
/// the web reads them from `Decimal(str(tick))`. `f64`'s `Display` is the
/// shortest form that reads back as the same number, so no trailing zero.
pub fn tick_decimals(tick: f64) -> i32 {
    let s = format!("{}", tick);
    s.split_once('.').map(|(_, f)| f.len() as i32).unwrap_or(0)
}

/// `value` on a multiple of `tick`, rounded down or up (web
/// `_snap_to_tick`). The ratio is rounded to six places first, so float
/// noise (804.5025 / 0.05 = 16090.049999...) cannot move it a tick.
pub fn snap_to_tick(value: f64, tick: f64, down: bool) -> f64 {
    let ratio = py_round(value / tick, 6);
    let k = if down { ratio.floor() } else { ratio.ceil() };
    py_round(k * tick, tick_decimals(tick))
}

/// A protective limit past `reference` in the direction the order fills
/// (web `_beyond`, `_slm_protected_price`, `slm_limit_from_trigger`): the
/// MPP slab of the reference, above it for a BUY and below it for a SELL,
/// at least one tick past it, and snapped away from it so rounding never
/// brings it back onto or across the reference. A SELL can come out at or
/// below zero; the caller decides what that means. `tick` must be a
/// positive tick size.
pub fn beyond(reference: f64, action: Action, instrument_type: &str, tick: f64) -> Result<f64> {
    let pct = mpp_percentage(reference, instrument_type)? / 100.0;
    Ok(match action {
        Action::Buy => snap_to_tick((reference * (1.0 + pct)).max(reference + tick), tick, false),
        Action::Sell => snap_to_tick((reference * (1.0 - pct)).min(reference - tick), tick, true),
    })
}

/// The limit of an SL-M order sent as a stop-limit, priced off its own
/// trigger, never the LTP: the limit only matters once the trigger is hit
/// (web #2390).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopLimit {
    /// The limit, on a tick, at least one tick past the trigger.
    Priced(f64),
    /// No positive tick size to snap the limit to.
    NoTick,
    /// A SELL trigger too low to leave a positive limit below it.
    TooLow,
}

/// [`StopLimit`] of an SL-M on `symbol` with this `trigger` and the
/// instrument's `tick` (the symbol picks the equity or the options slabs).
/// What a broker does without a price (refuse the order, or send the
/// trigger as the limit) is the broker's own rule.
pub fn stop_limit_off_trigger(
    symbol: &str,
    action: Action,
    trigger: f64,
    tick: f64,
) -> Result<StopLimit> {
    if !tick.is_finite() || tick <= 0.0 {
        return Ok(StopLimit::NoTick);
    }
    let limit = beyond(trigger, action, instrument_type_from_symbol(symbol), tick)?;
    Ok(if limit > 0.0 {
        StopLimit::Priced(limit)
    } else {
        StopLimit::TooLow
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_rounding() {
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(3.5, 0), 4.0);
        assert_eq!(py_round(102.0111, 2), 102.01);
    }

    #[test]
    fn slabs_match_web_examples() {
        assert_eq!(mpp_percentage(50.0, "EQ").unwrap(), 2.0);
        assert_eq!(mpp_percentage(50.0, "CE").unwrap(), 3.0);
        assert_eq!(mpp_percentage(5.0, "PE").unwrap(), 5.0);
        assert_eq!(mpp_percentage(600.0, "FUT").unwrap(), 0.5);
        assert_eq!(
            protected_price(100.0, Action::Buy, "EQ", None).unwrap(),
            101.0
        );
        assert_eq!(
            protected_price(5.0, Action::Buy, "CE", Some(0.05)).unwrap(),
            5.25
        );
        assert_eq!(
            protected_price(1000.0, Action::Sell, "EQ", Some(0.05)).unwrap(),
            995.0
        );
        assert_eq!(round_to_tick(102.0111, Some(0.05)), 102.0);
        assert_eq!(instrument_type_from_symbol("NIFTY24DEC24FUT"), "FUT");
        assert_eq!(instrument_type_from_symbol("SBIN"), "EQ");
    }

    /// Web test/test_mpp_slab_validation.py
    /// `TestGetMppPercentageSlabs::test_boundary_slabs_unchanged`.
    #[test]
    fn boundary_slabs_match_the_web() {
        for (price, itype, pct) in [
            (50.0, "EQ", 2.0),
            (99.99, "EQ", 2.0),
            (100.0, "EQ", 1.0),
            (499.99, "EQ", 1.0),
            (500.0, "EQ", 0.5),
            (10000.0, "EQ", 0.5),
            (50.0, "FUT", 2.0),
            (5.0, "PE", 5.0),
            (9.99, "CE", 5.0),
            (10.0, "CE", 3.0),
            (99.99, "CE", 3.0),
            (100.0, "CE", 2.0),
            (500.0, "CE", 1.0),
        ] {
            assert_eq!(
                mpp_percentage(price, itype).unwrap(),
                pct,
                "{} {}",
                price,
                itype
            );
        }
    }

    /// Web #2052 (test/test_mpp_slab_validation.py
    /// `TestGetMppPercentageValidation`): a NaN or infinite price matches no
    /// slab and is refused, never turned into a limit price. (The web's
    /// non-numeric cases cannot be written here: the price is an f64.)
    #[test]
    fn a_price_that_is_not_finite_is_refused() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for itype in ["EQ", "CE"] {
                let e = mpp_percentage(bad, itype).unwrap_err();
                assert!(matches!(e, AppError::Validation(_)), "{:?}", e);
                assert!(e.client_message().contains("not a valid number"));
            }
            for action in [Action::Buy, Action::Sell] {
                assert!(protected_price(bad, action, "EQ", Some(0.05)).is_err());
            }
        }
    }

    /// NSE/BSE symbols from the master contract that end in CE or PE.
    const CASH_SYMBOLS_ENDING_CE_PE: [&str; 16] = [
        "RELIANCE",
        "BAJFINANCE",
        "PRINCEPIPE",
        "REDTAPE",
        "DEFENCE",
        "MODEFENCE",
        "FINPIPE",
        "ENDURANCE",
        "RACE",
        "ACE",
        "ADVANCE",
        "ONESOURCE",
        "DIGISPICE",
        "KESARPE",
        "NIEHSPE",
        "MINDSPACE",
    ];

    /// Web test/test_mpp_slab_instrument_type.py
    /// `TestCashSymbols::test_symbol_alone_is_equity` and
    /// `test_plain_equity_unchanged` (web #2388).
    #[test]
    fn a_cash_symbol_ending_in_ce_or_pe_is_equity() {
        for symbol in CASH_SYMBOLS_ENDING_CE_PE {
            assert_eq!(instrument_type_from_symbol(symbol), "EQ", "{symbol}");
        }
        assert_eq!(instrument_type_from_symbol("SBIN"), "EQ");
        assert_eq!(instrument_type_from_symbol("INFY"), "EQ");
    }

    /// Web `TestDerivativeSymbols` (the exchange argument the web added is
    /// not carried: no web broker passes it, and the symbol alone decides
    /// every case below).
    #[test]
    fn a_derivative_needs_its_expiry_before_the_suffix() {
        for (symbol, expected) in [
            ("NIFTY28MAR2420800CE", "CE"),
            ("NIFTY28MAR2420800PE", "PE"),
            ("BANKNIFTY24APR2448000CE", "CE"),
            ("VEDL25APR24292.5CE", "CE"),
            ("SENSEX28MAR2472000PE", "PE"),
            ("CRUDEOIL17APR246750CE", "CE"),
            ("USDINR10MAY2482.5PE", "PE"),
            ("RELIANCE28MAR241400CE", "CE"),
            ("M&M28MAR241800PE", "PE"),
            ("BANKNIFTY24APR24FUT", "FUT"),
            ("CRUDEOILM20MAY24FUT", "FUT"),
            ("USDINR26APR24FUT", "FUT"),
            ("BAJAJ-AUTO28MAR24FUT", "FUT"),
            ("nifty28mar2420800ce", "CE"),
            ("", "EQ"),
        ] {
            assert_eq!(instrument_type_from_symbol(symbol), expected, "{symbol}");
        }
    }

    /// Web `TestMasterContractTypes`: the master contract's option types
    /// take the options slabs (the GTT paths pass `instrumenttype`); the
    /// others the equity and futures slabs (at 50: 3% and 2%).
    #[test]
    fn master_contract_option_types_take_the_options_slabs() {
        for itype in ["CE", "PE", "OPTIDX", "OPTSTK", "OPTFUT", "OPTCUR", "optidx"] {
            assert_eq!(mpp_percentage(50.0, itype).unwrap(), 3.0, "{itype}");
        }
        for itype in ["EQ", "FUT", "FUTIDX", "FUTSTK", "FUTCOM", ""] {
            assert_eq!(mpp_percentage(50.0, itype).unwrap(), 2.0, "{itype}");
        }
    }

    #[test]
    fn ticks_snap_away_from_the_reference() {
        assert_eq!(snap_to_tick(87.50249, 0.0025, true), 87.5);
        assert_eq!(snap_to_tick(87.5001, 0.0025, false), 87.5025);
        assert_eq!(snap_to_tick(804.5025, 0.05, false), 804.55);
        assert_eq!(tick_decimals(0.0025), 4);
        assert_eq!(tick_decimals(0.05), 2);
        assert_eq!(tick_decimals(1.0), 0);
        assert_eq!(tick_decimals(5.0), 0);
    }

    /// A SELL limit that would floor to zero or below is `TooLow`, a
    /// missing tick `NoTick`; the rest are priced off the trigger.
    #[test]
    fn a_stop_limit_is_priced_off_its_trigger() {
        let sl = |action, trigger, tick| stop_limit_off_trigger("SBIN", action, trigger, tick);
        assert_eq!(
            sl(Action::Sell, 980.0, 0.05).unwrap(),
            StopLimit::Priced(975.1)
        );
        assert_eq!(
            sl(Action::Buy, 1020.0, 0.05).unwrap(),
            StopLimit::Priced(1025.1)
        );
        assert_eq!(sl(Action::Sell, 0.05, 0.05).unwrap(), StopLimit::TooLow);
        for tick in [0.0, -0.05, f64::NAN] {
            assert_eq!(sl(Action::Buy, 100.0, tick).unwrap(), StopLimit::NoTick);
        }
        assert!(sl(Action::Buy, f64::NAN, 0.05).is_err());
    }

    /// Web `TestProtectedPrice`.
    #[test]
    fn cash_symbols_get_the_equity_buffer() {
        let price = |ltp: f64, action, symbol: &str, tick: f64| {
            protected_price(ltp, action, instrument_type_from_symbol(symbol), Some(tick)).unwrap()
        };
        // Live Flattrade log: LTP 1339.7, tick 0.1, BUY went out at 1353.1 (1%).
        assert_eq!(price(1339.7, Action::Buy, "RELIANCE", 0.1), 1346.4);
        for (symbol, ltp, tick, buy, sell) in [
            ("RELIANCE", 1400.0, 0.1, 1407.0, 1393.0),
            ("REDTAPE", 108.5, 0.05, 109.6, 107.4),
            ("DEFENCE", 78.0, 0.05, 79.55, 76.45),
        ] {
            assert_eq!(price(ltp, Action::Buy, symbol, tick), buy, "{symbol}");
            assert_eq!(price(ltp, Action::Sell, symbol, tick), sell, "{symbol}");
        }
        assert_eq!(price(5.0, Action::Buy, "NIFTY28MAR2420800CE", 0.05), 5.25);
        // An explicit instrument type still wins.
        assert_eq!(
            protected_price(1400.0, Action::Buy, "CE", Some(0.1)).unwrap(),
            1414.0
        );
    }
}
