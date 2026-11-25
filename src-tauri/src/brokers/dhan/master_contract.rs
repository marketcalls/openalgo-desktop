//! Dhan scrip master CSV -> OpenAlgo symbol master
//! (web `database/master_contract_db.py`).
//!
//! * Source: `https://images.dhan.co/api-data/api-scrip-master.csv`, columns
//!   located by header name (whitespace-stripped, like `df.columns.str.strip`).
//! * Rows are gated on `SEM_SEGMENT` (E, I, D, C, M) and the instrument name:
//!   Dhan security ids are unique per segment, so the segment is the only
//!   safe key (NSE segment M is NCO, never NFO).
//! * Symbols per `docs/prompt/symbol-format.md`: NSE equities carry a
//!   non-EQ series suffix (`ELECTCAST-W1`), futures are
//!   `<root><DDMMMYY>FUT`, options `<root><DDMMMYY><strike><CE|PE>` with the
//!   strike from `SEM_STRIKE_PRICE`.
//! * Expiry `DD-MMM-YY` uppercase; empty when the row does not expire.
//! * Tick size is in paise for every row except INDEX (rupees already); the
//!   sandbox keeps it unscaled, as the web's sandbox does.
//! * Derivative `name` is the underlying root parsed back from the symbol.

use super::{DhanBroker, Variant};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, parse_broker_expiry, rename, split_csv_line, CsvHeader, BSE_INDEX_CODE_RENAMES,
    BSE_INDEX_RENAMES,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};

const EQUITY_FNO: &[&str] = &["FUTIDX", "FUTSTK", "OPTIDX", "OPTSTK", "OPTFUT"];
const CURRENCY: &[&str] = &["FUTCUR", "OPTCUR"];
const COMMODITY: &[&str] = &["FUTCOM", "FUTIDX", "OPTFUT", "OPTIDX"];

