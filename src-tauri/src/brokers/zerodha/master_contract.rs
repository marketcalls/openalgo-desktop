//! Kite instrument dump -> OpenAlgo symbol master
//! (web `database/master_contract_db.py::process_zerodha_csv`).
//!
//! Columns are located by header name (the live header is
//! `instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,
//! strike,tick_size,lot_size,instrument_type,segment,exchange`), so a column
//! added or reordered by Kite cannot shift fields again.

use super::mapping::contract_size;
use super::{session_expired, ZerodhaBroker};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, future_symbol, option_symbol, parse_broker_expiry, rename, CsvHeader,
    BSE_INDEX_RENAMES, NSE_INDEX_RENAMES,
};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use std::collections::{HashMap, HashSet};

/// NSE series in which a company's own units trade outside EQ (web
/// `_NSE_SERIES_SUFFIXES`, openalgo#2463). Kite keeps the series on the
/// tradingsymbol of these (HFCL-BE, EMBASSY-RR, GOLDSTAR-ST), while
/// OpenAlgo's equity symbol is the bare exchange symbol, as NSE lists it
/// (Symbol HFCL, Series BE). `brsymbol` keeps Kite's tradingsymbol: every
/// order, quote, history and margin call sends `br_symbol()`, and every book
/// maps back through the resolver. Not stripped: the debt and government
/// security series (SG, GS, TB, N0 and the like), and -RL, whose bare names
/// exist as rows of their own.
const NSE_SERIES_SUFFIXES: [&str; 6] = ["BE", "BZ", "RR", "IV", "SM", "ST"];

/// Kite exchange -> OpenAlgo exchange (rows on any other exchange are dropped).
fn map_exchange(kite: &str) -> Option<&'static str> {
    Some(match kite {
        "NSE" => "NSE",
        "NFO" => "NFO",
        "CDS" => "CDS",
        "BSE" => "BSE",
        "BFO" => "BFO",
        "BCD" => "BCD",
        "MCX" => "MCX",
        "NCO" => "NCO",
        "NSE_INDEX" => "NSE_INDEX",
        "BSE_INDEX" => "BSE_INDEX",
        "GLOBAL" | "NSEIX" => "GLOBAL_INDEX",
        _ => return None,
    })
}

struct Cols {
    instrument_token: usize,
    exchange_token: usize,
    tradingsymbol: usize,
    name: usize,
    expiry: usize,
    strike: usize,
    tick_size: usize,
    lot_size: usize,
    instrument_type: usize,
    segment: usize,
    exchange: usize,
}

impl Cols {
    fn from_header(h: &CsvHeader) -> Result<Self> {
        let col = |n: &str| {
            h.index(n).ok_or_else(|| {
                tracing::error!("Kite instrument dump has no '{}' column", n);
                AppError::Broker(
                    "Zerodha's instrument list has an unexpected format. Try downloading the master contract again later."
                        .into(),
                )
            })
        };
        Ok(Self {
            instrument_token: col("instrument_token")?,
            exchange_token: col("exchange_token")?,
            tradingsymbol: col("tradingsymbol")?,
            name: col("name")?,
            expiry: col("expiry")?,
            strike: col("strike")?,
            tick_size: col("tick_size")?,
            lot_size: col("lot_size")?,
            instrument_type: col("instrument_type")?,
            segment: col("segment")?,
            exchange: col("exchange")?,
        })
    }
}

/// Parse the whole CSV into master rows.
pub fn parse_instruments(csv: &str) -> Result<Vec<SymToken>> {
    let mut lines = csv.lines();
    let header = lines
        .next()
        .ok_or_else(|| AppError::Broker("Zerodha returned an empty instrument list.".into()))?;
    let c = Cols::from_header(&CsvHeader::parse(header))?;
    let mut out = Vec::with_capacity(csv.len() / 90);
    let mut dropped = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = crate::brokers::common::master_contract::split_csv_line(line);
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        match parse_row(&c, &get) {
            Some(row) => out.push(row),
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::info!(
            "Kite instrument rows skipped (unknown exchange or malformed): {}",
            dropped
        );
    }
    strip_nse_series_suffix(&mut out);
    Ok(out)
}

/// The base symbol of a series-suffixed tradingsymbol (`HFCL` of
/// `HFCL-BE`), as the web's `^(.+)-(?:BE|BZ|RR|IV|SM|ST)$` reads it.
fn series_base(symbol: &str) -> Option<&str> {
    let (base, series) = symbol.rsplit_once('-')?;
    (!base.is_empty() && NSE_SERIES_SUFFIXES.contains(&series)).then_some(base)
}

