//! Samco scrip master (web `database/master_contract_db.py`).
//!
//! One consolidated `ScripMaster.csv` (public, no auth). Columns are read
//! by header name, accepting both spellings Samco has shipped (`Trading
//! Symbol` / `tradingSymbol`, ...). Rules, in the web's order:
//!
//! * `brexchange` = the CSV exchange; `MFO` (MCX F&O) becomes `MCX`;
//!   `INDEX` rows become `NSE_INDEX` / `BSE_INDEX` / `MCX_INDEX`.
//! * Equity symbols drop `-EQ`, `-BE`, `-MF`, `-SG`.
//! * Futures `name + DDMMMYY + FUT`, options `name + DDMMMYY + strike +
//!   CE|PE` (CE/PE from the trading symbol's suffix) for NFO, MCX, CDS, BFO.
//! * Expiry normalised to `DD-MMM-YY`; instrument types to `FUT` / `CE` / `PE`.
//! * The CSV has no indices: the 68 Samco indices are appended from the
//!   web's fixed list, brsymbol being the exact `indexName` Samco expects.
//! * Token is Samco's `symbolCode` (`41015_NFO`), the multiQuote and
//!   streaming join key.

use super::SamcoBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{format_strike, split_csv_line, CsvHeader};
use crate::brokers::common::redact;
use crate::brokers::types::{AuthToken, SymbolData};
use crate::error::{AppError, Result};
use chrono::NaiveDate;

/// web `convert_date_format`: many input encodings -> `DD-MMM-YY`; anything
/// unparsable is returned uppercased, empty stays empty.
pub fn convert_date(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("nan") {
        return String::new();
    }
    let up = s.to_ascii_uppercase();
    // (format, has a four-digit year). Python's %Y needs four digits.
    const FORMATS: &[(&str, bool)] = &[
        ("%Y-%m-%d", true),
        ("%d-%m-%Y", true),
        ("%d/%m/%Y", true),
        ("%d/%m/%y", false),
        ("%y/%m/%d", false),
        ("%d%b%Y", true),
        ("%d%b%y", false),
        ("%Y%m%d", true),
        ("%d-%b-%Y", true),
        ("%d-%b-%y", false),
    ];
    for (fmt, four) in FORMATS {
        if *fmt == "%Y%m%d" {
            if up.len() == 8 && up.bytes().all(|b| b.is_ascii_digit()) {
                if let (Ok(y), Ok(m), Ok(d)) = (up[..4].parse(), up[4..6].parse(), up[6..].parse())
                {
                    if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                        return crate::brokers::common::master_contract::format_expiry(date);
                    }
                }
            }
            continue;
        }
        if let Ok(d) = NaiveDate::parse_from_str(&up, fmt) {
            use chrono::Datelike;
            let year_ok = if *four {
                // The year field must have been written with four digits.
                d.year() >= 1000
            } else {
                true
            };
            if year_ok {
                return crate::brokers::common::master_contract::format_expiry(d);
            }
        }
    }
    up
}

/// Symbol text of a strike: Python `str(float)` with a trailing `.0` cut.
fn strike_text(strike: f64) -> String {
    format_strike(strike)
}

struct Cols {
    exchange: usize,
    brsymbol: usize,
    name: Option<usize>,
    instrument: Option<usize>,
    token: usize,
    lot: Option<usize>,
    tick: Option<usize>,
    expiry: Option<usize>,
    strike: Option<usize>,
}

fn col(h: &CsvHeader, names: &[&str]) -> Option<usize> {
    names.iter().find_map(|n| h.index(n))
}

fn bad_file() -> AppError {
    AppError::Broker(
        "The Samco instrument file has an unexpected layout. Try downloading the master contract again later."
            .into(),
    )
}

