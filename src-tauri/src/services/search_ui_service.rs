//! Symbol search for the pages (web `blueprints/search.py` over
//! `database/symbol.py` and the `token_db_enhanced` cache): the AJAX
//! search with F&O filters, distinct expiries and distinct underlyings, all
//! over one generation of the shared symbol master.

use super::core::float;
use super::symbol_service::freeze_qty_for_option;
use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::symbols::SymToken;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};

/// Web `FNO_EXCHANGES` (F&O plus crypto).
pub const FNO_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"];
const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
/// Web `fno_search_symbols` default limit.
pub const FNO_LIMIT: usize = 10_000;

pub fn is_fno(exchange: &str) -> bool {
    FNO_EXCHANGES.contains(&exchange)
}

/// `DDMMMYY` at the start of `s` (bytes, upper case).
fn date_at(s: &[u8]) -> bool {
    s.len() >= 7
        && s[0].is_ascii_digit()
        && s[1].is_ascii_digit()
        && std::str::from_utf8(&s[2..5])
            .map(|m| MONTHS.contains(&m))
            .unwrap_or(false)
        && s[5].is_ascii_digit()
        && s[6].is_ascii_digit()
}

/// `(\d+(\.\d+)?)?(FUT|CE|PE)?$` over the whole tail.
fn strike_suffix(tail: &str) -> bool {
    let rest = tail
        .strip_suffix("FUT")
        .or_else(|| tail.strip_suffix("CE"))
        .or_else(|| tail.strip_suffix("PE"))
        .unwrap_or(tail);
    if rest.is_empty() {
        return true;
    }
    let mut parts = rest.splitn(2, '.');
    let int = parts.next().unwrap_or_default();
    let ok_int = !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit());
    match parts.next() {
        None => ok_int,
        Some(frac) => ok_int && !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Web `extract_underlying_from_symbol`.
pub fn extract_underlying(symbol: &str, exchange: &str) -> Option<String> {
    if symbol.is_empty() || !is_fno(exchange) {
        return None;
    }
    let up = symbol.to_uppercase();
    let b = up.as_bytes();
    if exchange == "CRYPTO" {
        for i in 1..b.len() {
            if !b[i - 1].is_ascii_alphanumeric() {
                break;
            }
            if date_at(&b[i..]) {
                return Some(up[..i].to_string());
            }
        }
        let mut s = up.strip_suffix(".P").unwrap_or(&up).to_string();
        for suf in ["USDT", "USD", "_INR", "INR"] {
            if s.ends_with(suf) && s.len() > suf.len() {
                s.truncate(s.len() - suf.len());
                return Some(s);
            }
        }
        return Some(s);
    }
    for i in 1..b.len() {
        if !up.is_char_boundary(i) {
            continue;
        }
        if date_at(&b[i..]) && strike_suffix(&up[i + 7..]) {
            return Some(up[..i].to_string());
        }
    }
    None
}

/// The public row shape of `/search/api/search`.
pub fn api_row(r: &SymToken) -> Value {
    api_row_with(r, None)
}

/// `api_row` with the row's contract multiplier (crypto masters; `null`
/// for instruments that have none, as before).
pub fn api_row_with(r: &SymToken, contract_value: Option<f64>) -> Value {
    json!({
        "symbol": r.symbol,
        "brsymbol": r.brsymbol,
        "name": r.name,
        "exchange": r.exchange,
        "brexchange": r.brexchange,
        "token": r.token,
        "expiry": r.expiry,
        "strike": float(r.strike),
        "lotsize": r.lot_size,
        "contract_value": contract_value,
        "instrumenttype": r.instrument_type,
        "freeze_qty": freeze_qty_for_option(&r.symbol, &r.exchange),
    })
}

fn terms(q: Option<&str>) -> Vec<String> {
    q.unwrap_or_default()
        .split_whitespace()
        .map(str::to_uppercase)
        .collect()
}

/// Web `enhanced_search_symbols`: every term must match symbol, broker
/// symbol, name or token (case-insensitive), or equal the strike; master
/// order; no limit.
pub fn enhanced_search<'a>(
    rows: &'a [SymToken],
    query: Option<&str>,
    exchange: Option<&str>,
) -> Vec<&'a SymToken> {
    let ts = terms(query);
    if ts.is_empty() && exchange.is_none() {
        return Vec::new();
    }
    rows.iter()
        .filter(|r| exchange.is_none_or(|e| r.exchange == e))
        .filter(|r| {
            let (sym, br, name, tok) = (
                r.symbol.to_uppercase(),
                r.brsymbol.to_uppercase(),
                r.name.to_uppercase(),
                r.token.to_uppercase(),
            );
            ts.iter().all(|t| {
                sym.contains(t.as_str())
                    || br.contains(t.as_str())
                    || name.contains(t.as_str())
                    || tok.contains(t.as_str())
                    || t.parse::<f64>().map(|n| n == r.strike).unwrap_or(false)
            })
        })
        .collect()
}

