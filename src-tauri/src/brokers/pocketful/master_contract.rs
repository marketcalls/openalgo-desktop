//! Master contract (web `broker/pocketful/database/master_contract_db.py`,
//! rebuilt on Pocketful's new API in marketcalls/openalgo#2467).
//!
//! Source: the public exchange-wise ZIP at [`super::MASTER_URL`], the link
//! Pocketful's "Security Id List" page gives. It needs no session, so no
//! credential and nothing naming the account is sent. It holds
//! `{NSE,BSE,NFO,BFO,MCX}CompactScrip.csv`, every one with the same header:
//!
//! `exchange_token, trading_symbol, company_name, close_price, expiry,
//! strike, tick_size, lot_size, instrument_name, option_type, segment,
//! exchange, fin_instrm_pdct_tp_cd, asset_code`
//!
//! Into symtoken: `token` exchange_token, `brsymbol` trading_symbol
//! unchanged, `brexchange` the row's exchange (index rows keep NSE or BSE),
//! `exchange` the OpenAlgo exchange (NSE_INDEX for NSE segment `INDICES`,
//! BSE_INDEX for BSE segment `IDX`), `expiry` `19-Nov-2026` as `19-NOV-26`,
//! `strike` for options (futures carry -0.01, stored as 0), `lotsize` and
//! `tick_size` as given (already units and rupees on every segment, MCX
//! included), `instrumenttype` EQ for cash and index rows, FUT, CE or PE.
//!
//! * NSE: `segment` is the series and ends every trading symbol. The
//!   OpenAlgo symbol is NSE's base symbol, so the equity series (EQ, BE, BZ,
//!   RR, IV, SM, ST) lose that one suffix (BAJAJ-AUTO-EQ is BAJAJ-AUTO);
//!   debt, bonds and partly paid shares keep the trading symbol, and so does
//!   a row whose bare name is already a symbol.
//! * BSE: `segment` is the BSE group (RELIANCE-A, SPPL-RE1-R), stripped the
//!   same way. Index rows carry BSE's short codes (SNSX50, `BSE CG`).
//! * NFO and BFO: `option_type` CE, PE or XX (futures); `company_name` is
//!   the underlying, so the symbol is built from it and the expiry column,
//!   never parsed from the weekly trading symbols (NIFTY26D0125850PE).
//! * MCX: `company_name` is blank, so the underlying is the trading symbol
//!   without its own expiry and strike suffix (SILVER10026NOVFUT is
//!   SILVER100). Spot `COM` rows (expiry 01-Jan-0001) are not loaded.
//!
//! A download that fails answers the web's `FAILED_MESSAGE` (the stored
//! master stays in use), with the cause in the log only.
//!
//! Desktop differences, none of which changes a symbol the web builds:
//! every one of the five files must yield instruments (MC-02: a partial
//! master is refused by name and the stored one kept; the web refuses only
//! an empty whole); a repeated `(exchange, token)` keeps its first row, as
//! the symbol resolver does with what it loads; a blank `exchange` column
//! falls back to the file's exchange.

use super::{zip, PocketfulBroker};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, future_symbol, option_symbol, part_missing, rename,
    split_csv_line, BSE_INDEX_CODE_RENAMES, BSE_INDEX_RENAMES, NSE_INDEX_RENAMES,
    NSE_SERIES_SUFFIXES,
};
use crate::brokers::common::redact;
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The exchanges of the archive, one file each (web `CONTRACT_EXCHANGES`).
const EXCHANGES: [&str; 5] = ["NSE", "BSE", "NFO", "BFO", "MCX"];

/// Columns every file must have (web `REQUIRED_COLUMNS`).
const REQUIRED_COLUMNS: [&str; 11] = [
    "exchange_token",
    "trading_symbol",
    "company_name",
    "expiry",
    "strike",
    "tick_size",
    "lot_size",
    "instrument_name",
    "option_type",
    "segment",
    "exchange",
];

