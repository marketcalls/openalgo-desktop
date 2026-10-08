//! Resolve a strategy leg to an exact tradable OpenAlgo contract (web
//! `services/strategy_module/symbol_resolver.py`).
//!
//! A batch leg is written in relative terms ("the ATM call of the weekly
//! expiry, two lots"); before a run starts every leg becomes an exact
//! contract the master confirms, with that contract's own lot size. Failure
//! is a value with a machine-readable `code`, never a plausible substitute:
//! a wrong lot size or a neighbouring strike is a real position.
//!
//! * A lot size of zero or less is a refusal, never a silent 1.
//! * A strike stays a float (`VEDL25APR24292.5CE` is a real contract).
//! * The monthly rank is the last expiry within its calendar month, read off
//!   the data rather than a weekday.

use super::dispatch::OrderGateway;
use crate::brokers::common::master_contract::{format_strike, parse_oa_expiry};
use crate::brokers::common::symbols::{SymToken, SymbolGeneration};
use crate::services::options_service::{atm_index, available_strikes, option_exchange};
use chrono::NaiveDate;
use serde_json::Value;

pub const EXPIRY_RANKS: &[&str] = &[
    "weekly",
    "next_week",
    "monthly",
    "next_month",
    "current",
    "next",
];
pub const SEGMENTS: &[&str] = &["cash", "futures", "options"];
pub const DERIVATIVE_EXCHANGES: &[&str] =
    &["NFO", "BFO", "MCX", "CDS", "NCO", "BCD", "NCDEX", "CRYPTO"];

const NO_SPOT: &[&str] = &["MCX", "CDS", "BCD", "NCDEX", "NCO"];

pub fn is_derivative_exchange(exchange: &str) -> bool {
    DERIVATIVE_EXCHANGES.contains(&exchange.to_ascii_uppercase().as_str())
}

/// The exchange a derivative of this underlying is listed on.
pub fn derivatives_exchange(exchange: &str) -> String {
    let e = exchange.trim().to_ascii_uppercase();
    if is_derivative_exchange(&e) {
        e
    } else {
        option_exchange(&e)
    }
}

/// The contract lot size for a base or an exact symbol on an exchange, or
/// `None` when it cannot be said (cash venue, no master, no match). Matched
/// on `name` first, then on the symbol anchored at the root: `GOLD` must not
/// borrow `GOLDM`'s lot size.
pub fn lot_size_for(g: &SymbolGeneration, symbol: &str, exchange: &str) -> Option<i64> {
    let venue = exchange.to_ascii_uppercase();
    if !DERIVATIVE_EXCHANGES.contains(&venue.as_str()) || symbol.is_empty() {
        return None;
    }
    let root = symbol.to_ascii_uppercase();
    if let Some(row) = g
        .contracts(&crate::brokers::common::symbols::ContractQuery {
            exchange: &venue,
            underlying: &root,
            ..Default::default()
        })
        .into_iter()
        .find(|r| r.lot_size > 0)
    {
        return Some(i64::from(row.lot_size));
    }
    g.search_prefix(&root, Some(&venue), 200)
        .into_iter()
        .filter(|r| r.lot_size > 0)
        .find(|r| {
            let tail = &r.symbol[root.len().min(r.symbol.len())..];
            tail.is_empty() || tail.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .map(|r| i64::from(r.lot_size))
}

/// Whether the master lists this exact symbol on this exchange. True when
/// the master has no rows for the venue at all (not downloaded yet).
pub fn contract_exists(g: &SymbolGeneration, symbol: &str, exchange: &str) -> bool {
    if symbol.is_empty() || exchange.is_empty() {
        return false;
    }
    let (name, venue) = (symbol.to_ascii_uppercase(), exchange.to_ascii_uppercase());
    if g.by_symbol(&venue, &name).is_some() {
        return true;
    }
    !g.rows().iter().any(|r| r.exchange == venue)
}

/// Turn a configured quantity into the number the broker is sent.
/// Returns `(quantity, lot_size)` or the reason it cannot be.
pub fn resolve_quantity(
    g: &SymbolGeneration,
    count: i64,
    qty_mode: &str,
    symbol: &str,
    exchange: &str,
) -> Result<(i64, Option<i64>), String> {
    if count <= 0 {
        return Err("Quantity must be greater than zero".into());
    }
    let lot = lot_size_for(g, symbol, exchange);
    if qty_mode != "lots" {
        if let Some(l) = lot {
            if count % l != 0 {
                return Err(format!(
                    "{} is not a whole number of lots; {} trades in lots of {}",
                    count, symbol, l
                ));
            }
        }
        return Ok((count, lot));
    }
    match lot {
        Some(l) => Ok((count * l, Some(l))),
        None => Err(format!(
            "No lot size is known for {} on {}. Download the master contract, or set the quantity in units.",
            symbol, exchange
        )),
    }
}

/// `(whole, lot_size)`: true when not a derivative or the lot is unknown.
pub fn quantity_is_whole_lots(
    g: &SymbolGeneration,
    quantity: i64,
    symbol: &str,
    exchange: &str,
) -> (bool, Option<i64>) {
    match lot_size_for(g, symbol, exchange) {
        None => (true, None),
        Some(l) => (quantity > 0 && quantity % l == 0, Some(l)),
    }
}

/// One resolved expiry, in both spellings.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpiryResult {
    pub rank: String,
    /// `DD-MMM-YY`, as the master stores it.
    pub expiry: String,
    /// `DDMMMYY`, as a symbol embeds it.
    pub expiry_symbol: String,
    pub fallback: bool,
}

/// A failed resolution: a machine-readable code and a trader-facing reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub code: &'static str,
    pub error: String,
}

