//! Instrument master (web `database/master_contract_db.py`).
//!
//! `POST {md}/instruments/master {"exchangeSegmentList":[seg]}` per segment
//! (no auth header) returns one string: rows on `\n`, fields on `|`, with
//! the column names supplied client side. Indices come from
//! `GET {md}/instruments/indexlist?exchangeSegment=1|11`.
//!
//! The web reads the pipe rows through `pd.read_csv` with a fixed header.
//! XTS futures rows carry two fewer fields than option rows (no
//! `StrikePrice`/`OptionType`), so pandas shifts `PriceNumerator` (1) into
//! `OptionType` and a text field into `StrikePrice` (-> 1.0). The result is
//! "FUT" with strike 1.0; this parser reaches the same rows explicitly.
//! A member that reads answers as documented (5paisa XTS, web #2504) types
//! each row by its `InstrumentType` instead: 1 a future at strike 0, 2 an
//! option (`OptionType` 3 CE, 4 PE), anything else (4, a spread) is no
//! contract.

use super::{XtsBroker, XtsHooks};
use crate::brokers::common::http;
use crate::brokers::common::master_contract::{format_expiry, format_strike, parse_broker_expiry};
use crate::brokers::common::symbols::SymToken;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// CM column order (`mc:95`).
pub const CM_COLUMNS: &[&str] = &[
    "ExchangeSegment",
    "ExchangeInstrumentID",
    "InstrumentType",
    "Name",
    "Description",
    "Series",
    "NameWithSeries",
    "InstrumentID",
    "PriceBand.High",
    "PriceBand.Low",
    "FreezeQty",
    "TickSize",
    "LotSize",
    "Multiplier",
    "DisplayName",
    "ISIN",
    "PriceNumerator",
    "PriceDenominator",
    "DetailedDescription",
    "ExtendedSurvIndicator",
    "CautionIndicator",
    "GSMIndicator",
];

/// FO/CD column order (`mc:96`).
pub const FO_COLUMNS: &[&str] = &[
    "ExchangeSegment",
    "ExchangeInstrumentID",
    "InstrumentType",
    "Name",
    "Description",
    "Series",
    "NameWithSeries",
    "InstrumentID",
    "PriceBand.High",
    "PriceBand.Low",
    "FreezeQty",
    "TickSize",
    "LotSize",
    "Multiplier",
    "UnderlyingInstrumentId",
    "UnderlyingIndexName",
    "ContractExpiration",
    "StrikePrice",
    "OptionType",
    "DisplayName",
    "PriceNumerator",
    "PriceDenominator",
    "DetailedDescription",
];

const FO_STRIKE: usize = 17;
const FO_OPTION_TYPE: usize = 18;

/// The web's `BSE_INDEX_SYMBOL_MAP` (`mc:186-222`).
pub const BSE_INDEX_SYMBOL_MAP: &[(&str, &str)] = &[
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
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("FINSER", "BSEFINANCIALSERVICES"),
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
    ("BSEPBI", "BSEPSU"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

/// NSE index renames of `process_index_data` (`mc:504-513`).
pub const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTY 50", "NIFTY"),
    ("NIFTY BANK", "BANKNIFTY"),
    ("INDIA VIX", "INDIAVIX"),
    ("NIFTY FIN SERVICE", "FINNIFTY"),
    ("NIFTY MID SELECT", "MIDCPNIFTY"),
    ("NIFTY NEXT 50", "NIFTYNXT50"),
    ("HANGSENG BEES NAV", "HANGSENGBEESNAV"),
    ("HANGSENG BEES-NAV", "HANGSENGBEESNAV"),
];

fn lookup(table: &[(&str, &str)], name: &str) -> Option<String> {
    table
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// Upper, trim, collapse runs of whitespace (`normalize_bse_index_symbols`).
fn tidy(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

/// Remove spaces and hyphens (`str.replace(r"[\s\-]+", "")`).
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect()
}

/// `normalize_bse_index_symbols`.
pub fn bse_index_symbol(raw: &str) -> String {
    let t = tidy(raw);
    squash(&lookup(BSE_INDEX_SYMBOL_MAP, &t).unwrap_or(t))
}

fn field<'a>(row: &[&'a str], i: usize) -> &'a str {
    row.get(i).map(|s| s.trim()).unwrap_or("")
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

