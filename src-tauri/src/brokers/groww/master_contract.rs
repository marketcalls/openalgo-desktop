//! Groww instrument master (web `database/master_contract_db.py`).
//!
//! `https://growwapi-assets.groww.in/instruments/instrument.csv`, parsed by
//! header name. Rules, all from the web:
//! * `exchange` -> `brexchange`; `exchange_token` -> `token` (string,
//!   leading zeros kept); `trading_symbol` -> `brsymbol` and initial
//!   `symbol`;
//! * exchange: `NSE`/`BSE` + segment `FNO` -> `NFO`/`BFO`; segment or
//!   instrument type `IDX` -> `NSE_INDEX`/`BSE_INDEX`; others keep the CSV
//!   exchange;
//! * instrument type: EQ, IDX->EQ (an index is an EQ row on
//!   NSE_INDEX/BSE_INDEX, as Zerodha), FUT, CE, PE, ETF->EQ, CURR->CUR,
//!   COM->COM; missing: CASH->EQ, FNO strike>0 -> OPT, else FUT;
//! * expiry `yyyy-mm-dd` -> `DD-MMM-YY`; lot size NaN -> 1; strike NaN -> 0;
//!   tick size NaN -> 0.05;
//! * index renames on `symbol` only; NSE and BSE F&O and NSE commodity
//!   symbols rebuilt as `[underlying][DDMMMYY]FUT` /
//!   `[underlying][DDMMMYY][strike]CE|PE` (Groww's own are not OpenAlgo
//!   format: `SENSEX26O2274900CE`, `GOLD26NOVFUT`); NSE commodities stay on
//!   NSE; NFO options whose broker symbol has spaces get the spaces
//!   removed; rows with no symbol dropped;
//! * `name` is Groww's name, and the underlying for FUT/CE/PE rows;
//! * CASH rows sharing a trading symbol on one exchange (NSE bonds listed
//!   under one symbol for several series, IMC1 N1/N2/N3) take Groww's
//!   `internal_trading_symbol`, else `SYMBOL-SERIES` when a series is given,
//!   so `(symbol, exchange)` is unique (web #2194, MC-03).

