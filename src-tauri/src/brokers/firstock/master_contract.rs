//! Firstock master contract (web `database/master_contract_db.py`): CSVs
//! at `/V1/symbols/{NSE,BSE,NFO,BFO}` plus the authenticated
//! `/V1/indexList` (indices are no longer in the CSVs).

use super::{session, FirstockBroker};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    future_symbol, option_symbol, split_csv_line, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::families::noren::mapping::text;
use crate::brokers::families::noren::master_contract::expiry;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const FILES: &[&str] = &["NSE", "BSE", "NFO", "BFO"];

pub const NSE_INDICES: &[&str] = &[
    "NIFTY",
    "NIFTYNXT50",
    "FINNIFTY",
    "BANKNIFTY",
    "MIDCPNIFTY",
    "INDIAVIX",
    "HANGSENGBEESNAV",
    "NIFTY100",
    "NIFTY200",
    "NIFTY500",
    "NIFTYALPHA50",
    "NIFTYAUTO",
    "NIFTYCOMMODITIES",
    "NIFTYCONSUMPTION",
    "NIFTYCPSE",
    "NIFTYDIVOPPS50",
    "NIFTYENERGY",
    "NIFTYFMCG",
    "NIFTYGROWSECT15",
    "NIFTYGS10YR",
    "NIFTYGS10YRCLN",
    "NIFTYGS1115YR",
    "NIFTYGS15YRPLUS",
    "NIFTYGS48YR",
    "NIFTYGS813YR",
    "NIFTYGSCOMPSITE",
    "NIFTYINFRA",
    "NIFTYIT",
    "NIFTYMEDIA",
    "NIFTYMETAL",
    "NIFTYMIDLIQ15",
    "NIFTYMIDCAP100",
    "NIFTYMIDCAP150",
    "NIFTYMIDCAP50",
    "NIFTYMIDSML400",
    "NIFTYMNC",
    "NIFTYPHARMA",
    "NIFTYPSE",
    "NIFTYPSUBANK",
    "NIFTYPVTBANK",
    "NIFTYREALTY",
    "NIFTYSERVSECTOR",
    "NIFTYSMLCAP100",
    "NIFTYSMLCAP250",
    "NIFTYSMLCAP50",
    "NIFTY100EQLWGT",
    "NIFTY100LIQ15",
    "NIFTY100LOWVOL30",
    "NIFTY100QUALTY30",
    "NIFTY200QUALTY30",
    "NIFTY50DIVPOINT",
    "NIFTY50EQLWGT",
    "NIFTY50PR1XINV",
    "NIFTY50PR2XLEV",
    "NIFTY50TR1XINV",
    "NIFTY50TR2XLEV",
    "NIFTY50VALUE20",
];

pub const BSE_INDICES: &[&str] = &[
    "SENSEX",
    "BANKEX",
    "SENSEX50",
    "BSE100",
    "BSE150MIDCAPINDEX",
    "BSE200",
    "BSE250LARGEMIDCAPINDEX",
    "BSE400MIDSMALLCAPINDEX",
    "BSE500",
    "BSEAUTO",
    "BSECAPITALGOODS",
    "BSECARBONEX",
    "BSECONSUMERDURABLES",
    "BSECPSE",
    "BSEDOLLEX100",
    "BSEDOLLEX200",
    "BSEDOLLEX30",
    "BSEENERGY",
    "BSEFASTMOVINGCONSUMERGOODS",
    "BSEFINANCIALSERVICES",
    "BSEGREENEX",
    "BSEHEALTHCARE",
    "BSEINDIAINFRASTRUCTUREINDEX",
    "BSEINDUSTRIALS",
    "BSEINFORMATIONTECHNOLOGY",
    "BSEIPO",
    "BSELARGECAP",
    "BSEMETAL",
    "BSEMIDCAP",
    "BSEMIDCAPSELECTINDEX",
    "BSEOIL&GAS",
    "BSEPOWER",
    "BSEPSU",
    "BSEREALTY",
    "BSESENSEXNEXT50",
    "BSESMALLCAP",
    "BSESMALLCAPSELECTINDEX",
    "BSESMEIPO",
    "BSETECK",
    "BSETELECOM",
];

