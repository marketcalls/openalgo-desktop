//! Master contract (web `database/master_contract_db.py`).
//!
//! * Scrip masters: `GET {base}/getscripmastercsv?name=<NSE|BSE|NSEFO|NSECD|MCX|BSEFO>`
//!   (public CSV). Columns used: `scripcode` (token), `scripname`
//!   (brsymbol, e.g. `INFY EQ`, `TGBL 30-OCT-2025 CE 1180`),
//!   `scripshortname` (underlying), `marketlot`, `instrumentname`,
//!   `strikeprice`, `ticksize`, `exchangename` (brexchange), `optiontype`.
//! * Expiry comes from the `DD-MMM-YYYY` token inside `scripname`, not from
//!   `expirydate` (seconds since 1980, off by a day on MCX).
//! * Index masters: `GET {base}/getindexdatacsv?name=<NSE|BSE>` with
//!   `indexcode`, `indexname`, `exchangename`.
//! * Final dedupe on (symbol, exchange), first row wins; within a cash file
//!   the `EQ` series wins over temporary series of the same scrip.

use super::MotilalBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, split_csv_line, CsvHeader,
};
use crate::brokers::common::redact;
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use std::collections::HashSet;

/// Scrip-master files, in the web's order.
pub const SCRIP_EXCHANGES: &[&str] = &["NSE", "BSE", "NSEFO", "NSECD", "MCX", "BSEFO"];
/// Index-master files.
pub const INDEX_EXCHANGES: &[&str] = &["NSE", "BSE"];

/// web `_NSE_INDEX_ALIASES` (keys upper-cased, whitespace removed).
pub const NSE_INDEX_ALIASES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYFINSERV", "FINNIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYMIDCAPSELECT", "MIDCPNIFTY"),
    ("INDIAVIX", "INDIAVIX"),
];

