//! Master contract (web `database/master_contract_db.py`).
//!
//! Ten CSVs from `{BASE}/contractfiles/<SEGMENT>.csv`, each retried four
//! times with 2/4/8 s backoff; any segment that still fails aborts the
//! refresh so the previous master stays in place. Columns: `Exchange,
//! Underlying Instrument Symbol, Instrument ID, Instrument Type, Option
//! Type, Strike Price, Underlying Instrument Name, Trading Symbol, Expiry,
//! Lot Size, Tick Size`.

use super::IiflCapitalBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{format_expiry, split_csv_line, CsvHeader};
use crate::brokers::types::{AuthToken, SymbolData};
use crate::error::{AppError, Result};
use chrono::NaiveDate;

/// Segment file -> OpenAlgo exchange, in the web's processing order.
pub const SEGMENTS: &[(&str, &str)] = &[
    ("NSEEQ", "NSE"),
    ("BSEEQ", "BSE"),
    ("NSEFO", "NFO"),
    ("BSEFO", "BFO"),
    ("NSECURR", "CDS"),
    ("BSECURR", "BCD"),
    ("NSECOMM", "MCX"),
    ("MCXCOMM", "MCX"),
    ("NCDEXCOMM", "MCX"),
    ("INDICES", ""),
];

const DOWNLOAD_ATTEMPTS: u32 = 4;

/// web `NSE_INDEX_MAP` (applied after upper-casing and removing spaces).
pub const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYMIDCAPSELECT", "MIDCPNIFTY"),
];

/// web `BSE_INDEX_MAP`.
pub const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("AUTO", "BSEAUTO"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("TECK", "BSETECK"),
    ("METAL", "BSEMETAL"),
    ("OILGAS", "BSEOIL&GAS"),
    ("REALTY", "BSEREALTY"),
    ("POWER", "BSEPOWER"),
    ("GREENX", "BSEGREENEX"),
    ("CARBON", "BSECARBONEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("CPSE", "BSECPSE"),
    ("MIDCAP", "BSEMIDCAP"),
    ("SMLCAP", "BSESMALLCAP"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("ENERGY", "BSEENERGY"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("TELCOM", "BSETELECOM"),
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
];

fn rename<'a>(table: &'a [(&'a str, &'a str)], s: &str) -> Option<&'a str> {
    table.iter().find(|(a, _)| *a == s).map(|(_, b)| *b)
}

/// `30-Apr-2026`, `24-Apr-2026 23:59`, `2026-04-30` -> `30-APR-26`; empty
/// when unparsable (web `format_expiry`).
pub fn expiry(s: &str) -> String {
    let head = s.trim().split([' ', 'T']).next().unwrap_or("").trim();
    if head.is_empty() {
        return String::new();
    }
    for fmt in ["%d-%b-%Y", "%Y-%m-%d", "%d-%b-%y", "%d%b%Y", "%d/%m/%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(head, fmt) {
            return format_expiry(d);
        }
    }
    String::new()
}

/// Strike text in a symbol (web `format_strike`): whole numbers without a
/// fraction, others as Python prints them, empty when not positive.
pub fn strike_text(strike: f64) -> String {
    // Also rejects NaN.
    if strike.is_nan() || strike <= 0.0 {
        return String::new();
    }
    if strike.fract() == 0.0 {
        format!("{}", strike as i64)
    } else {
        format!("{}", strike)
    }
}

struct Cols {
    exchange: Option<usize>,
    underlying: usize,
    token: usize,
    itype: Option<usize>,
    otype: Option<usize>,
    strike: Option<usize>,
    name: Option<usize>,
    tsym: Option<usize>,
    expiry: Option<usize>,
    lot: Option<usize>,
    tick: Option<usize>,
}

impl Cols {
    fn from(h: &CsvHeader, name_col: &str) -> Option<Self> {
        Some(Self {
            exchange: h.index("Exchange"),
            underlying: h.index(name_col)?,
            token: h.index("Instrument ID")?,
            itype: h.index("Instrument Type"),
            otype: h.index("Option Type"),
            strike: h.index("Strike Price"),
            name: h.index("Underlying Instrument Name"),
            tsym: h.index("Trading Symbol"),
            expiry: h.index("Expiry"),
            lot: h.index("Lot Size"),
            tick: h.index("Tick Size"),
        })
    }
}

fn get(f: &[String], i: Option<usize>) -> &str {
    i.and_then(|i| f.get(i)).map(|s| s.trim()).unwrap_or("")
}

fn lot(f: &[String], c: &Cols) -> i32 {
    get(f, c.lot)
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| v as i32)
        .unwrap_or(1)
}

fn tick(f: &[String], c: &Cols) -> f64 {
    get(f, c.tick)
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(0.01)
}

