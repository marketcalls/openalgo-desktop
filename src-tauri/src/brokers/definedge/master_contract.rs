//! Master contract (web `database/master_contract_db.py`).
//!
//! `GET https://app.definedgesecurities.com/public/allmaster.zip` (public,
//! no auth) holds one headerless `allmaster.csv`:
//! `Exchange, Token, Name, TradingSymbol, InstrumentType, Expiry(DDMMYYYY),
//! TickSize(paise), LotSize, OptionType, StrikePrice(paise), Col10, Col11,
//! Col12, PriceFactor, Col14`.
//!
//! * NSE keeps only `EQ`, `BE`, `IDX`, `INDEX`; BSE rows need a symbol.
//! * NSE equities drop the `-EQ|-BE|-MF|-SG` suffix; equities get strike 1.
//! * `IDX`/`INDEX` rows become `NSE_INDEX`/`BSE_INDEX`/`MCX_INDEX`, NSE and
//!   BSE index symbols uppercased without spaces or hyphens, then renamed.
//! * derivatives: `name + DDMMMYY + FUT` and `name + DDMMMYY + strike + CE|PE`.
//!   Strike text follows `docs/prompt/symbol-format.md` (`292.5`), where
//!   the web drops the decimal point (`2925`).

use super::DefinedgeBroker;
use crate::brokers::common::master_contract::{format_expiry, format_strike, split_csv_line};
use crate::brokers::common::redact;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::families::noren::zip;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use std::collections::HashSet;

/// Index renames applied to `NSE_INDEX` / `BSE_INDEX` symbols after
/// clean-up (web step 3).
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
    ("HANGSENGBEESNAV", "HANGSENGBEESNAV"),
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
    ("BSECAPGOOD", "BSECAPITALGOODS"),
    ("BSECG", "BSECAPITALGOODS"),
    ("BSECARBON", "BSECARBONEX"),
    ("BSECONSDUR", "BSECONSUMERDURABLES"),
    ("BSECD", "BSECONSUMERDURABLES"),
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
    ("BSESMLCAP", "BSESMALLCAP"),
    ("BSESMEIPO", "BSESMEIPO"),
    ("BSEPBI", "BSEPSU"),
    ("BSEPSUBANK", "BSEPSU"),
];

/// `DDMMYYYY` (leading zero possibly lost) -> `DD-MMM-YY`; anything else is
/// kept as given (web `format_expiry_date`).
pub fn expiry(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let digits = t.split('.').next().unwrap_or(t);
    let padded = format!("{:0>8}", digits);
    match NaiveDate::parse_from_str(&padded, "%d%m%Y") {
        Ok(d) => format_expiry(d),
        Err(_) => t.to_string(),
    }
}

fn number(s: Option<&String>) -> Option<f64> {
    s.and_then(|x| x.trim().parse::<f64>().ok())
}

fn clean_index(s: &str) -> String {
    let up = s.to_ascii_uppercase().replace([' ', '-'], "");
    INDEX_RENAMES
        .iter()
        .find(|(from, _)| *from == up)
        .map(|(_, to)| to.to_string())
        .unwrap_or(up)
}

fn strip_eq_suffix(s: &str) -> String {
    for suf in ["-EQ", "-BE", "-MF", "-SG"] {
        if let Some(base) = s.strip_suffix(suf) {
            return base.to_string();
        }
    }
    s.to_string()
}