/// web `_BSE_INDEX_ALIASES_RAW` (keys upper-cased, whitespace collapsed).
pub const BSE_INDEX_ALIASES: &[(&str, &str)] = &[
    ("BSE SENSEX", "SENSEX"),
    ("BSE BANKEX", "BANKEX"),
    ("BSE SENSEX 50", "SENSEX50"),
    ("BSE 100", "BSE100"),
    ("BSE 150 MIDCAP", "BSE150MIDCAPINDEX"),
    ("BSE 200", "BSE200"),
    ("BSE 250 LARGEMIDCAP", "BSE250LARGEMIDCAPINDEX"),
    ("BSE 400 MIDSMALLCAP", "BSE400MIDSMALLCAPINDEX"),
    ("BSE 500", "BSE500"),
    ("BSE AUTO", "BSEAUTO"),
    ("BSE CAPGOOD", "BSECAPITALGOODS"),
    ("BSE CARBON", "BSECARBONEX"),
    ("BSE CONSDUR", "BSECONSUMERDURABLES"),
    ("BSE CPSE", "BSECPSE"),
    ("BSE DOL100", "BSEDOLLEX100"),
    ("BSE DOL200", "BSEDOLLEX200"),
    ("BSE DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("BSE FMCG", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("BSE GREENX", "BSEGREENEX"),
    ("BSE HEALTHC", "BSEHEALTHCARE"),
    ("BSE INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSE IPO", "BSEIPO"),
    ("LRGCAP", "BSELARGECAP"),
    ("BSE METAL", "BSEMETAL"),
    ("BSE MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("BSE OIL&GAS", "BSEOIL&GAS"),
    ("BSE POWER", "BSEPOWER"),
    ("BSE PSU", "BSEPSU"),
    ("BSE REALTY", "BSEREALTY"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("BSE SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("BSE SMEIPO", "BSESMEIPO"),
    ("BSE TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

/// web `_BFO_UNDERLYING_ALIASES` (BIT deliberately unmapped).
pub const BFO_UNDERLYING_ALIASES: &[(&str, &str)] =
    &[("BSX", "SENSEX"), ("BKX", "BANKEX"), ("SX50", "SENSEX50")];

fn lookup<'a>(table: &'a [(&'a str, &'a str)], k: &str) -> Option<&'a str> {
    table.iter().find(|(a, _)| *a == k).map(|(_, b)| *b)
}

/// pandas `read_csv` default NA strings.
fn is_na(s: &str) -> bool {
    matches!(
        s,
        "" | "#N/A"
            | "#N/A N/A"
            | "#NA"
            | "-1.#IND"
            | "-1.#QNAN"
            | "-NaN"
            | "-nan"
            | "1.#IND"
            | "1.#QNAN"
            | "<NA>"
            | "N/A"
            | "NA"
            | "NULL"
            | "NaN"
            | "None"
            | "n/a"
            | "nan"
            | "null"
    )
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

fn no_ws_upper(s: &str) -> String {
    s.split_whitespace()
        .collect::<String>()
        .to_ascii_uppercase()
}

/// web `_normalize_nse_index_symbol`.
pub fn normalize_nse_index(s: &str) -> String {
    let cleaned = no_ws_upper(s);
    lookup(NSE_INDEX_ALIASES, &cleaned)
        .map(str::to_string)
        .unwrap_or(cleaned)
}

/// web `_normalize_bse_index_symbol`.
pub fn normalize_bse_index(s: &str) -> String {
    lookup(BSE_INDEX_ALIASES, &collapse_ws(s))
        .map(str::to_string)
        .unwrap_or_else(|| no_ws_upper(s))
}

/// web `extract_expiry_from_scripname`: the `DD-MMM-YYYY` token as
/// `DD-MMM-YY`, empty when absent or unparsable.
pub fn expiry_from_scripname(scripname: &str) -> String {
    for part in scripname.split_whitespace() {
        let b = part.as_bytes();
        // re.match(r"\d{1,2}-[A-Za-z]{3}-\d{4}") at the start of the part.
        let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=2).contains(&digits) {
            continue;
        }
        let rest = &b[digits..];
        let shape = rest.len() >= 9
            && rest[0] == b'-'
            && rest[1..4].iter().all(u8::is_ascii_alphabetic)
            && rest[4] == b'-'
            && rest[5..9].iter().all(u8::is_ascii_digit);
        if !shape {
            continue;
        }
        return match NaiveDate::parse_from_str(&part.to_ascii_uppercase(), "%d-%b-%Y") {
            Ok(d) => format_expiry(d),
            Err(_) => String::new(),
        };
    }
    String::new()
}

fn exchange_map(brexchange: &str) -> String {
    match brexchange {
        "NSEFO" => "NFO",
        "NSECD" | "NSECO" => "CDS",
        "BSEFO" => "BFO",
        "BSECD" | "BSECO" => "BCD",
        other => other,
    }
    .to_string()
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn token_str(s: &str) -> String {
    let t = s.trim();
    match t.strip_suffix(".0") {
        Some(i) if i.chars().all(|c| c.is_ascii_digit() || c == '-') => i.to_string(),
        _ => t.to_string(),
    }
}

struct Cols {
    token: usize,
    scripname: usize,
    short: Option<usize>,
    lot: Option<usize>,
    itype: Option<usize>,
    strike: Option<usize>,
    tick: Option<usize>,
    brexchange: Option<usize>,
    option: Option<usize>,
}

/// web `process_motilal_csv` for one scrip-master file.
pub fn parse_scrip_csv(text: &str, file_exchange: &str) -> Vec<SymToken> {
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let (Some(token), Some(scripname)) = (h.index("scripcode"), h.index("scripname")) else {
        tracing::warn!(
            "Motilal Oswal {} master has no scripcode/scripname columns",
            file_exchange
        );
        return Vec::new();
    };
    let c = Cols {
        token,
        scripname,
        short: h.index("scripshortname"),
        lot: h.index("marketlot"),
        itype: h.index("instrumentname"),
        strike: h.index("strikeprice"),
        tick: h.index("ticksize"),
        brexchange: h.index("exchangename"),
        option: h.index("optiontype"),
    };
    // (row, cash series rank) in file order.
    let mut rows: Vec<(SymToken, Option<bool>)> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: Option<usize>| -> &str {
            i.and_then(|i| f.get(i)).map(String::as_str).unwrap_or("")
        };
        let get_na = |i: Option<usize>| -> Option<&str> {
            let v = get(i);
            (!is_na(v)).then_some(v)
        };
        let raw_token = get(Some(c.token));
        if is_na(raw_token) {
            continue;
        }
        let brsymbol = get_na(Some(c.scripname)).unwrap_or("").to_string();
        let mut name = get_na(c.short).unwrap_or("").to_string();
        let brexchange = get_na(c.brexchange).unwrap_or(file_exchange).to_string();
        let mut exchange = exchange_map(&brexchange);
        let expiry = expiry_from_scripname(&brsymbol);
        let mut strike = get_na(c.strike).and_then(num).unwrap_or(0.0);
        let lot_size = get_na(c.lot).and_then(num).map(|v| v as i32).unwrap_or(1);
        let tick_size = get_na(c.tick)
            .and_then(num)
            .filter(|t| *t > 0.0)
            .unwrap_or(0.05);
        let option = match c.option {
            Some(_) => get_na(c.option).unwrap_or("XX").to_string(),
            None => "XX".to_string(),
        };
        let mut itype = get_na(c.itype).unwrap_or("").trim().to_ascii_uppercase();
        if itype.contains("FUT") && option == "XX" {
            itype = "FUT".into();
        }
        if option == "CE" || option == "PE" {
            itype = option.clone();
        }
        if itype.contains("IDX") {
            match exchange.as_str() {
                "NSE" => exchange = "NSE_INDEX".into(),
                "BSE" => exchange = "BSE_INDEX".into(),
                "MCX" => exchange = "MCX_INDEX".into(),
                _ => {}
            }
        }
        if (exchange == "NSE" || exchange == "BSE") && itype.is_empty() {
            itype = "EQ".into();
        }
        if itype != "CE" && itype != "PE" {
            strike = 0.0;
        }
        if exchange == "BFO" {
            if let Some(a) = lookup(BFO_UNDERLYING_ALIASES, &name) {
                name = a.to_string();
            }
        }
        let deriv = matches!(itype.as_str(), "FUT" | "CE" | "PE");
        let compact = expiry.replace('-', "");
        let mut symbol = brsymbol.clone();
        let mut cash_rank = None;
        if deriv && !expiry.is_empty() {
            symbol = if itype == "FUT" {
                format!("{}{}FUT", name, compact)
            } else {
                format!("{}{}{}{}", name, compact, format_strike(strike), itype)
            };
        } else if deriv {
            tracing::debug!(
                "Motilal Oswal derivative {} has no expiry in its name",
                brsymbol
            );
        }
        if (exchange == "NSE" || exchange == "BSE") && !deriv {
            let series = option.trim();
            let bare = name.trim();
            let stripped = if !series.is_empty()
                && series != "XX"
                && brsymbol.ends_with(&format!(" {}", series))
            {
                brsymbol[..brsymbol.len() - series.len() - 1]
                    .trim()
                    .to_string()
            } else {
                brsymbol.trim().to_string()
            };
            symbol = if bare.is_empty() {
                stripped
            } else {
                bare.to_string()
            };
            cash_rank = Some(series != "EQ");
        }
        if exchange == "NSE_INDEX" {
            symbol = normalize_nse_index(&symbol);
        } else if exchange == "BSE_INDEX" {
            symbol = normalize_bse_index(&symbol);
        }
        rows.push((
            SymToken {
                symbol,
                brsymbol,
                name,
                exchange,
                brexchange,
                token: token_str(raw_token),
                expiry,
                strike,
                lot_size,
                instrument_type: itype,
                tick_size,
            },
            cash_rank,
        ));
    }
    // Cash rows: the EQ series wins its (symbol, exchange) (stable).
    let mut cash: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].1.is_some()).collect();
    cash.sort_by_key(|i| rows[*i].1.unwrap_or(true));
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut drop = vec![false; rows.len()];
    for i in cash {
        let k = (rows[i].0.symbol.clone(), rows[i].0.exchange.clone());
        if !seen.insert(k) {
            drop[i] = true;
        }
    }
    rows.into_iter()
        .zip(drop)
        .filter(|(_, d)| !d)
        .map(|((r, _), _)| r)
        .collect()
}

/// web `process_motilal_index_csv`.
pub fn parse_index_csv(text: &str, exchange_name: &str) -> Vec<SymToken> {
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let (Some(code), Some(name)) = (h.index("indexcode"), h.index("indexname")) else {
        tracing::warn!(
            "Motilal Oswal {} index master has no indexcode/indexname columns",
            exchange_name
        );
        return Vec::new();
    };
    let brex = h.index("exchangename");
    let exchange = format!("{}_INDEX", exchange_name);
    lines
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let f = split_csv_line(l);
            let token = f.get(code).map(|s| token_str(s)).filter(|s| !is_na(s))?;
            let raw = f
                .get(name)
                .cloned()
                .filter(|s| !is_na(s))
                .unwrap_or_default();
            let symbol = match exchange_name {
                "NSE" => normalize_nse_index(&raw),
                "BSE" => normalize_bse_index(&raw),
                _ => raw.clone(),
            };
            let brexchange = brex
                .and_then(|i| f.get(i))
                .filter(|s| !is_na(s))
                .cloned()
                .unwrap_or_else(|| exchange_name.to_string());
            Some(SymToken {
                symbol,
                brsymbol: raw.clone(),
                name: raw,
                exchange: exchange.clone(),
                brexchange,
                token,
                expiry: String::new(),
                strike: 0.0,
                lot_size: 1,
                instrument_type: "INDEX".into(),
                tick_size: 0.05,
            })
        })
        .collect()
}