fn fail(code: &'static str, error: impl Into<String>) -> Failure {
    Failure {
        code,
        error: error.into(),
    }
}

/// Last expiry of each calendar month, ascending (`pairs` sorted ascending).
fn monthly(pairs: &[(NaiveDate, String)]) -> Vec<(NaiveDate, String)> {
    let mut out: Vec<(NaiveDate, String)> = Vec::new();
    for (d, s) in pairs {
        use chrono::Datelike;
        match out.last_mut() {
            Some((ld, ls)) if ld.year() == d.year() && ld.month() == d.month() => {
                *ld = *d;
                *ls = s.clone();
            }
            _ => out.push((*d, s.clone())),
        }
    }
    out
}

/// Turn a relative expiry rank into a dated expiry.
pub fn resolve_expiry_rank(
    g: &SymbolGeneration,
    base: &str,
    exchange: &str,
    instrument: &str,
    rank: &str,
    today: NaiveDate,
) -> Result<ExpiryResult, Failure> {
    let rank = rank.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if !EXPIRY_RANKS.contains(&rank.as_str()) {
        return Err(fail(
            "invalid_rank",
            format!(
                "Unknown expiry rank {:?}. Supported ranks are {}.",
                rank,
                EXPIRY_RANKS.join(", ")
            ),
        ));
    }
    let deriv = derivatives_exchange(exchange);
    let mut raw: Vec<String> = match instrument {
        "futures" => g.expiries(&deriv, base, Some("FUT")),
        "options" => {
            let mut v = g.expiries(&deriv, base, Some("CE"));
            v.extend(g.expiries(&deriv, base, Some("PE")));
            v
        }
        _ => {
            return Err(fail(
                "invalid_instrument_type",
                format!(
                    "Unknown instrument type {:?}. Expiries are listed for options or futures.",
                    instrument
                ),
            ))
        }
    };
    raw.sort();
    raw.dedup();
    let mut pairs: Vec<(NaiveDate, String)> = raw
        .into_iter()
        .filter_map(|t| parse_oa_expiry(&t).map(|d| (d, t.to_ascii_uppercase())))
        .filter(|(d, _)| *d >= today)
        .collect();
    pairs.sort();
    pairs.dedup();
    if pairs.is_empty() {
        return Err(fail(
            "no_expiry",
            format!(
                "No live {} expiry found for {} on {}. The master contract may need re-downloading.",
                instrument, base, deriv
            ),
        ));
    }
    let mut fallback = false;
    let chosen = match rank.as_str() {
        "weekly" | "current" => pairs[0].clone(),
        "next_week" | "next" => {
            fallback = pairs.len() < 2;
            pairs.get(1).cloned().unwrap_or_else(|| pairs[0].clone())
        }
        _ => {
            let m = monthly(&pairs);
            if rank == "monthly" {
                m[0].clone()
            } else {
                fallback = m.len() < 2;
                m.get(1).cloned().unwrap_or_else(|| m[0].clone())
            }
        }
    };
    Ok(ExpiryResult {
        rank,
        expiry_symbol: chosen.0.format("%d%b%y").to_string().to_ascii_uppercase(),
        expiry: chosen.1,
        fallback,
    })
}

/// A leg turned into an exact contract.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolvedLeg {
    pub symbol: String,
    pub exchange: String,
    pub lotsize: i64,
    pub quantity: i64,
    pub lots: i64,
    pub strike: Option<f64>,
    pub expiry: Option<String>,
    pub expiry_rank: Option<String>,
    pub expiry_fallback: bool,
    pub underlying_ltp: Option<f64>,
}

