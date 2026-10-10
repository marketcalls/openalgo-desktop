//! Master contract (web `database/master_contract_db.py`).
//!
//! One file per exchange (`{host}/{EXCH}_symbols.txt.zip`, or flattrade's
//! S3 CSVs, NFO/BFO split in two). Columns are read by header name, with
//! both casings (`TradingSymbol`/`Tradingsymbol`, `StrikePrice`/`Strike`,
//! `LotSize`/`Lotsize`, `OptionType`/`Optiontype`).

use super::{BseIndices, IndexNaming, MasterFile, NorenBroker, NorenConfig, TickRule};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, future_symbol, option_symbol, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use chrono::NaiveDate;

/// Shoonya-style NSE index overrides (after strip of spaces/hyphens).
pub const NSE_INDEX_OVERRIDES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYINDEX", "NIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYFIN", "FINNIFTY"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYFINANCIALSERVICES", "FINNIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYMIDCAPSELECT", "MIDCPNIFTY"),
];

/// Zebu exact-name NSE index table.
pub const NSE_INDEX_EXACT: &[(&str, &str)] = &[
    ("NIFTY INDEX", "NIFTY"),
    ("NIFTY BANK", "BANKNIFTY"),
    ("NIFTY FIN SERVICE", "FINNIFTY"),
    ("NIFTY MIDCAP SELECT", "MIDCPNIFTY"),
    ("NIFTY NEXT 50", "NIFTYNXT50"),
    ("INDIA VIX", "INDIAVIX"),
];

/// Flattrade BSE `UNDIND` overrides (after strip).
pub const BSE_INDEX_OVERRIDES: &[(&str, &str)] = &[
    ("BSESENSEX", "SENSEX"),
    ("S&PBSESENSEX", "SENSEX"),
    ("BSESENSEX50", "SENSEX50"),
    ("S&PBSESENSEX50", "SENSEX50"),
    ("BSESENSEXNEXT50", "BSESENSEXNEXT50"),
    ("S&PBSESENSEXNEXT50", "BSESENSEXNEXT50"),
];

fn strip_upper(s: &str) -> String {
    s.to_ascii_uppercase().replace([' ', '-'], "")
}

fn lookup<'a>(t: &'a [(&'a str, &'a str)], k: &str) -> Option<&'a str> {
    t.iter().find(|(a, _)| *a == k).map(|(_, b)| *b)
}

/// OpenAlgo symbol of an NSE index row.
pub fn nse_index_symbol(naming: IndexNaming, tsym: &str) -> String {
    match naming {
        IndexNaming::StripAndOverride => {
            let s = strip_upper(tsym);
            lookup(NSE_INDEX_OVERRIDES, &s)
                .map(str::to_string)
                .unwrap_or(s)
        }
        IndexNaming::ExactName => lookup(NSE_INDEX_EXACT, tsym)
            .map(str::to_string)
            .unwrap_or_else(|| tsym.to_string()),
    }
}

/// `DD-MMM-YYYY` -> `DD-MMM-YY`; anything else is blank (web `None`).
pub fn expiry(s: &str) -> String {
    let t = s.trim();
    let up = t.to_ascii_uppercase();
    NaiveDate::parse_from_str(&up, "%d-%b-%Y")
        .map(format_expiry)
        .unwrap_or_default()
}

struct Cols {
    exchange: Option<usize>,
    token: Option<usize>,
    lot: Option<usize>,
    name: Option<usize>,
    tsym: Option<usize>,
    expiry: Option<usize>,
    instrument: Option<usize>,
    option: Option<usize>,
    strike: Option<usize>,
    tick: Option<usize>,
}

impl Cols {
    fn new(h: &CsvHeader) -> Self {
        let any = |names: &[&str]| names.iter().find_map(|n| h.index(n));
        Self {
            exchange: any(&["Exchange"]),
            token: any(&["Token"]),
            lot: any(&["LotSize", "Lotsize"]),
            name: any(&["Symbol"]),
            tsym: any(&["TradingSymbol", "Tradingsymbol"]),
            expiry: any(&["Expiry"]),
            instrument: any(&["Instrument"]),
            option: any(&["OptionType", "Optiontype"]),
            strike: any(&["StrikePrice", "Strike"]),
            tick: any(&["TickSize"]),
        }
    }
}

fn tick_size(rule: TickRule, exchange: &str, raw: &str) -> f64 {
    let v = raw.trim().parse::<f64>().unwrap_or(0.0);
    match rule {
        TickRule::Raw => v,
        TickRule::CashInPaise if matches!(exchange, "NSE" | "BSE") => v / 100.0,
        TickRule::CashInPaise => v,
        TickRule::Fixed if exchange == "CDS" => 0.0025,
        TickRule::Fixed => 0.05,
    }
}