/// Final `(symbol, exchange)` dedupe, first wins.
pub fn dedupe(rows: Vec<SymToken>) -> Vec<SymToken> {
    let mut seen: HashSet<(String, String)> = HashSet::with_capacity(rows.len());
    rows.into_iter()
        .filter(|r| seen.insert((r.symbol.clone(), r.exchange.clone())))
        .collect()
}

async fn fetch(b: &MotilalBroker, path: &str, name: &str) -> Result<String> {
    let resp = b
        .http
        .get(format!("{}{}", b.base_url, path))
        .query(&[("name", name)])
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(redact::http)?;
    if !resp.status().is_success() {
        return Err(AppError::Broker(format!(
            "Motilal Oswal {} instrument file is unavailable",
            name
        )));
    }
    resp.text().await.map_err(redact::http)
}

/// web `master_contract_download`, all or nothing (MC-02, a hardening over
/// the web, which keeps whatever files it got): every scrip and index file
/// must download and yield instruments, or the download fails naming the
/// file, so the stored master is kept rather than replaced by a partial one.
pub async fn download(b: &MotilalBroker) -> Result<Vec<SymToken>> {
    let mut all = Vec::new();
    for ex in SCRIP_EXCHANGES {
        let rows = match fetch(b, super::paths::SCRIP_MASTER, ex).await {
            Ok(text) => parse_scrip_csv(&text, ex),
            Err(e) => {
                tracing::warn!("Motilal Oswal {} master download failed: {}", ex, e);
                return Err(incomplete(ex));
            }
        };
        if rows.is_empty() {
            tracing::warn!("Motilal Oswal {} master had no instruments", ex);
            return Err(incomplete(ex));
        }
        all.extend(rows);
    }
    for ex in INDEX_EXCHANGES {
        let rows = match fetch(b, super::paths::INDEX_MASTER, ex).await {
            Ok(text) => parse_index_csv(&text, ex),
            Err(e) => {
                tracing::warn!("Motilal Oswal {} index master download failed: {}", ex, e);
                return Err(incomplete(&format!("{} index", ex)));
            }
        };
        if rows.is_empty() {
            tracing::warn!("Motilal Oswal {} index master had no instruments", ex);
            return Err(incomplete(&format!("{} index", ex)));
        }
        all.extend(rows);
    }
    Ok(dedupe(all))
}

fn incomplete(file: &str) -> AppError {
    AppError::Broker(format!(
        "The Motilal Oswal {} instrument list could not be downloaded. Your existing symbols were kept; try the download again later.",
        file
    ))
}
