//! Master contract (web `database/master_contract_db.py`).
//!
//! One public ZIP (`/api/v1/contract/Compact?info=download&exchanges=...`)
//! holding `{NSE,BSE,NFO,BFO,MCX}CompactScrip.csv`. Columns (lowercased):
//! `trading_symbol, company_name, exchange, exchange_token, lot_size,
//! tick_size, instrument_name, option_type, strike, expiry, segment`.
//!
//! * NSE: `instrument_name == EQ`, symbol = trading_symbol without `-EQ`.
//! * NSE indices: `segment == INDICES` -> `NSE_INDEX`, renamed by the web's
//!   six-entry map (`Nifty 50 -> NIFTY`, ...), brexchange stays `NSE`.
//! * BSE: every row, symbol = trading_symbol without `-<series>`;
//!   `segment == IDX` -> `BSE_INDEX`; `SNSX50 -> SENSEX50`.
//! * NFO / BFO: `option_type XX` (BFO `SF`/`IF` instruments set to XX) ->
//!   `company_name + DDMMMYY + FUT`; CE/PE -> `+ strike + CE|PE`.
//! * MCX: `COM` rows dropped; `FUTCOM`/`FUTIDX` -> XX; the base is the
//!   leading letters of trading_symbol.
//! * token = exchange_token, brsymbol = trading_symbol, expiry `DD-MMM-YY`.
//!
//! Deviations from the web, both fixes: rows are de-duplicated per
//! (exchange, token) instead of on token across all exchanges (which drops
//! a BSE scrip whose code equals an NSE token), and BFO strikes use the
//! shared strike text (the web's `str(strike).replace(".", "")` turns a
//! float strike `81500.0` into `815000`).

use super::{zip, PocketfulBroker};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, future_symbol, option_symbol, parse_broker_expiry, split_csv_line,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use std::collections::{HashMap, HashSet};

/// The CSV files of the archive, in the web's load order.
pub const FILES: &[&str] = &[
    "NSECompactScrip.csv",
    "BSECompactScrip.csv",
    "NFOCompactScrip.csv",
    "MCXCompactScrip.csv",
    "BFOCompactScrip.csv",
];

/// web `process_pocketful_indices_csv` map.
pub const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("Nifty 50", "NIFTY"),
    ("Nifty Bank", "BANKNIFTY"),
    ("India VIX", "INDIAVIX"),
    ("Nifty Fin Service", "FINNIFTY"),
    ("NIFTY MID SELECT", "MIDCPNIFTY"),
    ("Nifty Next 50", "NIFTYNXT50"),
];

/// A CSV with lowercased, trimmed header names.
struct Table<'a> {
    cols: HashMap<String, usize>,
    lines: std::iter::Skip<std::str::Lines<'a>>,
}

impl<'a> Table<'a> {
    fn parse(text: &'a str) -> Self {
        let header = text.lines().next().unwrap_or("");
        let cols = split_csv_line(header)
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                (
                    n.trim().trim_start_matches('\u{feff}').to_ascii_lowercase(),
                    i,
                )
            })
            .collect();
        Self {
            cols,
            lines: text.lines().skip(1),
        }
    }

    fn has(&self, name: &str) -> bool {
        self.cols.contains_key(name)
    }
}

struct Row<'t> {
    fields: Vec<String>,
    cols: &'t HashMap<String, usize>,
}

impl Row<'_> {
    fn get(&self, name: &str) -> &str {
        self.cols
            .get(name)
            .and_then(|&i| self.fields.get(i))
            .map(|s| s.trim())
            .unwrap_or("")
    }

    fn f64(&self, name: &str) -> f64 {
        self.get(name).parse::<f64>().unwrap_or(0.0)
    }

    fn lot(&self) -> i32 {
        self.f64("lot_size") as i32
    }
}

fn rows<'t>(t: &'t mut Table<'_>) -> Vec<Row<'t>> {
    let cols = &t.cols;
    t.lines
        .by_ref()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Row {
            fields: split_csv_line(l),
            cols,
        })
        .collect()
}

fn missing(file: &str) -> AppError {
    tracing::warn!("Pocketful master file {} has an unexpected format", file);
    AppError::Broker(
        "Pocketful's instrument list came back in an unexpected format. Try downloading the master contract again later."
            .into(),
    )
}