pub const ALIASES: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    // Not in the web table (it falls through as NIFTYBANK there); added so
    // the bank index resolves to the OpenAlgo symbol.
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYFINSERV", "FINNIFTY"),
    ("NIFTYFINANCIALSERVICES", "FINNIFTY"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("NIFTYSMALLCAP50", "NIFTYSMLCAP50"),
    ("NIFTYSMALLCAP100", "NIFTYSMLCAP100"),
    ("NIFTYSMALLCAP250", "NIFTYSMLCAP250"),
    ("NIFTYINFRASTRUCTURE", "NIFTYINFRA"),
    ("SPBSESENSEX", "SENSEX"),
    ("BSESENSEX", "SENSEX"),
    ("SPBSEBANKEX", "BANKEX"),
    ("SPBSESENSEX50", "SENSEX50"),
    ("BSESENSEX50", "SENSEX50"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEFMCG", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSECDGS", "BSECONSUMERDURABLES"),
    ("BSECG", "BSECAPITALGOODS"),
];

/// Tolerant key: uppercase, no spaces, hyphens, underscores, `&`, `AND`.
fn norm(s: &str) -> String {
    s.to_ascii_uppercase()
        .replace([' ', '-', '_', '&'], "")
        .replace("AND", "")
}

/// OpenAlgo symbol for an index (web `map_to_openalgo_index_symbol`).
pub fn index_symbol(tsym: &str, idxname: &str, exchange: &str) -> String {
    let canon: &[&str] = match exchange {
        "NSE" => NSE_INDICES,
        "BSE" => BSE_INDICES,
        _ => &[],
    };
    if canon.contains(&tsym) {
        return tsym.to_string();
    }
    if !canon.is_empty() {
        for key in [norm(tsym), norm(idxname)] {
            if key.is_empty() {
                continue;
            }
            if let Some((_, v)) = ALIASES.iter().find(|(a, _)| *a == key) {
                return v.to_string();
            }
            if let Some(c) = canon.iter().find(|c| norm(c) == key) {
                return c.to_string();
            }
        }
    }
    tsym.to_ascii_uppercase().replace([' ', '-'], "")
}

/// Parse one exchange CSV.
pub fn parse_file(exchange: &str, text_in: &str) -> Vec<SymToken> {
    let mut lines = text_in.lines();
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let h = CsvHeader::parse(head);
    let col = |n: &str| h.index(n);
    let (tok, lot, name, tsym, company, isin, tick, freeze, exp, opt, strike) = (
        col("Token"),
        col("LotSize"),
        col("Symbol"),
        col("TradingSymbol"),
        col("CompanyName"),
        col("ISIN"),
        col("TickSize"),
        col("FreezeQty"),
        col("Expiry"),
        col("OptionType"),
        col("StrikePrice"),
    );
    let mut out = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f = split_csv_line(line);
        let get = |i: Option<usize>| {
            i.and_then(|i| f.get(i))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let num = |i: Option<usize>| get(i).parse::<f64>().unwrap_or(0.0);
        let (token, br) = (get(tok), get(tsym));
        if token.is_empty() || br.is_empty() {
            continue;
        }
        let base = SymToken {
            symbol: String::new(),
            brsymbol: br.clone(),
            name: get(company),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token,
            expiry: String::new(),
            strike: -1.0,
            lot_size: num(lot) as i32,
            instrument_type: "EQ".into(),
            tick_size: num(tick),
        };
        match exchange {
            "NSE" | "BSE" => {
                let is_index = get(isin).is_empty() && num(tick) == 0.0 && num(freeze) == 0.0;
                if is_index {
                    out.push(SymToken {
                        symbol: index_symbol(&br, &base.name, exchange),
                        exchange: format!("{}_INDEX", exchange),
                        instrument_type: "INDEX".into(),
                        ..base
                    });
                } else {
                    let symbol = if exchange == "NSE" {
                        br.replace("-EQ", "").replace("-BE", "")
                    } else {
                        br.clone()
                    };
                    let itype = if br.contains("-BE") { "BE" } else { "EQ" };
                    out.push(SymToken {
                        symbol,
                        instrument_type: itype.into(),
                        ..base
                    });
                }
            }
            _ => {
                let e = expiry(&get(exp));
                let o = get(opt);
                let itype = if o == "XX" { "FUT".to_string() } else { o };
                let k = get(strike).parse::<f64>().unwrap_or(-1.0);
                let n = get(name);
                let symbol = if itype == "FUT" {
                    future_symbol(&n, &e)
                } else {
                    option_symbol(&n, &e, k, &itype)
                };
                out.push(SymToken {
                    symbol,
                    name: n,
                    expiry: e,
                    strike: k,
                    instrument_type: itype,
                    ..base
                });
            }
        }
    }
    out
}

