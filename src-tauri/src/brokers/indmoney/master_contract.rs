//! Master contract (web `database/master_contract_db.py`).
//!
//! Three authenticated CSV downloads, `GET /market/instruments?source=`
//! `equity`, `fno`, `index`. Columns are read by header name: `EXCH`,
//! `SEGMENT`, `SECURITY_ID`, `INSTRUMENT_NAME`, `TRADING_SYMBOL`,
//! `LOT_UNITS`, `EXPIRY_DATE`, `STRIKE_PRICE`, `OPTION_TYPE`, `TICK_SIZE`,
//! `SYMBOL_NAME`.
//!
//! * Venue: index file -> `NSE_INDEX`/`BSE_INDEX` (`INDEX`); `SEGMENT` `E`
//!   -> NSE/BSE `EQ`; `D` or `FNO` -> NFO/BFO with `FUT`/`CE`/`PE`.
//! * Symbol: equity `TRADING_SYMBOL`; futures `{base}{DDMMMYY}FUT`; options
//!   `{base}{DDMMMYY}{strike}{CE|PE}` where base is `TRADING_SYMBOL` before
//!   the first `-`; index the `SEGMENT` value (the index name in that file),
//!   then the web renames (`NIFTY 50` -> `NIFTY`, ...).
//! * `brsymbol` is `TRADING_SYMBOL` (index file: `SEGMENT`), `token` is
//!   `SECURITY_ID`, expiry `DD-MMM-YY`.

use super::{token, Bucket, IndmoneyBroker};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, parse_broker_expiry, split_csv_line, CsvHeader,
};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::NaiveDate;

pub const SOURCES: &[&str] = &["equity", "fno", "index"];

/// web special-case symbol renames.
pub const RENAMES: &[(&str, &str)] = &[
    ("NIFTY 50", "NIFTY"),
    ("Nifty Next 50", "NIFTYNXT50"),
    ("Nifty Financial", "FINNIFTY"),
    ("BANK NIFTY", "BANKNIFTY"),
    ("Nifty Midcap Sel", "MIDCPNIFTY"),
    ("India VIX", "INDIAVIX"),
    ("S&P BSE SENSEX 50", "SENSEX50"),
];

fn parse_expiry(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() || s == "-1" {
        return None;
    }
    parse_broker_expiry(s).or_else(|| {
        let head = s.split([' ', 'T']).next().unwrap_or(s);
        ["%d/%m/%Y", "%Y/%m/%d", "%d-%m-%Y", "%Y%m%d"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(head, f).ok())
    })
}

/// `(exchange, brexchange, instrumenttype)` of a row (web `assign_values`),
/// `None` for rows the web files as `Unknown`.
pub fn assign(
    exch: &str,
    segment: &str,
    instrument: &str,
    option_type: &str,
    index_file: bool,
) -> Option<(&'static str, &'static str, String)> {
    let opt = if instrument.starts_with("FUT") {
        "FUT"
    } else {
        option_type
    };
    let deriv = || {
        if opt.is_empty() {
            "FUT".to_string()
        } else {
            opt.to_string()
        }
    };
    let is_index = instrument == "INDEX" || index_file;
    match (exch, segment) {
        ("NSE", _) if is_index => Some(("NSE_INDEX", "NSE", "INDEX".into())),
        ("BSE", _) if is_index => Some(("BSE_INDEX", "BSE", "INDEX".into())),
        ("NSE", "E") => Some(("NSE", "NSE", "EQ".into())),
        ("BSE", "E") => Some(("BSE", "BSE", "EQ".into())),
        ("NSE", "D") | ("NSE", "FNO") => Some(("NFO", "NSE", deriv())),
        ("BSE", "D") | ("BSE", "FNO") => Some(("BFO", "BSE", deriv())),
        _ => None,
    }
}

fn base_of(trading_symbol: &str) -> &str {
    trading_symbol.split('-').next().unwrap_or(trading_symbol)
}

/// web `reformat_symbol`.
#[allow(clippy::too_many_arguments)]
pub fn reformat_symbol(
    instrument: &str,
    option_type: &str,
    symbol_name: &str,
    trading_symbol: &str,
    segment: &str,
    expiry: &str,
    strike: f64,
    index_file: bool,
) -> String {
    let exp = expiry.replace('-', "").to_ascii_uppercase();
    if instrument == "EQUITY" {
        trading_symbol.to_string()
    } else if instrument == "INDEX" || index_file {
        if index_file && !segment.is_empty() {
            segment.to_string()
        } else if !symbol_name.is_empty() {
            symbol_name.to_string()
        } else {
            trading_symbol.to_string()
        }
    } else if instrument == "FUTSTK"
        || instrument == "FUTIDX"
        || (instrument.starts_with("FUT") && option_type.is_empty())
    {
        format!("{}{}FUT", base_of(trading_symbol), exp)
    } else if instrument == "OPTSTK" || instrument == "OPTIDX" || matches!(option_type, "CE" | "PE")
    {
        let k = if strike > 0.0 {
            format_strike(strike)
        } else {
            String::new()
        };
        format!("{}{}{}{}", base_of(trading_symbol), exp, k, option_type)
    } else {
        trading_symbol.to_string()
    }
}

