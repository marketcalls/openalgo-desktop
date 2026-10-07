//! Security master (web `database/master_contract_db.py`).
//!
//! `GET https://hdfcsky.com/api/v1/contract/Compact?info=download` (public)
//! is a ZIP holding `CompactScrip.csv` with the columns `exchange_token,
//! trading_symbol, company_name, close_price, expiry (DD-Mon-YYYY), strike,
//! tick_size, lot_size, instrument_name, option_type, segment, exchange, ...`.
//!
//! * `exchange` is already the OpenAlgo code; indices are NSE rows with
//!   segment `INDICES` and BSE rows with segment `IDX`.
//! * NSE cash strips only `-EQ`; BSE cash strips `-<group>` (the segment).
//! * Derivatives are `<UNDERLYING><YY><MON|M DD|O/N/D DD><strike?><TYPE>`;
//!   the underlying is recovered by stripping that suffix (monthly, weekly
//!   with a month digit, weekly with the O/N/D letters), else company name.
//! * Futures carry the strike placeholder -0.01 (clipped to 0); the expiry
//!   sentinel `01-Jan-0001` means none.
//! * MCX non-derivative rows are dropped; duplicates on (symbol, exchange)
//!   keep the first.

use super::{HdfcSkyBroker, USER_AGENT};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    expiry_compact, format_expiry, format_strike, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate};
use std::collections::HashSet;

/// NSE index display names, keyed uppercase without spaces.
const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("INDIAVIX", "INDIAVIX"),
];

