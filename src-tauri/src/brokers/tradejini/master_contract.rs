//! Master contract (web `database/master_contract_db.py`).
//!
//! `GET {base}/api/mkt-data/scrips/symbol-store?version=0` lists the scrip
//! groups (`d.symbolStore[]` with `name` and `idFormat`); each group is a
//! CSV at `/api/mkt-data/scrips/symbol-store/{group}?version=0` (plain
//! comma split, header row first, rows with a different field count are
//! dropped). Both are unauthenticated.
//!
//! * `Securities` (id `EQT_RELIANCE_EQ_NSE`): symbol = `dispName`, `EQ`.
//! * `FutureContracts` / `CurrencyFuture` / `CommodityFuture`:
//!   `{symbol}{DDMMMYY}FUT`.
//! * `NSEOptions` / `BSEOptions` / `CurrencyOptions` / `CommodityOptions`:
//!   `{symbol}{DDMMMYY}{strike}{CE|PE}`.
//! * `Index`: exchange `NSE_INDEX` / `BSE_INDEX`, symbol renamed by the
//!   tables below (unlisted: uppercased, spaces removed).
//!
//! `brsymbol` is the scrip id (the books and history look it up), `token`
//! is `excToken`, the id components are read by name through `idFormat`.

use super::TradejiniBroker;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{format_expiry, format_strike};
use crate::brokers::types::SymbolData;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One scrip group from the symbol store.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub id_format: String,
}

/// web `nse_index_map` (exact match; unlisted names are uppercased with
/// spaces removed).
pub const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTY", "NIFTY"),
    ("NIFTYNXT50", "NIFTYNXT50"),
    ("FINNIFTY", "FINNIFTY"),
    ("BANKNIFTY", "BANKNIFTY"),
    ("MIDCPNIFTY", "MIDCPNIFTY"),
    ("India VIX", "INDIAVIX"),
    ("Nifty 100", "NIFTY100"),
    ("Nifty 200", "NIFTY200"),
    ("NIFTY 500", "NIFTY500"),
    ("Nifty Auto", "NIFTYAUTO"),
    ("Nifty Commodities", "NIFTYCOMMODITIES"),
    ("Nifty Consumption", "NIFTYCONSUMPTION"),
    ("Nifty Energy", "NIFTYENERGY"),
    ("Nifty FMCG", "NIFTYFMCG"),
    ("NIFTY HEALTHCARE", "NIFTYHEALTHCARE"),
    ("Nifty Infra", "NIFTYINFRA"),
    ("Nifty IT", "NIFTYIT"),
    ("Nifty Media", "NIFTYMEDIA"),
    ("Nifty Metal", "NIFTYMETAL"),
    ("Nifty MNC", "NIFTYMNC"),
    ("NIFTY OIL AND GAS", "NIFTYOILANDGAS"),
    ("Nifty Pharma", "NIFTYPHARMA"),
    ("Nifty PSE", "NIFTYPSE"),
    ("Nifty PSU Bank", "NIFTYPSUBANK"),
    ("Nifty Pvt Bank", "NIFTYPVTBANK"),
    ("Nifty Midcap 50", "NIFTYMIDCAP50"),
    ("NIFTY MIDCAP 100", "NIFTYMIDCAP100"),
    ("NIFTY SMLCAP 50", "NIFTYSMLCAP50"),
    ("NIFTY SMLCAP 100", "NIFTYSMLCAP100"),
    ("NIFTY INDIA MFG", "NIFTYINDIAMFG"),
    ("Nifty MidSml Hlth", "NIFTYMIDSMLHLTH"),
    ("Nifty Tata 25 Cap", "NIFTYTATA25CAP"),
];

/// web `bse_index_map`.
pub const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("SENSEX", "SENSEX"),
    ("BANKEX", "BANKEX"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("SENSEX50", "SENSEX50"),
    ("BSE100", "BSE100"),
    ("BSE200", "BSE200"),
    ("BSE500", "BSE500"),
    ("AUTO", "BSEAUTO"),
    ("BSE HC", "BSEHEALTHCARE"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSEIPO", "BSEIPO"),
    ("BSEPSU", "BSEPSU"),
    ("ENERGY", "BSEENERGY"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("GREENX", "BSEGREENEX"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("SMLCAP", "BSESMALLCAP"),
    ("TELCOM", "BSETELECOM"),
    ("BSEEVI", "BSEEVI"),
    ("BSEPBI", "BSEPBI"),
    ("MFG", "BSEMFG"),
];

/// OpenAlgo index symbol for a Tradejini index symbol.
pub fn index_symbol(exchange: &str, raw: &str) -> String {
    let table = if exchange == "BSE_INDEX" {
        BSE_INDEX_MAP
    } else {
        NSE_INDEX_MAP
    };
    table
        .iter()
        .find(|(k, _)| *k == raw)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| raw.to_uppercase().replace(' ', ""))
}

/// web `parse_expiry`: ids carry ISO dates; compact exchange forms too.
pub fn parse_expiry(v: &str) -> Option<NaiveDate> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        return Some(d);
    }
    let up = v.to_ascii_uppercase();
    // Python's `%Y` takes exactly four digits, chrono's takes any count, so
    // a two-digit year must not satisfy the `%Y` forms.
    let four_digit_year = up
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .map(|y| y.len() == 4)
        .unwrap_or(false);
    let fmts: &[&str] = if four_digit_year {
        &["%d%b%Y", "%d-%b-%Y"]
    } else {
        &["%d%b%y", "%d-%b-%y"]
    };
    fmts.iter()
        .find_map(|fmt| NaiveDate::parse_from_str(&up, fmt).ok())
}

