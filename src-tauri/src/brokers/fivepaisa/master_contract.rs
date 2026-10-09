//! Scrip master (web `database/master_contract_db.py`).
//!
//! `GET https://openapi.5paisa.com/VendorsAPI/Service1.svc/ScripMaster/segment/all`
//! (unauthenticated CSV). Columns used: `Exch, ExchType, ScripCode, Name,
//! Expiry, ScripType, StrikeRate, SymbolRoot, Series, LotSize, TickSize`.
//!
//! * exchange from `(Exch, ExchType)`; `(N|B, C)` with `ScripCode > 999900`
//!   is `NSE_INDEX` / `BSE_INDEX`.
//! * rows kept when `Series` is `EQ`, `BE`, `XX` or two spaces; `XX` and
//!   blank series are replaced by `ScripType` (`XX`, `CE`, `PE`).
//! * symbol: EQ/BE `SymbolRoot`; XX `root+DDMMMYY+FUT`; CE/PE
//!   `root+DDMMMYY+strike+CE|PE`; `instrumenttype` XX -> FUT, BE -> EQ
//!   (trade-for-trade stocks are still equity; web #2195).
//! * one row per (symbol, exchange): the master lists some contracts under
//!   several ScripCodes identical in every other column (BFO ITC around the
//!   ITC Hotels demerger, long-dated SENSEX strikes); the highest ScripCode
//!   (newest listing) is kept. Index rows keep the first one, as before.
//! * `brsymbol = Name` uppercased and right-trimmed; `token = ScripCode`;
//!   `brexchange = exchange`.
//! * index symbols uppercased with spaces and hyphens removed, then renamed;
//!   `name` is `SymbolRoot` for derivatives, else the symbol; duplicate
//!   index symbols keep the first row, and index rows follow the others.

use super::FivepaisaBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, parse_broker_expiry, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use std::collections::{HashMap, HashSet};

