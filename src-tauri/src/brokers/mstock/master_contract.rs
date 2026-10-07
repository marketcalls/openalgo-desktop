//! mStock master contract (web `database/master_contract_db.py`).
//!
//! * `GET {base}/instruments/OpenAPIScripMaster` with the session headers
//!   returns a JSON array (`:91-120`). Columns: API `symbol` -> `name`, API
//!   `name` -> `brsymbol`, `exch_seg` -> `exchange` (and `brexchange`),
//!   plus `token, lotsize, instrumenttype, expiry, strike, tick_size`
//!   (`:299-317`).
//! * NSE / BSE rows of currency instruments become CDS / BCD (brexchange
//!   stays NSE / BSE) (`:324-333`). Equity symbols drop `-EQ` / `-BZ`
//!   (`:336`). Expiry is `DD-MMM-YY` (`convert_date`, `:126-181`).
//! * BSE index tokens listed on the mStock Annexure page turn their BSE rows
//!   into `BSE_INDEX` with OpenAlgo names (`:362-428`); NSE indices are not
//!   in the API and come from the same page (`:200-268`), renamed to
//!   OpenAlgo names and appended unless their token is already present
//!   (`copy_from_dataframe` skips existing tokens). An unreachable page only
//!   leaves the indices out, as on the web.
//! * Derivative symbols: `name + DDMMMYY + FUT` and
//!   `name + DDMMMYY + strike + CE|PE` for NFO, BFO, CDS, BCD and MCX
//!   (`:437-680`); instrument types normalised to FUT / CE / PE
//!   (`:683-700`).
//! * Where two equity rows share an OpenAlgo symbol, `-EQ` is listed before
//!   `-BZ` and `-BE`, so symbol lookups pick it (web `get_mstock_symbol`
//!   priority, `transform_data.py:11-59`).

use super::mapping::s;
use super::{session_expired, MstockBroker, MstockSession};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{expiry_compact, format_strike, rename};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;

pub const MASTER_PATH: &str = "/instruments/OpenAPIScripMaster";

/// NSE index names on the Annexure page -> OpenAlgo symbols (`:228-247`).
pub const NSE_INDEX_RENAMES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
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
    ("HANGSENGBEES-NAV", "HANGSENGBEESNAV"),
];