/// web `process_samco_data` for one data row.
pub fn row_to_symbol(f: &[String], c: &CsvColumns) -> Option<SymbolData> {
    let c = &c.0;
    let get = |i: Option<usize>| -> String {
        i.and_then(|i| f.get(i))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let numf = |i: Option<usize>| -> Option<f64> {
        let t = get(i);
        t.replace(',', "")
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    };
    let brexchange = get(Some(c.exchange));
    let brsymbol = get(Some(c.brsymbol));
    let token = get(Some(c.token));
    if brsymbol.is_empty() && token.is_empty() {
        return None;
    }
    let name = get(c.name);
    let mut instrument = get(c.instrument);
    let lot = numf(c.lot).map(|v| v as i32).unwrap_or(1);
    let tick = numf(c.tick).unwrap_or(0.05);
    let strike = numf(c.strike).unwrap_or(0.0);
    let expiry = convert_date(&get(c.expiry));

    let mut exchange = if brexchange == "MFO" {
        "MCX".to_string()
    } else {
        brexchange.clone()
    };
    if instrument == "INDEX" {
        match exchange.as_str() {
            "NSE" => exchange = "NSE_INDEX".into(),
            "BSE" => exchange = "BSE_INDEX".into(),
            "MCX" => exchange = "MCX_INDEX".into(),
            _ => {}
        }
    }

    let mut symbol = brsymbol.clone();
    if instrument == "EQ" {
        for suffix in ["-EQ", "-BE", "-MF", "-SG"] {
            symbol = symbol.replace(suffix, "");
        }
    }
    let exp = expiry.replace('-', "");
    let fut = || format!("{}{}FUT", name, exp);
    let opt = |kind: &str| format!("{}{}{}{}", name, exp, strike_text(strike), kind);
    let suffix = if brsymbol.ends_with("CE") {
        Some("CE")
    } else if brsymbol.ends_with("PE") {
        Some("PE")
    } else {
        None
    };
    let it = instrument.as_str();
    match exchange.as_str() {
        "NFO" => {
            if matches!(it, "FUTIDX" | "FUTSTK" | "FUT") {
                symbol = fut();
            } else if matches!(it, "OPTIDX" | "OPTSTK" | "CE" | "PE") {
                if let Some(k) = suffix {
                    symbol = opt(k);
                }
            }
        }
        "MCX" => {
            if matches!(it, "FUTCOM" | "FUT") {
                symbol = fut();
            } else if matches!(it, "OPTFUT" | "CE" | "PE") {
                if let Some(k) = suffix {
                    symbol = opt(k);
                }
            }
        }
        "CDS" => {
            if matches!(it, "FUTCUR" | "FUTIRC" | "FUT") {
                symbol = fut();
            } else if matches!(it, "OPTCUR" | "OPTIRC" | "CE" | "PE") {
                if let Some(k) = suffix {
                    symbol = opt(k);
                }
            }
        }
        "BFO" => {
            if let Some(k) = suffix {
                symbol = opt(k);
            }
            if brsymbol.ends_with("FUT") || it.to_ascii_uppercase().contains("FUT") {
                symbol = fut();
            }
        }
        _ => {}
    }

    symbol = match symbol.as_str() {
        "Nifty 50" | "NIFTY 50" => "NIFTY".into(),
        "Nifty Next 50" => "NIFTYNXT50".into(),
        "Nifty Fin Service" | "NIFTY FIN SERVICE" => "FINNIFTY".into(),
        "Nifty Bank" | "NIFTY BANK" => "BANKNIFTY".into(),
        "NIFTY MID SELECT" => "MIDCPNIFTY".into(),
        "India VIX" | "INDIA VIX" => "INDIAVIX".into(),
        _ => symbol,
    };

    if matches!(
        instrument.as_str(),
        "OPTIDX" | "OPTSTK" | "OPTFUT" | "OPTCUR" | "OPTIRC"
    ) {
        if symbol.ends_with("CE") {
            instrument = "CE".into();
        } else if symbol.ends_with("PE") {
            instrument = "PE".into();
        }
    }
    if matches!(
        instrument.as_str(),
        "FUTIDX" | "FUTSTK" | "FUTCOM" | "FUTCUR" | "FUTIRC"
    ) {
        instrument = "FUT".into();
    }

    Some(SymbolData {
        symbol,
        brsymbol,
        name,
        exchange,
        brexchange,
        token,
        expiry,
        strike,
        lot_size: lot,
        instrument_type: instrument,
        tick_size: tick,
    })
}

/// Column positions resolved from the header.
pub struct CsvColumns(Cols);

impl CsvColumns {
    pub fn from_header(line: &str) -> Result<Self> {
        let h = CsvHeader::parse(line);
        Ok(Self(Cols {
            exchange: col(&h, &["Exchange", "exchange"]).ok_or_else(bad_file)?,
            brsymbol: col(&h, &["Trading Symbol", "tradingSymbol"]).ok_or_else(bad_file)?,
            name: col(&h, &["Symbol Name", "symbolName"]),
            instrument: col(&h, &["Instrument", "instrument"]),
            token: col(&h, &["symbolCode", "Symbol Code", "Token", "token"])
                .ok_or_else(bad_file)?,
            lot: col(&h, &["Lot Size", "lotSize"]),
            tick: col(&h, &["Tick Size", "tickSize"]),
            expiry: col(&h, &["Expiry Date", "expiryDate"]),
            strike: col(&h, &["Strike Price", "strikePrice"]),
        }))
    }
}

/// The fixed index rows (web `get_index_data`).
pub fn index_rows() -> Vec<SymbolData> {
    INDICES
        .iter()
        .map(|(symbol, br, name, ex, brex, token)| SymbolData {
            symbol: symbol.to_string(),
            brsymbol: br.to_string(),
            name: name.to_string(),
            exchange: ex.to_string(),
            brexchange: brex.to_string(),
            token: token.to_string(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "INDEX".into(),
            tick_size: 0.05,
        })
        .collect()
}

/// Parse the whole CSV and append the index list.
pub fn parse(csv: &str) -> Result<Vec<SymbolData>> {
    let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().ok_or_else(bad_file)?;
    let cols = CsvColumns::from_header(header)?;
    let mut out: Vec<SymbolData> = lines
        .filter_map(|l| row_to_symbol(&split_csv_line(l), &cols))
        .collect();
    out.extend(index_rows());
    Ok(out)
}

pub async fn download(b: &SamcoBroker, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
    let resp = b
        .http
        .get(&b.master_url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(redact::http)?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Samco scrip master download failed"
        );
        return Err(AppError::Broker(
            "The Samco instrument file could not be downloaded. Try again shortly.".into(),
        ));
    }
    let bytes = resp.bytes().await.map_err(redact::http)?;
    let text = String::from_utf8_lossy(&bytes);
    let rows = parse(&text)?;
    tracing::info!("Samco master contract: {} instruments", rows.len());
    Ok(rows)
}

/// web `get_index_data()`: (symbol, brsymbol, name, exchange, brexchange, token).
pub const INDICES: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "NIFTY",
        "NIFTY 50",
        "Nifty 50",
        "NSE_INDEX",
        "NSE",
        "NIFTY_50",
    ),
    (
        "BANKNIFTY",
        "NIFTY BANK",
        "Nifty Bank",
        "NSE_INDEX",
        "NSE",
        "NIFTY_BANK",
    ),
    (
        "FINNIFTY",
        "NIFTY FIN SERVICE",
        "Nifty Fin Service",
        "NSE_INDEX",
        "NSE",
        "NIFTY_FIN_SERVICE",
    ),
    (
        "MIDCPNIFTY",
        "NIFTY MID SELECT",
        "Nifty Mid Select",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MID_SELECT",
    ),
    (
        "NIFTYNXT50",
        "NIFTY NEXT 50",
        "Nifty Next 50",
        "NSE_INDEX",
        "NSE",
        "NIFTY_NEXT_50",
    ),
    (
        "NIFTY100",
        "NIFTY 100",
        "Nifty 100",
        "NSE_INDEX",
        "NSE",
        "NIFTY_100",
    ),
    (
        "NIFTY200",
        "NIFTY 200",
        "Nifty 200",
        "NSE_INDEX",
        "NSE",
        "NIFTY_200",
    ),
    (
        "NIFTY500",
        "NIFTY 500",
        "Nifty 500",
        "NSE_INDEX",
        "NSE",
        "NIFTY_500",
    ),
    (
        "NIFTYMIDCAP50",
        "NIFTY MIDCAP 50",
        "Nifty Midcap 50",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MIDCAP_50",
    ),
    (
        "NIFTYIT",
        "NIFTY IT",
        "Nifty IT",
        "NSE_INDEX",
        "NSE",
        "NIFTY_IT",
    ),
    (
        "NIFTYAUTO",
        "NIFTY AUTO",
        "Nifty Auto",
        "NSE_INDEX",
        "NSE",
        "NIFTY_AUTO",
    ),
    (
        "NIFTYPHARMA",
        "NIFTY PHARMA",
        "Nifty Pharma",
        "NSE_INDEX",
        "NSE",
        "NIFTY_PHARMA",
    ),
    (
        "NIFTYMETAL",
        "NIFTY METAL",
        "Nifty Metal",
        "NSE_INDEX",
        "NSE",
        "NIFTY_METAL",
    ),
    (
        "NIFTYFMCG",
        "NIFTY FMCG",
        "Nifty FMCG",
        "NSE_INDEX",
        "NSE",
        "NIFTY_FMCG",
    ),
    (
        "NIFTYREALTY",
        "NIFTY REALTY",
        "Nifty Realty",
        "NSE_INDEX",
        "NSE",
        "NIFTY_REALTY",
    ),
    (
        "NIFTYENERGY",
        "NIFTY ENERGY",
        "Nifty Energy",
        "NSE_INDEX",
        "NSE",
        "NIFTY_ENERGY",
    ),
    (
        "NIFTYMEDIA",
        "NIFTY MEDIA",
        "Nifty Media",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MEDIA",
    ),
    (
        "NIFTYPSUBANK",
        "NIFTY PSU BANK",
        "Nifty PSU Bank",
        "NSE_INDEX",
        "NSE",
        "NIFTY_PSU_BANK",
    ),
    (
        "NIFTYPVTBANK",
        "NIFTY PVT BANK",
        "Nifty Pvt Bank",
        "NSE_INDEX",
        "NSE",
        "NIFTY_PVT_BANK",
    ),
    (
        "NIFTYINFRA",
        "NIFTY INFRA",
        "Nifty Infra",
        "NSE_INDEX",
        "NSE",
        "NIFTY_INFRA",
    ),
    (
        "NIFTYCPSE",
        "NIFTY CPSE",
        "Nifty CPSE",
        "NSE_INDEX",
        "NSE",
        "NIFTY_CPSE",
    ),
    (
        "NIFTYCOMMODITIES",
        "NIFTY COMMODITIES",
        "Nifty Commodities",
        "NSE_INDEX",
        "NSE",
        "NIFTY_COMMODITIES",
    ),
    (
        "NIFTYCONSUMPTION",
        "NIFTY CONSUMPTION",
        "Nifty Consumption",
        "NSE_INDEX",
        "NSE",
        "NIFTY_CONSUMPTION",
    ),
    (
        "NIFTYMNC",
        "NIFTY MNC",
        "Nifty MNC",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MNC",
    ),
    (
        "NIFTYPSE",
        "NIFTY PSE",
        "Nifty PSE",
        "NSE_INDEX",
        "NSE",
        "NIFTY_PSE",
    ),
    (
        "NIFTYSERVSECTOR",
        "NIFTY SERV SECTOR",
        "Nifty Services Sector",
        "NSE_INDEX",
        "NSE",
        "NIFTY_SERV_SECTOR",
    ),
    (
        "NIFTYGROWSECT15",
        "NIFTY GROWSECT 15",
        "Nifty Growth Sectors 15",
        "NSE_INDEX",
        "NSE",
        "NIFTY_GROWSECT_15",
    ),
    (
        "NIFTYDIVOPPS50",
        "NIFTY DIV OPPS 50",
        "Nifty Dividend Opportunities 50",
        "NSE_INDEX",
        "NSE",
        "NIFTY_DIV_OPPS_50",
    ),
    (
        "NIFTY50VALUE20",
        "NIFTY50 VALUE 20",
        "Nifty50 Value 20",
        "NSE_INDEX",
        "NSE",
        "NIFTY50_VALUE_20",
    ),
    (
        "NIFTYQUALITY30",
        "NIFTY Quality 30",
        "Nifty Quality 30",
        "NSE_INDEX",
        "NSE",
        "NIFTY_QUALITY_30",
    ),
    (
        "NIFTYMIDLIQ15",
        "NIFTY Mid LIQ 15",
        "Nifty Midcap Liquid 15",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MID_LIQ_15",
    ),
    (
        "NIFTY100LIQ15",
        "NIFTY100 LIQ 15",
        "Nifty100 Liquid 15",
        "NSE_INDEX",
        "NSE",
        "NIFTY100_LIQ_15",
    ),
    (
        "NIFTYMID100FREE",
        "NIFTY MID100 FREE",
        "Nifty Midcap 100 Free Float",
        "NSE_INDEX",
        "NSE",
        "NIFTY_MID100_FREE",
    ),
    (
        "NIFTYSML100FREE",
        "NIFTY SML100 FREE",
        "Nifty Smallcap 100 Free Float",
        "NSE_INDEX",
        "NSE",
        "NIFTY_SML100_FREE",
    ),
    (
        "NIFTY50PR1XINV",
        "NIFTY50 PR 1x INV",
        "Nifty50 PR 1x Inverse",
        "NSE_INDEX",
        "NSE",
        "NIFTY50_PR_1X_INV",
    ),
    (
        "NIFTY50PR2XLEV",
        "NIFTY50 PR 2x LEV",
        "Nifty50 PR 2x Leverage",
        "NSE_INDEX",
        "NSE",
        "NIFTY50_PR_2X_LEV",
    ),
    (
        "NIFTY50TR1XINV",
        "NIFTY50 TR 1x INV",
        "Nifty50 TR 1x Inverse",
        "NSE_INDEX",
        "NSE",
        "NIFTY50_TR_1X_INV",
    ),
    (
        "NIFTY50TR2XLEV",
        "NIFTY50 TR 2x LEV",
        "Nifty50 TR 2x Leverage",
        "NSE_INDEX",
        "NSE",
        "NIFTY50_TR_2X_LEV",
    ),
    (
        "INDIAVIX",
        "INDIA VIX",
        "India VIX",
        "NSE_INDEX",
        "NSE",
        "INDIA_VIX",
    ),
    (
        "SENSEX",
        "SENSEX",
        "S&P BSE SENSEX",
        "BSE_INDEX",
        "BSE",
        "SENSEX",
    ),
    (
        "BANKEX",
        "BANKEX",
        "S&P BSE BANKEX",
        "BSE_INDEX",
        "BSE",
        "BANKEX",
    ),
    (
        "SENSEX50",
        "SNSX50",
        "S&P BSE SENSEX 50",
        "BSE_INDEX",
        "BSE",
        "SENSEX_50",
    ),
    (
        "BSENXT50",
        "SNXT50",
        "S&P BSE SENSEX Next 50",
        "BSE_INDEX",
        "BSE",
        "BSE_NXT_50",
    ),
    (
        "BSEIT",
        "BSE IT",
        "S&P BSE IT",
        "BSE_INDEX",
        "BSE",
        "BSE_IT",
    ),
    (
        "BSEHC",
        "BSE HC",
        "S&P BSE Healthcare",
        "BSE_INDEX",
        "BSE",
        "BSE_HC",
    ),
    (
        "BSECG",
        "BSE CG",
        "S&P BSE Capital Goods",
        "BSE_INDEX",
        "BSE",
        "BSE_CG",
    ),
    (
        "BSECD",
        "BSE CD",
        "S&P BSE Consumer Durables",
        "BSE_INDEX",
        "BSE",
        "BSE_CD",
    ),
    (
        "BSEPSU",
        "BSEPSU",
        "S&P BSE PSU",
        "BSE_INDEX",
        "BSE",
        "BSE_PSU",
    ),
    (
        "BSEFMC",
        "BSEFMC",
        "S&P BSE Fast Moving Consumer Goods",
        "BSE_INDEX",
        "BSE",
        "BSE_FMC",
    ),
    (
        "BSEMETAL",
        "METAL",
        "S&P BSE Metal",
        "BSE_INDEX",
        "BSE",
        "BSE_METAL",
    ),
    (
        "BSEOILGAS",
        "OILGAS",
        "S&P BSE Oil & Gas",
        "BSE_INDEX",
        "BSE",
        "BSE_OILGAS",
    ),
    (
        "BSEAUTO",
        "AUTO",
        "S&P BSE Auto",
        "BSE_INDEX",
        "BSE",
        "BSE_AUTO",
    ),
    (
        "BSEPOWER",
        "POWER",
        "S&P BSE Power",
        "BSE_INDEX",
        "BSE",
        "BSE_POWER",
    ),
    (
        "BSEREALTY",
        "REALTY",
        "S&P BSE Realty",
        "BSE_INDEX",
        "BSE",
        "BSE_REALTY",
    ),
    (
        "BSETECK",
        "TECK",
        "S&P BSE Teck",
        "BSE_INDEX",
        "BSE",
        "BSE_TECK",
    ),
    (
        "BSECDGS",
        "CDGS",
        "S&P BSE Consumer Discretionary Goods & Services",
        "BSE_INDEX",
        "BSE",
        "BSE_CDGS",
    ),
    (
        "BSEBASMTR",
        "BASMTR",
        "S&P BSE Basic Materials",
        "BSE_INDEX",
        "BSE",
        "BSE_BASMTR",
    ),
    (
        "BSEGREENX",
        "GREENX",
        "S&P BSE Greenex",
        "BSE_INDEX",
        "BSE",
        "BSE_GREENX",
    ),
    (
        "BSECARBON",
        "CARBON",
        "S&P BSE Carbonex",
        "BSE_INDEX",
        "BSE",
        "BSE_CARBON",
    ),
    (
        "BSEIPO",
        "BSEIPO",
        "S&P BSE IPO",
        "BSE_INDEX",
        "BSE",
        "BSE_IPO",
    ),
    (
        "SMEIPO",
        "SMEIPO",
        "S&P BSE SME IPO",
        "BSE_INDEX",
        "BSE",
        "SME_IPO",
    ),
    (
        "BSEALLCAP",
        "ALLCAP",
        "S&P BSE AllCap",
        "BSE_INDEX",
        "BSE",
        "BSE_ALLCAP",
    ),
    (
        "BSELRGCAP",
        "LRGCAP",
        "S&P BSE LargeCap",
        "BSE_INDEX",
        "BSE",
        "BSE_LRGCAP",
    ),
    (
        "BSEMIDSEL",
        "MIDSEL",
        "S&P BSE MidCap Select",
        "BSE_INDEX",
        "BSE",
        "BSE_MIDSEL",
    ),
    (
        "BSESMLSEL",
        "SMLSEL",
        "S&P BSE SmallCap Select",
        "BSE_INDEX",
        "BSE",
        "BSE_SMLSEL",
    ),
    (
        "BSEDOL30",
        "DOL30",
        "S&P BSE Dollex 30",
        "BSE_INDEX",
        "BSE",
        "BSE_DOL30",
    ),
    (
        "BSEDOL100",
        "DOL100",
        "S&P BSE Dollex 100",
        "BSE_INDEX",
        "BSE",
        "BSE_DOL100",
    ),
    (
        "BSEDOL200",
        "DOL200",
        "S&P BSE Dollex 200",
        "BSE_INDEX",
        "BSE",
        "BSE_DOL200",
    ),
];