fn row_exchange(r: &Row<'_>, default: &str) -> String {
    let e = r.get("exchange");
    if e.is_empty() {
        default.to_string()
    } else {
        e.to_string()
    }
}

fn base_row(r: &Row<'_>, symbol: String, exchange: String, brexchange: String) -> SymToken {
    SymToken {
        symbol,
        brsymbol: r.get("trading_symbol").to_string(),
        name: r.get("company_name").to_string(),
        exchange,
        brexchange,
        token: r.get("exchange_token").to_string(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: r.lot(),
        instrument_type: String::new(),
        tick_size: r.f64("tick_size"),
    }
}

/// web `process_pocketful_nse_csv`: equities only.
pub fn parse_nse(text: &str) -> Result<Vec<SymToken>> {
    let mut t = Table::parse(text);
    for c in [
        "instrument_name",
        "trading_symbol",
        "company_name",
        "exchange",
        "exchange_token",
        "lot_size",
        "tick_size",
    ] {
        if !t.has(c) {
            return Err(missing("NSECompactScrip.csv"));
        }
    }
    Ok(rows(&mut t)
        .into_iter()
        .filter(|r| r.get("instrument_name") == "EQ")
        .map(|r| {
            let ex = row_exchange(&r, "NSE");
            let mut row = base_row(
                &r,
                r.get("trading_symbol").replace("-EQ", ""),
                ex.clone(),
                ex,
            );
            row.instrument_type = "EQ".into();
            row
        })
        .collect())
}

/// web `process_pocketful_indices_csv`: NSE `segment == INDICES` rows.
pub fn parse_nse_indices(text: &str) -> Result<Vec<SymToken>> {
    let mut t = Table::parse(text);
    if !t.has("segment") || !t.has("trading_symbol") {
        return Err(missing("NSECompactScrip.csv"));
    }
    Ok(rows(&mut t)
        .into_iter()
        .filter(|r| r.get("segment") == "INDICES")
        .map(|r| {
            let br = r.get("trading_symbol").to_string();
            let symbol = NSE_INDEX_MAP
                .iter()
                .find(|(from, _)| *from == br)
                .map(|(_, to)| to.to_string())
                .unwrap_or_else(|| br.clone());
            let mut row = base_row(&r, symbol, "NSE_INDEX".into(), row_exchange(&r, "NSE"));
            row.name = br;
            // web maps FUT/CE/PE and keeps anything else as sent.
            row.instrument_type = r.get("instrument_name").to_string();
            row
        })
        .collect())
}

/// web `process_pocketful_bse_csv`: every row; `segment == IDX` is an index.
pub fn parse_bse(text: &str) -> Result<Vec<SymToken>> {
    let mut t = Table::parse(text);
    if !t.has("trading_symbol") || !t.has("exchange_token") {
        return Err(missing("BSECompactScrip.csv"));
    }
    Ok(rows(&mut t)
        .into_iter()
        .map(|r| {
            let br = r.get("trading_symbol");
            // `str.replace(r"-.*$", "")`: drop from the first hyphen.
            let mut symbol = br.split('-').next().unwrap_or(br).to_string();
            if symbol == "SNSX50" {
                symbol = "SENSEX50".into();
            }
            let brex = row_exchange(&r, "BSE");
            let exchange = if r.get("segment") == "IDX" {
                "BSE_INDEX".to_string()
            } else {
                brex.clone()
            };
            let mut row = base_row(&r, symbol, exchange, brex);
            row.instrument_type = r.get("instrument_name").to_string();
            row
        })
        .collect())
}

/// `pd.to_datetime(expiry, errors="coerce")` for the encodings the
/// Pocketful files use.
pub fn parse_expiry(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    parse_broker_expiry(s).or_else(|| {
        ["%d %b %Y", "%d-%m-%Y", "%Y%m%d"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(s, f).ok())
    })
}

/// Derivative segment of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Nfo,
    Bfo,
    Mcx,
}

impl Segment {
    fn exchange(self) -> &'static str {
        match self {
            Segment::Nfo => "NFO",
            Segment::Bfo => "BFO",
            Segment::Mcx => "MCX",
        }
    }
}