/// web `parse_scrip_id`: components keyed by the group's `idFormat`.
pub fn parse_scrip_id(id: &str, id_format: &str) -> HashMap<String, String> {
    let parts: Vec<&str> = id.split('_').collect();
    let names: Vec<&str> = id_format.split('_').filter(|n| !n.is_empty()).collect();
    if names.is_empty() || parts.len() < names.len() {
        return HashMap::new();
    }
    names
        .iter()
        .zip(parts.iter())
        .map(|(n, p)| (n.to_string(), p.to_string()))
        .collect()
}

/// web `get_scrip_groups` response.
pub fn parse_groups(v: &Value) -> Vec<Group> {
    if v.get("s").and_then(Value::as_str) != Some("ok") {
        return Vec::new();
    }
    v.get("d")
        .and_then(|d| d.get("symbolStore"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|g| {
                    let name = g.get("name").and_then(Value::as_str)?.to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(Group {
                        name,
                        id_format: g
                            .get("idFormat")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn lot(r: &HashMap<&str, &str>) -> i32 {
    r.get("lot")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v as i32)
        .unwrap_or(1)
}

fn tick(r: &HashMap<&str, &str>) -> f64 {
    r.get("tick")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.05)
}

/// Parse one group's CSV into master rows (web `process_scrip_data`).
pub fn parse_group(csv: &str, group: &Group) -> Vec<SymbolData> {
    let mut lines = csv.trim().lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let headers: Vec<&str> = header.trim().split(',').collect();
    let mut out = Vec::new();
    for line in lines {
        let values: Vec<&str> = line.trim().split(',').collect();
        if values.len() != headers.len() {
            continue;
        }
        let r: HashMap<&str, &str> = headers.iter().copied().zip(values).collect();
        if let Some(row) = parse_row(&r, group) {
            out.push(row);
        }
    }
    out
}

fn parse_row(r: &HashMap<&str, &str>, group: &Group) -> Option<SymbolData> {
    let get = |k: &str| r.get(k).copied().unwrap_or("").trim().to_string();
    let id = get("id");
    let token = get("excToken");
    if id.is_empty() {
        return None;
    }
    let parts: Vec<&str> = id.split('_').collect();
    if group.name == "Index" {
        if parts.len() < 3 {
            return None;
        }
        let raw_exchange = parts[parts.len() - 1];
        let exchange = match raw_exchange {
            "NSE" => "NSE_INDEX",
            "BSE" => "BSE_INDEX",
            other => other,
        };
        return Some(SymbolData {
            symbol: index_symbol(exchange, &get("symbol")),
            brsymbol: id.clone(),
            name: get("dispName"),
            exchange: exchange.to_string(),
            brexchange: raw_exchange.to_string(),
            token,
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "INDEX".into(),
            tick_size: 0.05,
        });
    }
    if id.to_ascii_lowercase().contains("spot") || get("asset") == "spot" || parts.len() < 2 {
        return None;
    }
    let c = parse_scrip_id(&id, &group.id_format);
    let comp = |k: &str, item: &str, idx: usize| -> String {
        c.get(k)
            .filter(|v| !v.is_empty())
            .cloned()
            .or_else(|| Some(get(item)).filter(|v| !v.is_empty()))
            .or_else(|| parts.get(idx).map(|s| s.to_string()))
            .unwrap_or_default()
    };
    match group.name.as_str() {
        "Securities" => {
            if parts.len() < 4 {
                return None;
            }
            let exchange = c
                .get("exchange")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| parts[parts.len() - 1].to_string());
            let disp = get("dispName");
            let desc = get("desc");
            Some(SymbolData {
                symbol: disp.clone(),
                brsymbol: id.clone(),
                name: if desc.is_empty() { disp } else { desc },
                exchange: exchange.clone(),
                brexchange: exchange,
                token,
                expiry: String::new(),
                strike: 0.0,
                lot_size: lot(r),
                instrument_type: "EQ".into(),
                tick_size: tick(r),
            })
        }
        "FutureContracts" | "CurrencyFuture" | "CommodityFuture" => {
            let base = comp("symbol", "symbol", 1);
            let exchange = comp("exchange", "", 2);
            let expiry = parse_expiry(&comp("expiry", "expiry", 3))?;
            if base.is_empty() || exchange.is_empty() {
                return None;
            }
            let exp = format_expiry(expiry);
            Some(SymbolData {
                symbol: format!("{}{}FUT", base, exp.replace('-', "")),
                brsymbol: id.clone(),
                name: base,
                exchange: exchange.clone(),
                brexchange: exchange,
                token,
                expiry: exp,
                strike: 0.0,
                lot_size: lot(r),
                instrument_type: "FUT".into(),
                tick_size: tick(r),
            })
        }
        "NSEOptions" | "BSEOptions" | "CurrencyOptions" | "CommodityOptions" => {
            let base = comp("symbol", "symbol", 1);
            let exchange = comp("exchange", "", 2);
            let expiry = parse_expiry(&comp("expiry", "expiry", 3))?;
            let strike: f64 = comp("strike", "strike", 4).parse().ok()?;
            let opt = comp("optType", "optType", 5);
            if base.is_empty() || exchange.is_empty() || opt.is_empty() {
                return None;
            }
            let exp = format_expiry(expiry);
            Some(SymbolData {
                symbol: format!(
                    "{}{}{}{}",
                    base,
                    exp.replace('-', ""),
                    format_strike(strike),
                    opt
                ),
                brsymbol: id.clone(),
                name: base,
                exchange: exchange.clone(),
                brexchange: exchange,
                token,
                expiry: exp,
                strike,
                lot_size: lot(r),
                instrument_type: opt,
                tick_size: tick(r),
            })
        }
        _ => None,
    }
}

async fn get_text(b: &TradejiniBroker, url: &str) -> Result<String> {
    let resp = b
        .http
        .get(url)
        .query(&[("version", "0")])
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Tradejini symbol store refused"
        );
        return Err(AppError::Broker(
            "Tradejini did not send the instrument list. Try the download again shortly.".into(),
        ));
    }
    Ok(resp.text().await?)
}

/// Rows a group's CSV carries, parsed or not: lines after the header with
/// the header's field count (web `get_scrip_data`).
pub fn sent_rows(csv: &str) -> usize {
    let mut lines = csv.trim().lines();
    let Some(header) = lines.next() else {
        return 0;
    };
    let fields = header.trim().split(',').count();
    lines
        .filter(|l| l.trim().split(',').count() == fields)
        .count()
}

/// One group's rows (web #2198). A group that sent rows of which none
/// parsed fails the download, since replacing the master then would
/// silently drop that group's symbols; a group that sent nothing yields no
/// rows and is skipped.
pub fn group_rows(csv: &str, group: &Group) -> Result<Vec<SymbolData>> {
    let parsed = parse_group(csv, group);
    if parsed.is_empty() && sent_rows(csv) > 0 {
        return Err(AppError::Broker(format!(
            "Tradejini returned no usable symbols for {}. Your existing symbols were kept; try the download again.",
            group.name
        )));
    }
    Ok(parsed)
}

/// The first row for each token across groups (web `drop_duplicates(
/// subset=["token"], keep="first")`), keyed by exchange as well so that two
/// exchanges' independent token numbers never drop each other's
/// instruments.
pub fn first_per_token(rows: Vec<SymbolData>) -> Vec<SymbolData> {
    let mut seen: HashSet<(String, String)> = HashSet::with_capacity(rows.len());
    rows.into_iter()
        .filter(|r| seen.insert((r.exchange.clone(), r.token.clone())))
        .collect()
}

/// Download every group before anything is replaced (web #2198): a group
/// that fails, or sends rows none of which parse, fails the download so
/// the stored master is kept (the service swaps the table in one
/// transaction); a group with nothing in it is skipped. No group list, or
/// no usable row at all, is an error.
pub async fn download(b: &TradejiniBroker) -> Result<Vec<SymbolData>> {
    let base = format!("{}/api/mkt-data/scrips/symbol-store", b.base_url);
    let text = get_text(b, &base).await?;
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        AppError::Broker("Tradejini sent an instrument list OpenAlgo could not read.".into())
    })?;
    let groups = parse_groups(&v);
    if groups.is_empty() {
        return Err(AppError::Broker(
            "Tradejini returned no instrument groups. Try the download again shortly.".into(),
        ));
    }
    let mut rows = Vec::new();
    for g in &groups {
        let csv = get_text(b, &format!("{}/{}", base, g.name))
            .await
            .map_err(|e| {
                tracing::warn!("Tradejini master group {} failed: {}", g.name, e.code());
                AppError::Broker(format!(
                    "Could not download the Tradejini {} symbols. Your existing symbols were kept; try the download again.",
                    g.name
                ))
            })?;
        let parsed = group_rows(&csv, g)?;
        if parsed.is_empty() {
            tracing::warn!("Tradejini master group {} has no symbols", g.name);
            continue;
        }
        tracing::info!("Tradejini master: {} rows from {}", parsed.len(), g.name);
        rows.extend(parsed);
    }
    if rows.iter().all(|r| r.token.is_empty()) {
        return Err(AppError::Broker(
            "Tradejini returned no usable symbols. Your existing symbols were kept; try the download again."
                .into(),
        ));
    }
    Ok(first_per_token(rows))
}