fn rows(body: &str) -> impl Iterator<Item = Vec<&str>> {
    body.split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('|').collect())
}

/// One cash-market segment (`process_*_nse_csv` / `_bse_csv`, `mc:233-293`).
fn parse_cm(segment: &str, body: &str) -> Vec<SymToken> {
    let bse = segment == "BSECM";
    let mut out = Vec::new();
    for r in rows(body) {
        let series = field(&r, 5);
        if !bse && series != "EQ" {
            continue;
        }
        let token = field(&r, 1);
        if token.is_empty() {
            continue;
        }
        let name = field(&r, 3).to_string();
        let index = bse && series == "SPOT";
        let symbol = if index {
            bse_index_symbol(&name)
        } else {
            name.clone()
        };
        out.push(SymToken {
            name: if index { symbol.clone() } else { name },
            symbol,
            brsymbol: field(&r, 14).to_string(),
            exchange: if index {
                "BSE_INDEX"
            } else if bse {
                "BSE"
            } else {
                "NSE"
            }
            .into(),
            brexchange: field(&r, 0).to_string(),
            token: token.to_string(),
            expiry: String::new(),
            strike: 1.0,
            lot_size: num(field(&r, 12)).unwrap_or(1.0) as i32,
            instrument_type: series.to_string(),
            tick_size: num(field(&r, 11)).unwrap_or(0.05),
        });
    }
    out
}

/// A contract's kind and strike as the template reads an F&O row: a row
/// short of the options layout, or `OptionType` 1, is a future; 3 a call,
/// anything else a put; the strike 1.0 where it cannot be read.
fn template_kind(r: &[&str]) -> (&'static str, f64) {
    let option_row = r.len() >= FO_COLUMNS.len();
    let kind = match (option_row, field(r, FO_OPTION_TYPE)) {
        (false, _) | (true, "1") => "FUT",
        (true, "3") => "CE",
        _ => "PE",
    };
    let strike = if option_row {
        num(field(r, FO_STRIKE)).unwrap_or(1.0)
    } else {
        1.0
    };
    (kind, strike)
}

/// A contract's kind and strike as the XTS v2 docs lay a row out (web
/// fivepaisaxts `_fo_contracts`, #2504): futures and spreads have 21
/// fields, options 23 with `StrikePrice` and `OptionType`. `InstrumentType`
/// 1 is a future at strike 0; 2 an option, a call for `OptionType` 3 and a
/// put for 4, with a strike that reads as a number; anything else, a
/// spread (4) among them, is no contract.
fn documented_kind(r: &[&str]) -> Option<(&'static str, f64)> {
    match num(field(r, 2))? as i64 {
        1 => Some(("FUT", 0.0)),
        2 if r.len() >= FO_COLUMNS.len() => {
            let kind = match num(field(r, FO_OPTION_TYPE))? as i64 {
                3 => "CE",
                4 => "PE",
                _ => return None,
            };
            Some((kind, num(field(r, FO_STRIKE))?))
        }
        _ => None,
    }
}