fn finish(
    row: &SymToken,
    lots: i64,
    strike: Option<f64>,
    expiry: Option<&ExpiryResult>,
    ltp: Option<f64>,
) -> Result<ResolvedLeg, Failure> {
    if row.lot_size <= 0 {
        return Err(fail(
            "invalid_lotsize",
            format!(
                "The master contract gives {} on {} a lot size of {}, so no quantity can be derived from it. Re-download the master contract.",
                row.symbol, row.exchange, row.lot_size
            ),
        ));
    }
    let lotsize = i64::from(row.lot_size);
    Ok(ResolvedLeg {
        symbol: row.symbol.clone(),
        exchange: row.exchange.clone(),
        lotsize,
        quantity: lots * lotsize,
        lots,
        strike,
        expiry: expiry.map(|e| e.expiry.clone()),
        expiry_rank: expiry.map(|e| e.rank.clone()),
        expiry_fallback: expiry.map(|e| e.fallback).unwrap_or(false),
        underlying_ltp: ltp,
    })
}

/// The instrument an ATM strike is measured against.
fn quote_target(
    g: &SymbolGeneration,
    base: &str,
    exchange: &str,
    today: NaiveDate,
) -> Option<(String, String)> {
    let ex = exchange.to_ascii_uppercase();
    if NO_SPOT.contains(&ex.as_str()) {
        // An MCX commodity option prices off its nearest unexpired future.
        let mut futs: Vec<(NaiveDate, String)> = g
            .rows()
            .iter()
            .filter(|r| r.exchange == ex && r.instrument_type == "FUT" && r.name == base)
            .filter_map(|r| parse_oa_expiry(&r.expiry).map(|d| (d, r.symbol.clone())))
            .filter(|(d, _)| *d >= today)
            .collect();
        futs.sort();
        return futs.into_iter().next().map(|(_, s)| (s, ex));
    }
    Some((base.to_string(), ex))
}