/// A cell pandas reads as missing (`read_csv`'s default NA strings), as
/// the web sees the master: Flattrade writes `NULL` into stale rows.
fn is_na(cell: &str) -> bool {
    matches!(
        cell,
        "" | "NULL"
            | "null"
            | "NaN"
            | "nan"
            | "-NaN"
            | "-nan"
            | "None"
            | "N/A"
            | "n/a"
            | "NA"
            | "<NA>"
            | "#N/A"
            | "#NA"
    )
}

/// Parse one exchange file into SymToken rows.
pub fn parse_file(cfg: &NorenConfig, exchange: &str, text: &str) -> Vec<SymToken> {
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let c = Cols::new(&CsvHeader::parse(head));
    let drop_stale = exchange == "BSE" && cfg.bse_drop_without_exchange;
    if drop_stale && c.exchange.is_none() {
        tracing::warn!(
            broker = cfg.id,
            "BSE master has no Exchange column; stale rows are kept"
        );
    }
    let mut stale = 0usize;
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: Option<usize>| {
            i.and_then(|i| f.get(i))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let token = get(c.token);
        let tsym = get(c.tsym);
        let name = get(c.name);
        if token.is_empty() || tsym.is_empty() {
            continue;
        }
        if drop_stale && c.exchange.is_some() && is_na(&get(c.exchange)) {
            stale += 1;
            continue;
        }
        let lot_raw = get(c.lot);
        let lot = lot_raw.parse::<f64>().map(|v| v as i32).unwrap_or(
            if cfg.tick_rule == TickRule::Fixed && matches!(exchange, "NSE" | "BSE") {
                1
            } else {
                0
            },
        );
        let tick = tick_size(cfg.tick_rule, exchange, &get(c.tick));
        let instrument = get(c.instrument);
        let base = SymToken {
            symbol: String::new(),
            brsymbol: tsym.clone(),
            name: name.clone(),
            exchange: exchange.to_string(),
            brexchange: exchange.to_string(),
            token,
            expiry: String::new(),
            strike: -1.0,
            lot_size: lot,
            instrument_type: String::new(),
            tick_size: tick,
        };
        match exchange {
            "NSE" => {
                if name.is_empty() && c.name.is_some() && cfg.tick_rule != TickRule::Fixed {
                    continue;
                }
                let index = instrument == "INDEX";
                let row = if index {
                    SymToken {
                        symbol: nse_index_symbol(cfg.index_naming, &tsym),
                        exchange: "NSE_INDEX".into(),
                        brexchange: cfg.nse_index_brexchange.into(),
                        instrument_type: cfg.index_instrument_type.into(),
                        ..base
                    }
                } else {
                    SymToken {
                        symbol: tsym.replace("-EQ", "").replace("-BE", ""),
                        instrument_type: if matches!(instrument.as_str(), "EQ" | "BE" | "") {
                            "EQ".into()
                        } else {
                            instrument.clone()
                        },
                        ..base
                    }
                };
                out.push(row);
            }
            "BSE" => {
                if cfg.bse_indices == BseIndices::FromMaster && instrument == "UNDIND" {
                    let s = strip_upper(&tsym);
                    out.push(SymToken {
                        symbol: lookup(BSE_INDEX_OVERRIDES, &s)
                            .map(str::to_string)
                            .unwrap_or(s),
                        exchange: "BSE_INDEX".into(),
                        brexchange: "BSE".into(),
                        instrument_type: cfg.index_instrument_type.into(),
                        ..base
                    });
                } else {
                    out.push(SymToken {
                        symbol: tsym.clone(),
                        instrument_type: "EQ".into(),
                        ..base
                    });
                }
            }
            _ => {
                if exchange == "CDS"
                    && cfg.tick_rule != TickRule::Fixed
                    && base.token.parse::<i64>().map(|t| t <= 100).unwrap_or(false)
                {
                    continue;
                }
                let exp = expiry(&get(c.expiry));
                let opt = get(c.option);
                let from_tsym = exchange == "BFO" && cfg.bfo_from_tsym;
                let itype = if from_tsym {
                    if tsym.ends_with("FUT") {
                        "FUT".to_string()
                    } else if tsym.ends_with("CE") {
                        "CE".into()
                    } else if tsym.ends_with("PE") {
                        "PE".into()
                    } else {
                        "UNKNOWN".into()
                    }
                } else if opt == "XX" {
                    "FUT".into()
                } else if opt == "CE" || opt == "PE" {
                    opt.clone()
                } else {
                    instrument.clone()
                };
                let underlying = if from_tsym {
                    tsym.chars()
                        .take_while(|c| c.is_ascii_alphabetic())
                        .collect::<String>()
                } else {
                    name.clone()
                };
                let strike = get(c.strike).parse::<f64>().unwrap_or(-1.0);
                let symbol = if itype == "FUT" {
                    future_symbol(&underlying, &exp)
                } else if itype == "CE" || itype == "PE" {
                    option_symbol(&underlying, &exp, strike, &itype)
                } else {
                    format!(
                        "{}{}{}{}",
                        underlying,
                        exp.replace('-', ""),
                        format_strike(strike),
                        itype
                    )
                };
                out.push(SymToken {
                    symbol,
                    name: underlying,
                    expiry: exp,
                    strike,
                    instrument_type: itype,
                    ..base
                });
            }
        }
    }
    if exchange == "BSE" && cfg.bse_indices == BseIndices::Manual {
        for (sym, token) in [("SENSEX", "1"), ("BANKEX", "12")] {
            out.push(SymToken {
                symbol: sym.into(),
                brsymbol: sym.into(),
                name: sym.into(),
                exchange: "BSE_INDEX".into(),
                brexchange: "BSE_INDEX".into(),
                token: token.into(),
                expiry: String::new(),
                strike: -1.0,
                lot_size: 1,
                instrument_type: cfg.index_instrument_type.into(),
                tick_size: 0.05,
            });
        }
    }
    if stale > 0 {
        tracing::info!(
            broker = cfg.id,
            "Dropped {} BSE master rows with no exchange",
            stale
        );
    }
    out
}

