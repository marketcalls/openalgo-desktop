//! Symbol search for the pages (web `blueprints/search.py` over
//! `database/symbol.py` and the `token_db_enhanced` cache): the AJAX
//! search with F&O filters, distinct expiries and distinct underlyings, all
//! over one generation of the shared symbol master.

use super::core::float;
use super::freeze_qty_service::FreezeLimits;
use super::options_service::listed_live;
use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::common::underlying::{
    contract_kind, is_derivative_of, row_underlying, ContractKind,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

pub use crate::brokers::common::underlying::{extract_underlying, is_fno, FNO_EXCHANGES};

/// Web `fno_search_symbols` default limit.
pub const FNO_LIMIT: usize = 10_000;

/// The public row shape of `/search/api/search`, with the row's contract
/// multiplier (crypto masters; `null` for instruments that have none).
pub fn api_row_with(r: &SymToken, contract_value: Option<f64>, freeze: &FreezeLimits) -> Value {
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
        "freeze_qty": freeze.for_option(&r.symbol, &r.exchange),
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
        let und = row_underlying(r);
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
    api_search_with(
        rows,
        f,
        exchanges,
        inst_types,
        &|_| None,
        &FreezeLimits::default(),
    )
}

/// `api_search` with each row's contract multiplier (web `search.py`
/// returns `contract_value` from the master).
pub fn api_search_with(
    rows: &[SymToken],
    f: &FnoFilter<'_>,
    exchanges: &[String],
    inst_types: &[String],
    contract_value: &dyn Fn(&SymToken) -> Option<f64>,
    freeze: &FreezeLimits,
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
                    out.push(api_row_with(r, contract_value(r), freeze));
                }
            }
        }
    }
    let total = out.len();
    json!({"results": out, "total": total})
}