/// BSE index short names -> OpenAlgo symbols (`:381-421`).
pub const BSE_INDEX_RENAMES: &[(&str, &str)] = &[
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSE CG", "BSECAPITALGOODS"),
    ("BSECG", "BSECAPITALGOODS"),
    ("CARBON", "BSECARBONEX"),
    ("BSE CD", "BSECONSUMERDURABLES"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("GREENX", "BSEGREENEX"),
    ("BSE HC", "BSEHEALTHCARE"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIPO", "BSEIPO"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("BSEPSU", "BSEPSU"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

const OPTION_TYPES: &[&str] = &["OPTIDX", "OPTSTK", "OPTFUT", "OPTCUR", "OPTIRC"];
const FUTURE_TYPES: &[&str] = &["FUTIDX", "FUTSTK", "FUTCOM", "FUTCUR", "FUTIRC", "FUTIRT"];
const CURRENCY_TYPES: &[&str] = &["OPTCUR", "FUTCUR", "OPTIRC", "FUTIRC"];

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

fn fmt_date(d: NaiveDate) -> String {
    format!(
        "{:02}-{}-{:02}",
        d.day(),
        MONTHS[d.month0() as usize],
        d.year() % 100
    )
}

/// web `convert_date`: any mStock expiry encoding -> `DD-MMM-YY`.
/// `19MAR2024`, `19-MAR-2024`, `2024-03-19`, `19-MAR-24`, `19MAR24`;
/// `19-MAR` (six characters) takes `current_year`; anything else is hyphenated
/// (`25DEC24` style) or uppercased, as the web does.
pub fn convert_date(raw: &str, current_year: i32) -> String {
    let d = raw.trim();
    if d.is_empty() {
        return String::new();
    }
    let b = d.as_bytes();
    if d.len() == 9 && b[2] == b'-' && b[6] == b'-' {
        return d.to_ascii_uppercase();
    }
    let up = d.to_ascii_uppercase();
    if d.len() >= 9 {
        if let Ok(x) = NaiveDate::parse_from_str(&up.replace('-', ""), "%d%b%Y") {
            return fmt_date(x);
        }
    }
    if let Ok(x) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
        return fmt_date(x);
    }
    if let Ok(x) = NaiveDate::parse_from_str(&up, "%d-%b-%y") {
        return fmt_date(x);
    }
    if d.len() == 7 {
        if let Ok(x) = NaiveDate::parse_from_str(&up, "%d%b%y") {
            return fmt_date(x);
        }
    }
    if matches!(d.len(), 6 | 7)
        && d.replace('-', "")
            .chars()
            .all(|c| c.is_ascii_alphanumeric())
    {
        let with_year = format!("{}{}", up.replace('-', ""), current_year);
        if let Ok(x) = NaiveDate::parse_from_str(&with_year, "%d%b%Y") {
            return fmt_date(x);
        }
    }
    if !d.contains('-') && d.len() >= 7 && d.is_char_boundary(2) && d.is_char_boundary(5) {
        return format!("{}-{}-{}", &d[..2], &d[2..5], &d[5..]).to_ascii_uppercase();
    }
    up
}

/// Index rows of the Annexure page: `(script, token, name, exchange)` for
/// every `<tr>` of exactly four cells whose second cell is a number (the
/// web's regex, `:207-208`).
pub fn parse_annexure(html: &str) -> Vec<(String, String, String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("<tr") {
        let start = pos + start;
        let Some(end_rel) = lower[start..].find("</tr>") else {
            break;
        };
        let end = start + end_rel;
        pos = end + 5;
        let row = &html[start..end];
        let row_l = &lower[start..end];
        let mut cells = Vec::new();
        let mut p = 0;
        let mut ok = true;
        while let Some(td) = row_l[p..].find("<td") {
            let td = p + td;
            let Some(gt) = row_l[td..].find('>') else {
                ok = false;
                break;
            };
            let content_start = td + gt + 1;
            let Some(close) = row_l[content_start..].find("</td>") else {
                ok = false;
                break;
            };
            let content = &row[content_start..content_start + close];
            if content.contains('<') {
                ok = false;
                break;
            }
            cells.push(content.trim().to_string());
            p = content_start + close + 5;
        }
        if !ok || cells.len() != 4 {
            continue;
        }
        let token = &cells[1];
        if token.is_empty() || !token.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if cells[0].is_empty() || cells[2].is_empty() {
            continue;
        }
        out.push((
            cells[0].clone(),
            token.clone(),
            cells[2].clone(),
            cells[3].to_ascii_uppercase(),
        ));
    }
    out
}

/// NSE index rows from the Annexure (`fetch_and_process_mstock_indices`).
pub fn nse_index_rows(annexure: &[(String, String, String, String)]) -> Vec<SymToken> {
    annexure
        .iter()
        .filter(|(_, _, _, ex)| ex == "NSE")
        .map(|(script, token, name, _)| SymToken {
            symbol: rename(NSE_INDEX_RENAMES, script)
                .map(str::to_string)
                .unwrap_or_else(|| script.clone()),
            brsymbol: name.clone(),
            name: script.clone(),
            exchange: "NSE_INDEX".into(),
            brexchange: "NSE".into(),
            token: token.clone(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "INDEX".into(),
            tick_size: 0.05,
        })
        .collect()
}

fn strip_eq(brsymbol: &str) -> String {
    brsymbol
        .strip_suffix("-EQ")
        .or_else(|| brsymbol.strip_suffix("-BZ"))
        .unwrap_or(brsymbol)
        .to_string()
}

/// One API row -> master row (`process_mstock_json`).
pub fn parse_row(r: &Value, bse_index_tokens: &HashSet<String>, current_year: i32) -> SymToken {
    let name = s(r, "symbol");
    let brsymbol = s(r, "name");
    let instrumenttype = s(r, "instrumenttype");
    let brexchange = s(r, "exch_seg");
    let mut exchange = brexchange.clone();
    if CURRENCY_TYPES.contains(&instrumenttype.as_str()) {
        match exchange.as_str() {
            "NSE" => exchange = "CDS".into(),
            "BSE" => exchange = "BCD".into(),
            _ => {}
        }
    }
    let token = match r.get("token") {
        Some(Value::Number(n)) => n.to_string(),
        _ => s(r, "token"),
    };
    let expiry = convert_date(&s(r, "expiry"), current_year);
    let num_or = |k: &str, default: f64| -> f64 {
        match r.get(k) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
            Some(Value::String(x)) if !x.trim().is_empty() => x.trim().parse().unwrap_or(default),
            _ => default,
        }
    };
    let strike = num_or("strike", 0.0);
    let lot = num_or("lotsize", 1.0) as i32;
    let tick = num_or("tick_size", 0.05);
    let mut symbol = strip_eq(&brsymbol);
    if exchange == "BSE" && bse_index_tokens.contains(&token) {
        exchange = "BSE_INDEX".into();
        symbol = rename(BSE_INDEX_RENAMES, &name)
            .map(str::to_string)
            .unwrap_or_else(|| name.clone());
    }
    let it = instrumenttype.as_str();
    let deriv = matches!(exchange.as_str(), "NFO" | "BFO");
    let ccy = matches!(exchange.as_str(), "CDS" | "BCD");
    let is_future = (deriv && matches!(it, "FUTIDX" | "FUTSTK"))
        || (ccy && matches!(it, "FUTCUR" | "FUTIRC"))
        || (exchange == "MCX" && it == "FUTCOM");
    let is_option = (deriv && matches!(it, "OPTIDX" | "OPTSTK"))
        || (ccy && matches!(it, "OPTCUR" | "OPTIRC"))
        || (exchange == "MCX" && it == "OPTFUT");
    let compact = expiry_compact(&expiry);
    if is_future && !expiry.is_empty() {
        symbol = format!("{}{}FUT", name, compact);
    } else if is_option {
        for ot in ["CE", "PE"] {
            if brsymbol.ends_with(ot) {
                symbol = format!("{}{}{}{}", name, compact, format_strike(strike), ot);
            }
        }
    }
    let instrument_type = if OPTION_TYPES.contains(&it) && symbol.ends_with("CE") {
        "CE".to_string()
    } else if OPTION_TYPES.contains(&it) && symbol.ends_with("PE") {
        "PE".to_string()
    } else if FUTURE_TYPES.contains(&it) {
        "FUT".to_string()
    } else {
        instrumenttype.clone()
    };
    SymToken {
        symbol,
        brsymbol,
        name,
        exchange,
        brexchange,
        token,
        expiry,
        strike,
        lot_size: lot,
        instrument_type,
        tick_size: tick,
    }
}

/// `-EQ` before `-BZ` before `-BE` before anything else.
fn suffix_priority(brsymbol: &str) -> u8 {
    match brsymbol.rsplit_once('-').map(|(_, s)| s) {
        Some("EQ") => 1,
        Some("BZ") => 2,
        Some("BE") => 3,
        _ => 9,
    }
}

/// The whole pipeline over an already-downloaded API array and Annexure
/// page (empty when unreachable).
pub fn build(rows: &[Value], annexure_html: Option<&str>, current_year: i32) -> Vec<SymToken> {
    let annexure = annexure_html.map(parse_annexure).unwrap_or_default();
    let bse_tokens: HashSet<String> = annexure
        .iter()
        .filter(|(_, _, _, ex)| ex == "BSE")
        .map(|(_, t, _, _)| t.clone())
        .collect();
    let mut out: Vec<SymToken> = rows
        .iter()
        .map(|r| parse_row(r, &bse_tokens, current_year))
        .filter(|r| !r.token.is_empty())
        .collect();
    out.sort_by_key(|r| suffix_priority(&r.brsymbol));
    let seen: HashSet<String> = out.iter().map(|r| r.token.clone()).collect();
    out.extend(
        nse_index_rows(&annexure)
            .into_iter()
            .filter(|r| !seen.contains(&r.token)),
    );
    out
}

/// The API rows of a download (a bare array, or `{"data": [...]}`).
fn api_rows(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("data") {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

async fn fetch_annexure(b: &MstockBroker) -> Option<String> {
    let resp = b
        .http
        .get(&b.annexure_url)
        .timeout(Duration::from_secs(30))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        Ok(r) => {
            tracing::warn!(
                status = r.status().as_u16(),
                "mStock index list page unavailable; indices left out"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "mStock index list page unreachable ({}); indices left out",
                e
            );
            None
        }
    }
}

pub async fn download(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<SymToken>> {
    let session = MstockSession::parse(auth)?;
    let resp = b
        .request(Method::GET, MASTER_PATH, &session)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(session_expired());
    }
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "mStock master contract download failed"
        );
        return Err(AppError::Broker(
            "mStock did not send the instrument list. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    let bytes = resp.bytes().await?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| {
        tracing::warn!("mStock master contract is not JSON: {}", e);
        AppError::Broker(
            "mStock sent an instrument list OpenAlgo could not read. Try downloading the master contract again shortly."
                .into(),
        )
    })?;
    let rows = api_rows(v);
    if rows.is_empty() {
        return Err(AppError::Broker(
            "mStock sent an empty instrument list. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    let html = fetch_annexure(b).await;
    let year = chrono::Utc::now().year();
    let out = build(&rows, html.as_deref(), year);
    tracing::info!("mStock master contract: {} instruments", out.len());
    Ok(out)
}
