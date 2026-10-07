//! Paytm Money security master (web `database/master_contract_db.py`).
//!
//! One public CSV, `/data/v1/scrips/security_master.csv`, read by header
//! name. `instrument_type` decides the OpenAlgo exchange and type:
//!
//! | instrument_type | NSE row | BSE row | instrumenttype |
//! | --- | --- | --- | --- |
//! | `ES`, `ETF` | NSE | BSE | EQ |
//! | `I` | NSE_INDEX | BSE_INDEX | INDEX |
//! | `FUTIDX`, `FUTSTK` | NFO | BFO | FUT |
//! | `OPTIDX`, `OPTSTK` | NFO | BFO | CE / PE (from CALL / PUT in `name`) |
//!
//! brexchange is the parent exchange (`NSE`/`BSE`), token the
//! `security_id`, brsymbol the `symbol` column (indices: the name with
//! spaces removed, uppercased). Derivative symbols are
//! `base + DDMMMYY + FUT` and `base + DDMMMYY + strike + CE|PE` with the base
//! the first word of `name`. Rows of any other type are dropped (the web
//! stores them under exchange `Unknown`, which nothing can address).

use super::PaytmBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, future_symbol, option_symbol, parse_broker_expiry, rename, split_csv_line,
    CsvHeader, BSE_INDEX_RENAMES, NSE_INDEX_RENAMES,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::NaiveDate;

/// web `nse_index_map` (applied to NSE_INDEX rows only).
pub const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYMCAP50", "NIFTYMIDCAP50"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
];