/// Parse one instruments CSV (`source` is `equity`, `fno` or `index`).
pub fn parse_csv(source: &str, text: &str) -> Vec<SymbolData> {
    let index_file = source == "index";
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let col = |name: &str| h.index(name);
    let (c_exch, c_seg, c_id, c_inst, c_ts, c_lot, c_exp, c_strike, c_opt, c_tick, c_name) = (
        col("EXCH"),
        col("SEGMENT"),
        col("SECURITY_ID"),
        col("INSTRUMENT_NAME"),
        col("TRADING_SYMBOL"),
        col("LOT_UNITS"),
        col("EXPIRY_DATE"),
        col("STRIKE_PRICE"),
        col("OPTION_TYPE"),
        col("TICK_SIZE"),
        col("SYMBOL_NAME"),
    );
    let mut out = Vec::new();
    for line in lines {
        let f = split_csv_line(line);
        let get = |c: Option<usize>| {
            c.and_then(|i| f.get(i))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let token = {
            let t = get(c_id);
            // pandas reads an integer column; a float-looking id keeps its
            // integer text.
            t.strip_suffix(".0").map(str::to_string).unwrap_or(t)
        };
        if token.is_empty() {
            continue;
        }
        let exch = get(c_exch);
        let segment = get(c_seg);
        let instrument = get(c_inst);
        let option_type = get(c_opt);
        let Some((exchange, brexchange, itype)) =
            assign(&exch, &segment, &instrument, &option_type, index_file)
        else {
            continue;
        };
        let expiry = parse_expiry(&get(c_exp))
            .map(format_expiry)
            .unwrap_or_default();
        let strike: f64 = get(c_strike).parse().unwrap_or(0.0);
        let strike = if strike.is_finite() { strike } else { 0.0 };
        let trading_symbol = get(c_ts);
        let raw_symbol = reformat_symbol(
            &instrument,
            &option_type,
            &get(c_name),
            &trading_symbol,
            &segment,
            &expiry,
            strike,
            index_file,
        );
        let symbol = RENAMES
            .iter()
            .find(|(from, _)| *from == raw_symbol)
            .map(|(_, to)| to.to_string())
            .unwrap_or(raw_symbol);
        let name = if matches!(itype.as_str(), "CE" | "PE" | "FUT") {
            let b = base_of(&trading_symbol).trim();
            if b.is_empty() {
                symbol.clone()
            } else {
                b.to_string()
            }
        } else {
            symbol.clone()
        };
        let lot: f64 = get(c_lot).parse().unwrap_or(1.0);
        let tick: f64 = get(c_tick).parse().unwrap_or(0.05);
        out.push(SymbolData {
            brsymbol: if index_file {
                segment.clone()
            } else {
                trading_symbol
            },
            symbol,
            name,
            exchange: exchange.into(),
            brexchange: brexchange.into(),
            token,
            expiry,
            strike,
            lot_size: if lot.is_finite() { lot as i32 } else { 1 },
            instrument_type: itype,
            tick_size: if tick.is_finite() { tick } else { 0.05 },
        });
    }
    out
}

pub async fn download(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<SymbolData>> {
    let tok = token(auth)?;
    let mut rows = Vec::new();
    let mut fetched = 0;
    for src in SOURCES {
        b.pace(Bucket::Data).await;
        let resp = b
            .http
            .get(b.url("/market/instruments"))
            .query(&[("source", *src)])
            .header("Authorization", tok)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "INDmoney {} instruments download failed: {}",
                    src,
                    if e.is_timeout() {
                        "timed out"
                    } else {
                        "network error"
                    }
                );
                continue;
            }
        };
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(super::session_expired());
        }
        if status != 200 {
            tracing::error!(status, "INDmoney {} instruments download refused", src);
            continue;
        }
        let text = resp.text().await?;
        fetched += 1;
        let parsed = parse_csv(src, &text);
        tracing::info!("INDmoney {} instruments: {} rows", src, parsed.len());
        rows.extend(parsed);
    }
    if fetched == 0 || rows.is_empty() {
        return Err(AppError::Broker(
            "No data downloaded from INDmoney. Check your connection and log in again if the problem continues."
                .into(),
        ));
    }
    Ok(rows)
}