const NSE_INDEX_SEGMENT: &str = "INDICES";
const BSE_INDEX_SEGMENT: &str = "IDX";

/// Exchange test indices ("INDEX1 NSETEST"): not real indices, not loaded.
const NSE_TEST_INDEX: &str = "NSETEST";

/// NSE index names beyond [`NSE_INDEX_RENAMES`]: the index NFO lists as the
/// NIFTYFPI underlying (symbol-format.md); the plain rule gives NIFTYFPI150,
/// and the options tools join an index to its chain by that symbol.
const NSE_INDEX_EXTRA: &[(&str, &str)] = &[("NIFTYFPI150", "NIFTYFPI")];

/// BSE index codes beyond the shared tables (web, from `broker/sahi`).
const BSE_INDEX_EXTRA: &[(&str, &str)] = &[("BSHOSP", "BSEHOSPITALS")];

/// Weekly derivative symbols carry the month as one character.
const WEEKLY_MONTH_CODES: [char; 12] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', 'O', 'N', 'D'];

/// web `FAILED_MESSAGE`: what the trader reads when a download fails.
const FAILED_MESSAGE: &str = "Could not refresh the Pocketful symbol list. Symbols from the last successful download are still in use. Try again in a few minutes.";

/// A failed download, as the trader reads it; `cause` goes to the log.
fn failed(cause: &str) -> AppError {
    tracing::warn!("Pocketful master contract download failed: {}", cause);
    AppError::Broker(FAILED_MESSAGE.into())
}

fn file_name(exchange: &str) -> String {
    format!("{}CompactScrip.csv", exchange)
}

/// A file missing from the archive, empty, or with no instruments (MC-02).
fn incomplete(exchange: &str) -> AppError {
    tracing::warn!(
        "Pocketful master file {} is missing or has no instruments",
        file_name(exchange)
    );
    part_missing("Pocketful", exchange)
}

/// One exchange's file, its columns by lowercased header name (web
/// `_read_contract_csv`).
struct ContractFile {
    cols: HashMap<String, usize>,
    rows: Vec<Vec<String>>,
}

/// One row of a [`ContractFile`].
#[derive(Clone, Copy)]
struct Line<'a> {
    fields: &'a [String],
    cols: &'a HashMap<String, usize>,
}

impl<'a> Line<'a> {
    fn get(&self, name: &str) -> &'a str {
        self.cols
            .get(name)
            .and_then(|&i| self.fields.get(i))
            .map(|s| s.trim())
            .unwrap_or("")
    }
}

impl ContractFile {
    /// Read `exchange`'s file, refused when a required column is missing.
    fn read(text: &str, exchange: &str) -> Result<Self> {
        let mut lines = text.lines();
        let cols: HashMap<String, usize> = split_csv_line(lines.next().unwrap_or(""))
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let name = n.trim().trim_start_matches('\u{feff}').trim();
                (name.to_lowercase(), i)
            })
            .collect();
        let missing: Vec<&str> = REQUIRED_COLUMNS
            .into_iter()
            .filter(|c| !cols.contains_key(*c))
            .collect();
        if !missing.is_empty() {
            return Err(failed(&format!(
                "{} has no column {}",
                file_name(exchange),
                missing.join(", ")
            )));
        }
        let rows = lines
            .filter(|l| !l.trim().is_empty())
            .map(split_csv_line)
            .collect();
        Ok(Self { cols, rows })
    }

    fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        self.rows.iter().map(|fields| Line {
            fields,
            cols: &self.cols,
        })
    }
}