/// Parse one `allmaster.csv` line; `None` for rows the web filters out.
pub fn parse_line(line: &str) -> Option<SymToken> {
    let c = split_csv_line(line);
    let get = |i: usize| c.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
    let brexchange = get(0);
    let token = get(1);
    let brsymbol = get(3);
    if brexchange.is_empty() || token.is_empty() {
        return None;
    }
    let name = get(2);
    let mut itype = get(4);
    if itype.is_empty() {
        itype = "EQ".into();
    }
    let option_type = get(8);
    let mut strike = number(c.get(9)).unwrap_or(0.0) / 100.0;
    let tick = number(c.get(6)).unwrap_or(5.0) / 100.0;
    let lot = number(c.get(7)).map(|x| x as i32).unwrap_or(1);
    let mut exp = get(5);
    let mut symbol = brsymbol.clone();
    let mut exchange = brexchange.clone();

    match brexchange.as_str() {
        "NSE" if !matches!(itype.as_str(), "EQ" | "BE" | "INDEX" | "IDX") => return None,
        "BSE" if brsymbol.is_empty() => return None,
        _ => {}
    }

    let is_index = matches!(itype.as_str(), "INDEX" | "IDX");
    if matches!(brexchange.as_str(), "NSE" | "BSE") && matches!(itype.as_str(), "EQ" | "BE") {
        if brexchange == "NSE" {
            symbol = strip_eq_suffix(&brsymbol);
        }
        itype = "EQ".into();
        exp.clear();
        strike = 1.0;
    } else if is_index && matches!(brexchange.as_str(), "NSE" | "BSE" | "MCX") {
        exchange = format!("{}_INDEX", brexchange);
        itype = "IDX".into();
        exp.clear();
        strike = 1.0;
        if brexchange != "MCX" {
            symbol = clean_index(&symbol);
        }
    } else if matches!(brexchange.as_str(), "NFO" | "BFO" | "CDS" | "MCX") {
        exp = expiry(&exp);
        let compact = exp.replace('-', "");
        let fut = match brexchange.as_str() {
            "NFO" | "BFO" => matches!(itype.as_str(), "FUTIDX" | "FUTSTK"),
            "CDS" => matches!(itype.as_str(), "FUTCUR" | "FUTIRC"),
            _ => itype == "FUTCOM",
        };
        let opt = match brexchange.as_str() {
            "NFO" | "BFO" => matches!(itype.as_str(), "OPTIDX" | "OPTSTK"),
            "CDS" => matches!(itype.as_str(), "OPTCUR" | "OPTIRC"),
            _ => itype == "OPTFUT",
        };
        if fut {
            symbol = format!("{}{}FUT", name, compact);
            itype = "FUT".into();
        } else if opt {
            symbol = format!(
                "{}{}{}{}",
                name,
                compact,
                format_strike(strike),
                option_type
            );
            itype = option_type.clone();
        }
    }
    if symbol.is_empty() {
        return None;
    }
    Some(SymToken {
        symbol,
        brsymbol,
        name,
        exchange,
        brexchange,
        token,
        expiry: exp,
        strike,
        lot_size: lot,
        instrument_type: itype,
        tick_size: tick,
    })
}

/// Parse the whole file; duplicate index symbols keep the first row.
pub fn parse_allmaster(text: &str) -> Vec<SymToken> {
    let mut seen_index: HashSet<(String, String)> = HashSet::new();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_line)
        .filter(|r| {
            if matches!(r.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX") {
                seen_index.insert((r.exchange.clone(), r.symbol.clone()))
            } else {
                true
            }
        })
        .collect()
}

pub async fn download(b: &DefinedgeBroker) -> Result<Vec<SymToken>> {
    let resp = b
        .http
        .get(&b.urls.master)
        .timeout(crate::brokers::common::http::DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(redact::http)?;
    if !resp.status().is_success() {
        tracing::warn!(
            broker = "definedge",
            status = resp.status().as_u16(),
            "Master contract download refused"
        );
        return Err(AppError::Broker(
            "Definedge's instrument list could not be downloaded. Try again shortly.".into(),
        ));
    }
    let bytes = resp.bytes().await.map_err(redact::http)?;
    let csv = zip::first_entry(&bytes)?;
    let text = String::from_utf8_lossy(&csv);
    let rows = parse_allmaster(&text);
    if rows.is_empty() {
        return Err(AppError::Broker(
            "Definedge's instrument list was empty. Try downloading the master contract again."
                .into(),
        ));
    }
    Ok(rows)
}
