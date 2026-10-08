//! Master contract (web `database/master_contract_db.py`).
//!
//! Eight CSVs under `contract_master/V2/`: `NSE, BSE, NFO, CDS, MCX, BFO,
//! BCD` (columns `Exch, Exchange Segment, Symbol, Token, Instrument Type,
//! Option Type, Strike Price, Instrument Name, Formatted Ins Name, Trading
//! Symbol, Expiry Date, Lot Size, Tick Size, Group Name`) and `INDICES`
//! (`symbol, exch, token`). `brexchange` is `Exch`; the token is the
//! integer form of `Token`.
//!
//! Symbols follow the web exactly: equities use `Symbol`; NFO/CDS/BCD
//! futures are `Trading Symbol + "UT"` (AliceBlue's trading symbol ends in
//! `F`, so `NIFTY24MARF` becomes `NIFTY24MARFUT`), MCX futures `Trading
//! Symbol + "FUT"`, options `Symbol + DDMMMYY + strike + CE|PE`, and BFO
//! uses `Formatted Ins Name` without spaces.

use super::AliceBlueBroker;
use crate::brokers::common::http;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, parse_broker_expiry, split_csv_line, CsvHeader,
};
use crate::brokers::common::redact;
use crate::brokers::types::SymbolData;
use crate::error::{AppError, Result};
use chrono::NaiveDate;

/// Files in the order the web loads them.
pub const FILES: &[&str] = &["NSE", "BSE", "NFO", "CDS", "MCX", "BFO", "BCD", "INDICES"];

/// web `_INDEX_SYMBOL_ALIASES`.
pub const INDEX_ALIASES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYMIDCAPSELECT", "MIDCPNIFTY"),
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSE CG", "BSECAPITALGOODS"),
    ("CARBON", "BSECARBONEX"),
    ("BSE CD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("GREENX", "BSEGREENEX"),
    ("BSE HC", "BSEHEALTHCARE"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

/// BSE indices that keep their bare name.
const KEEP_AS_IS: &[&str] = &["SENSEX", "BANKEX", "SENSEX50"];

fn alias(s: &str) -> String {
    INDEX_ALIASES
        .iter()
        .find(|(k, _)| *k == s)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| s.to_string())
}

/// web index symbol: alias, strip spaces, alias again, then prefix
/// undocumented BSE codes with `BSE`.
pub fn index_symbol(raw: &str, exchange: &str) -> String {
    let sym = alias(&alias(raw).replace(' ', ""));
    if exchange != "BSE_INDEX" || KEEP_AS_IS.contains(&sym.as_str()) || sym.starts_with("BSE") {
        sym
    } else {
        format!("BSE{}", sym)
    }
}

/// web `_clean_tokens`: integer string, or empty when not numeric.
pub fn clean_token(raw: &str) -> String {
    match raw.trim().parse::<f64>() {
        Ok(f) if f.is_finite() => format!("{}", f.trunc() as i64),
        _ => String::new(),
    }
}

/// `pd.to_datetime` on the `Expiry Date` column.
pub fn parse_expiry(raw: &str) -> Option<NaiveDate> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    parse_broker_expiry(s).or_else(|| {
        ["%m/%d/%Y", "%d/%m/%Y", "%Y/%m/%d", "%d-%m-%Y"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(s.split(' ').next().unwrap_or(s), f).ok())
    })
}

struct Cols {
    exch: Option<usize>,
    segment: Option<usize>,
    symbol: Option<usize>,
    token: Option<usize>,
    inst_type: Option<usize>,
    opt_type: Option<usize>,
    strike: Option<usize>,
    inst_name: Option<usize>,
    formatted: Option<usize>,
    trading: Option<usize>,
    expiry: Option<usize>,
    lot: Option<usize>,
    tick: Option<usize>,
    group: Option<usize>,
}

impl Cols {
    fn new(h: &CsvHeader) -> Self {
        Self {
            exch: h.index("Exch"),
            segment: h.index("Exchange Segment"),
            symbol: h.index("Symbol"),
            token: h.index("Token"),
            inst_type: h.index("Instrument Type"),
            opt_type: h.index("Option Type"),
            strike: h.index("Strike Price"),
            inst_name: h.index("Instrument Name"),
            formatted: h.index("Formatted Ins Name"),
            trading: h.index("Trading Symbol"),
            expiry: h.index("Expiry Date"),
            lot: h.index("Lot Size"),
            tick: h.index("Tick Size"),
            group: h.index("Group Name"),
        }
    }
}

fn get(f: &[String], i: Option<usize>) -> &str {
    i.and_then(|i| f.get(i)).map(|s| s.trim()).unwrap_or("")
}