/// Dhan's compact NSE index names -> OpenAlgo (after upper/space removal).
const NSE_INDEX_FIXUPS: &[(&str, &str)] = &[
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYMCAP50", "NIFTYMIDCAP50"),
    ("NIFTYMIDSMALLCAP400", "NIFTYMIDSML400"),
    ("NIFTYSMALLCAP100", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYSMALLCAP50", "NIFTYSMLCAP50"),
    ("NIFTY100EQUALWEIGHT", "NIFTY100EQLWGT"),
    ("NIFTY100LOWVOLATILITY30", "NIFTY100LOWVOL30"),
    ("NIFTYMID100FREE", "NIFTYMIDCAP100"),
];

/// (exchange, brexchange, instrumenttype) for a row, `None` to drop it
/// (web `assign_values`).
pub fn assign_values(
    exch_id: &str,
    segment: &str,
    instrument: &str,
    option_type: &str,
) -> Option<(&'static str, &'static str, String)> {
    let deriv = if instrument.contains("OPT") {
        option_type.trim().to_string()
    } else {
        "FUT".to_string()
    };
    Some(match (segment, exch_id) {
        ("E", "NSE") if instrument == "EQUITY" => ("NSE", "NSE_EQ", "EQ".into()),
        ("E", "BSE") if instrument == "EQUITY" => ("BSE", "BSE_EQ", "EQ".into()),
        // Typed EQ, as Kite types its INDICES segment: the *_INDEX exchange
        // says the row is an index (web openalgo#2306, QA MC-04). History
        // still asks Dhan for instrument INDEX, derived from the exchange.
        ("I", "NSE") if instrument == "INDEX" => ("NSE_INDEX", "IDX_I", "EQ".into()),
        ("I", "BSE") if instrument == "INDEX" => ("BSE_INDEX", "IDX_I", "EQ".into()),
        ("D", "NSE") if EQUITY_FNO.contains(&instrument) => ("NFO", "NSE_FNO", deriv),
        ("D", "BSE") if EQUITY_FNO.contains(&instrument) => ("BFO", "BSE_FNO", deriv),
        ("C", "NSE") if CURRENCY.contains(&instrument) => ("CDS", "NSE_CURRENCY", deriv),
        ("C", "BSE") if CURRENCY.contains(&instrument) => ("BCD", "BSE_CURRENCY", deriv),
        ("M", "MCX") if COMMODITY.contains(&instrument) => ("MCX", "MCX_COMM", deriv),
        ("M", "NSE") if COMMODITY.contains(&instrument) => ("NCO", "NSE_COMM", deriv),
        _ => return None,
    })
}

/// Strike text: exact value, no trailing zeros (web `format_strike`).
pub fn format_strike(strike: f64) -> String {
    let s = format!("{:.6}", strike);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// NSE symbol qualified by a non-EQ series (web `qualify_equity_symbol`).
pub fn qualify_equity_symbol(trading_symbol: &str, series: &str) -> String {
    let s = series.trim();
    if s.is_empty() || s == "EQ" || s.eq_ignore_ascii_case("nan") {
        trading_symbol.to_string()
    } else {
        format!("{}-{}", trading_symbol, s)
    }
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn is_strike_tail(s: &str) -> bool {
    // `(?:\d+(?:\.\d+)?)?(?:FUT|CE|PE)?$`
    let body = s
        .strip_suffix("FUT")
        .or_else(|| s.strip_suffix("CE"))
        .or_else(|| s.strip_suffix("PE"))
        .unwrap_or(s);
    if body.is_empty() {
        return true;
    }
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    !int.is_empty()
        && int.bytes().all(|c| c.is_ascii_digit())
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()))
}

/// The underlying of an OpenAlgo F&O symbol: the shortest prefix followed by
/// a `DDMMMYY` date and an optional strike and FUT/CE/PE (web
/// `extract_underlying_from_symbol`).
pub fn extract_underlying(symbol: &str) -> Option<String> {
    if !symbol.is_ascii() {
        return None;
    }
    let s = symbol.to_ascii_uppercase();
    let b = s.as_bytes();
    for i in 1..b.len() {
        if i + 7 > b.len() {
            break;
        }
        let d = &s[i..i + 7];
        let db = d.as_bytes();
        if db[0].is_ascii_digit()
            && db[1].is_ascii_digit()
            && MONTHS.contains(&&d[2..5])
            && db[5].is_ascii_digit()
            && db[6].is_ascii_digit()
            && is_strike_tail(&s[i + 7..])
        {
            return Some(s[..i].to_string());
        }
    }
    None
}

struct Cols {
    exch: usize,
    segment: usize,
    security_id: usize,
    instrument: usize,
    trading_symbol: usize,
    lot: usize,
    custom_symbol: usize,
    expiry: usize,
    strike: usize,
    option_type: usize,
    tick: usize,
    series: Option<usize>,
    name: Option<usize>,
}

impl Cols {
    fn from_header(h: &CsvHeader) -> Result<Self> {
        let col = |n: &str| {
            h.index(n).ok_or_else(|| {
                tracing::error!("Dhan scrip master has no '{}' column", n);
                AppError::Broker(
                    "Dhan's instrument list has an unexpected format. Try downloading the master contract again later."
                        .into(),
                )
            })
        };
        Ok(Self {
            exch: col("SEM_EXM_EXCH_ID")?,
            segment: col("SEM_SEGMENT")?,
            security_id: col("SEM_SMST_SECURITY_ID")?,
            instrument: col("SEM_INSTRUMENT_NAME")?,
            trading_symbol: col("SEM_TRADING_SYMBOL")?,
            lot: col("SEM_LOT_UNITS")?,
            custom_symbol: col("SEM_CUSTOM_SYMBOL")?,
            expiry: col("SEM_EXPIRY_DATE")?,
            strike: col("SEM_STRIKE_PRICE")?,
            option_type: col("SEM_OPTION_TYPE")?,
            tick: col("SEM_TICK_SIZE")?,
            series: h.index("SEM_SERIES"),
            name: h.index("SM_SYMBOL_NAME"),
        })
    }
}

/// Every NSE index, upper case without spaces or hyphens, then renamed
/// where Dhan's name differs (web openalgo#2306, QA MC-08: an allow-list
/// once kept Dhan's raw name for any index missing from it, so
/// "Nifty100 Alpha 30" never became NIFTY100ALPHA30).
fn nse_index_symbol(raw: &str) -> String {
    let compact = raw.to_ascii_uppercase().replace([' ', '-'], "");
    rename(NSE_INDEX_FIXUPS, &compact)
        .map(str::to_string)
        .unwrap_or(compact)
}

/// BSE indices Dhan lists under the shared names, or under BSE's own index
/// codes (DSI, LVI, QUI; `SEM_CUSTOM_SYMBOL` "BSE Momentum Index"), which
/// [`BSE_INDEX_CODE_RENAMES`] maps (web openalgo#2306, QA MC-08).
fn bse_index_symbol(raw: &str) -> String {
    rename(BSE_INDEX_RENAMES, raw)
        .or_else(|| rename(BSE_INDEX_CODE_RENAMES, raw))
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string())
}