/// Filters of the F&O search.
#[derive(Debug, Default, Clone)]
pub struct FnoFilter<'a> {
    pub query: Option<&'a str>,
    pub exchange: Option<&'a str>,
    pub expiry: Option<&'a str>,
    pub instrumenttype: Option<&'a str>,
    pub strike_min: Option<f64>,
    pub strike_max: Option<f64>,
    pub underlying: Option<&'a str>,
}

/// Web `fno_search_symbols` (cache path), sorted by underlying match,
/// prefix, then symbol, at most [`FNO_LIMIT`].
pub fn fno_search<'a>(rows: &'a [SymToken], f: &FnoFilter<'_>) -> Vec<&'a SymToken> {
    let ts = terms(f.query);
    let nums: Vec<f64> = ts.iter().filter_map(|t| t.parse::<f64>().ok()).collect();
    let underlying = f.underlying.map(|u| u.trim().to_uppercase());
    let expiry = f.expiry.map(str::trim);
    let inst = f.instrumenttype.map(|t| t.trim().to_uppercase());
    let mut out: Vec<(Option<String>, &SymToken)> = Vec::new();
    for r in rows {
        if f.exchange.is_some_and(|e| r.exchange != e) {
            continue;
        }
        let und = extract_underlying(&r.symbol, &r.exchange);
        if let Some(u) = &underlying {
            if und.as_deref() != Some(u.as_str()) {
                continue;
            }
        }
        if expiry.is_some_and(|e| r.expiry != e) {
            continue;
        }
        if let Some(t) = inst.as_deref() {
            let su = r.symbol.to_uppercase();
            let keep = match t {
                "FUT" => su.ends_with("FUT"),
                "CE" => su.ends_with("CE"),
                "PE" => su.ends_with("PE"),
                "PERPFUT" => r.instrument_type.eq_ignore_ascii_case("PERPFUT"),
                _ => true,
            };
            if !keep {
                continue;
            }
        }
        if f.strike_min.is_some_and(|m| r.strike < m) || f.strike_max.is_some_and(|m| r.strike > m)
        {
            continue;
        }
        if !ts.is_empty() {
            let (sym, br, name) = (
                r.symbol.to_uppercase(),
                r.brsymbol.to_uppercase(),
                r.name.to_uppercase(),
            );
            let mut all = ts.iter().all(|t| {
                sym.contains(t.as_str())
                    || br.contains(t.as_str())
                    || (!name.is_empty() && name.contains(t.as_str()))
                    || (!r.token.is_empty() && r.token.contains(t.as_str()))
            });
            if !all && r.strike != 0.0 && nums.contains(&r.strike) {
                all = true;
            }
            if !all {
                continue;
            }
        }
        out.push((und, r));
    }
    let primary = ts.first().cloned();
    out.sort_by_key(|(u, r)| {
        let p = primary.as_deref();
        let exact = !matches!((p, u.as_deref()), (Some(p), Some(u)) if u == p);
        let starts = !matches!((p, u.as_deref()), (Some(p), Some(u)) if u.starts_with(p));
        let sym = !p.is_some_and(|p| r.symbol.to_uppercase().starts_with(p));
        (exact, starts, sym, r.symbol.clone())
    });
    out.into_iter().take(FNO_LIMIT).map(|(_, r)| r).collect()
}