/// Leading letters of a trading symbol, uppercased (web MCX
/// `extract_base_symbol`).
pub fn leading_letters(s: &str) -> String {
    let up = s.to_ascii_uppercase();
    let base: String = up.chars().take_while(|c| c.is_ascii_uppercase()).collect();
    if base.is_empty() {
        up
    } else {
        base
    }
}

/// web `process_pocketful_{nfo,bfo,mcx}_csv`.
pub fn parse_derivatives(text: &str, seg: Segment) -> Result<Vec<SymToken>> {
    let mut t = Table::parse(text);
    for c in ["trading_symbol", "exchange_token", "option_type", "expiry"] {
        if !t.has(c) {
            return Err(missing(seg.exchange()));
        }
    }
    let mut out = Vec::new();
    for r in rows(&mut t) {
        let instrument = r.get("instrument_name");
        if seg == Segment::Mcx && instrument == "COM" {
            continue;
        }
        let mut option_type = r.get("option_type").to_string();
        let forced_future = match seg {
            Segment::Bfo => matches!(instrument, "SF" | "IF"),
            Segment::Mcx => matches!(instrument, "FUTCOM" | "FUTIDX"),
            Segment::Nfo => false,
        };
        if forced_future {
            option_type = "XX".into();
        }
        let br = r.get("trading_symbol").to_string();
        let base = match seg {
            Segment::Mcx => leading_letters(&br),
            _ => r.get("company_name").to_string(),
        };
        let expiry = parse_expiry(r.get("expiry")).map(format_expiry);
        let exp_text = expiry.clone().unwrap_or_default();
        let strike = r.f64("strike");
        let (symbol, itype) = match option_type.as_str() {
            "XX" => (future_symbol(&base, &exp_text), "FUT"),
            "CE" | "PE" => (
                option_symbol(&base, &exp_text, strike, &option_type),
                if option_type == "CE" { "CE" } else { "PE" },
            ),
            _ => (br.clone(), ""),
        };
        if symbol.is_empty() {
            continue;
        }
        let ex = row_exchange(&r, seg.exchange());
        let mut row = base_row(&r, symbol, ex.clone(), ex);
        if seg == Segment::Mcx {
            row.name = base;
        }
        row.expiry = expiry.unwrap_or_default();
        row.strike = strike;
        row.instrument_type = itype.to_string();
        out.push(row);
    }
    Ok(out)
}

/// Parse the archive's files (web load order), de-duplicated per
/// (exchange, token), first row wins.
pub fn parse_archive(files: &[(String, Vec<u8>)]) -> Result<Vec<SymToken>> {
    let find = |name: &str| -> Option<String> {
        files
            .iter()
            .find(|(n, _)| n.rsplit('/').next().unwrap_or(n).eq_ignore_ascii_case(name))
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
    };
    let nse = find("NSECompactScrip.csv").ok_or_else(|| missing("NSECompactScrip.csv"))?;
    let mut all = parse_nse(&nse)?;
    if let Some(bse) = find("BSECompactScrip.csv") {
        all.extend(parse_bse(&bse)?);
    }
    if let Some(nfo) = find("NFOCompactScrip.csv") {
        all.extend(parse_derivatives(&nfo, Segment::Nfo)?);
    }
    if let Some(mcx) = find("MCXCompactScrip.csv") {
        all.extend(parse_derivatives(&mcx, Segment::Mcx)?);
    }
    if let Some(bfo) = find("BFOCompactScrip.csv") {
        all.extend(parse_derivatives(&bfo, Segment::Bfo)?);
    }
    all.extend(parse_nse_indices(&nse)?);
    let mut seen = HashSet::new();
    all.retain(|r| !r.token.is_empty() && seen.insert((r.exchange.clone(), r.token.clone())));
    Ok(all)
}

pub async fn download(b: &PocketfulBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.urls.master)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "Pocketful master contract download failed"
        );
        return Err(AppError::Broker(
            "Pocketful's instrument list could not be downloaded right now. Try again in a few minutes."
                .into(),
        ));
    }
    let bytes = resp.bytes().await?;
    let files = zip::entries(&bytes)?;
    drop(bytes);
    parse_archive(&files)
}