/// `/indexList` rows -> index SymTokens.
pub fn parse_index_list(v: &Value) -> Vec<SymToken> {
    v.get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|it| {
                    let ex = text(it, "exchange");
                    let ts = text(it, "tradingSymbol");
                    let tok = text(it, "token");
                    if ts.is_empty() || tok.is_empty() {
                        return None;
                    }
                    let idx = text(it, "idxname");
                    Some(SymToken {
                        symbol: index_symbol(&ts, &idx, &ex),
                        brsymbol: ts,
                        name: idx,
                        exchange: if ex == "NSE" || ex == "BSE" {
                            format!("{}_INDEX", ex)
                        } else {
                            ex.clone()
                        },
                        brexchange: ex,
                        token: tok,
                        expiry: String::new(),
                        strike: -1.0,
                        lot_size: 0,
                        instrument_type: "INDEX".into(),
                        tick_size: 0.0,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The index rows of the master in use, to carry forward when the index
/// list cannot be fetched (web `get_existing_index_rows`, #2198): each
/// exchange and token once, and none the fresh rows already hold (the web
/// drops a token already taken).
pub fn carried_index_rows(current: &[SymToken], fresh: &[SymToken]) -> Vec<SymToken> {
    let mut have: HashSet<(&str, &str)> = fresh
        .iter()
        .map(|r| (r.exchange.as_str(), r.token.as_str()))
        .collect();
    current
        .iter()
        .filter(|r| {
            r.instrument_type == "INDEX" && have.insert((r.exchange.as_str(), r.token.as_str()))
        })
        .cloned()
        .collect()
}

/// One symbol file's text, or `None` when it could not be fetched or came
/// back empty (the reason is logged).
async fn fetch_file(b: &FirstockBroker, ex: &str) -> Option<String> {
    let url = format!("{}/symbols/{}?ref=firstock.in", b.base_url, ex);
    let text = match b.http.get(&url).timeout(DOWNLOAD_TIMEOUT).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    broker = "firstock",
                    "Master file {} failed: {}",
                    ex,
                    crate::brokers::common::redact::url_safe_error(&e)
                );
                return None;
            }
        },
        Ok(r) => {
            tracing::warn!(
                broker = "firstock",
                "Master file {} answered {}",
                ex,
                r.status()
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                broker = "firstock",
                "Master file {} failed: {}",
                ex,
                crate::brokers::common::redact::url_safe_error(&e)
            );
            return None;
        }
    };
    if text.trim().is_empty() {
        tracing::warn!(broker = "firstock", "Master file {} came back empty", ex);
        return None;
    }
    Some(text)
}

/// Every symbol file must arrive and yield rows, or the download fails and
/// the stored master is kept (web #2198; the service swaps the table in
/// one transaction). Indices come from the authenticated index list; when
/// it fails or is empty, the index rows of the master in use are carried
/// forward instead of being dropped.
pub async fn download(b: &FirstockBroker, auth: &AuthToken) -> Result<Vec<SymToken>> {
    let mut texts = Vec::with_capacity(FILES.len());
    let mut failed = Vec::new();
    for ex in FILES {
        match fetch_file(b, ex).await {
            Some(t) => texts.push((*ex, t)),
            None => failed.push(*ex),
        }
    }
    if !failed.is_empty() {
        return Err(AppError::Broker(format!(
            "Could not download the Firstock symbol files for {}. Your existing symbols were kept; try the download again.",
            failed.join(", ")
        )));
    }
    let mut rows = Vec::new();
    for (ex, text) in &texts {
        let parsed = parse_file(ex, text);
        if parsed.is_empty() {
            return Err(AppError::Broker(format!(
                "The Firstock {} symbol file had no usable rows. Your existing symbols were kept; try the download again.",
                ex
            )));
        }
        rows.extend(parsed);
    }
    drop(texts);
    let fresh = match session(auth) {
        Ok(s) => match b.call_ok("/indexList", json!({}), &s).await {
            Ok(v) => parse_index_list(&v),
            Err(e) => {
                tracing::warn!(broker = "firstock", "Index list failed: {}", e.code());
                Vec::new()
            }
        },
        Err(_) => {
            tracing::warn!(broker = "firstock", "No session for the index list");
            Vec::new()
        }
    };
    if fresh.is_empty() {
        let kept = carried_index_rows(b.symbols.snapshot().rows(), &rows);
        tracing::warn!(
            broker = "firstock",
            "Index list unavailable; keeping {} existing index rows",
            kept.len()
        );
        rows.extend(kept);
    } else {
        rows.extend(fresh);
    }
    Ok(rows)
}