/// Web `strip_nse_series_suffix`: NSE cash rows traded outside the EQ
/// series get their base symbol; `brsymbol` stays Kite's tradingsymbol.
///
/// A row keeps its suffix when the base is already another NSE cash row's
/// symbol, or when two suffixed rows would claim the same base, so no two
/// NSE rows share a symbol. The master has no unique key on
/// `(symbol, exchange)` (migration 043) and the resolver answers a
/// duplicate pair with its first row, so a shared symbol would quietly
/// send one stock's orders to the other.
fn strip_nse_series_suffix(rows: &mut [SymToken]) {
    let is_cash = |r: &&SymToken| r.exchange == "NSE" && r.instrument_type == "EQ";
    let (renames, suffixed): (Vec<(usize, String)>, usize) = {
        let taken: HashSet<&str> = rows
            .iter()
            .filter(is_cash)
            .map(|r| r.symbol.as_str())
            .collect();
        let mut claims: HashMap<&str, usize> = HashMap::new();
        for base in rows
            .iter()
            .filter(is_cash)
            .filter_map(|r| series_base(&r.symbol))
        {
            *claims.entry(base).or_default() += 1;
        }
        let renames = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| is_cash(r))
            .filter_map(|(i, r)| {
                let base = series_base(&r.symbol)?;
                (!taken.contains(base) && claims.get(base) == Some(&1))
                    .then(|| (i, base.to_string()))
            })
            .collect();
        (renames, claims.values().sum())
    };
    if suffixed == 0 {
        return;
    }
    let stripped = renames.len();
    for (i, base) in renames {
        rows[i].symbol = base;
    }
    let kept = suffixed - stripped;
    if kept > 0 {
        tracing::info!(
            "NSE series suffix removed from {} symbols, kept on {} whose bare name is already in use",
            stripped,
            kept
        );
    } else {
        tracing::info!("NSE series suffix removed from {} symbols", stripped);
    }
}

fn parse_row<'a>(c: &Cols, get: &dyn Fn(usize) -> &'a str) -> Option<SymToken> {
    let kite_exchange = get(c.exchange);
    let mut exchange = map_exchange(kite_exchange)?.to_string();
    if get(c.segment) == "INDICES" {
        match exchange.as_str() {
            "NSE" => exchange = "NSE_INDEX".into(),
            "BSE" => exchange = "BSE_INDEX".into(),
            "MCX" => exchange = "MCX_INDEX".into(),
            "CDS" => exchange = "CDS_INDEX".into(),
            _ => {}
        }
    }
    let inst = get(c.instrument_token);
    let exch_tok = get(c.exchange_token);
    if inst.is_empty() {
        return None;
    }
    let brsymbol = get(c.tradingsymbol).to_string();
    let expiry_date = parse_broker_expiry(get(c.expiry));
    let expiry = expiry_date.map(format_expiry).unwrap_or_default();
    let strike: f64 = get(c.strike).parse().unwrap_or(0.0);
    let instrument_type = get(c.instrument_type).to_string();
    let mut lot_size: i32 = get(c.lot_size)
        .parse::<f64>()
        .map(|v| v as i32)
        .unwrap_or(1);
    let raw_name = get(c.name);
    // Kite reports lot_size 1 for every MCX row (it counts contracts); the
    // master carries the real lot, sized per expiry, and the adapter converts
    // back to contracts at the Kite boundary.
    if exchange == "MCX" && !raw_name.is_empty() {
        match contract_size(raw_name, expiry_date) {
            Some(s) => lot_size = i32::try_from(s).unwrap_or(lot_size),
            None => tracing::debug!("No MCX contract size for {}", raw_name),
        }
    }
    // Blank names (debt, NCO spot underlyings) fall back to the tradingsymbol.
    let name = if raw_name.is_empty() {
        brsymbol.clone()
    } else {
        raw_name.to_string()
    };
    let mut symbol = match instrument_type.as_str() {
        "FUT" => future_symbol(&name, &expiry),
        "CE" | "PE" => option_symbol(&name, &expiry, strike, &instrument_type),
        _ => brsymbol.clone(),
    };
    if let Some(s) = rename(NSE_INDEX_RENAMES, &symbol) {
        symbol = s.to_string();
    }
    if exchange == "BSE_INDEX" {
        if let Some(s) = rename(BSE_INDEX_RENAMES, &symbol) {
            symbol = s.to_string();
        }
    }
    if exchange == "GLOBAL_INDEX" && symbol == "GIFT NIFTY" {
        symbol = "GIFTNIFTY".into();
    }
    Some(SymToken {
        symbol,
        brsymbol,
        name,
        exchange,
        brexchange: kite_exchange.to_string(),
        token: format!("{}::::{}", inst, exch_tok),
        expiry,
        strike,
        lot_size,
        instrument_type,
        tick_size: get(c.tick_size).parse().unwrap_or(0.0),
    })
}

pub async fn download(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<SymToken>> {
    if auth.pair().is_none() {
        return Err(session_expired());
    }
    let resp = b
        .http
        .get(format!("{}/instruments", b.base_url))
        .header("X-Kite-Version", "3")
        .header("Authorization", format!("token {}", auth.raw()))
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(status = status.as_u16(), "Kite instrument download refused");
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(session_expired());
        }
        return Err(AppError::Broker(
            "Zerodha did not send the instrument list. Try downloading the master contract again."
                .into(),
        ));
    }
    let text = resp.text().await?;
    let rows = parse_instruments(&text)?;
    drop(text);
    tracing::info!("Zerodha master contract parsed: {} instruments", rows.len());
    Ok(rows)
}
