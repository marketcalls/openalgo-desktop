//! The option chain with what the broker could not supply (TOOLS-04,
//! TOOLS-13): `/api/v1/optionchain` answers its body with the web's zeros
//! for a missing leg (contract), and the chain tools read which legs came
//! back without a quote or without open interest, so they never total a
//! gap as zero.

use super::*;
use crate::brokers::types::{ChainQuoteRequest, ChainQuotes};
use crate::error::AppError;
use std::collections::{HashMap, HashSet};

/// An option chain with what the broker could not supply: legs that came
/// back without a quote, and legs whose open interest did not come back.
#[derive(Debug, Clone)]
pub struct ChainAnswer {
    pub body: Value,
    /// Symbols (OpenAlgo) with no quote.
    pub quote_missing: HashSet<String>,
    /// Symbols (OpenAlgo) quoted without their open interest.
    pub oi_missing: HashSet<String>,
}

impl ChainAnswer {
    /// Legs in the chain whose open interest is not a reading.
    pub fn oi_unknown(&self) -> HashSet<&str> {
        self.quote_missing
            .iter()
            .chain(self.oi_missing.iter())
            .map(String::as_str)
            .collect()
    }
}

/// One strike of the requested window with its two contracts, if listed.
struct WindowStrike {
    strike: f64,
    ce: Option<SymToken>,
    pe: Option<SymToken>,
    /// Position among all listed strikes (for the ITM/OTM label).
    index: usize,
}

/// A chain's strikes and contracts before any leg is quoted: the
/// underlying, its price, the listed strikes, ATM and the window.
struct Prepared {
    h: BrokerHandle,
    u: Underlying,
    uq: Quote,
    opt_ex: String,
    strikes: Vec<f64>,
    atm: usize,
    window: Vec<WindowStrike>,
}

async fn prepare(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry_date: &str,
    strike_count: Option<i64>,
) -> Result<Prepared, Reply> {
    let h = broker_handle(ctx)?;
    let u = resolve_underlying(ctx, underlying, exchange, Some(expiry_date), true)?;
    let uq = underlying_ltp(ctx, &h, &u, ":").await?;
    let opt_ex = option_exchange(&u.quote_exchange);
    let snap = ctx.symbols.snapshot();
    let strikes = available_strikes(snap.rows(), &u.base, &u.expiry, "CE", &opt_ex);
    if strikes.is_empty() {
        return Err(no_strikes(&u.base, &u.expiry));
    }
    let Some(atm) = atm_index(&strikes, uq.ltp) else {
        return Err(Reply::error(500, "Failed to determine ATM strike"));
    };
    let (start, end) = strike_window(&strikes, atm, strike_count);
    let window = window_strikes(&snap, &u, &opt_ex, &strikes[start..end], start);
    Ok(Prepared {
        h,
        u,
        uq,
        opt_ex,
        strikes,
        atm,
        window,
    })
}

/// The chain's answer body from its rows.
fn chain_body(
    ctx: &AppState,
    p: &Prepared,
    rows: Vec<ChainRow>,
    with_quotes: bool,
    with_greeks: bool,
    forward: Option<f64>,
) -> Value {
    let chain: Vec<Value> = rows
        .into_iter()
        .map(|(k, c, p)| {
            json!({
                "strike": float(k),
                "ce": c.map(Value::Object).unwrap_or(Value::Null),
                "pe": p.map(Value::Object).unwrap_or(Value::Null),
            })
        })
        .collect();
    let expiry_dt = expiry_datetime(&p.u.expiry, &p.opt_ex, None);
    json!({
        "status": "success",
        "underlying": p.u.base,
        "underlying_symbol": p.u.quote_symbol,
        "underlying_exchange": p.u.quote_exchange,
        "underlying_ltp": num(p.uq.ltp),
        "underlying_prev_close": num(p.uq.close),
        "expiry_date": p.u.expiry,
        "expiry_ts": expiry_dt.map(|e| json!(e.timestamp())).unwrap_or(Value::Null),
        "server_ts": ctx.now().timestamp(),
        "atm_strike": float(p.strikes[p.atm]),
        "quotes_included": with_quotes,
        "greeks_included": with_quotes && with_greeks,
        "forward_price": forward.map(num).unwrap_or(Value::Null),
        "chain": chain,
    })
}

/// The chain without quotes (the web's `get_option_chain(...,
/// with_quotes=False)`): strikes, contracts, labels, lot and tick sizes and
/// the underlying's price, every leg's price fields zero. One underlying
/// quote, no per-strike broker call (the Flow editor's strike picker).
pub async fn option_chain_structure(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry_date: &str,
    strike_count: Option<i64>,
) -> Result<Value, Reply> {
    let p = prepare(ctx, underlying, exchange, expiry_date, strike_count).await?;
    let rows = chain_rows(&p.window, &HashMap::new(), p.atm);
    Ok(chain_body(ctx, &p, rows, false, false, None))
}