/// One derivatives segment (`process_*_nfo/bfo/cds/mcx_csv`, `mc:296-480`),
/// each row typed as the member reads it.
fn parse_fo(segment: &str, body: &str, hooks: &XtsHooks) -> Vec<SymToken> {
    let exchange = match segment {
        "NSEFO" => "NFO",
        "BSEFO" => "BFO",
        "NSECD" => "CDS",
        "MCXFO" => "MCX",
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for r in rows(body) {
        let token = field(&r, 1);
        let expiration = field(&r, 16);
        if token.is_empty() || (segment == "MCXFO" && expiration == "1") {
            continue;
        }
        // CDS drops rows without an OptionType column (`mc:350`).
        if segment == "NSECD" && field(&r, FO_OPTION_TYPE).is_empty() {
            continue;
        }
        let Some(date) = parse_broker_expiry(expiration) else {
            continue;
        };
        let expiry = format_expiry(date);
        let typed = if hooks.answers_as_documented {
            documented_kind(&r)
        } else {
            Some(template_kind(&r))
        };
        let Some((kind, strike)) = typed else {
            continue;
        };
        let name = field(&r, 3).to_string();
        let compact = expiry.replace('-', "");
        let symbol = if kind == "FUT" {
            format!("{}{}FUT", name, compact)
        } else {
            format!("{}{}{}{}", name, compact, format_strike(strike), kind)
        };
        out.push(SymToken {
            symbol,
            brsymbol: field(&r, 4).to_string(),
            name,
            exchange: exchange.into(),
            brexchange: field(&r, 0).to_string(),
            token: token.to_string(),
            expiry,
            strike,
            lot_size: num(field(&r, 12)).unwrap_or(1.0) as i32,
            instrument_type: kind.into(),
            tick_size: num(field(&r, 11)).unwrap_or(0.05),
        });
    }
    out
}

/// Parse one segment's `result` string as the member reads it (`hooks`).
pub fn parse_segment(segment: &str, body: &str, hooks: &XtsHooks) -> Vec<SymToken> {
    match segment {
        "NSECM" | "BSECM" => parse_cm(segment, body),
        _ => parse_fo(segment, body, hooks),
    }
}

/// `fetch_index_list` + `process_index_data` for one segment code
/// (1 -> NSE_INDEX, 11 -> BSE_INDEX). Entries are `"NIFTY 50_26000"`.
pub fn parse_index_list(code: u8, result: &Value) -> Vec<SymToken> {
    let exchange = if code == 1 { "NSE_INDEX" } else { "BSE_INDEX" };
    result
        .get("indexList")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter_map(|entry| {
                    let (raw, token) = entry.rsplit_once('_')?;
                    let t = tidy(raw);
                    let renamed = if code == 1 {
                        lookup(NSE_INDEX_MAP, &t).unwrap_or(t)
                    } else {
                        bse_index_symbol(&t)
                    };
                    let symbol = squash(&renamed);
                    Some(SymToken {
                        name: symbol.clone(),
                        symbol,
                        brsymbol: entry.to_string(),
                        exchange: exchange.into(),
                        brexchange: exchange.into(),
                        token: token.trim().to_string(),
                        expiry: String::new(),
                        strike: 1.0,
                        lot_size: 1,
                        instrument_type: "INDEX".into(),
                        tick_size: 0.05,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Download every configured segment plus both index lists. A segment
/// download failure aborts (the web raises before truncating the table);
/// a failing index list is logged and skipped (`mc:146-151`).
pub(crate) async fn download(b: &XtsBroker) -> Result<Vec<SymToken>> {
    let mut out = Vec::new();
    for seg in b.cfg.master_segments {
        let resp = b
            .http
            .post(b.md_url("/instruments/master"))
            .header("Content-Type", "application/json")
            .timeout(http::DOWNLOAD_TIMEOUT)
            .json(&json!({"exchangeSegmentList": [seg]}))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                broker = b.cfg.id,
                status = status.as_u16(),
                "Master download for {} failed",
                seg
            );
            return Err(AppError::Broker(format!(
                "{} did not send the instrument list. Try the download again shortly.",
                b.cfg.name
            )));
        }
        let (_, v): (_, Value) = http::read_json(b.cfg.id, resp).await?;
        let Some(body) = v.get("result").and_then(Value::as_str) else {
            return Err(AppError::Broker(format!(
                "{} sent an instrument list OpenAlgo could not read. Try again shortly.",
                b.cfg.name
            )));
        };
        let rows = parse_segment(seg, body, &b.cfg.hooks);
        tracing::info!(
            broker = b.cfg.id,
            "Master segment {}: {} instruments",
            seg,
            rows.len()
        );
        out.extend(rows);
    }
    for code in [1u8, 11] {
        let url = format!(
            "{}?exchangeSegment={}",
            b.md_url("/instruments/indexlist"),
            code
        );
        let res = async {
            let resp = b
                .http
                .get(&url)
                .header("Content-Type", "application/json")
                .send()
                .await?;
            let (_, v): (_, Value) = http::read_json(b.cfg.id, resp).await?;
            Ok::<Value, AppError>(v)
        }
        .await;
        match res {
            Ok(v) => out.extend(parse_index_list(
                code,
                v.get("result").unwrap_or(&Value::Null),
            )),
            Err(e) => tracing::warn!(
                broker = b.cfg.id,
                "Index list {} unavailable: {}",
                code,
                e.code()
            ),
        }
    }
    Ok(out)
}
