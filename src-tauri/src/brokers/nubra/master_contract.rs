//! Nubra master contract (web `database/master_contract_db.py`).
//!
//! * Instruments: `GET /refdata/refdata/{YYYY-MM-DD}?exchange={NSE,BSE,MCX}`
//!   with the session headers; rows under `refdata`. An exchange that fails
//!   or sends nothing fails the whole download (the web skips it), so the
//!   stored master is kept rather than replaced by a partial one.
//! * Indices: `GET /public/indexes?format=csv` (no authentication), columns
//!   `EXCHANGE, INDEX_SYMBOL, ZANSKAR_INDEX_SYMBOL, INDEX_NAME`.
//!
//! `token` is Nubra's `ref_id` (what the order API takes). NSE / BSE rows
//! that are not `STOCK` live under NFO / BFO. Expiry (`YYYYMMDD` int) is
//! stored `DD-MMM-YY`; strike and tick size are paise. F&O symbols follow
//! the OpenAlgo format `[asset][DDMMMYY]FUT` / `[asset][DDMMMYY][strike][CE|PE]`.

use super::{refused, NubraBroker, DEVICE_ID};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, future_symbol, option_symbol, rename, split_csv_line, CsvHeader,
};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use serde_json::Value;

/// web `nubra_to_openalgo_index`.
pub const INDEX_RENAMES: &[(&str, &str)] = &[
    ("INDIA_VIX", "INDIAVIX"),
    ("NIFTYALPHA", "NIFTYALPHA50"),
    ("NIFTYCDTY", "NIFTYCOMMODITIES"),
    ("NIFTYCONSUMP", "NIFTYCONSUMPTION"),
    ("NIFTYDIVOPPT", "NIFTYDIVOPPS50"),
    ("NIFTYGSCOMP", "NIFTYGSCOMPSITE"),
    ("NIFTYINFRAST", "NIFTYINFRA"),
    ("LIX15MIDCAP", "NIFTYMIDLIQ15"),
    ("NIFTYMIDCAP", "NIFTYMIDCAP100"),
    ("NIFTYSMALL", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYSMALLCAP50", "NIFTYSMLCAP50"),
    ("NIFTYMIDSMALL400", "NIFTYMIDSML400"),
    ("NIFTYEQWGT", "NIFTY50EQLWGT"),
    ("NIFTY100WEIGHT", "NIFTY100EQLWGT"),
    ("LIQ15", "NIFTY100LIQ15"),
    ("NIFTYLOWVOL", "NIFTY100LOWVOL30"),
    ("NSEQ30", "NIFTY100QUALTY30"),
    ("NIFTY200QLTY30", "NIFTY200QUALTY30"),
    ("NIFTYPR1X", "NIFTY50PR1XINV"),
    ("NIFTYPR2X", "NIFTY50PR2XLEV"),
    ("NIFTYTR1X", "NIFTY50TR1XINV"),
    ("NIFTYTR2X", "NIFTY50TR2XLEV"),
    ("NIFTYV20", "NIFTY50VALUE20"),
    ("NIFTY10YRBMGSEC", "NIFTYGS10YR"),
    ("NIFTY10YRBMSECCP", "NIFTYGS10YRCLN"),
    ("NIFTY11-15YRGSEC", "NIFTYGS1115YR"),
    ("NIFTY15YRABOVEGSEC", "NIFTYGS15YRPLUS"),
    ("NIFTY4-8YRGESC", "NIFTYGS48YR"),
    ("NIFTY8-13YRGSEC", "NIFTYGS813YR"),
    ("NIFTYSERVICE", "NIFTYSERVSECTOR"),
    ("NIFTYPTBNK", "NIFTYPVTBANK"),
    ("NIFTY50DIVPOINT", "NIFTY50DIVPOINT"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("INDSTR", "BSEINDUSTRIALS"),
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

/// Exchanges whose refdata is downloaded.
pub const REFDATA_EXCHANGES: &[&str] = &["NSE", "BSE", "MCX"];

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.to_string(),
        },
        _ => String::new(),
    }
}