/// [`super::option_chain`] with the legs the broker could not supply.
pub async fn option_chain_answer(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry_date: &str,
    strike_count: Option<i64>,
    with_greeks: bool,
    interest_rate: Option<f64>,
) -> Result<ChainAnswer, Reply> {
    let p = prepare(ctx, underlying, exchange, expiry_date, strike_count).await?;
    let keys: Vec<QuoteKey> = p
        .window
        .iter()
        .flat_map(|w| [&w.ce, &w.pe])
        .flatten()
        .map(|r| QuoteKey::new(r.exchange.clone(), r.symbol.clone()))
        .collect();
    if keys.is_empty() {
        return Err(Reply::error(
            404,
            "No valid option symbols found for the given parameters",
        ));
    }
    let fetched = chain_quotes(&p.h, &p.u, &keys, strike_count).await?;
    let quotes: HashMap<String, Quote> = fetched
        .quotes
        .into_iter()
        .filter_map(|r| r.data.map(|q| (r.symbol, q)))
        .collect();
    let quote_missing: HashSet<String> = keys
        .iter()
        .filter(|k| !quotes.contains_key(&k.symbol))
        .map(|k| k.symbol.clone())
        .collect();
    let mut rows = chain_rows(&p.window, &quotes, p.atm);
    let forward = if with_greeks {
        let r = interest_rate.unwrap_or(0.0) / 100.0;
        let expiry_dt = expiry_datetime(&p.u.expiry, &p.opt_ex, None);
        let t = expiry_dt
            .and_then(|e| time_to_expiry(e, ctx.now()))
            .map(|t| t.1);
        Some(add_chain_greeks(
            &mut rows,
            p.strikes[p.atm],
            p.uq.ltp,
            t,
            r,
        ))
    } else {
        None
    };
    let body = chain_body(ctx, &p, rows, true, with_greeks, forward);
    Ok(ChainAnswer {
        body,
        quote_missing,
        oi_missing: fetched.oi_missing,
    })
}

/// The strikes `strike_count` each side of ATM, or all of them.
fn strike_window(strikes: &[f64], atm: usize, strike_count: Option<i64>) -> (usize, usize) {
    match strike_count {
        Some(sc) if sc > 0 => {
            let sc = sc as usize;
            (atm.saturating_sub(sc), (atm + sc + 1).min(strikes.len()))
        }
        _ => (0, strikes.len()),
    }
}

/// The window's strikes with their CE and PE contracts from the master.
fn window_strikes(
    snap: &crate::brokers::common::symbols::SymbolGeneration,
    u: &Underlying,
    opt_ex: &str,
    window: &[f64],
    start: usize,
) -> Vec<WindowStrike> {
    window
        .iter()
        .enumerate()
        .map(|(off, k)| {
            let find = |t: &str| {
                let sym = format!("{}{}{}{}", u.base, u.expiry, format_strike(*k), t);
                snap.by_symbol(opt_ex, &sym).cloned()
            };
            WindowStrike {
                strike: *k,
                ce: find("CE"),
                pe: find("PE"),
                index: start + off,
            }
        })
        .collect()
}

/// The legs' quotes: through the broker's chain quotes (which say which
/// legs came back without OI) when the expiry reads, else a batch quote.
async fn chain_quotes(
    h: &BrokerHandle,
    u: &Underlying,
    keys: &[QuoteKey],
    strike_count: Option<i64>,
) -> Result<ChainQuotes, Reply> {
    let fetched = match parse_compact_expiry(&u.expiry) {
        Some(expiry) => {
            let req = ChainQuoteRequest {
                underlying: QuoteKey::new(u.quote_exchange.clone(), u.quote_symbol.clone()),
                expiry,
                keys: keys.to_vec(),
                strikes_each_side: strike_count.filter(|n| *n > 0).map(|n| n as usize),
            };
            h.broker.get_chain_quotes(&h.auth, &req).await
        }
        None => h
            .broker
            .get_multiquotes(&h.auth, keys)
            .await
            .map(|quotes| ChainQuotes {
                quotes,
                oi_missing: HashSet::new(),
            }),
    };
    fetched.map_err(|e| match e {
        // Not sent: the web answers its busy refusal as is (429), since a
        // chain of zero prices would look like a dead market.
        AppError::Busy(_) => Reply::broker_failure(&e, 500),
        _ => Reply::error(
            500,
            format!(
                "An error occurred while fetching option chain: {}",
                e.client_message()
            ),
        ),
    })
}

/// One row per strike with each listed leg's quote and label.
fn chain_rows(
    window: &[WindowStrike],
    quotes: &HashMap<String, Quote>,
    atm: usize,
) -> Vec<ChainRow> {
    window
        .iter()
        .map(|w| {
            let leg = |r: &Option<SymToken>, call: bool| {
                r.as_ref()
                    .map(|r| leg_json(r, &label(w.index, atm, call), quotes.get(&r.symbol)))
            };
            (w.strike, leg(&w.ce, true), leg(&w.pe, false))
        })
        .collect()
}

/// Greeks on every leg, priced against the forward read from the ATM
/// strike's call and put (the underlying's LTP when either has no price);
/// returns that forward. Without a time to expiry the legs keep no Greeks.
fn add_chain_greeks(
    rows: &mut [ChainRow],
    atm_strike: f64,
    underlying_ltp: f64,
    t_years: Option<f64>,
    r: f64,
) -> f64 {
    let leg_ltp = |l: &Option<Map<String, Value>>| {
        l.as_ref()
            .and_then(|m| m.get("ltp"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let atm_row = rows.iter().find(|(k, _, _)| (*k - atm_strike).abs() < 1e-9);
    let forward = match atm_row {
        Some((k, c, p)) if leg_ltp(c) > 0.0 && leg_ltp(p) > 0.0 => {
            bs::py_round(k + leg_ltp(c) - leg_ltp(p), 4)
        }
        _ => underlying_ltp,
    };
    if let Some(t) = t_years {
        for (k, c, p) in rows.iter_mut() {
            if let Some(c) = c.as_mut() {
                add_leg_greeks(c, true, forward, *k, t, r);
            }
            if let Some(p) = p.as_mut() {
                add_leg_greeks(p, false, forward, *k, t, r);
            }
        }
    }
    forward
}