/// Parse one segment file into master rows.
pub fn parse_segment(segment: &str, exchange: &str, csv: &str) -> Vec<SymbolData> {
    let mut lines = csv.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(header);
    let name_col = if segment == "INDICES" {
        "Underlying Instrument Name"
    } else {
        "Underlying Instrument Symbol"
    };
    let Some(c) = Cols::from(&h, name_col) else {
        tracing::warn!(
            "IIFL Capital {} contract file has unexpected columns",
            segment
        );
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let token = get(&f, Some(c.token));
        let underlying = get(&f, Some(c.underlying));
        if token.is_empty() || underlying.is_empty() {
            continue;
        }
        let row = match segment {
            "NSEEQ" | "BSEEQ" => {
                if get(&f, c.itype) == "INDEX" {
                    continue;
                }
                SymbolData {
                    symbol: underlying.to_string(),
                    brsymbol: get(&f, c.tsym).to_string(),
                    name: get(&f, c.name).to_string(),
                    exchange: exchange.to_string(),
                    brexchange: segment.to_string(),
                    token: token.to_string(),
                    expiry: String::new(),
                    strike: 0.0,
                    lot_size: lot(&f, &c),
                    instrument_type: "EQ".into(),
                    tick_size: tick(&f, &c),
                }
            }
            "INDICES" => {
                let csv_ex = get(&f, c.exchange).to_ascii_uppercase();
                let oa_ex = match csv_ex.as_str() {
                    "NSEEQ" => "NSE_INDEX",
                    "BSEEQ" => "BSE_INDEX",
                    _ => continue,
                };
                let base = underlying.to_ascii_uppercase().replace(' ', "");
                let table = if oa_ex == "NSE_INDEX" {
                    NSE_INDEX_MAP
                } else {
                    BSE_INDEX_MAP
                };
                let symbol = rename(table, &base).map(str::to_string).unwrap_or(base);
                SymbolData {
                    symbol: symbol.clone(),
                    brsymbol: get(&f, c.tsym).to_string(),
                    name: symbol,
                    exchange: oa_ex.to_string(),
                    brexchange: get(&f, c.exchange).to_string(),
                    token: token.to_string(),
                    expiry: String::new(),
                    strike: 0.0,
                    lot_size: 1,
                    instrument_type: "INDEX".into(),
                    tick_size: 0.01,
                }
            }
            _ => {
                let itype = match get(&f, c.otype).to_ascii_uppercase().as_str() {
                    "XX" => "FUT",
                    "CE" => "CE",
                    "PE" => "PE",
                    _ => continue,
                };
                let exp = expiry(get(&f, c.expiry));
                let compact = exp.replace('-', "");
                let strike = get(&f, c.strike).parse::<f64>().unwrap_or(0.0);
                let symbol = if itype == "FUT" {
                    format!("{}{}FUT", underlying, compact)
                } else {
                    format!("{}{}{}{}", underlying, compact, strike_text(strike), itype)
                };
                SymbolData {
                    symbol,
                    brsymbol: get(&f, c.tsym).to_string(),
                    name: get(&f, c.name).to_string(),
                    exchange: exchange.to_string(),
                    brexchange: segment.to_string(),
                    token: token.to_string(),
                    expiry: exp,
                    strike,
                    lot_size: lot(&f, &c),
                    instrument_type: itype.into(),
                    tick_size: tick(&f, &c),
                }
            }
        };
        out.push(row);
    }
    out
}

async fn fetch(b: &IiflCapitalBroker, segment: &str) -> Result<String> {
    let url = format!("{}/contractfiles/{}.csv", b.base_url(), segment);
    let mut last = String::new();
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        let res = b.http.get(&url).timeout(DOWNLOAD_TIMEOUT).send().await;
        match res {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(t) if !t.trim().is_empty() => return Ok(t),
                Ok(_) => last = "empty file".into(),
                Err(e) => last = e.to_string(),
            },
            Ok(r) => last = format!("HTTP {}", r.status().as_u16()),
            Err(e) => last = e.to_string(),
        }
        if attempt < DOWNLOAD_ATTEMPTS {
            let wait = b.download_backoff * (1u32 << (attempt - 1));
            tracing::warn!(
                "IIFL Capital {} contract download failed (attempt {}/{}): {}; retrying in {:?}",
                segment,
                attempt,
                DOWNLOAD_ATTEMPTS,
                last,
                wait
            );
            tokio::time::sleep(wait).await;
        }
    }
    tracing::error!(
        "IIFL Capital {} contract download gave up: {}",
        segment,
        last
    );
    Err(AppError::Broker(format!(
        "The IIFL Capital master contract could not be downloaded ({} segment). Your previous symbols were kept; try the download again shortly.",
        segment
    )))
}

pub async fn download(b: &IiflCapitalBroker, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
    // Each file is parsed and dropped as it lands; any segment that fails
    // returns an error before the caller replaces the master (the web aborts
    // before truncating its table).
    let mut rows = Vec::new();
    for (segment, exchange) in SEGMENTS {
        let text = fetch(b, segment).await?;
        rows.extend(parse_segment(segment, exchange, &text));
    }
    tracing::info!("IIFL Capital master contract parsed: {} rows", rows.len());
    Ok(rows)
}