/// Parse the whole scrip master. `variant` decides the tick scaling.
pub fn parse_scrip_master(csv: &str, variant: Variant) -> Result<Vec<SymToken>> {
    let mut lines = csv.lines();
    let header = lines
        .next()
        .ok_or_else(|| AppError::Broker("Dhan returned an empty instrument list.".into()))?;
    let c = Cols::from_header(&CsvHeader::parse(header))?;
    let mut out = Vec::with_capacity(csv.len() / 120);
    let mut dropped = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        match parse_row(&c, &get, variant) {
            Some(r) => out.push(r),
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::info!(
            "Dhan master contract: {} rows with no OpenAlgo exchange dropped",
            dropped
        );
    }
    Ok(out)
}

fn parse_row<'a>(c: &Cols, get: &dyn Fn(usize) -> &'a str, variant: Variant) -> Option<SymToken> {
    let instrument = get(c.instrument);
    let (exchange, brexchange, itype) =
        assign_values(get(c.exch), get(c.segment), instrument, get(c.option_type))?;
    let token = get(c.security_id).to_string();
    if token.is_empty() {
        return None;
    }
    let brsymbol = get(c.trading_symbol).to_string();
    let expiry = parse_broker_expiry(get(c.expiry))
        .map(format_expiry)
        .unwrap_or_default();
    let strike: f64 = get(c.strike).parse().unwrap_or(0.0);
    let compact = expiry.replace('-', "");
    let custom = get(c.custom_symbol);
    let series = c.series.map(get).unwrap_or("");
    let mut symbol = if instrument == "EQUITY" {
        if exchange == "NSE" {
            qualify_equity_symbol(&brsymbol, series)
        } else {
            brsymbol.clone()
        }
    } else if instrument == "INDEX" {
        brsymbol.clone()
    } else if itype == "FUT" {
        let parts: Vec<&str> = custom.split(' ').collect();
        if parts.len() == 3 || parts.len() == 4 {
            format!("{}{}FUT", parts[0], compact)
        } else {
            custom.to_string()
        }
    } else if itype == "CE" || itype == "PE" {
        let parts: Vec<&str> = custom.split(' ').collect();
        if parts.len() == 4 || parts.len() == 5 {
            format!("{}{}{}{}", parts[0], compact, format_strike(strike), itype)
        } else {
            custom.to_string()
        }
    } else {
        custom.to_string()
    };
    match exchange {
        "NSE_INDEX" => symbol = nse_index_symbol(&symbol),
        "BSE_INDEX" => symbol = bse_index_symbol(&symbol),
        _ => {}
    }
    let raw_name = c.name.map(get).unwrap_or("").to_string();
    let name = if matches!(itype.as_str(), "CE" | "PE" | "FUT") {
        extract_underlying(&symbol)
            .filter(|n| !n.is_empty())
            .unwrap_or(raw_name)
    } else {
        raw_name
    };
    let tick_raw: f64 = get(c.tick).parse().unwrap_or(0.0);
    let tick_size = if variant == Variant::Live && instrument != "INDEX" {
        tick_raw / 100.0
    } else {
        tick_raw
    };
    let lot_size = get(c.lot).parse::<f64>().map(|v| v as i32).unwrap_or(1);
    Some(SymToken {
        symbol,
        brsymbol,
        name,
        exchange: exchange.to_string(),
        brexchange: brexchange.to_string(),
        token,
        expiry,
        strike,
        lot_size,
        instrument_type: itype,
        tick_size,
    })
}

pub async fn download(b: &DhanBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.master_url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Dhan scrip master download refused"
        );
        return Err(AppError::Broker(
            "Dhan did not send the instrument list. Try downloading the master contract again."
                .into(),
        ));
    }
    let text = resp.text().await?;
    let rows = parse_scrip_master(&text, b.variant)?;
    drop(text);
    tracing::info!("Dhan master contract parsed: {} instruments", rows.len());
    Ok(rows)
}