/// web `bse_index_map` (applied to BSE_INDEX rows only: equities share
/// short names such as `AUTO`, `METAL`).
pub const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("AUTO", "BSEAUTO"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("ENERGY", "BSEENERGY"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

/// Expiry as Paytm writes it (`2026-10-27`, `2026-10-27 14:30:00`,
/// `27-10-2026`, `27-Oct-2026`, ...). Numeric day-month dates are read day
/// first, as Indian exchanges write them.
pub fn parse_expiry(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() || s == "-1" || s == "0" {
        return None;
    }
    parse_broker_expiry(s).or_else(|| {
        let head = s.split([' ', 'T']).next().unwrap_or(s);
        ["%d-%m-%Y", "%d/%m/%Y", "%Y/%m/%d", "%d-%m-%y"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(head, f).ok())
    })
}

/// `(exchange, brexchange, instrumenttype)` for a row (web `assign_values`).
fn classify(exchange: &str, instrument: &str, name: &str) -> Option<(&'static str, String)> {
    let nse = match exchange.trim() {
        "NSE" => true,
        "BSE" => false,
        _ => return None,
    };
    let pick = |a: &'static str, b: &'static str| if nse { a } else { b };
    Some(match instrument.trim() {
        "ES" | "ETF" => (pick("NSE", "BSE"), "EQ".to_string()),
        "I" => (pick("NSE_INDEX", "BSE_INDEX"), "INDEX".to_string()),
        "FUTIDX" | "FUTSTK" => (pick("NFO", "BFO"), "FUT".to_string()),
        "OPTIDX" | "OPTSTK" => (pick("NFO", "BFO"), option_kind(name).to_string()),
        _ => return None,
    })
}

/// `CE`/`PE` from the row name (`NIFTY 12 MAY 17850 CALL`), web
/// `_paytm_option_ce_pe`.
pub fn option_kind(name: &str) -> &'static str {
    let upper = name.to_ascii_uppercase();
    if upper.contains("CALL") {
        "CE"
    } else if upper.contains("PUT") {
        "PE"
    } else {
        match upper.split_whitespace().last() {
            Some("CE") => "CE",
            Some("PE") => "PE",
            _ => "OPT",
        }
    }
}

fn joined_upper(name: &str) -> String {
    name.split_whitespace()
        .collect::<String>()
        .to_ascii_uppercase()
}

/// OpenAlgo index symbol: the web's Paytm map on the joined name, then the
/// shared OpenAlgo index renames on the spaced name (`NIFTY 50` -> `NIFTY`,
/// `NIFTY BANK` -> `BANKNIFTY`), else the joined name.
pub fn index_symbol(exchange: &str, name: &str) -> String {
    let joined = joined_upper(name);
    let spaced = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    let (own, shared) = if exchange == "NSE_INDEX" {
        (NSE_INDEX_MAP, NSE_INDEX_RENAMES)
    } else {
        (BSE_INDEX_MAP, BSE_INDEX_RENAMES)
    };
    rename(own, &joined)
        .or_else(|| rename(shared, &spaced))
        .unwrap_or(&joined)
        .to_string()
}

fn num(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

/// Parse the security master CSV.
pub fn parse_security_master(text: &str) -> Result<Vec<SymToken>> {
    let mut lines = text.lines();
    let header = CsvHeader::parse(lines.next().unwrap_or(""));
    let col = |n: &str| header.index(n);
    let (
        Some(i_id),
        Some(i_sym),
        Some(i_name),
        Some(i_ex),
        Some(i_type),
        Some(i_exp),
        Some(i_strike),
        Some(i_lot),
        Some(i_tick),
    ) = (
        col("security_id"),
        col("symbol"),
        col("name"),
        col("exchange"),
        col("instrument_type"),
        col("expiry_date"),
        col("strike_price"),
        col("lot_size"),
        col("tick_size"),
    )
    else {
        tracing::warn!("Paytm Money security master has an unexpected format");
        return Err(AppError::Broker(
            "The Paytm Money instrument list came in an unexpected format. Try downloading the master contract again later."
                .into(),
        ));
    };
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        let name = get(i_name);
        let instrument = get(i_type);
        let Some((exchange, itype)) = classify(get(i_ex), instrument, name) else {
            continue;
        };
        let raw_symbol = get(i_sym).to_string();
        let token = get(i_id).to_string();
        if token.is_empty() {
            continue;
        }
        let expiry = parse_expiry(get(i_exp))
            .map(format_expiry)
            .unwrap_or_default();
        let strike = num(get(i_strike));
        let base = name.split(' ').next().unwrap_or("").trim().to_string();
        let (symbol, brsymbol, row_name) = match itype.as_str() {
            "INDEX" => (
                index_symbol(exchange, name),
                joined_upper(name),
                name.to_string(),
            ),
            "FUT" => (future_symbol(&base, &expiry), raw_symbol, base.clone()),
            "EQ" => (raw_symbol.clone(), raw_symbol, name.to_string()),
            _ => {
                // Options: CE/PE from CALL/PUT, else the name's last word.
                let suffix = match itype.as_str() {
                    "CE" | "PE" => itype.clone(),
                    _ => name.split(' ').next_back().unwrap_or("").to_string(),
                };
                (
                    option_symbol(&base, &expiry, strike, &suffix),
                    raw_symbol,
                    base.clone(),
                )
            }
        };
        if symbol.trim().is_empty() {
            continue;
        }
        out.push(SymToken {
            symbol,
            brsymbol,
            name: row_name,
            exchange: exchange.to_string(),
            brexchange: if exchange.starts_with('N') {
                "NSE"
            } else {
                "BSE"
            }
            .to_string(),
            token,
            expiry,
            strike,
            lot_size: num(get(i_lot)) as i32,
            instrument_type: itype,
            tick_size: num(get(i_tick)),
        });
    }
    if out.is_empty() {
        return Err(AppError::Broker(
            "The Paytm Money instrument list was empty. Try downloading the master contract again later."
                .into(),
        ));
    }
    Ok(out)
}

pub async fn download(b: &PaytmBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.urls.master)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Paytm Money security master download failed"
        );
        return Err(AppError::Broker(
            "Paytm Money's instrument list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let text = resp.text().await.map_err(|e| super::redact(e.into()))?;
    parse_security_master(&text)
}