/// BSE index short codes, keyed uppercase without spaces.
const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("SENSEX", "SENSEX"),
    ("BANKEX", "BANKEX"),
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("BSE100", "BSE100"),
    ("BSE200", "BSE200"),
    ("BSE500", "BSE500"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIPO", "BSEIPO"),
    ("METAL", "BSEMETAL"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("BSEPSU", "BSEPSU"),
    ("REALTY", "BSEREALTY"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
    ("UTILS", "BSEUTILITIES"),
    ("ESG100", "ESG100"),
    ("BHRT22", "BHRT22"),
    ("FOCIT", "FOCIT"),
];

/// Index display name -> OpenAlgo symbol; unmapped names fall back to the
/// uppercase, space-free form (the rest of the web NSE table is exactly
/// that identity).
pub fn classify_index(display: &str, exchange: &str) -> String {
    let key: String = display.to_uppercase().split_whitespace().collect();
    let table = if exchange == "NSE_INDEX" {
        NSE_INDEX_MAP
    } else {
        BSE_INDEX_MAP
    };
    table
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or(key)
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn bad_format() -> AppError {
    AppError::Broker(
        "The HDFC Sky instrument list has an unexpected format. Try downloading the master contract again later."
            .into(),
    )
}

/// Parse `CompactScrip.csv`.
pub fn parse_csv(text: &str) -> Result<Vec<SymToken>> {
    let mut lines = text.lines();
    let header = CsvHeader::parse(lines.next().ok_or_else(bad_format)?);
    let col = |n: &str| header.index(n).ok_or_else(bad_format);
    let c_token = col("exchange_token")?;
    let c_ts = col("trading_symbol")?;
    let c_company = col("company_name")?;
    let c_expiry = col("expiry")?;
    let c_strike = col("strike")?;
    let c_tick = col("tick_size")?;
    let c_lot = col("lot_size")?;
    let c_opt = col("option_type")?;
    let c_seg = col("segment")?;
    let c_ex = col("exchange")?;

    let mut out = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        if let Some(row) = parse_row(
            get(c_token),
            get(c_ts),
            get(c_company),
            get(c_expiry),
            get(c_strike),
            get(c_tick),
            get(c_lot),
            get(c_opt),
            get(c_seg),
            get(c_ex),
        ) {
            if seen.insert((row.symbol.clone(), row.exchange.clone())) {
                out.push(row);
            }
        }
    }
    Ok(out)
}

fn parse_expiry(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%d-%b-%Y")
        .ok()
        .filter(|d| d.year() > 1900)
}

#[allow(clippy::too_many_arguments)]
fn parse_row(
    token: &str,
    ts: &str,
    company: &str,
    expiry_raw: &str,
    strike_raw: &str,
    tick_raw: &str,
    lot_raw: &str,
    opt_raw: &str,
    seg: &str,
    ex_raw: &str,
) -> Option<SymToken> {
    let brex = ex_raw.to_ascii_uppercase();
    let opt = opt_raw.to_ascii_uppercase();
    let exchange = match (brex.as_str(), seg) {
        ("NSE", "INDICES") => "NSE_INDEX".to_string(),
        ("BSE", "IDX") => "BSE_INDEX".to_string(),
        _ => brex.clone(),
    };
    let is_index = exchange.ends_with("_INDEX");
    let expiry_date = parse_expiry(expiry_raw);
    let expiry = expiry_date.map(format_expiry).unwrap_or_default();
    let compact = expiry_compact(&expiry);
    let strike = strike_raw.parse::<f64>().unwrap_or(0.0).max(0.0) + 0.0;
    let tick = tick_raw.parse::<f64>().unwrap_or(0.0);
    let lot = lot_raw.parse::<f64>().unwrap_or(0.0) as i32;
    let strike_str = format_strike(strike);

    let is_option = opt == "CE" || opt == "PE";
    let is_future = !is_option && !expiry.is_empty() && ts.ends_with("FUT");
    let is_deriv = is_option || is_future;

    // Underlying: strip the broker's expiry/strike suffix.
    let mut underlying = String::new();
    if is_deriv {
        if let Some(d) = expiry_date {
            let yy = &expiry[7..9];
            let mon = MONTHS[d.month0() as usize];
            let day = &expiry[0..2];
            let tail = if is_future {
                "FUT".to_string()
            } else {
                format!("{}{}", strike_str, opt)
            };
            let digit = if d.month() <= 9 {
                d.month().to_string()
            } else {
                String::new()
            };
            let letter = match d.month() {
                10 => "O",
                11 => "N",
                12 => "D",
                _ => "",
            };
            let mut suffixes = vec![format!("{}{}{}", yy, mon, tail)];
            if !digit.is_empty() {
                suffixes.push(format!("{}{}{}{}", yy, digit, day, tail));
            }
            if !letter.is_empty() {
                suffixes.push(format!("{}{}{}{}", yy, letter, day, tail));
            }
            for suf in suffixes {
                if let Some(base) = ts.strip_suffix(suf.as_str()) {
                    underlying = base.to_string();
                    break;
                }
            }
        }
    }
    if underlying.is_empty() {
        underlying = company.to_string();
    }

    let mut symbol = ts.to_string();
    if brex == "NSE" {
        if let Some(b) = ts.strip_suffix("-EQ") {
            symbol = b.to_string();
        }
    }
    if brex == "BSE" && seg != "IDX" && !seg.is_empty() {
        if let Some(b) = ts.strip_suffix(&format!("-{}", seg)) {
            symbol = b.to_string();
        }
    }
    if is_future {
        symbol = format!("{}{}FUT", underlying, compact);
    } else if is_option {
        symbol = format!("{}{}{}{}", underlying, compact, strike_str, opt);
    }
    if is_index {
        symbol = classify_index(ts, &exchange);
    }

    let instrument_type = if is_future {
        "FUT".to_string()
    } else if is_option {
        opt.clone()
    } else {
        "EQ".to_string()
    };
    let mut name = if company.is_empty() {
        ts.to_string()
    } else {
        company.to_string()
    };
    if is_deriv && !underlying.is_empty() {
        name = underlying;
    }

    if brex == "MCX" && !is_deriv {
        return None;
    }
    if symbol.is_empty() || token.is_empty() {
        return None;
    }
    Some(SymToken {
        symbol,
        brsymbol: ts.to_string(),
        name,
        exchange,
        brexchange: brex,
        token: token.to_string(),
        expiry,
        strike,
        lot_size: lot,
        instrument_type,
        tick_size: tick,
    })
}

/// Download the ZIP, read its CSV member, parse it.
pub async fn download(b: &HdfcSkyBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.urls.master)
        .header("User-Agent", USER_AGENT)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| AppError::from(e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Sky security master download failed"
        );
        return Err(AppError::Broker(
            "HDFC Sky's instrument list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    let csv = crate::brokers::families::noren::zip::first_entry(&bytes)?;
    let text = String::from_utf8_lossy(&csv);
    let rows = parse_csv(&text)?;
    tracing::info!("Processed {} HDFC Sky instruments", rows.len());
    Ok(rows)
}
