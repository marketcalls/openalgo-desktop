//! Arrow instrument master -> OpenAlgo symbol master (web
//! `database/master_contract_db.py`, `mapping/exchange.py`).
//!
//! * `GET /all` (authenticated CSV, about 221k rows) plus
//!   `GET /info/index-list` (`[{name, token}]`, merged by token).
//! * `ExchSeg` drives the exchange: `NSECM NSE, BSECM BSE, NSEFO NFO,
//!   BSEFO BFO, NSECD CDS, BSECD BCD, NSECO NCO, MCXFO MCX, NSEIDX
//!   NSE_INDEX, BSEIDX BSE_INDEX, MCXIDX MCX_INDEX`; brexchange is ExchSeg.
//! * Currency segments ship strikes x100000 and ticks in paise.
//! * Futures are `OptionType == "XX"` (or carry an expiry on a derivative
//!   exchange); symbols follow `docs/prompt/symbol-format.md`.
//! * Index display names are standardised (`_NSE_INDEX_MAP`,
//!   `_BSE_INDEX_MAP`, `_MCX_INDEX_MAP`); indices are stored as `EQ`.

use super::{ArrowBroker, Category};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, future_symbol, option_symbol, CsvHeader,
};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use reqwest::Method;
use serde_json::Value;
use std::collections::HashSet;

/// Arrow ExchSeg -> OpenAlgo exchange (`exchange.py:25-37`).
pub fn exchseg_to_oa(seg: &str) -> Option<&'static str> {
    Some(match seg {
        "NSECM" => "NSE",
        "BSECM" => "BSE",
        "NSEFO" => "NFO",
        "BSEFO" => "BFO",
        "NSECD" => "CDS",
        "BSECD" => "BCD",
        "NSECO" => "NCO",
        "MCXFO" => "MCX",
        "NSEIDX" => "NSE_INDEX",
        "BSEIDX" => "BSE_INDEX",
        "MCXIDX" => "MCX_INDEX",
        _ => return None,
    })
}

/// NSE index display names (uppercased, no spaces) -> OpenAlgo symbols.
const NSE_INDEX_MAP: &[(&str, &str)] = &[
    ("NIFTY50", "NIFTY"),
    ("NIFTYNEXT50", "NIFTYNXT50"),
    ("NIFTYFINSERVICE", "FINNIFTY"),
    ("NIFTYBANK", "BANKNIFTY"),
    ("NIFTYMIDSELECT", "MIDCPNIFTY"),
    ("INDIAVIX", "INDIAVIX"),
    ("HANGSENGBEES-NAV", "HANGSENGBEESNAV"),
    ("NIFTY100", "NIFTY100"),
    ("NIFTY200", "NIFTY200"),
    ("NIFTY500", "NIFTY500"),
    ("NIFTYALPHA50", "NIFTYALPHA50"),
    ("NIFTYAUTO", "NIFTYAUTO"),
    ("NIFTYCOMMODITIES", "NIFTYCOMMODITIES"),
    ("NIFTYCONSUMPTION", "NIFTYCONSUMPTION"),
    ("NIFTYCPSE", "NIFTYCPSE"),
    ("NIFTYDIVOPPS50", "NIFTYDIVOPPS50"),
    ("NIFTYENERGY", "NIFTYENERGY"),
    ("NIFTYFMCG", "NIFTYFMCG"),
    ("NIFTYGROWSECT15", "NIFTYGROWSECT15"),
    ("NIFTYINFRA", "NIFTYINFRA"),
    ("NIFTYIT", "NIFTYIT"),
    ("NIFTYMEDIA", "NIFTYMEDIA"),
    ("NIFTYMETAL", "NIFTYMETAL"),
    ("NIFTYMNC", "NIFTYMNC"),
    ("NIFTYPHARMA", "NIFTYPHARMA"),
    ("NIFTYPSE", "NIFTYPSE"),
    ("NIFTYPSUBANK", "NIFTYPSUBANK"),
    ("NIFTYPVTBANK", "NIFTYPVTBANK"),
    ("NIFTYREALTY", "NIFTYREALTY"),
    ("NIFTYSERVSECTOR", "NIFTYSERVSECTOR"),
    ("NIFTYMIDLIQ15", "NIFTYMIDLIQ15"),
    ("NIFTYMIDCAP50", "NIFTYMIDCAP50"),
    ("NIFTYMIDCAP100", "NIFTYMIDCAP100"),
    ("NIFTYMIDCAP150", "NIFTYMIDCAP150"),
    ("NIFTYMIDSML400", "NIFTYMIDSML400"),
    ("NIFTYSMLCAP50", "NIFTYSMLCAP50"),
    ("NIFTYSMLCAP100", "NIFTYSMLCAP100"),
    ("NIFTYSMLCAP250", "NIFTYSMLCAP250"),
    ("NIFTY100EQLWGT", "NIFTY100EQLWGT"),
    ("NIFTY100LIQ15", "NIFTY100LIQ15"),
    ("NIFTY100LOWVOL30", "NIFTY100LOWVOL30"),
    ("NIFTY100QUALTY30", "NIFTY100QUALTY30"),
    ("NIFTY200QUALTY30", "NIFTY200QUALTY30"),
    ("NIFTY50DIVPOINT", "NIFTY50DIVPOINT"),
    ("NIFTY50EQLWGT", "NIFTY50EQLWGT"),
    ("NIFTY50PR1XINV", "NIFTY50PR1XINV"),
    ("NIFTY50PR2XLEV", "NIFTY50PR2XLEV"),
    ("NIFTY50TR1XINV", "NIFTY50TR1XINV"),
    ("NIFTY50TR2XLEV", "NIFTY50TR2XLEV"),
    ("NIFTY50VALUE20", "NIFTY50VALUE20"),
    ("NIFTYGS10YR", "NIFTYGS10YR"),
    ("NIFTYGS10YRCLN", "NIFTYGS10YRCLN"),
    ("NIFTYGS1115YR", "NIFTYGS1115YR"),
    ("NIFTYGS15YRPLUS", "NIFTYGS15YRPLUS"),
    ("NIFTYGS48YR", "NIFTYGS48YR"),
    ("NIFTYGS813YR", "NIFTYGS813YR"),
    ("NIFTYGSCOMPSITE", "NIFTYGSCOMPSITE"),
];