use super::GrowwCore;
use crate::brokers::common::http;
use crate::brokers::common::master_contract::{
    expiry_compact, format_expiry, format_strike, rename, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use std::collections::HashMap;

pub const MASTER_URL: &str = "https://growwapi-assets.groww.in/instruments/instrument.csv";

/// Groww index names -> OpenAlgo symbols (applied to `symbol` only).
pub const INDEX_RENAMES: &[(&str, &str)] = &[
    ("NIFTYJR", "NIFTYNXT50"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYMIDCAP", "NIFTYMIDCAP100"),
    ("NIFTYSMALL", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYCDTY", "NIFTYCOMMODITIES"),
    ("MIDCAP50", "NIFTYMIDCAP50"),
    ("BSESMLCAP", "BSESMALLCAP"),
];

const REQUIRED: &[&str] = &[
    "exchange",
    "exchange_token",
    "trading_symbol",
    "groww_symbol",
    "name",
    "instrument_type",
    "segment",
    "underlying_symbol",
    "expiry_date",
    "strike_price",
    "lot_size",
    "tick_size",
];

fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("nan") {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn instrument_type(raw: &str, segment: &str, strike: Option<f64>) -> &'static str {
    match raw {
        "EQ" | "ETF" => "EQ",
        "IDX" => "EQ",
        "FUT" => "FUT",
        "CE" => "CE",
        "PE" => "PE",
        "CURR" => "CUR",
        "COM" => "COM",
        _ => match segment {
            "CASH" => "EQ",
            "FNO" if strike.unwrap_or(0.0) > 0.0 => "OPT",
            "FNO" => "FUT",
            _ => "EQ",
        },
    }
}

/// Parse the CSV into master rows.
pub fn parse_instruments(csv: &str) -> Result<Vec<SymToken>> {
    let mut lines = csv.lines();
    let header = CsvHeader::parse(lines.next().unwrap_or(""));
    let mut idx = Vec::with_capacity(REQUIRED.len());
    for name in REQUIRED {
        match header.index(name) {
            Some(i) => idx.push(i),
            None => {
                tracing::error!("Groww instrument file has no '{}' column", name);
                return Err(AppError::Broker(
                    "Groww's instrument list arrived in an unexpected format. Try downloading the master contract again later."
                        .into(),
                ));
            }
        }
    }
    fn field<'a>(f: &'a [String], idx: &[usize], n: usize) -> &'a str {
        f.get(idx[n]).map(|s| s.trim()).unwrap_or("")
    }
    let i_series = header.index("series");
    let i_internal = header.index("internal_trading_symbol");
    let opt = |f: &[String], i: Option<usize>| -> String {
        i.and_then(|i| f.get(i))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    // Per CASH row: (index in `out`, series, internal trading symbol).
    let mut cash: Vec<(usize, String, String)> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let col = |n: usize| field(&f, &idx, n);
        let exchange_csv = col(0);
        let token = col(1);
        let trading_symbol = col(2);
        let name = col(4);
        let raw_type = col(5);
        let segment = col(6);
        let underlying = col(7);
        let expiry_raw = col(8);
        let strike = parse_num(col(9));
        let lot = parse_num(col(10)).map(|v| v as i32).unwrap_or(1);
        let tick = parse_num(col(11)).unwrap_or(0.05);

        let is_index = segment == "IDX" || raw_type == "IDX";
        let itype = instrument_type(raw_type, segment, strike);
        let exchange = match (exchange_csv, segment, is_index) {
            ("NSE", _, true) => "NSE_INDEX",
            ("BSE", _, true) => "BSE_INDEX",
            ("NSE", "FNO", _) => "NFO",
            ("BSE", "FNO", _) => "BFO",
            (e, _, _) => e,
        };
        let expiry = chrono::NaiveDate::parse_from_str(
            expiry_raw.split([' ', 'T']).next().unwrap_or(""),
            "%Y-%m-%d",
        )
        .map(format_expiry)
        .unwrap_or_default();
        let strike = strike.unwrap_or(0.0);

        let mut symbol = rename(INDEX_RENAMES, trading_symbol)
            .unwrap_or(trading_symbol)
            .to_string();
        let rebuild = matches!(
            (exchange_csv, segment),
            ("NSE", "FNO") | ("BSE", "FNO") | ("NSE", "COMMODITY")
        );
        if rebuild && !expiry.is_empty() {
            let base = if underlying.is_empty() {
                symbol.as_str()
            } else {
                underlying
            };
            let compact = expiry_compact(&expiry);
            match (itype, raw_type) {
                ("FUT", _) => symbol = format!("{}{}FUT", base, compact),
                (_, "CE") | (_, "PE") => {
                    symbol = format!("{}{}{}{}", base, compact, format_strike(strike), raw_type)
                }
                _ => {}
            }
        }
        if exchange == "NFO" && matches!(itype, "CE" | "PE") && trading_symbol.contains(' ') {
            symbol = trading_symbol.replace(' ', "");
        }
        let brsymbol = if trading_symbol.is_empty() {
            symbol.clone()
        } else {
            trading_symbol.to_string()
        };
        if symbol.trim().is_empty() {
            continue;
        }
        let name = if matches!(itype, "FUT" | "CE" | "PE") && !underlying.is_empty() {
            underlying.to_string()
        } else if !name.is_empty() {
            name.to_string()
        } else {
            symbol.clone()
        };
        if segment == "CASH" {
            cash.push((out.len(), opt(&f[..], i_series), opt(&f[..], i_internal)));
        }
        out.push(SymToken {
            symbol,
            brsymbol,
            name,
            exchange: exchange.to_string(),
            brexchange: exchange_csv.to_string(),
            token: token.to_string(),
            expiry,
            strike,
            lot_size: lot,
            instrument_type: itype.to_string(),
            tick_size: tick,
        });
    }
    disambiguate_series(&mut out, &cash);
    if out.is_empty() {
        return Err(AppError::Broker(
            "Groww's instrument list was empty. Try downloading the master contract again later."
                .into(),
        ));
    }
    Ok(out)
}

/// CASH rows sharing `(symbol, exchange)` take Groww's
/// `internal_trading_symbol` (`IMC1-N2`), else `SYMBOL-SERIES` where a
/// series is given; a row without either keeps its symbol.
fn disambiguate_series(rows: &mut [SymToken], cash: &[(usize, String, String)]) {
    let mut count: HashMap<(String, String), usize> = HashMap::new();
    for (i, _, _) in cash {
        let r = &rows[*i];
        *count
            .entry((r.symbol.clone(), r.exchange.clone()))
            .or_default() += 1;
    }
    let mut renamed = Vec::new();
    for (i, series, internal) in cash {
        let r = &mut rows[*i];
        if count
            .get(&(r.symbol.clone(), r.exchange.clone()))
            .copied()
            .unwrap_or(0)
            < 2
        {
            continue;
        }
        let new = if !internal.is_empty() {
            internal.clone()
        } else if !series.is_empty() {
            format!("{}-{}", r.symbol, series)
        } else {
            continue;
        };
        renamed.push((*i, new));
    }
    if !renamed.is_empty() {
        tracing::info!(
            "Disambiguated {} Groww CASH rows sharing a trading symbol",
            renamed.len()
        );
    }
    for (i, new) in renamed {
        rows[i].symbol = new;
    }
}

pub(crate) async fn download(core: &GrowwCore) -> Result<Vec<SymToken>> {
    download_from(core, MASTER_URL).await
}

pub(crate) async fn download_from(core: &GrowwCore, url: &str) -> Result<Vec<SymToken>> {
    let resp = core
        .http
        .get(url)
        .timeout(http::DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Groww instrument download refused"
        );
        return Err(AppError::Broker(
            "Groww's instrument list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let text = resp.text().await?;
    // The text is dropped when parsing returns; only the rows are kept.
    parse_instruments(&text)
}