fn number(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `YYYYMMDD` (int or string) -> date.
fn expiry_date(v: &Value) -> Option<NaiveDate> {
    let raw = match v.get("expiry")? {
        Value::Number(n) => n.as_i64()?.to_string(),
        Value::String(s) => s.trim().to_string(),
        _ => return None,
    };
    NaiveDate::parse_from_str(&raw, "%Y%m%d").ok()
}

/// One refdata row (web `process_nubra_json`).
pub fn parse_refdata_row(r: &Value) -> Option<SymbolData> {
    let token = text(r, "ref_id");
    if token.is_empty() {
        return None;
    }
    let brexchange = text(r, "exchange");
    let dt = text(r, "derivative_type");
    let opt = text(r, "option_type");
    let exchange = match brexchange.as_str() {
        "NSE" if dt != "STOCK" => "NFO".to_string(),
        "BSE" if dt != "STOCK" => "BFO".to_string(),
        other => other.to_string(),
    };
    let mut kind = String::new();
    if dt == "FUT" {
        kind = "FUT".into();
    }
    if opt == "CE" || opt == "PE" {
        kind = opt.clone();
    }
    if dt == "STOCK" {
        kind = "EQ".into();
    }
    let asset = text(r, "asset");
    let brsymbol = text(r, "stock_name");
    let strike = number(r, "strike_price") / 100.0;
    let fo = matches!(kind.as_str(), "FUT" | "CE" | "PE");
    let expiry = if fo {
        expiry_date(r).map(format_expiry).unwrap_or_default()
    } else {
        String::new()
    };
    let symbol = if fo && !expiry.is_empty() {
        if kind == "FUT" {
            future_symbol(&asset, &expiry)
        } else {
            option_symbol(&asset, &expiry, strike, &kind)
        }
    } else {
        brsymbol.clone()
    };
    Some(SymbolData {
        symbol,
        brsymbol,
        name: asset,
        exchange,
        brexchange,
        token,
        expiry,
        strike,
        lot_size: number(r, "lot_size") as i32,
        instrument_type: kind,
        tick_size: number(r, "tick_size") / 100.0,
    })
}

/// Every refdata row.
pub fn parse_refdata(rows: &[Value]) -> Vec<SymbolData> {
    rows.iter().filter_map(parse_refdata_row).collect()
}

/// The index CSV (web `process_nubra_indexes`).
pub fn parse_indexes(csv: &str) -> Vec<SymbolData> {
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let (Some(ie), Some(is), Some(iz), Some(iname)) = (
        h.index("EXCHANGE"),
        h.index("INDEX_SYMBOL"),
        h.index("ZANSKAR_INDEX_SYMBOL"),
        h.index("INDEX_NAME"),
    ) else {
        tracing::warn!("Nubra index list is missing expected columns");
        return Vec::new();
    };
    lines
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let c = split_csv_line(l);
            let get = |i: usize| c.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
            let brexchange = get(ie);
            let raw = get(is);
            let zanskar = get(iz);
            if zanskar.is_empty() {
                return None;
            }
            let symbol = rename(INDEX_RENAMES, &raw).unwrap_or(&raw).to_string();
            Some(SymbolData {
                symbol,
                brsymbol: zanskar.clone(),
                name: get(iname),
                exchange: format!("{}_INDEX", brexchange),
                brexchange,
                token: zanskar,
                expiry: String::new(),
                strike: 0.0,
                lot_size: 0,
                instrument_type: "INDEX".into(),
                tick_size: 0.05,
            })
        })
        .collect()
}

fn incomplete(exchange: &str) -> AppError {
    AppError::Broker(format!(
        "Nubra did not send its {} instruments. Your existing symbols were kept; try downloading the master contract again shortly.",
        exchange
    ))
}

/// Today's date in IST (the refdata snapshot date).
fn today_ist() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::seconds(19_800)).date_naive()
}

pub async fn download(b: &NubraBroker, auth: &AuthToken) -> Result<Vec<SymbolData>> {
    if auth.raw().trim().is_empty() {
        return Err(super::session_expired());
    }
    let date = today_ist().format("%Y-%m-%d").to_string();
    let mut rows: Vec<Value> = Vec::new();
    for ex in REFDATA_EXCHANGES {
        let url = b.url(&format!("/refdata/refdata/{}?exchange={}", date, ex));
        let resp = b
            .http
            .get(&url)
            .timeout(DOWNLOAD_TIMEOUT)
            .header("Authorization", format!("Bearer {}", auth.raw()))
            .header("Accept", "application/json")
            .header("x-device-id", DEVICE_ID)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == super::SESSION_EXPIRED_STATUS {
            return Err(super::session_expired());
        }
        let bytes = resp.bytes().await?;
        if status != 200 {
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            tracing::warn!(
                status,
                "Nubra instrument list for {} failed: {}",
                ex,
                refused(&v, status)
            );
            return Err(incomplete(ex));
        }
        // All or nothing (MC-02, a hardening over the web, which keeps the
        // exchanges it got): an exchange with no instruments fails the
        // download, so the stored master is kept.
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        match v.get("refdata").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => rows.extend(a.iter().cloned()),
            _ => {
                tracing::warn!("Nubra instrument list for {} had no instruments", ex);
                return Err(incomplete(ex));
            }
        }
    }
    let mut out = parse_refdata(&rows);
    drop(rows);
    let idx = b
        .http
        .get(b.url("/public/indexes?format=csv"))
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = idx.status().as_u16();
    if status != 200 {
        tracing::warn!(status, "Nubra index list download failed");
        return Err(AppError::Broker(
            "Nubra's index list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let csv = idx.text().await?;
    out.extend(parse_indexes(&csv));
    tracing::info!("Nubra master contract parsed: {} rows", out.len());
    Ok(out)
}