/// web `pd.to_numeric(..., errors="coerce").fillna(0)`.
fn number(s: &str) -> f64 {
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

/// A symtoken row with the columns every segment shares (web `_rows`).
fn base_row(line: Line<'_>, symbol: String, name: String, exchange: &str, file: &str) -> SymToken {
    let brexchange = match line.get("exchange") {
        "" => file,
        e => e,
    };
    SymToken {
        symbol,
        brsymbol: line.get("trading_symbol").to_string(),
        name,
        exchange: exchange.to_string(),
        brexchange: brexchange.to_string(),
        token: line.get("exchange_token").to_string(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: number(line.get("lot_size")) as i32,
        instrument_type: "EQ".into(),
        tick_size: number(line.get("tick_size")),
    }
}

/// Cash rows' name: the company, else the trading symbol.
fn company(line: Line<'_>) -> String {
    match line.get("company_name") {
        "" => line.get("trading_symbol").to_string(),
        n => n.to_string(),
    }
}

/// The OpenAlgo symbol of each cash row (web `_strip_series`): its trading
/// symbol without its own "-<segment>" suffix, where `tier` gives the row a
/// tier to be stripped in (lower first; `None`: never stripped). A row keeps
/// its trading symbol when the bare name is already a symbol (a row that is
/// not stripped, or one stripped in an earlier tier) or when two rows of its
/// tier would claim it. Also returns how many rows kept their suffix.
fn strip_series(lines: &[Line<'_>], tier: impl Fn(&str) -> Option<usize>) -> (Vec<String>, usize) {
    let candidates: Vec<Option<(usize, &str)>> = lines
        .iter()
        .map(|l| {
            let (trading, series) = (l.get("trading_symbol"), l.get("segment"));
            let bare = trading
                .strip_suffix(series)
                .and_then(|t| t.strip_suffix('-'))
                .filter(|b| !b.is_empty())?;
            Some((tier(series)?, bare))
        })
        .collect();
    let mut symbols: Vec<String> = lines
        .iter()
        .map(|l| l.get("trading_symbol").to_string())
        .collect();
    let mut taken: HashSet<&str> = lines
        .iter()
        .zip(&candidates)
        .filter(|(_, c)| c.is_none())
        .map(|(l, _)| l.get("trading_symbol"))
        .collect();
    let tiers = candidates.iter().flatten().map(|(t, _)| t + 1).max();
    let mut kept = 0;
    for t in 0..tiers.unwrap_or(0) {
        let in_tier = || {
            candidates
                .iter()
                .enumerate()
                .filter_map(move |(i, c)| match c {
                    Some((ct, bare)) if *ct == t => Some((i, *bare)),
                    _ => None,
                })
        };
        let mut claims: HashMap<&str, usize> = HashMap::new();
        for (_, bare) in in_tier() {
            *claims.entry(bare).or_default() += 1;
        }
        let stripped: Vec<(usize, &str)> = in_tier()
            .filter(|(_, bare)| !taken.contains(bare) && claims.get(bare) == Some(&1))
            .collect();
        kept += in_tier().count() - stripped.len();
        for (i, bare) in stripped {
            symbols[i] = bare.to_string();
            taken.insert(bare);
        }
    }
    (symbols, kept)
}

/// web `process_pocketful_nse_csv`: NSE cash rows of every series, EQ
/// first, so a stock listed in EQ and another series keeps the bare symbol
/// on its EQ row.
fn nse_cash(file: &ContractFile) -> Vec<SymToken> {
    let lines: Vec<Line<'_>> = file
        .lines()
        .filter(|l| l.get("segment") != NSE_INDEX_SEGMENT)
        .collect();
    let (symbols, kept) = strip_series(&lines, |series| {
        let series = series.to_uppercase();
        if series == "EQ" {
            Some(0)
        } else {
            NSE_SERIES_SUFFIXES.contains(&series.as_str()).then_some(1)
        }
    });
    if kept > 0 {
        tracing::info!(
            "NSE series suffix kept on {} symbols whose bare name is in use",
            kept
        );
    }
    lines
        .iter()
        .zip(symbols)
        .map(|(&l, symbol)| base_row(l, symbol, company(l), "NSE", "NSE"))
        .collect()
}

/// web `process_pocketful_bse_csv`: BSE cash rows without the group suffix.
fn bse_cash(file: &ContractFile) -> Vec<SymToken> {
    let lines: Vec<Line<'_>> = file
        .lines()
        .filter(|l| l.get("segment") != BSE_INDEX_SEGMENT)
        .collect();
    let (symbols, kept) = strip_series(&lines, |_| Some(0));
    if kept > 0 {
        tracing::info!(
            "BSE group suffix kept on {} symbols whose bare name is in use",
            kept
        );
    }
    lines
        .iter()
        .zip(symbols)
        .map(|(&l, symbol)| base_row(l, symbol, company(l), "BSE", "BSE"))
        .collect()
}

/// `name` in capitals without the characters `skip` names.
fn compact(name: &str, skip: fn(char) -> bool) -> String {
    name.to_uppercase().chars().filter(|c| !skip(*c)).collect()
}

/// A rename table looked up by a `compact` key, each entry's name compacted
/// the same way (the shared tables keep the spaces brokers ship).
fn compact_rename(
    table: &[(&str, &'static str)],
    key: &str,
    skip: fn(char) -> bool,
) -> Option<&'static str> {
    table
        .iter()
        .find(|(from, _)| compact(from, skip) == key)
        .map(|(_, to)| *to)
}

/// web `_nse_index_symbol`: the name in capitals without spaces or hyphens,
/// unless symbol-format.md names the index otherwise ("Nifty 50" is NIFTY).
fn nse_index_symbol(name: &str) -> String {
    let skip = |c: char| c.is_whitespace() || c == '-';
    let key = compact(name, skip);
    compact_rename(NSE_INDEX_RENAMES, &key, skip)
        .or_else(|| rename(NSE_INDEX_EXTRA, &key))
        .map_or(key, str::to_string)
}

/// web `_bse_index_symbol`: BSE's short code without spaces, renamed where
/// symbol-format.md names the index ("BSE CG" is BSECAPITALGOODS). A code
/// no table lists keeps its own name, so a new index is still addressable.
fn bse_index_symbol(code: &str) -> String {
    let skip = |c: char| c.is_whitespace();
    let key = compact(code, skip);
    compact_rename(BSE_INDEX_RENAMES, &key, skip)
        .or_else(|| rename(BSE_INDEX_CODE_RENAMES, &key))
        .or_else(|| rename(BSE_INDEX_EXTRA, &key))
        .map_or(key, str::to_string)
}

/// web `process_pocketful_indices_csv`: NSE and BSE index rows, as
/// NSE_INDEX and BSE_INDEX, named by the broker's index name.
fn indices(nse: &ContractFile, bse: &ContractFile) -> Vec<SymToken> {
    let nse_rows = nse
        .lines()
        .filter(|l| l.get("segment") == NSE_INDEX_SEGMENT)
        .filter(|l| {
            !l.get("trading_symbol")
                .to_uppercase()
                .contains(NSE_TEST_INDEX)
        })
        .map(|l| {
            (
                l,
                nse_index_symbol(l.get("trading_symbol")),
                "NSE_INDEX",
                "NSE",
            )
        });
    let bse_rows = bse
        .lines()
        .filter(|l| l.get("segment") == BSE_INDEX_SEGMENT)
        .map(|l| {
            (
                l,
                bse_index_symbol(l.get("trading_symbol")),
                "BSE_INDEX",
                "BSE",
            )
        });
    nse_rows
        .chain(bse_rows)
        .map(|(l, symbol, exchange, file)| {
            base_row(
                l,
                symbol,
                l.get("trading_symbol").to_string(),
                exchange,
                file,
            )
        })
        .collect()
}

/// web `pd.to_datetime(expiry, format="%d-%b-%Y", errors="coerce")`, with
/// the spot rows' sentinel year 1 read as no expiry.
fn contract_expiry(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%d-%b-%Y")
        .ok()
        .filter(|d| d.year() > 1900)
}

/// web `_underlying_from_symbol`: the trading symbol without its expiry and
/// strike suffix, rebuilt from the row's own expiry in the monthly form
/// (26NOV + tail) and the weekly form (26N19 + tail), so a digit inside an
/// underlying (SILVER100, SENSEX50) is never taken for part of the date.
/// Empty where neither form matches.
fn underlying_from_symbol(symbol: &str, expiry: NaiveDate, tail: &str) -> String {
    let year = expiry.year() % 100;
    let month = expiry.format("%b").to_string().to_uppercase();
    let weekly_month = WEEKLY_MONTH_CODES[expiry.month0() as usize];
    let monthly = format!("{:02}{}{}", year, month, tail);
    let weekly = format!("{:02}{}{:02}{}", year, weekly_month, expiry.day(), tail);
    [monthly, weekly]
        .iter()
        .find(|suffix| symbol.len() > suffix.len() && symbol.ends_with(suffix.as_str()))
        .map(|suffix| symbol[..symbol.len() - suffix.len()].to_string())
        .unwrap_or_default()
}

/// Why an F&O file's row is not loaded.
enum NotLoaded {
    /// No option type CE, PE or XX: MCX spot definitions.
    NotAContract,
    /// An expiry `%d-%b-%Y` does not read, or the spot sentinel year 1.
    NoExpiry,
    /// No company name, and none in the trading symbol either.
    NoUnderlying,
}

/// One F&O row in the OpenAlgo format.
fn contract(line: Line<'_>, exchange: &str) -> std::result::Result<SymToken, NotLoaded> {
    let option_type = line.get("option_type").to_uppercase();
    let is_option = matches!(option_type.as_str(), "CE" | "PE");
    if !is_option && option_type != "XX" {
        return Err(NotLoaded::NotAContract);
    }
    let expiry = contract_expiry(line.get("expiry")).ok_or(NotLoaded::NoExpiry)?;
    // Futures carry the placeholder strike -0.01.
    let strike = if is_option {
        number(line.get("strike"))
    } else {
        0.0
    };
    let underlying = match line.get("company_name") {
        "" => {
            let tail = if is_option {
                format!("{}{}", format_strike(strike), option_type)
            } else {
                "FUT".to_string()
            };
            underlying_from_symbol(line.get("trading_symbol"), expiry, &tail)
        }
        name => name.to_string(),
    };
    if underlying.is_empty() {
        return Err(NotLoaded::NoUnderlying);
    }
    let expiry = format_expiry(expiry);
    let (symbol, kind) = if is_option {
        let symbol = option_symbol(&underlying, &expiry, strike, &option_type);
        (symbol, option_type)
    } else {
        (future_symbol(&underlying, &expiry), "FUT".to_string())
    };
    let mut row = base_row(line, symbol, underlying, exchange, exchange);
    row.expiry = expiry;
    row.strike = strike;
    row.instrument_type = kind;
    Ok(row)
}

/// web `_derivatives`: futures and options of one F&O file. Futures:
/// `[underlying][DDMMMYY]FUT`; options: `[underlying][DDMMMYY][strike][CE|PE]`.
fn derivatives(file: &ContractFile, exchange: &str) -> Vec<SymToken> {
    let (mut unreadable, mut unnamed) = (0usize, 0usize);
    let mut out = Vec::with_capacity(file.rows.len());
    for line in file.lines() {
        match contract(line, exchange) {
            Ok(row) => out.push(row),
            Err(NotLoaded::NotAContract) => {}
            Err(NotLoaded::NoExpiry) => unreadable += 1,
            Err(NotLoaded::NoUnderlying) => unnamed += 1,
        }
    }
    if unreadable > 0 {
        tracing::warn!(
            "Skipping {} {} contracts with no readable expiry",
            unreadable,
            exchange
        );
    }
    if unnamed > 0 {
        tracing::warn!(
            "Skipping {} {} contracts with no underlying",
            unnamed,
            exchange
        );
    }
    out
}

/// `rows` of one file, refused when it yielded none (MC-02).
fn required(rows: Vec<SymToken>, exchange: &str) -> Result<Vec<SymToken>> {
    if rows.is_empty() {
        return Err(incomplete(exchange));
    }
    Ok(rows)
}

/// web `process_pocketful_contract`'s last step: a `(symbol, exchange)` pair
/// names one instrument, else `get_token` picks one of two at random, so the
/// first row is kept and the rest are logged and dropped; a row without a
/// symbol or token is not an instrument. A repeated `(exchange, token)`
/// also keeps its first row, as the resolver does with what it loads.
fn one_row_each(rows: Vec<SymToken>) -> Vec<SymToken> {
    let mut symbols = HashSet::new();
    let mut tokens = HashSet::new();
    let (mut repeated, mut repeated_token) = (0usize, 0usize);
    let mut sample = Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if r.symbol.is_empty() || r.token.is_empty() {
            continue;
        }
        let by_symbol = (r.exchange.clone(), r.symbol.clone());
        let by_token = (r.exchange.clone(), r.token.clone());
        if symbols.contains(&by_symbol) {
            repeated += 1;
            if sample.len() < 10 {
                sample.push(format!("{}:{}", r.exchange, r.symbol));
            }
        } else if tokens.contains(&by_token) {
            repeated_token += 1;
        } else {
            symbols.insert(by_symbol);
            tokens.insert(by_token);
            out.push(r);
        }
    }
    if repeated > 0 {
        tracing::warn!(
            "Dropped {} Pocketful rows whose symbol repeats on its exchange, for example {:?}",
            repeated,
            sample
        );
    }
    if repeated_token > 0 {
        tracing::warn!(
            "Dropped {} Pocketful rows whose token repeats on its exchange",
            repeated_token
        );
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &out {
        *counts.entry(r.exchange.as_str()).or_default() += 1;
    }
    tracing::info!(
        "Pocketful contract processed: {} instruments {:?}",
        out.len(),
        counts
    );
    out
}

/// Every symtoken row from the archive's five files, in the web's order
/// (NSE, BSE, indices, NFO, BFO, MCX). Each file is required and must yield
/// instruments, so the stored master is kept rather than replaced by a
/// partial one (MC-02).
pub fn parse_archive(files: &[(String, Vec<u8>)]) -> Result<Vec<SymToken>> {
    let read = |exchange: &str| -> Result<ContractFile> {
        let name = file_name(exchange);
        let text = files
            .iter()
            .find(|(n, _)| {
                n.rsplit('/')
                    .next()
                    .unwrap_or(n)
                    .eq_ignore_ascii_case(&name)
            })
            .map(|(_, b)| String::from_utf8_lossy(b))
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| incomplete(exchange))?;
        ContractFile::read(&text, exchange)
    };
    let [nse, bse, nfo, bfo, mcx] = EXCHANGES.map(read);
    let (nse, bse, nfo, bfo, mcx) = (nse?, bse?, nfo?, bfo?, mcx?);
    let mut all = required(nse_cash(&nse), "NSE")?;
    all.extend(required(bse_cash(&bse), "BSE")?);
    all.extend(indices(&nse, &bse));
    all.extend(required(derivatives(&nfo, "NFO"), "NFO")?);
    all.extend(required(derivatives(&bfo, "BFO"), "BFO")?);
    all.extend(required(derivatives(&mcx, "MCX"), "MCX")?);
    Ok(one_row_each(all))
}

/// Download the public contract and parse it. No session header: the file
/// is public (see the module doc). Every failure answers the web's
/// sentence; the cause is logged without the address.
pub async fn download(b: &PocketfulBroker) -> Result<Vec<SymToken>> {
    let unreachable = |e: reqwest::Error| failed(&redact::url_safe_error(&e.without_url()));
    let resp = b
        .http
        .get(&b.urls.master)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(unreachable)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(failed(&format!("HTTP {}", status.as_u16())));
    }
    let bytes = resp.bytes().await.map_err(unreachable)?;
    let files = zip::entries(&bytes).map_err(|_| failed("the archive could not be read"))?;
    drop(bytes);
    parse_archive(&files)
}