/// BSE index short codes -> OpenAlgo symbols.
const BSE_INDEX_MAP: &[(&str, &str)] = &[
    ("SENSEX", "SENSEX"),
    ("BANKEX", "BANKEX"),
    ("SENSEX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("BSE100", "BSE100"),
    ("BSE200", "BSE200"),
    ("BSE500", "BSE500"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSECG", "BSECAPITALGOODS"),
    ("CARBON", "BSECARBONEX"),
    ("BSECD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("DOL30", "BSEDOLLEX30"),
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("ENERGYINDEX", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FIN", "BSEFINANCIALSERVICES"),
    ("GREENX", "BSEGREENEX"),
    ("BSEHC", "BSEHEALTHCARE"),
    ("INFRAINDEX", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSEIT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIPO", "BSEIPO"),
    ("LRGCAP", "BSELARGECAP"),
    ("METALINDEX", "BSEMETAL"),
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

/// MCX iCOMDEX names -> OpenAlgo symbols.
const MCX_INDEX_MAP: &[(&str, &str)] = &[
    ("COMPOSITE", "MCXCOMPDEX"),
    ("BULLION", "MCXBULLDEX"),
    ("BASEMETAL", "MCXMETLDEX"),
    ("ENERGY", "MCXENERGY"),
    ("GOLD", "MCXGOLDEX"),
    ("SILVER", "MCXSILVDEX"),
    ("COPPER", "MCXCOPRDEX"),
    ("CRUDEOIL", "MCXCRUDEX"),
    ("ALUMINIUM", "MCXALUMINIUM"),
    ("LEAD", "MCXLEAD"),
    ("ZINC", "MCXZINC"),
    ("NATURALGAS", "MCXNATURALGAS"),
];

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Uppercase with all whitespace removed (`exchange.py::_norm`).
pub fn norm(name: &str) -> String {
    name.split_whitespace().collect::<String>().to_uppercase()
}

/// Arrow index display name -> `(symbol, exchange)`
/// (`exchange.py::classify_index_symbol`). With a known index exchange only
/// the symbol is standardised; without one (the index-list endpoint), BSE
/// membership comes from the BSE code table and the rest is NSE.
pub fn classify_index_symbol(name: &str, exchange: Option<&str>) -> (String, &'static str) {
    let key = norm(name);
    let table = |ex: &str| match ex {
        "NSE_INDEX" => Some((NSE_INDEX_MAP, "NSE_INDEX")),
        "BSE_INDEX" => Some((BSE_INDEX_MAP, "BSE_INDEX")),
        "MCX_INDEX" => Some((MCX_INDEX_MAP, "MCX_INDEX")),
        _ => None,
    };
    if let Some((t, ex)) = exchange.and_then(table) {
        return (lookup(t, &key).map(str::to_string).unwrap_or(key), ex);
    }
    if let Some(s) = lookup(BSE_INDEX_MAP, &key) {
        return (s.to_string(), "BSE_INDEX");
    }
    (
        lookup(NSE_INDEX_MAP, &key)
            .map(str::to_string)
            .unwrap_or(key),
        "NSE_INDEX",
    )
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn bad_format(col: &str) -> AppError {
    tracing::error!("Arrow instrument list has no '{}' column", col);
    AppError::Broker(
        "Arrow's instrument list has an unexpected format. Try downloading the master contract again later."
            .into(),
    )
}

/// Parse the `/all` CSV into master rows (`process_arrow_csv`).
pub fn parse_instruments(csv: &str) -> Result<Vec<SymToken>> {
    let mut lines = csv.lines();
    let header = CsvHeader::parse(lines.next().ok_or_else(|| bad_format("ExchSeg"))?);
    let col = |n: &str| header.index(n).ok_or_else(|| bad_format(n));
    let c_seg = col("ExchSeg")?;
    let c_und = col("Underlying")?;
    let c_sym = col("Symbol")?;
    let c_ts = col("TradingSymbol")?;
    let c_full = col("FullName")?;
    let c_opt = col("OptionType")?;
    let c_exp = col("Expiry")?;
    let c_strike = col("StrikePrice")?;
    let c_tick = col("TickSize")?;
    let c_lot = col("LotSize")?;
    let c_tok = col("Token")?;

    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = crate::brokers::common::master_contract::split_csv_line(line);
        let g = |i: usize| f.get(i).map(|s| s.trim()).unwrap_or("");
        let seg = g(c_seg).to_ascii_uppercase();
        let Some(exchange) = exchseg_to_oa(&seg) else {
            continue;
        };
        let base = if g(c_und).is_empty() {
            g(c_sym)
        } else {
            g(c_und)
        }
        .to_string();
        let tradingsymbol = if g(c_ts).is_empty() {
            g(c_sym)
        } else {
            g(c_ts)
        }
        .to_string();
        let fullname = g(c_full).to_string();
        let option_type = g(c_opt).to_ascii_uppercase();
        let expiry = NaiveDate::parse_from_str(g(c_exp), "%d-%b-%Y")
            .map(format_expiry)
            .unwrap_or_default();
        let currency = seg == "NSECD" || seg == "BSECD";
        let mut strike: f64 = g(c_strike).parse().unwrap_or(0.0);
        if !strike.is_finite() {
            strike = 0.0;
        }
        if currency {
            strike = round6(strike / 100_000.0);
        }
        // Futures carry -0.01 / 0 placeholders.
        let strike = strike.max(0.0) + 0.0;
        let mut tick: f64 = g(c_tick).parse().unwrap_or(0.0);
        if currency {
            tick = round6(tick / 100.0);
        }
        let lot = g(c_lot).parse::<f64>().map(|v| v as i32).unwrap_or(0);

        let is_option = option_type == "CE" || option_type == "PE";
        let is_future = !is_option
            && (option_type == "XX"
                || (!expiry.is_empty()
                    && matches!(exchange, "NFO" | "BFO" | "MCX" | "CDS" | "BCD" | "NCO")));
        let is_index = exchange.ends_with("_INDEX");

        let (symbol, instrument_type) = if is_option {
            (
                option_symbol(&base, &expiry, strike, &option_type),
                option_type.clone(),
            )
        } else if is_future {
            (future_symbol(&base, &expiry), "FUT".to_string())
        } else if is_index {
            (
                classify_index_symbol(&tradingsymbol, Some(exchange)).0,
                "EQ".to_string(),
            )
        } else {
            (
                tradingsymbol
                    .strip_suffix("-EQ")
                    .unwrap_or(&tradingsymbol)
                    .to_string(),
                "EQ".to_string(),
            )
        };
        let name = if (is_option || is_future) && !base.is_empty() {
            base.clone()
        } else if !fullname.is_empty() {
            fullname
        } else {
            base.clone()
        };
        out.push(SymToken {
            symbol,
            brsymbol: tradingsymbol,
            name,
            exchange: exchange.to_string(),
            brexchange: seg,
            token: g(c_tok).to_string(),
            expiry,
            strike,
            lot_size: lot,
            instrument_type,
            tick_size: tick,
        });
    }
    Ok(out)
}

/// Index rows from `/info/index-list` not already in the CSV (by token)
/// (`build_index_rows`); brexchange `INDEX`.
pub fn index_rows(list: &Value, existing: &HashSet<String>) -> Vec<SymToken> {
    let Some(items) = list.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let token = match it.get("token")? {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            if token.is_empty() || existing.contains(&token) {
                return None;
            }
            let name = it.get("name").and_then(Value::as_str).unwrap_or("");
            let (symbol, exchange) = classify_index_symbol(name, None);
            Some(SymToken {
                symbol,
                brsymbol: name.to_string(),
                name: name.to_string(),
                exchange: exchange.to_string(),
                brexchange: "INDEX".into(),
                token,
                expiry: String::new(),
                strike: 0.0,
                lot_size: 0,
                instrument_type: "EQ".into(),
                tick_size: 0.0,
            })
        })
        .collect()
}

pub async fn download(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<SymToken>> {
    let (app_id, jwt) = ArrowBroker::credentials(auth)?;
    // About 221k rows: the download budget, not the REST one.
    let resp = b
        .http
        .get(format!("{}/all", b.urls().rest))
        .header("appID", app_id)
        .header("token", jwt)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(super::session_expired());
    }
    if !status.is_success() {
        tracing::warn!(status = status.as_u16(), "Arrow instrument download failed");
        return Err(AppError::Broker(
            "Arrow did not send its instrument list. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    let text = resp.text().await.map_err(|e| super::redact(e.into()))?;
    let mut rows = parse_instruments(&text)?;
    drop(text);
    // The index list is best effort (`fetch_index_list` returns [] on error).
    let list = match b
        .call(Method::GET, "/info/index-list", auth, None, Category::Other)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Arrow index list unavailable: {}", e.code());
            Value::Null
        }
    };
    let existing: HashSet<String> = rows.iter().map(|r| r.token.clone()).collect();
    rows.extend(index_rows(&list, &existing));
    Ok(rows)
}