/// One master file's text, or `None` when it could not be fetched (the
/// reason is logged).
async fn fetch_file(b: &NorenBroker, file: &MasterFile, url: &str) -> Option<String> {
    let resp = match b.http.get(url).timeout(DOWNLOAD_TIMEOUT).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Master file {} answered {}",
                file.exchange,
                r.status()
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Master file {} failed: {}",
                file.exchange,
                e
            );
            return None;
        }
    };
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Master file {} failed: {}",
                file.exchange,
                e
            );
            return None;
        }
    };
    let raw = if file.zipped {
        match super::zip::first_entry(&bytes) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    broker = b.cfg.id,
                    "Master file {} is not a readable zip",
                    file.exchange
                );
                return None;
            }
        }
    } else {
        bytes.to_vec()
    };
    // No copy for a valid UTF-8 file (the usual case; masters run to tens
    // of megabytes).
    Some(
        String::from_utf8(raw)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    )
}

/// Each name once, in first-seen order (NFO and BFO span two files).
fn unique<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for &n in names {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// Download and parse every file of the member's set, all or nothing
/// (web #2198 for Flattrade; MC-02 for Shoonya, Zebu and TradeSmart, whose
/// web download fails too when a file is missing): a file that fails or
/// comes back empty, or a segment that yields no rows, refuses the whole
/// download, so the stored master is kept rather than replaced by a
/// partial one.
pub async fn download(b: &NorenBroker) -> Result<Vec<SymToken>> {
    let mut rows = Vec::new();
    let mut failed: Vec<&str> = Vec::new();
    // Rows per segment, in file order (NFO and BFO add up over two files).
    let mut segments: Vec<(&str, usize)> = Vec::new();
    let mut bse_seen = false;
    for (file, url) in b.endpoints.master.iter().zip(&b.endpoints.master_urls) {
        let Some(text) = fetch_file(b, file, url).await else {
            failed.push(file.exchange);
            continue;
        };
        if text.trim().is_empty() {
            tracing::warn!(
                broker = b.cfg.id,
                "Master file {} came back empty",
                file.exchange
            );
            failed.push(file.exchange);
            continue;
        }
        let mut parsed = parse_file(b.cfg, file.exchange, &text);
        // Manual BSE index rows are added once even when BSE is split.
        if file.exchange == "BSE" {
            if bse_seen {
                parsed.retain(|r| {
                    r.exchange != "BSE_INDEX" || b.cfg.bse_indices != BseIndices::Manual
                });
            }
            bse_seen = true;
        }
        match segments.iter_mut().find(|(ex, _)| *ex == file.exchange) {
            Some((_, n)) => *n += parsed.len(),
            None => segments.push((file.exchange, parsed.len())),
        }
        rows.extend(parsed);
    }
    if !failed.is_empty() {
        return Err(AppError::Broker(format!(
            "Could not download the {} symbol files for {}. Your existing symbols were kept; try the download again.",
            b.cfg.name,
            unique(&failed).join(", ")
        )));
    }
    if let Some((segment, _)) = segments.iter().find(|(_, n)| *n == 0) {
        return Err(AppError::Broker(format!(
            "The {} {} symbol file had no usable rows. Your existing symbols were kept; try the download again.",
            b.cfg.name, segment
        )));
    }
    Ok(rows)
}
