//! Nuvama's contract file -> OpenAlgo symbol master (web
//! `database/master_contract_db.py`).
//!
//! `https://nc.nuvamawealth.com/app/toccontracts/instruments.zip` is public
//! (no session; the SDK downloads the same file): one CSV with
//! `exchangetoken, tradingsymbol, symbolname, description, expiry,
//! strikeprice, ticksize, lotsize, optiontype, series, assettype, exchange,
//! isin, ...`. As the web read it from the live file (185k rows, November
//! 2026):
//!
//! * Equity rows carry the ISIN in `tradingsymbol` and the exchange symbol
//!   in `symbolname` (M&M, BAJAJ-AUTO). Orders send the ISIN as `trdSym`,
//!   so it is the brsymbol; the OpenAlgo symbol is `symbolname`.
//! * Index rows have a bare negative `exchangetoken` (-29 for Nifty 50) and
//!   the display name in `tradingsymbol`.
//! * Futures have option type `XX` and strike -0.01 or 0; `expiry` is
//!   `DD/MMM/YY`; `lotsize` is in units in every segment.
//! * NSE rows can repeat, identical; they are dropped on `exchangetoken`.

use super::mapping::{oa_index_exchange, oa_index_symbol};
use super::NuvamaBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::families::noren::zip;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use std::collections::HashSet;

fn unreadable() -> AppError {
    AppError::Broker(
        "Nuvama's instrument file could not be read. Your existing symbols were kept; try the download again later."
            .into(),
    )
}

/// The columns the master is built from.
struct Columns {
    token: usize,
    trading_symbol: usize,
    symbol_name: usize,
    description: usize,
    expiry: usize,
    strike: usize,
    tick: usize,
    lot: usize,
    option_type: usize,
    asset_type: usize,
    exchange: usize,
}

impl Columns {
    fn of(header: &CsvHeader) -> Option<Self> {
        Some(Self {
            token: header.index("exchangetoken")?,
            trading_symbol: header.index("tradingsymbol")?,
            symbol_name: header.index("symbolname")?,
            description: header.index("description")?,
            expiry: header.index("expiry")?,
            strike: header.index("strikeprice")?,
            tick: header.index("ticksize")?,
            lot: header.index("lotsize")?,
            option_type: header.index("optiontype")?,
            asset_type: header.index("assettype")?,
            exchange: header.index("exchange")?,
        })
    }
}

fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `DD/MMM/YY` (`27/NOV/26`).
pub fn parse_expiry(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%d/%b/%y").ok()
}

/// NCDEX option rows carry the underlying's own expiry in `symbolname`
/// (`TMCFGRNZM18DEC2026`); the underlying is `TMCFGRNZM` (web: strip
/// `\d{2}[A-Z]{3}\d{4}$`).
pub fn underlying(symbol_name: &str) -> String {
    let s = symbol_name.trim();
    let b = s.as_bytes();
    let n = b.len();
    let dated = n >= 9
        && b[n - 9..n - 7].iter().all(u8::is_ascii_digit)
        && b[n - 7..n - 4].iter().all(u8::is_ascii_uppercase)
        && b[n - 4..].iter().all(u8::is_ascii_digit);
    if dated {
        s[..n - 9].to_string()
    } else {
        s.to_string()
    }
}

/// One contract-file row -> a master row; `None` for a row the web drops
/// (no symbol or token, a derivative without a readable expiry).
fn row(f: &[String], c: &Columns) -> Option<SymToken> {
    let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
    let exchange = get(c.exchange).to_ascii_uppercase();
    let asset = get(c.asset_type).to_ascii_uppercase();
    let token = get(c.token).to_string();
    let tick = number(get(c.tick)).unwrap_or(0.0);
    let lot = number(get(c.lot)).map_or(1, |v| v as i32);
    let base = SymToken {
        symbol: String::new(),
        brsymbol: get(c.trading_symbol).to_string(),
        name: String::new(),
        exchange: exchange.clone(),
        brexchange: exchange.clone(),
        token,
        expiry: String::new(),
        strike: 0.0,
        lot_size: lot,
        instrument_type: "EQ".into(),
        tick_size: tick,
    };
    let out = match asset.as_str() {
        "EQUITY" => SymToken {
            symbol: get(c.symbol_name).to_string(),
            name: get(c.description).to_string(),
            ..base
        },
        "INDEX" => SymToken {
            symbol: oa_index_symbol(get(c.trading_symbol), &exchange),
            name: get(c.symbol_name).to_string(),
            exchange: oa_index_exchange(&exchange).to_string(),
            tick_size: if tick > 0.0 { tick } else { 0.05 },
            ..base
        },
        _ => {
            let expiry = format_expiry(parse_expiry(get(c.expiry))?);
            let code = expiry.replace('-', "");
            let under = underlying(get(c.symbol_name));
            let opt = get(c.option_type).to_ascii_uppercase();
            let is_opt = matches!(opt.as_str(), "CE" | "PE");
            let strike = if is_opt {
                number(get(c.strike)).unwrap_or(0.0).max(0.0) + 0.0
            } else {
                0.0
            };
            let symbol = if is_opt {
                format!("{}{}{}{}", under, code, format_strike(strike), opt)
            } else {
                format!("{}{}FUT", under, code)
            };
            SymToken {
                symbol,
                name: under,
                expiry,
                strike,
                instrument_type: if is_opt { opt } else { "FUT".into() },
                ..base
            }
        }
    };
    (!out.symbol.is_empty() && !out.token.is_empty()).then_some(out)
}

/// The contract CSV -> master rows (web `process_nuvama_csv`).
pub fn parse_master(text: &str) -> Result<Vec<SymToken>> {
    let mut lines = text.lines();
    let header = CsvHeader::parse(lines.next().ok_or_else(unreadable)?);
    let c = Columns::of(&header).ok_or_else(unreadable)?;
    let mut tokens = HashSet::new();
    let mut keys = HashSet::new();
    let mut out = Vec::new();
    let mut undated = 0usize;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f = split_csv_line(line);
        let token = f.get(c.token).map(|s| s.as_str()).unwrap_or("");
        if !tokens.insert(token.to_string()) {
            continue;
        }
        match row(&f, &c) {
            Some(r) => {
                if keys.insert((r.symbol.clone(), r.exchange.clone())) {
                    out.push(r);
                }
            }
            None => undated += 1,
        }
    }
    if undated > 0 {
        tracing::warn!(
            "Skipped {} Nuvama rows without a symbol, token or expiry",
            undated
        );
    }
    Ok(out)
}

pub async fn download(b: &NuvamaBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.urls.contract)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(crate::brokers::common::redact::http)?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Nuvama instrument download failed"
        );
        return Err(AppError::Broker(
            "Nuvama did not send its instrument file. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(crate::brokers::common::redact::http)?;
    let csv = zip::first_entry(&bytes)?;
    let text = String::from_utf8_lossy(&csv);
    let rows = parse_master(&text)?;
    if rows.is_empty() {
        return Err(AppError::Broker(
            "Nuvama's instrument file was empty. Your existing symbols were kept; try the download again later."
                .into(),
        ));
    }
    Ok(rows)
}