fn f64_of(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

#[allow(clippy::too_many_arguments)]
fn row(
    symbol: String,
    brsymbol: &str,
    name: &str,
    exchange: &str,
    token: &str,
    expiry: String,
    strike: f64,
    lot: i32,
    itype: &str,
    tick: f64,
) -> SymbolData {
    SymbolData {
        symbol,
        brsymbol: brsymbol.to_string(),
        name: name.to_string(),
        exchange: exchange.to_string(),
        brexchange: exchange.to_string(),
        token: clean_token(token),
        expiry,
        strike,
        lot_size: lot,
        instrument_type: itype.to_string(),
        tick_size: tick,
    }
}

fn lot_of(s: &str) -> i32 {
    f64_of(s).map(|x| x as i32).unwrap_or(0)
}

/// Parse one exchange CSV (`file` is the CSV name, e.g. `NFO`).
pub fn parse_exchange_csv(file: &str, csv: &str) -> Vec<SymbolData> {
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let c = Cols::new(&CsvHeader::parse(head));
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let exch = get(&f, c.exch);
        let trading = get(&f, c.trading);
        let name = get(&f, c.inst_name);
        let token = get(&f, c.token);
        let lot = lot_of(get(&f, c.lot));
        let tick = f64_of(get(&f, c.tick)).unwrap_or(0.0);
        match file {
            "NSE" | "BSE" => {
                if file == "NSE" && !matches!(get(&f, c.group), "EQ" | "BE") {
                    continue;
                }
                if file == "BSE" && trading.is_empty() {
                    continue;
                }
                let symbol = get(&f, c.symbol);
                if symbol.is_empty() {
                    continue;
                }
                out.push(row(
                    symbol.to_string(),
                    trading,
                    name,
                    exch,
                    token,
                    String::new(),
                    1.0,
                    lot,
                    "EQ",
                    tick,
                ));
            }
            _ => {
                if file == "MCX" && get(&f, c.segment) == "mcx_idx" {
                    continue;
                }
                let itype = get(&f, c.inst_type);
                let mut opt = get(&f, c.opt_type).to_string();
                let fut_types: &[&str] = match file {
                    "NFO" => &["FUTSTK", "FUTIDX"],
                    "CDS" | "BCD" => &["FUTCUR"],
                    "MCX" => &["FUTCOM", "FUTIDX"],
                    "BFO" => &["SF", "IF"],
                    _ => &[],
                };
                if fut_types.contains(&itype) {
                    opt = "XX".into();
                }
                let mut strike = f64_of(get(&f, c.strike));
                if file == "BCD" && itype == "FUTCUR" {
                    strike = Some(1.0);
                }
                let expiry = parse_expiry(get(&f, c.expiry));
                let compact = expiry
                    .map(|d| format_expiry(d).replace('-', ""))
                    .unwrap_or_else(|| "NOEXP".to_string());
                let base = get(&f, c.symbol);
                let option_sym = |kind: &str| {
                    format!(
                        "{}{}{}{}",
                        base,
                        compact,
                        format_strike(strike.unwrap_or(f64::NAN)),
                        kind
                    )
                };
                let symbol = match (file, opt.as_str()) {
                    ("BFO", "XX" | "CE" | "PE") => {
                        let s = get(&f, c.formatted).replace(' ', "");
                        if s.is_empty() {
                            continue;
                        }
                        s
                    }
                    ("MCX", "XX") => format!("{}FUT", trading),
                    (_, "XX") => format!("{}UT", trading),
                    (_, "CE") => option_sym("CE"),
                    (_, "PE") => option_sym("PE"),
                    _ => continue,
                };
                let itype_oa = match opt.as_str() {
                    "XX" => "FUT",
                    "CE" => "CE",
                    _ => "PE",
                };
                out.push(row(
                    symbol,
                    trading,
                    name,
                    exch,
                    token,
                    expiry.map(format_expiry).unwrap_or_default(),
                    strike.unwrap_or(0.0),
                    lot,
                    itype_oa,
                    tick,
                ));
            }
        }
    }
    out
}

/// Parse `INDICES.csv` (`symbol, exch, token`).
pub fn parse_indices_csv(csv: &str) -> Vec<SymbolData> {
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let (si, ei, ti) = (h.index("symbol"), h.index("exch"), h.index("token"));
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let raw = get(&f, si);
        let exch = get(&f, ei);
        let exchange = match exch {
            "NSE" => "NSE_INDEX",
            "BSE" => "BSE_INDEX",
            "MCX" => "MCX_INDEX",
            _ => continue,
        };
        if raw.is_empty() {
            continue;
        }
        out.push(SymbolData {
            symbol: index_symbol(raw, exchange),
            brsymbol: raw.to_string(),
            name: raw.to_string(),
            exchange: exchange.to_string(),
            brexchange: exch.to_string(),
            token: clean_token(get(&f, ti)),
            expiry: String::new(),
            strike: 1.0,
            lot_size: 1,
            instrument_type: exchange.to_string(),
            tick_size: 0.01,
        });
    }
    out
}

/// Parse every file, in the web's load order.
pub fn parse_all(files: &[(&str, String)]) -> Vec<SymbolData> {
    let mut out = Vec::new();
    for (name, body) in files {
        if *name == "INDICES" {
            out.extend(parse_indices_csv(body));
        } else {
            out.extend(parse_exchange_csv(name, body));
        }
    }
    out
}

pub async fn download(b: &AliceBlueBroker) -> Result<Vec<SymbolData>> {
    let mut files = Vec::with_capacity(FILES.len());
    for name in FILES {
        let url = format!("{}/{}.csv", b.ep.master.trim_end_matches('/'), name);
        let resp = b
            .http
            .get(&url)
            .timeout(http::DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(redact::http)?;
        if !resp.status().is_success() {
            tracing::warn!(
                status = resp.status().as_u16(),
                "AliceBlue master contract file {} could not be downloaded",
                name
            );
            return Err(AppError::Broker(
                "AliceBlue's instrument list could not be downloaded. Try downloading the master contract again shortly."
                    .into(),
            ));
        }
        let body = resp.text().await.map_err(redact::http)?;
        files.push((*name, body));
    }
    let rows = parse_all(&files);
    drop(files);
    if rows.is_empty() {
        return Err(AppError::Broker(
            "AliceBlue's instrument list was empty. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    Ok(rows)
}