/// Comma-separated multi-value parameter, upper-cased.
pub fn parse_multi(v: Option<&str>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `/search/api/search`.
pub fn api_search(
    rows: &[SymToken],
    f: &FnoFilter<'_>,
    exchanges: &[String],
    inst_types: &[String],
) -> Value {
    api_search_with(rows, f, exchanges, inst_types, &|_| None)
}

/// `api_search` with each row's contract multiplier (web `search.py`
/// returns `contract_value` from the master).
pub fn api_search_with(
    rows: &[SymToken],
    f: &FnoFilter<'_>,
    exchanges: &[String],
    inst_types: &[String],
    contract_value: &dyn Fn(&SymToken) -> Option<f64>,
) -> Value {
    let has_fno_filters = f.expiry.is_some()
        || !inst_types.is_empty()
        || f.underlying.is_some()
        || f.strike_min.is_some_and(|x| x != 0.0)
        || f.strike_max.is_some_and(|x| x != 0.0);
    if f.query.is_none() && exchanges.is_empty() {
        return json!({"results": [], "total": 0});
    }
    let exch: Vec<Option<&str>> = if exchanges.is_empty() {
        vec![None]
    } else {
        exchanges.iter().map(|e| Some(e.as_str())).collect()
    };
    let insts: Vec<Option<&str>> = if inst_types.is_empty() {
        vec![None]
    } else {
        inst_types.iter().map(|e| Some(e.as_str())).collect()
    };
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for e in &exch {
        let fno = has_fno_filters || e.is_some_and(is_fno);
        for i in &insts {
            let found = if fno {
                fno_search(
                    rows,
                    &FnoFilter {
                        exchange: *e,
                        instrumenttype: *i,
                        ..f.clone()
                    },
                )
            } else {
                enhanced_search(rows, f.query, *e)
            };
            for r in found {
                if seen.insert((r.symbol.clone(), r.exchange.clone())) {
                    out.push(api_row_with(r, contract_value(r)));
                }
            }
        }
    }
    let total = out.len();
    json!({"results": out, "total": total})
}

fn expiry_date(e: &str) -> NaiveDate {
    parse_oa_expiry(e)
        .or_else(|| NaiveDate::parse_from_str(&title(e), "%d-%b-%Y").ok())
        .unwrap_or(NaiveDate::MAX)
}

fn title(e: &str) -> String {
    e.split('-')
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn sorted_expiries(set: BTreeSet<String>) -> Vec<String> {
    let mut v: Vec<(NaiveDate, String)> = set.into_iter().map(|e| (expiry_date(&e), e)).collect();
    v.sort();
    v.into_iter().map(|(_, e)| e).collect()
}

/// `/search/api/expiries`. With `instrumenttype` the web reads the table
/// (underlying by `name`, options or futures, expired dates kept); without
/// it the cache (underlying from the symbol, live dates only).
pub fn expiries(
    rows: &[SymToken],
    exchange: Option<&str>,
    underlying: Option<&str>,
    instrumenttype: Option<&str>,
    today: NaiveDate,
) -> Vec<String> {
    let und = underlying.map(|u| u.trim().to_uppercase());
    let mut set = BTreeSet::new();
    if let Some(it) = instrumenttype {
        let wanted = it.trim().to_lowercase();
        for r in rows {
            if exchange.is_some_and(|e| r.exchange != e) || r.expiry.is_empty() {
                continue;
            }
            if und
                .as_deref()
                .is_some_and(|u| !r.name.eq_ignore_ascii_case(u))
            {
                continue;
            }
            let keep = match wanted.as_str() {
                "options" | "option" => r.instrument_type == "CE" || r.instrument_type == "PE",
                "futures" | "future" | "fut" => r.instrument_type == "FUT",
                _ => true,
            };
            if keep {
                set.insert(r.expiry.clone());
            }
        }
        return sorted_expiries(set);
    }
    for r in rows {
        if r.expiry.is_empty() || exchange.is_some_and(|e| r.exchange != e) {
            continue;
        }
        if let Some(u) = &und {
            if exchange.is_some()
                && extract_underlying(&r.symbol, &r.exchange).as_deref() != Some(u.as_str())
            {
                continue;
            }
        }
        set.insert(r.expiry.clone());
    }
    sorted_expiries(set)
        .into_iter()
        .filter(|e| expiry_date(e) >= today)
        .collect()
}

/// `/search/api/underlyings`: options-bearing underlyings, or with
/// `include_futures` also those with a live future; exchange test symbols
/// removed; sorted.
pub fn underlyings(
    rows: &[SymToken],
    exchange: Option<&str>,
    include_futures: bool,
    today: NaiveDate,
) -> Vec<String> {
    let mut set = BTreeSet::new();
    for r in rows {
        if exchange.is_some_and(|e| r.exchange != e) {
            continue;
        }
        let Some(u) = extract_underlying(&r.symbol, &r.exchange) else {
            continue;
        };
        let su = r.symbol.to_uppercase();
        let option = su.ends_with("CE") || su.ends_with("PE");
        let live_future = include_futures
            && su.ends_with("FUT")
            && parse_oa_expiry(&r.expiry).is_some_and(|d| d >= today);
        if option || live_future {
            set.insert(u);
        }
    }
    set.into_iter()
        .filter(|u| !u.contains("NSETEST") && !u.contains("BSETEST"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        symbol: &str,
        exchange: &str,
        name: &str,
        expiry: &str,
        strike: f64,
        it: &str,
    ) -> SymToken {
        SymToken {
            symbol: symbol.into(),
            brsymbol: symbol.into(),
            name: name.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: format!("T{}", symbol.len()),
            expiry: expiry.into(),
            strike,
            lot_size: 1,
            instrument_type: it.into(),
            tick_size: 0.05,
        }
    }

    /// Web #2164: brokers reuse a token across exchanges (Shoonya NSE INFY
    /// and CDS EURINR26NOV26113CE are both 1594). The web's unfiltered
    /// search walked a dict keyed by token alone, so one row hid the other;
    /// the desktop keys the master by exchange and token and searches every
    /// row, so both are found whatever the insertion order.
    #[test]
    fn a_token_shared_across_exchanges_hides_neither_row() {
        use crate::brokers::common::symbols::SymbolGeneration;
        let mut infy = row("INFY", "NSE", "INFY", "", -1.0, "EQ");
        infy.token = "1594".into();
        let mut cds = row(
            "EURINR26NOV26113CE",
            "CDS",
            "EURINR",
            "26-NOV-26",
            113.0,
            "CE",
        );
        cds.token = "1594".into();
        for rows in [
            vec![infy.clone(), cds.clone()],
            vec![cds.clone(), infy.clone()],
        ] {
            let g = SymbolGeneration::build(rows, 1);
            assert_eq!(g.len(), 2);
            let found = enhanced_search(g.rows(), Some("INFY"), None);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].exchange, "NSE");
            let found = enhanced_search(g.rows(), Some("EURINR26NOV26113CE"), None);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].exchange, "CDS");
            assert_eq!(enhanced_search(g.rows(), Some("1594"), None).len(), 2);
            assert_eq!(g.search_prefix("INFY", None, 10).len(), 1);
        }
    }

    #[test]
    fn underlying_extraction_matches_the_web_regexes() {
        assert_eq!(
            extract_underlying("NIFTY28MAR2420800CE", "NFO").as_deref(),
            Some("NIFTY")
        );
        assert_eq!(
            extract_underlying("BANKNIFTY24APR24FUT", "NFO").as_deref(),
            Some("BANKNIFTY")
        );
        assert_eq!(
            extract_underlying("M&M28MAR24FUT", "NFO").as_deref(),
            Some("M&M")
        );
        assert_eq!(
            extract_underlying("USDINR28MAR2483.25CE", "CDS").as_deref(),
            Some("USDINR")
        );
        assert_eq!(
            extract_underlying("BTC28FEB2580000CE", "CRYPTO").as_deref(),
            Some("BTC")
        );
        assert_eq!(
            extract_underlying("1INCH28FEB25FUT", "CRYPTO").as_deref(),
            Some("1INCH")
        );
        assert_eq!(
            extract_underlying("BTCUSD.P", "CRYPTO").as_deref(),
            Some("BTC")
        );
        assert_eq!(extract_underlying("SBIN", "NSE"), None);
        assert_eq!(extract_underlying("SBIN", "NFO"), None);
    }

    #[test]
    fn searches_filter_and_rank() {
        let rows = vec![
            row(
                "BANKNIFTY27OCT26FUT",
                "NFO",
                "BANKNIFTY",
                "27-OCT-26",
                0.0,
                "FUT",
            ),
            row(
                "NIFTY27OCT2622000CE",
                "NFO",
                "NIFTY",
                "27-OCT-26",
                22000.0,
                "CE",
            ),
            row("NIFTY27OCT26FUT", "NFO", "NIFTY", "27-OCT-26", 0.0, "FUT"),
            row("NIFTY01JAN25FUT", "NFO", "NIFTY", "01-JAN-25", 0.0, "FUT"),
            row("SBIN", "NSE", "STATE BANK", "", 0.0, "EQ"),
            row("NIFTY", "NSE_INDEX", "NIFTY 50", "", 0.0, "EQ"),
        ];
        let f = FnoFilter {
            query: Some("nifty"),
            ..Default::default()
        };
        let r = fno_search(
            &rows,
            &FnoFilter {
                exchange: Some("NFO"),
                ..f.clone()
            },
        );
        assert_eq!(r[0].symbol, "NIFTY01JAN25FUT");
        assert_eq!(r.last().unwrap().symbol, "BANKNIFTY27OCT26FUT");
        let r = fno_search(
            &rows,
            &FnoFilter {
                exchange: Some("NFO"),
                instrumenttype: Some("CE"),
                ..f.clone()
            },
        );
        assert_eq!(r.len(), 1);
        let r = fno_search(
            &rows,
            &FnoFilter {
                query: Some("22000"),
                exchange: Some("NFO"),
                ..Default::default()
            },
        );
        assert_eq!(r.len(), 1);
        assert_eq!(enhanced_search(&rows, Some("state bank"), None).len(), 1);
        assert!(enhanced_search(&rows, None, None).is_empty());
        let v = api_search(&rows, &f, &["NSE".into(), "NFO".into()], &[]);
        assert_eq!(
            v["total"], 4,
            "NSE has no NIFTY row; NFO goes through the F&O search"
        );
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("nifty"), None, today),
            vec!["27-OCT-26"]
        );
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("NIFTY"), Some("futures"), today),
            vec!["01-JAN-25", "27-OCT-26"]
        );
        assert_eq!(underlyings(&rows, Some("NFO"), false, today), vec!["NIFTY"]);
        assert_eq!(
            underlyings(&rows, Some("NFO"), true, today),
            vec!["BANKNIFTY", "NIFTY"]
        );
    }
}