/// Resolve one batch leg. `shared_ltp` carries the underlying's price from
/// the first leg so every leg of one spread settles around the same ATM.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_leg(
    g: &SymbolGeneration,
    gateway: &dyn OrderGateway,
    leg: &Value,
    underlying: &str,
    underlying_exchange: &str,
    today: NaiveDate,
    shared_ltp: Option<f64>,
) -> Result<ResolvedLeg, Failure> {
    let (base, _) = crate::services::options_service::parse_underlying(underlying);
    if base.is_empty() {
        return Err(fail("invalid_underlying", "No underlying symbol supplied."));
    }
    let segment = leg["segment"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !SEGMENTS.contains(&segment.as_str()) {
        return Err(fail(
            "invalid_segment",
            format!(
                "Unknown segment {:?} on a {} leg. Supported segments are cash, futures, options.",
                segment, base
            ),
        ));
    }
    let lots = match &leg["lots"] {
        Value::Null => 1,
        v => match crate::risk::value_to_f64(v) {
            Some(f) if f.is_finite() && f > 0.0 && f.fract() == 0.0 => f as i64,
            _ => {
                return Err(fail(
                    "invalid_lots",
                    format!(
                        "Lots on a {} {} leg must be a whole number above zero, got {}.",
                        base, segment, v
                    ),
                ))
            }
        },
    };

    let uex = underlying_exchange.trim().to_ascii_uppercase();
    if segment == "cash" {
        let exchange = match uex.as_str() {
            "NSE_INDEX" => "NSE".to_string(),
            "BSE_INDEX" => "BSE".to_string(),
            other => other.to_string(),
        };
        let Some(row) = g.by_symbol(&exchange, &base) else {
            let index = uex.ends_with("_INDEX");
            return Err(fail(
                "contract_not_found",
                format!(
                    "No cash contract found for {} on {}.{}",
                    base,
                    exchange,
                    if index {
                        " An index has no cash instrument of its own and cannot be traded directly."
                    } else {
                        ""
                    }
                ),
            ));
        };
        return finish(row, lots, None, None, None);
    }

    let exchange = derivatives_exchange(&uex);
    let instrument = if segment == "futures" {
        "futures"
    } else {
        "options"
    };
    let declared = leg["expiry"].as_str().unwrap_or("current");
    let expiry = match parse_literal_expiry(declared) {
        Some(e) => e,
        None => resolve_expiry_rank(g, &base, &uex, instrument, declared, today)?,
    };

    if segment == "futures" {
        let symbol = format!("{}{}FUT", base, expiry.expiry_symbol);
        let Some(row) = g.by_symbol(&exchange, &symbol) else {
            return Err(fail(
                "contract_not_found",
                format!(
                    "No futures contract found for {} {} on {} (looked for {}).",
                    base, expiry.expiry, exchange, symbol
                ),
            ));
        };
        return finish(row, lots, None, Some(&expiry), None);
    }

    let option_type = leg["option_type"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if option_type != "CE" && option_type != "PE" {
        return Err(fail(
            "invalid_leg",
            format!(
                "Invalid option_type: '{}'. Supported option types are CE and PE.",
                option_type
            ),
        ));
    }
    let strike_mode = leg["strike_mode"]
        .as_str()
        .unwrap_or("atm")
        .to_ascii_lowercase();
    let mut ltp = None;
    let strike = if strike_mode == "strike" {
        match crate::risk::value_to_f64(&leg["strike"]) {
            Some(k) if k.is_finite() && k > 0.0 => k,
            _ => {
                return Err(fail(
                    "invalid_strike",
                    format!(
                        "Strike {} on a {} option leg is not a usable price. A strike must be a positive number, and may be fractional.",
                        leg["strike"], base
                    ),
                ))
            }
        }
    } else if strike_mode == "atm" {
        let offset = leg["atm_offset"]
            .as_str()
            .unwrap_or("ATM")
            .trim()
            .to_ascii_uppercase();
        let valid = offset == "ATM"
            || ["ITM", "OTM"].iter().any(|p| {
                offset
                    .strip_prefix(p)
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| (1..=5).contains(&n))
            });
        if !valid {
            return Err(fail(
                "invalid_offset",
                format!(
                    "Unknown offset {:?} on a {} option leg. Supported offsets are ATM, ITM1 to ITM5 and OTM1 to OTM5.",
                    offset, base
                ),
            ));
        }
        let price = match shared_ltp {
            Some(p) => p,
            None => {
                let Some((qs, qx)) = quote_target(g, &base, &uex, today) else {
                    return Err(fail(
                        "no_underlying_contract",
                        format!(
                            "No unexpired futures contract for {} on {}, so there is no reference price for the ATM strike. Check the symbol, or re-download the master contract.",
                            base, uex
                        ),
                    ));
                };
                gateway
                    .ltp(&qs, &qx)
                    .await
                    .map_err(|e| fail("quote_failed", e))?
            }
        };
        ltp = Some(price);
        let strikes = available_strikes(
            g.rows(),
            &base,
            &expiry.expiry_symbol,
            &option_type,
            &exchange,
        );
        if strikes.is_empty() {
            return Err(fail(
                "no_strikes",
                format!(
                    "No {} strikes listed for {} expiring {} on {}. Check the expiry, or re-download the master contract.",
                    option_type, base, expiry.expiry_symbol, exchange
                ),
            ));
        }
        let Some(atm) = atm_index(&strikes, price) else {
            return Err(fail(
                "no_atm_strike",
                format!(
                    "Could not pick an ATM strike for {} from a last price of {}.",
                    base, price
                ),
            ));
        };
        let n: i64 = offset.get(3..).and_then(|n| n.parse().ok()).unwrap_or(0);
        let call = option_type == "CE";
        // PE ITM and CE OTM move up the ladder.
        let up = offset.starts_with("ITM") != call;
        let idx = if offset == "ATM" {
            atm as i64
        } else if up {
            atm as i64 + n
        } else {
            atm as i64 - n
        };
        if idx < 0 || idx >= strikes.len() as i64 {
            return Err(fail(
                "offset_out_of_range",
                format!(
                    "Offset {} runs off the end of the {} {} {} chain, which lists {} strikes around an ATM of {}.",
                    offset,
                    base,
                    expiry.expiry_symbol,
                    option_type,
                    strikes.len(),
                    format_strike(strikes[atm])
                ),
            ));
        }
        strikes[idx as usize]
    } else {
        return Err(fail(
            "invalid_strike_mode",
            format!(
                "Unknown strike mode {:?} on a {} option leg. Supported modes are atm, strike.",
                strike_mode, base
            ),
        ));
    };
    let symbol = format!(
        "{}{}{}{}",
        base,
        expiry.expiry_symbol,
        format_strike(strike),
        option_type
    );
    let Some(row) = g.by_symbol(&exchange, &symbol) else {
        return Err(fail(
            "contract_not_found",
            format!(
                "No option contract found for {} {} {} {} on {} (looked for {}).",
                base,
                expiry.expiry,
                format_strike(strike),
                option_type,
                exchange,
                symbol
            ),
        ));
    };
    finish(row, lots, Some(strike), Some(&expiry), ltp)
}

/// A literal `DD-MMM-YY` / `DDMMMYY` (two or four digit year) expiry.
fn parse_literal_expiry(text: &str) -> Option<ExpiryResult> {
    let t = text.trim().to_ascii_uppercase();
    let compact = t.replace('-', "");
    if compact.len() != 7 && compact.len() != 9 {
        return None;
    }
    let (d, rest) = compact.split_at(2);
    let (m, y) = rest.split_at(3);
    if !d.chars().all(|c| c.is_ascii_digit()) || !y.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let date = parse_oa_expiry(&format!("{}-{}-{}", d, m, y))?;
    Some(ExpiryResult {
        rank: "literal".into(),
        expiry: t,
        expiry_symbol: date.format("%d%b%y").to_string().to_ascii_uppercase(),
        fallback: false,
    })
}