/// web index rename map (applied after the cleanup), index exchanges only.
pub const INDEX_RENAMES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYFINANCIALSERVICES", "FINNIFTY"),
    ("NIFTYFIN", "FINNIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYMIDCAPSELECT", "MIDCPNIFTY"),
    ("NIFTYMCAP50", "NIFTYMIDCAP50"),
    ("NIFTYMIDSMALLCAP400", "NIFTYMIDSML400"),
    ("NIFTYSMALLCAP100", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYSMALLCAP50", "NIFTYSMLCAP50"),
    ("NIFTY100EQUALWEIGHT", "NIFTY100EQLWGT"),
    ("NIFTY100LOWVOLATILITY30", "NIFTY100LOWVOL30"),
    ("NIFTYMID100FREE", "NIFTYMIDCAP100"),
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("ENERGY", "BSEENERGY"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("LRGCAP", "BSELARGECAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("TELCOM", "BSETELECOM"),
    ("BSESENSEX50", "SENSEX50"),
    ("BSEBANKEX", "BANKEX"),
    ("BSEAUTO", "BSEAUTO"),
    ("BSECAPGOOD", "BSECAPITALGOODS"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSECARBON", "BSECARBONEX"),
    ("BSECONSDUR", "BSECONSUMERDURABLES"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("BSECPSE", "BSECPSE"),
    ("BSEDOL100", "BSEDOLLEX100"),
    ("BSEDOL200", "BSEDOLLEX200"),
    ("BSEDOL30", "BSEDOLLEX30"),
    ("BSEFMCG", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSEGREENX", "BSEGREENEX"),
    ("BSEHEALTHC", "BSEHEALTHCARE"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("BSEINDIA150", "BSE150MIDCAPINDEX"),
    ("BSEINFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIPO", "BSEIPO"),
    ("BSEMETAL", "BSEMETAL"),
    ("BSEMIDCAP", "BSEMIDCAP"),
    ("BSEOIL&GAS", "BSEOIL&GAS"),
    ("BSEPOWER", "BSEPOWER"),
    ("BSEPSU", "BSEPSU"),
    ("BSEPBI", "BSEPSU"),
    ("BSEREALTY", "BSEREALTY"),
    ("BSESMLCAP", "BSESMALLCAP"),
    ("BSESMEIPO", "BSESMEIPO"),
    ("BSETECK", "BSETECK"),
    ("BSEPSUBANK", "BSEPSU"),
];

/// OpenAlgo exchange of a master row.
pub fn row_exchange(exch: &str, exch_type: &str, scrip_code: i64) -> Option<&'static str> {
    match (exch, exch_type) {
        ("N", "C") => Some(if scrip_code > 999_900 {
            "NSE_INDEX"
        } else {
            "NSE"
        }),
        ("B", "C") => Some(if scrip_code > 999_900 {
            "BSE_INDEX"
        } else {
            "BSE"
        }),
        _ => super::mapping::reverse_exchange(exch, exch_type),
    }
}

/// Index symbol cleanup and rename.
pub fn index_symbol(raw: &str) -> String {
    let cleaned: String = raw
        .to_ascii_uppercase()
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .collect();
    INDEX_RENAMES
        .iter()
        .find(|(from, _)| *from == cleaned)
        .map(|(_, to)| to.to_string())
        .unwrap_or(cleaned)
}

/// (symbol, exchange).
type RowKey = (String, String);

/// Parse the whole CSV.
pub fn parse_csv(text: &str) -> Vec<SymToken> {
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let col = |n: &str| h.index(n);
    let (
        Some(i_exch),
        Some(i_type),
        Some(i_code),
        Some(i_name),
        Some(i_exp),
        Some(i_stype),
        Some(i_strike),
        Some(i_root),
        Some(i_series),
        Some(i_lot),
        Some(i_tick),
    ) = (
        col("Exch"),
        col("ExchType"),
        col("ScripCode"),
        col("Name"),
        col("Expiry"),
        col("ScripType"),
        col("StrikeRate"),
        col("SymbolRoot"),
        col("Series"),
        col("LotSize"),
        col("TickSize"),
    )
    else {
        tracing::warn!(
            broker = "fivepaisa",
            "Scrip master header is missing columns"
        );
        return Vec::new();
    };
    let mut rows: Vec<SymToken> = Vec::new();
    // (symbol, exchange) -> (position in `rows`, ScripCode kept there).
    let mut kept: HashMap<RowKey, (usize, i64)> = HashMap::new();
    let mut duplicates = 0usize;
    let mut index_rows = Vec::new();
    let mut seen_index: HashSet<(String, String)> = HashSet::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).map(String::as_str).unwrap_or("");
        let code: i64 = match get(i_code).trim().parse::<f64>() {
            Ok(c) => c as i64,
            Err(_) => continue,
        };
        let Some(exchange) = row_exchange(get(i_exch).trim(), get(i_type).trim(), code) else {
            continue;
        };
        let raw_series = get(i_series);
        if !matches!(raw_series, "EQ" | "BE" | "XX" | "  ") {
            continue;
        }
        let series = if matches!(raw_series, "XX" | "  ") {
            get(i_stype).trim().to_string()
        } else {
            raw_series.to_string()
        };
        let expiry = parse_broker_expiry(get(i_exp))
            .map(format_expiry)
            .unwrap_or_default();
        let compact = expiry.replace('-', "");
        let strike: f64 = get(i_strike).trim().parse().unwrap_or(0.0);
        let root = get(i_root).trim().to_string();
        let mut symbol = match series.as_str() {
            "EQ" | "BE" => root.clone(),
            "XX" => format!("{}{}FUT", root, compact),
            "CE" | "PE" => format!("{}{}{}{}", root, compact, format_strike(strike), series),
            _ => root.clone(),
        };
        let is_index = exchange.ends_with("_INDEX");
        if is_index {
            symbol = index_symbol(&symbol);
        }
        let instrument_type = match series.as_str() {
            "XX" => "FUT".to_string(),
            "BE" => "EQ".to_string(),
            _ => series.clone(),
        };
        let name = if matches!(instrument_type.as_str(), "CE" | "PE" | "FUT") && !root.is_empty() {
            root.clone()
        } else {
            symbol.clone()
        };
        let row = SymToken {
            symbol,
            brsymbol: get(i_name).to_ascii_uppercase().trim_end().to_string(),
            name,
            exchange: exchange.to_string(),
            brexchange: exchange.to_string(),
            token: code.to_string(),
            expiry,
            strike,
            lot_size: get(i_lot).trim().parse::<f64>().unwrap_or(0.0) as i32,
            instrument_type,
            tick_size: get(i_tick).trim().parse().unwrap_or(0.0),
        };
        if is_index {
            if seen_index.insert((row.symbol.clone(), row.exchange.clone())) {
                index_rows.push(row);
            }
        } else {
            let k = (row.symbol.clone(), row.exchange.clone());
            match kept.get_mut(&k) {
                Some((at, best)) => {
                    duplicates += 1;
                    if code > *best {
                        *best = code;
                        rows[*at] = row;
                    }
                }
                None => {
                    kept.insert(k, (rows.len(), code));
                    rows.push(row);
                }
            }
        }
    }
    if duplicates > 0 {
        tracing::info!(
            broker = "fivepaisa",
            "Dropped {} duplicate (symbol, exchange) rows, kept the highest ScripCode",
            duplicates
        );
    }
    rows.extend(index_rows);
    rows
}

/// Download with up to three attempts on a timeout (web).
pub async fn download(b: &FivepaisaBroker) -> Result<Vec<SymToken>> {
    let mut attempt = 0;
    let text = loop {
        attempt += 1;
        let res = async {
            let r = b
                .http
                .get(&b.master_url)
                .timeout(DOWNLOAD_TIMEOUT)
                .send()
                .await?;
            if !r.status().is_success() {
                tracing::warn!(broker = "fivepaisa", "Scrip master answered {}", r.status());
                return Err(AppError::Broker(
                    "5paisa did not send the master contract. Try the download again shortly."
                        .into(),
                ));
            }
            Ok(r.text().await?)
        }
        .await;
        match res {
            Ok(t) => break t,
            Err(AppError::Http(e)) if e.is_timeout() && attempt < 3 => {
                tracing::info!(broker = "fivepaisa", "Scrip master timed out, retrying");
            }
            Err(AppError::Http(e)) if e.is_timeout() => {
                return Err(AppError::Broker(
                    "The 5paisa master contract download timed out. Try again shortly.".into(),
                ))
            }
            Err(e) => return Err(e),
        }
    };
    let rows = parse_csv(&text);
    if rows.is_empty() {
        return Err(AppError::Broker(
            "The 5paisa master contract was empty. Try the download again shortly.".into(),
        ));
    }
    Ok(rows)
}