/// `/search/api/expiries`: the live expiries of an underlying, earliest
/// first, for the option tools' dropdowns.
///
/// One rule for every request, typed or not: a contract belongs to the
/// underlying read from its canonical symbol (never the broker's `name`
/// text, which is a description on Fyers), `instrumenttype` keeps options
/// (`CE`/`PE`) or futures (`FUT`) as the symbol says, so MCX's separate
/// option and future calendars stay apart, and a date is listed until the
/// venue's expiry cut-off on that day. Expired and unreadable dates are
/// never listed: the page selects the first date. The web reads `name`
/// and keeps expired dates on the typed path (CLAUDE.md, known pitfalls).
pub fn expiries(
    rows: &[SymToken],
    exchange: Option<&str>,
    underlying: Option<&str>,
    instrumenttype: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<String> {
    let und = underlying
        .map(|u| u.trim().to_uppercase())
        .filter(|u| !u.is_empty());
    let wanted = instrumenttype.map(|t| t.trim().to_lowercase());
    let mut set: BTreeSet<(&str, &str)> = BTreeSet::new();
    for r in rows {
        if r.expiry.is_empty() || exchange.is_some_and(|e| r.exchange != e) {
            continue;
        }
        if und.as_deref().is_some_and(|u| !is_derivative_of(r, u)) {
            continue;
        }
        let kind = contract_kind(&r.symbol);
        let keep = match wanted.as_deref() {
            Some("options" | "option") => kind.is_some_and(ContractKind::is_option),
            Some("futures" | "future" | "fut") => kind == Some(ContractKind::Future),
            _ => true,
        };
        if keep {
            set.insert((r.expiry.as_str(), r.exchange.as_str()));
        }
    }
    let mut live: Vec<(NaiveDate, &str)> = set
        .into_iter()
        .filter(|(e, x)| listed_live(e, x, now))
        .filter_map(|(e, _)| parse_oa_expiry(e).map(|d| (d, e)))
        .collect();
    live.sort();
    live.dedup();
    live.into_iter().map(|(_, e)| e.to_string()).collect()
}

/// `/search/api/underlyings`: underlyings with a live option, or with
/// `include_futures` also those with a live future; exchange test symbols
/// removed; sorted.
pub fn underlyings(
    rows: &[SymToken],
    exchange: Option<&str>,
    include_futures: bool,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut set = BTreeSet::new();
    // A master holds a few dozen distinct expiries across ~10^5 rows: decide
    // each (expiry, venue) once.
    let mut live: HashMap<(&str, &str), bool> = HashMap::new();
    for r in rows {
        if exchange.is_some_and(|e| r.exchange != e) {
            continue;
        }
        let wanted = match contract_kind(&r.symbol) {
            Some(k) if k.is_option() => true,
            Some(ContractKind::Future) => include_futures,
            _ => false,
        };
        if !wanted
            || !*live
                .entry((r.expiry.as_str(), r.exchange.as_str()))
                .or_insert_with(|| listed_live(&r.expiry, &r.exchange, now))
        {
            continue;
        }
        if let Some(u) = row_underlying(r) {
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
        let now = ist(5, 10, 0);
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("nifty"), None, now),
            vec!["27-OCT-26"]
        );
        // The January 2025 future has expired: a live dropdown never lists
        // it, typed or not (the page would select it first).
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("NIFTY"), Some("futures"), now),
            vec!["27-OCT-26"]
        );
        assert_eq!(underlyings(&rows, Some("NFO"), false, now), vec!["NIFTY"]);
        assert_eq!(
            underlyings(&rows, Some("NFO"), true, now),
            vec!["BANKNIFTY", "NIFTY"]
        );
    }

    /// 2026-10-`d` at `h:m` IST.
    fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        chrono_tz::Asia::Kolkata
            .with_ymd_and_hms(2026, 10, d, h, m, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    /// TOOLS-01: Fyers stores the contract description in `name`. Typed
    /// discovery compared it with the underlying and found nothing, so the
    /// Option Chain had no expiry to select.
    #[test]
    fn typed_discovery_reads_the_underlying_from_the_symbol_not_the_name() {
        let rows = vec![
            row(
                "NIFTY13OCT2625000CE",
                "NFO",
                "NIFTY 13 Oct 26 25000 CE",
                "13-OCT-26",
                25000.0,
                "CE",
            ),
            row(
                "NIFTY20OCT2625000PE",
                "NFO",
                "NIFTY 20 Oct 26 25000 PE",
                "20-OCT-26",
                25000.0,
                "PE",
            ),
            row(
                "NIFTY27OCT26FUT",
                "NFO",
                "NIFTY 27 Oct 26 FUT",
                "27-OCT-26",
                0.0,
                "FUT",
            ),
            // Upstox stores the company name.
            row(
                "VEDL27OCT26292.5CE",
                "NFO",
                "VEDANTA LIMITED",
                "27-OCT-26",
                292.5,
                "CE",
            ),
        ];
        let now = ist(12, 10, 0);
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("NIFTY"), Some("options"), now),
            ["13-OCT-26", "20-OCT-26"]
        );
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("NIFTY"), Some("futures"), now),
            ["27-OCT-26"]
        );
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("VEDL"), Some("options"), now),
            ["27-OCT-26"]
        );
        // The description itself is not an underlying.
        assert!(expiries(
            &rows,
            Some("NFO"),
            Some("NIFTY 13 Oct 26 25000 CE"),
            Some("options"),
            now
        )
        .is_empty());
    }

    /// TOOLS-01/T02: the identity is exact (no prefix or wildcard), keeps
    /// digits, `&` and `-`, and each venue's calendar is its own.
    #[test]
    fn discovery_keeps_underlyings_and_venues_apart() {
        let o = |sym: &str, ex: &str, exp: &str| {
            let it = &sym[sym.len() - 2..];
            row(sym, ex, "SAME NAME FOR EVERY ROW", exp, 1.0, it)
        };
        let rows = vec![
            o("NIFTY13OCT2625000CE", "NFO", "13-OCT-26"),
            o("NIFTYNXT5027OCT2668000CE", "NFO", "27-OCT-26"),
            o("BANKNIFTY28OCT2651000PE", "NFO", "28-OCT-26"),
            o("FINNIFTY27OCT2623000CE", "NFO", "27-OCT-26"),
            o("M&M27OCT263000CE", "NFO", "27-OCT-26"),
            o("M27OCT2610CE", "NFO", "29-OCT-26"),
            o("BAJAJ-AUTO27OCT269000PE", "NFO", "27-OCT-26"),
            o("360ONE27OCT261200CE", "NFO", "27-OCT-26"),
            o("SENSEX15OCT2682000CE", "BFO", "15-OCT-26"),
            o("SENSEX5016OCT2627000PE", "BFO", "16-OCT-26"),
            o("USDINR28OCT2687.5CE", "CDS", "28-OCT-26"),
        ];
        let now = ist(12, 10, 0);
        let opts = |ex: &str, u: &str| expiries(&rows, Some(ex), Some(u), Some("options"), now);
        assert_eq!(opts("NFO", "NIFTY"), ["13-OCT-26"]);
        assert_eq!(opts("NFO", "NIFTYNXT50"), ["27-OCT-26"]);
        assert_eq!(opts("NFO", "BANKNIFTY"), ["28-OCT-26"]);
        assert_eq!(opts("NFO", "FINNIFTY"), ["27-OCT-26"]);
        assert_eq!(opts("NFO", "M&M"), ["27-OCT-26"]);
        assert_eq!(opts("NFO", "m"), ["29-OCT-26"]);
        assert_eq!(opts("NFO", "BAJAJ-AUTO"), ["27-OCT-26"]);
        assert_eq!(opts("NFO", "360ONE"), ["27-OCT-26"]);
        assert!(opts("NFO", "NIFT").is_empty());
        assert!(opts("NFO", "NIFTY%").is_empty());
        assert!(opts("NFO", "SENSEX").is_empty(), "SENSEX lists on BFO");
        assert_eq!(opts("BFO", "SENSEX"), ["15-OCT-26"]);
        assert_eq!(opts("BFO", "SENSEX50"), ["16-OCT-26"]);
        assert_eq!(opts("CDS", "USDINR"), ["28-OCT-26"]);
        let mut under = underlyings(&rows, Some("NFO"), false, now);
        under.sort();
        assert_eq!(
            under,
            [
                "360ONE",
                "BAJAJ-AUTO",
                "BANKNIFTY",
                "FINNIFTY",
                "M",
                "M&M",
                "NIFTY",
                "NIFTYNXT50"
            ]
        );
    }

    /// MCX lists options and futures on different days (GOLDM futures
    /// expire on the 5th, its options weeks earlier): each request gets
    /// its own calendar.
    #[test]
    fn option_and_future_calendars_stay_separate() {
        let rows = vec![
            row("GOLDM05NOV26FUT", "MCX", "GOLDM", "05-NOV-26", 0.0, "FUT"),
            row("GOLDM05DEC26FUT", "MCX", "GOLDM", "05-DEC-26", 0.0, "FUT"),
            row(
                "GOLDM27OCT26120000CE",
                "MCX",
                "GOLDM",
                "27-OCT-26",
                120000.0,
                "CE",
            ),
            row(
                "GOLDM25NOV26120000PE",
                "MCX",
                "GOLDM",
                "25-NOV-26",
                120000.0,
                "OPTFUT",
            ),
        ];
        let now = ist(12, 10, 0);
        assert_eq!(
            expiries(&rows, Some("MCX"), Some("GOLDM"), Some("options"), now),
            ["27-OCT-26", "25-NOV-26"],
            "options are read from the symbol, whatever the broker's type"
        );
        assert_eq!(
            expiries(&rows, Some("MCX"), Some("GOLDM"), Some("futures"), now),
            ["05-NOV-26", "05-DEC-26"]
        );
        assert_eq!(
            expiries(&rows, Some("MCX"), Some("GOLDM"), None, now),
            ["27-OCT-26", "05-NOV-26", "25-NOV-26", "05-DEC-26"]
        );
    }

    /// TOOLS-03/T04: expired and unreadable dates are never listed; a
    /// contract expiring today stays until its venue's cut-off (NFO 15:30,
    /// CDS 12:30, MCX 23:30, crypto 17:30 IST), and the next day's first
    /// request no longer lists it without any restart.
    #[test]
    fn live_discovery_drops_expired_and_unreadable_dates_at_the_venue_cutoff() {
        let rows = vec![
            row(
                "NIFTY06OCT2622400PE",
                "NFO",
                "NIFTY",
                "06-OCT-26",
                1.0,
                "PE",
            ),
            row(
                "NIFTY13OCT2622400PE",
                "NFO",
                "NIFTY",
                "13-OCT-26",
                1.0,
                "PE",
            ),
            row(
                "NIFTY20OCT2622400PE",
                "NFO",
                "NIFTY",
                "20-OCT-26",
                1.0,
                "PE",
            ),
            row(
                "NIFTY99XYZ2622400PE",
                "NFO",
                "NIFTY",
                "99-XYZ-26",
                1.0,
                "PE",
            ),
            row(
                "NIFTY31FEB2722400PE",
                "NFO",
                "NIFTY",
                "31-FEB-27",
                1.0,
                "PE",
            ),
            row("NIFTY10NOV2622400CE", "NFO", "NIFTY", "garbage", 1.0, "CE"),
            row(
                "USDINR13OCT2687.5CE",
                "CDS",
                "USDINR",
                "13-OCT-26",
                1.0,
                "CE",
            ),
            row(
                "USDINR27OCT2687.5CE",
                "CDS",
                "USDINR",
                "27-OCT-26",
                1.0,
                "CE",
            ),
            row(
                "CRUDEOIL13OCT266000CE",
                "MCX",
                "CRUDEOIL",
                "13-OCT-26",
                1.0,
                "CE",
            ),
            row(
                "CRUDEOIL16NOV266000CE",
                "MCX",
                "CRUDEOIL",
                "16-NOV-26",
                1.0,
                "CE",
            ),
            row(
                "BTC13OCT26100000CE",
                "CRYPTO",
                "BTC",
                "13-OCT-26",
                1.0,
                "CE",
            ),
            row(
                "BTC14OCT26100000CE",
                "CRYPTO",
                "BTC",
                "14-OCT-26",
                1.0,
                "CE",
            ),
        ];
        let at = |ex: &str, u: &str, now| expiries(&rows, Some(ex), Some(u), Some("options"), now);
        // The day before: only the 6th is gone.
        assert_eq!(
            at("NFO", "NIFTY", ist(12, 10, 0)),
            ["13-OCT-26", "20-OCT-26"]
        );
        // Expiry day, before and after each venue's cut-off.
        assert_eq!(
            at("NFO", "NIFTY", ist(13, 15, 29)),
            ["13-OCT-26", "20-OCT-26"]
        );
        assert_eq!(at("NFO", "NIFTY", ist(13, 15, 30)), ["20-OCT-26"]);
        assert_eq!(
            at("CDS", "USDINR", ist(13, 12, 29)),
            ["13-OCT-26", "27-OCT-26"]
        );
        assert_eq!(at("CDS", "USDINR", ist(13, 12, 30)), ["27-OCT-26"]);
        assert_eq!(
            at("MCX", "CRUDEOIL", ist(13, 23, 29)),
            ["13-OCT-26", "16-NOV-26"]
        );
        assert_eq!(at("MCX", "CRUDEOIL", ist(13, 23, 30)), ["16-NOV-26"]);
        assert_eq!(
            at("CRYPTO", "BTC", ist(13, 17, 29)),
            ["13-OCT-26", "14-OCT-26"]
        );
        assert_eq!(at("CRYPTO", "BTC", ist(13, 17, 30)), ["14-OCT-26"]);
        // Overnight: the first request after midnight no longer lists it.
        assert_eq!(at("NFO", "NIFTY", ist(14, 0, 1)), ["20-OCT-26"]);
        assert_eq!(at("MCX", "CRUDEOIL", ist(14, 0, 1)), ["16-NOV-26"]);
        // Untyped discovery follows the same rule.
        assert_eq!(
            expiries(&rows, Some("NFO"), Some("NIFTY"), None, ist(13, 15, 30)),
            ["20-OCT-26"]
        );
    }
}
