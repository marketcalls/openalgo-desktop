//! Symbol services (web `symbol_service.py`, `search_service.py`,
//! `expiry_service.py`, `instruments_service.py`, `qty_freeze_db.py`).
//!
//! Everything reads one consistent generation of the shared symbol master.

use super::core::{float, Reply};
use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::symbols::{SymToken, SymbolGeneration};
use crate::state::AppState;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

// ------------------------------------------------------------------ freeze

/// NSE's `qtyfreeze.csv`, seeded for NFO exactly as the web seeds its table.
const QTY_FREEZE_CSV: &str = include_str!("qtyfreeze.csv");

fn freeze_table() -> &'static HashMap<String, i64> {
    static T: OnceLock<HashMap<String, i64>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        let mut lines = QTY_FREEZE_CSV.lines();
        let header: Vec<String> = lines
            .next()
            .unwrap_or_default()
            .split(',')
            .map(|h| h.trim().to_ascii_uppercase())
            .collect();
        let sym_i = header.iter().position(|h| h == "SYMBOL");
        let frz_i = header.iter().position(|h| h.contains("FRZ"));
        if let (Some(si), Some(fi)) = (sym_i, frz_i) {
            for line in lines {
                let cols: Vec<&str> = line.split(',').collect();
                let (Some(s), Some(q)) = (cols.get(si), cols.get(fi)) else {
                    continue;
                };
                if let Ok(q) = q.trim().parse::<i64>() {
                    let s = s.trim();
                    if !s.is_empty() {
                        m.insert(format!("NFO:{}", s), q);
                    }
                }
            }
        }
        m
    })
}

/// Web `get_freeze_qty`: 0 when no limit is configured.
pub fn freeze_qty(symbol: &str, exchange: &str) -> i64 {
    freeze_table()
        .get(&format!("{}:{}", exchange, symbol))
        .copied()
        .unwrap_or(0)
}

/// Web `get_freeze_qty_for_option`: the underlying from an F&O symbol.
pub fn freeze_qty_for_option(symbol: &str, exchange: &str) -> i64 {
    let up = symbol.to_ascii_uppercase();
    for idx in [
        "BANKNIFTY",
        "FINNIFTY",
        "MIDCPNIFTY",
        "NIFTYNXT50",
        "NIFTY",
        "SENSEX50",
        "BANKEX",
        "SENSEX",
    ] {
        if up.starts_with(idx) {
            return freeze_qty(idx, exchange);
        }
    }
    let base: String = up
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || *c == '&' || *c == '-')
        .collect();
    if base.is_empty() {
        0
    } else {
        freeze_qty(&base, exchange)
    }
}

// ------------------------------------------------------------------ rows

/// A master row of `snap` in the web's `symbol`/`search`/`instruments`
/// shape (`lotsize` exact for a fractional crypto lot, MC-04).
pub fn row_json(snap: &SymbolGeneration, r: &SymToken, with_freeze: bool) -> Value {
    let mut v = json!({
        "symbol": r.symbol,
        "brsymbol": r.brsymbol,
        "name": r.name,
        "exchange": r.exchange,
        "brexchange": r.brexchange,
        "token": r.token,
        "expiry": r.expiry,
        "strike": float(r.strike),
        "lotsize": snap.lotsize_json(r),
        "instrumenttype": r.instrument_type,
        "tick_size": float(r.tick_size),
    });
    if with_freeze {
        if let Some(m) = v.as_object_mut() {
            m.insert(
                "freeze_qty".into(),
                json!(freeze_qty_for_option(&r.symbol, &r.exchange)),
            );
        }
    }
    v
}

/// `symbol`.
pub fn symbol(ctx: &AppState, symbol: &str, exchange: &str) -> Reply {
    let snap = ctx.symbols.snapshot();
    let Some(r) = snap.by_symbol(exchange, symbol) else {
        return Reply::error(
            404,
            format!("Symbol {} not found in exchange {}", symbol, exchange),
        );
    };
    // The row's position in the master stands in for the table id.
    let rows = snap.rows();
    let ptr = r as *const SymToken;
    let id = if rows.as_ptr_range().contains(&ptr) {
        (ptr as usize - rows.as_ptr() as usize) / std::mem::size_of::<SymToken>() + 1
    } else {
        0
    };
    let mut data = row_json(&snap, r, true);
    if let Some(m) = data.as_object_mut() {
        m.insert("id".into(), json!(id));
    }
    Reply::ok(json!({"status": "success", "data": data}))
}

// ------------------------------------------------------------------ search

/// Web search cache limit.
pub const SEARCH_LIMIT: usize = 500;

/// Web `search_symbols` (cache path): every whitespace term must match the
/// symbol, broker symbol, name or token (or equal the strike); ranked exact,
/// prefix, symbol-contains, other; then shorter symbols, then alphabetical.
pub fn search_rows<'a>(
    rows: &'a [SymToken],
    query: &str,
    exchange: Option<&str>,
) -> Vec<&'a SymToken> {
    let q = query.trim();
    let q_up = q.to_ascii_uppercase();
    let terms: Vec<String> = q
        .split_whitespace()
        .map(|t| t.to_ascii_uppercase())
        .collect();
    let numeric: Vec<f64> = terms.iter().filter_map(|t| t.parse::<f64>().ok()).collect();
    let pool: Vec<(usize, &SymToken)> = {
        let filtered: Vec<(usize, &SymToken)> = match exchange {
            Some(e) => rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.exchange == e)
                .collect(),
            None => Vec::new(),
        };
        if filtered.is_empty() {
            rows.iter().enumerate().collect()
        } else {
            filtered
        }
    };
    let mut hits: Vec<(u8, usize, String, usize, &SymToken)> = Vec::new();
    for (seq, r) in pool {
        let sym = r.symbol.to_ascii_uppercase();
        let br = r.brsymbol.to_ascii_uppercase();
        let name = r.name.to_ascii_uppercase();
        let strike_hit = r.strike != 0.0 && numeric.contains(&r.strike);
        let all = terms.iter().all(|t| {
            sym.contains(t.as_str())
                || br.contains(t.as_str())
                || (!name.is_empty() && name.contains(t.as_str()))
                || (!r.token.is_empty() && r.token.contains(t.as_str()))
                || strike_hit
        });
        if !all {
            continue;
        }
        let score = if sym == q_up {
            0
        } else if sym.starts_with(&q_up) {
            1
        } else if terms.iter().all(|t| sym.contains(t.as_str())) {
            2
        } else {
            3
        };
        hits.push((score, r.symbol.len(), sym, seq, r));
    }
    hits.sort_by(|a, b| (a.0, a.1, &a.2, a.3).cmp(&(b.0, b.1, &b.2, b.3)));
    hits.into_iter().take(SEARCH_LIMIT).map(|h| h.4).collect()
}

/// `search`.
pub fn search(ctx: &AppState, query: &str, exchange: Option<&str>) -> Reply {
    if query.trim().is_empty() {
        return Reply::error(400, "Query parameter is required and cannot be empty");
    }
    let snap = ctx.symbols.snapshot();
    let found = search_rows(snap.rows(), query, exchange);
    if found.is_empty() {
        return Reply::ok(
            json!({"status": "success", "message": "No matching symbols found", "data": []}),
        );
    }
    let data: Vec<Value> = found.iter().map(|r| row_json(&snap, r, true)).collect();
    Reply::ok(json!({
        "status": "success",
        "message": format!("Found {} matching symbols", data.len()),
        "data": data,
    }))
}

// ------------------------------------------------------------------ expiry

fn type_filter(exchange: &str, futures: bool) -> Option<&'static [&'static str]> {
    match (exchange, futures) {
        ("NFO" | "BFO", true) => Some(&["FUTSTK", "FUTIDX", "FUT"]),
        ("NFO" | "BFO", false) => Some(&["OPTSTK", "OPTIDX", "CE", "PE"]),
        ("MCX" | "NCDEX", true) => Some(&["FUTCOM", "FUTENR", "FUT"]),
        ("MCX" | "NCDEX", false) => Some(&["OPTFUT", "CE", "PE"]),
        ("CDS" | "BCD", true) => Some(&["FUTCUR", "FUTIRC", "FUT"]),
        ("CDS" | "BCD", false) => Some(&["OPTCUR", "OPTIRC", "CE", "PE"]),
        ("CRYPTO", true) => Some(&["FUT", "PERPFUT"]),
        ("CRYPTO", false) => Some(&["CE", "PE"]),
        _ => None,
    }
}

/// `SYMBOL` followed by `DDMMMYY`.
fn has_date_after(sym: &str, base: &str) -> bool {
    let Some(rest) = sym.strip_prefix(base) else {
        return false;
    };
    let b = rest.as_bytes();
    b.len() >= 7
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2..5].iter().all(u8::is_ascii_uppercase)
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
}

/// Distinct expiries for an underlying, today or later, earliest first.
pub fn expiry_dates(
    rows: &[SymToken],
    symbol: &str,
    exchange: &str,
    futures: bool,
    today: NaiveDate,
) -> Vec<String> {
    let base = symbol.trim().to_ascii_uppercase();
    let filter = type_filter(exchange, futures);
    let candidates: Vec<&SymToken> = rows
        .iter()
        .filter(|r| r.exchange == exchange && !r.expiry.is_empty())
        .filter(|r| r.symbol.to_ascii_uppercase().starts_with(&base))
        .filter(|r| filter.is_none_or(|f| f.contains(&r.instrument_type.as_str())))
        .collect();
    let mut seen: Vec<String> = Vec::new();
    for r in candidates
        .iter()
        .filter(|r| has_date_after(&r.symbol.to_ascii_uppercase(), &base))
    {
        if !seen.contains(&r.expiry) {
            seen.push(r.expiry.clone());
        }
    }
    let mut dated: Vec<(NaiveDate, String)> = seen
        .into_iter()
        .map(|e| (parse_oa_expiry(&e).unwrap_or(NaiveDate::MAX), e))
        .filter(|(d, _)| *d >= today)
        .collect();
    dated.sort();
    dated.into_iter().map(|(_, e)| e).collect()
}

/// `expiry`.
pub fn expiry(ctx: &AppState, symbol: &str, exchange: &str, instrumenttype: &str) -> Reply {
    if symbol.trim().is_empty() {
        return Reply::error(400, "Symbol parameter is required and cannot be empty");
    }
    let sym = symbol.trim().to_ascii_uppercase();
    let ex = exchange.trim().to_ascii_uppercase();
    let it = instrumenttype.trim().to_ascii_lowercase();
    let today = super::options_service::today_ist(ctx);
    let snap = ctx.symbols.snapshot();
    let dates = expiry_dates(snap.rows(), &sym, &ex, it == "futures", today);
    if dates.is_empty() {
        return Reply::ok(json!({
            "status": "success",
            "message": format!("No expiry dates found for {} {} in {}", sym, it, ex),
            "data": [],
        }));
    }
    Reply::ok(json!({
        "status": "success",
        "message": format!("Found {} expiry dates for {} {} in {}", dates.len(), sym, it, ex),
        "data": dates,
    }))
}

// ------------------------------------------------------------------ instruments

/// CSV field with Python `csv` minimal quoting.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Python `str(float)`.
fn py_float(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}

pub const INSTRUMENT_COLUMNS: &str =
    "symbol,brsymbol,name,exchange,brexchange,token,expiry,strike,lotsize,instrumenttype,tick_size";

/// Instruments of one exchange as the web's CSV download.
pub fn instruments_csv(rows: &[&SymToken]) -> String {
    let mut out = String::with_capacity(rows.len() * 80 + 128);
    out.push_str(INSTRUMENT_COLUMNS);
    out.push_str("\r\n");
    for r in rows {
        let cols = [
            csv_field(&r.symbol),
            csv_field(&r.brsymbol),
            csv_field(&r.name),
            csv_field(&r.exchange),
            csv_field(&r.brexchange),
            csv_field(&r.token),
            csv_field(&r.expiry),
            py_float(r.strike),
            r.lot_size.to_string(),
            csv_field(&r.instrument_type),
            py_float(r.tick_size),
        ];
        out.push_str(&cols.join(","));
        out.push_str("\r\n");
    }
    out
}

/// What `/instruments` answers.
pub enum Instruments {
    Json(Reply),
    Csv { filename: String, body: String },
}

pub fn instruments(ctx: &AppState, exchange: &str, csv: bool) -> Instruments {
    let snap = ctx.symbols.snapshot();
    let rows: Vec<&SymToken> = snap
        .rows()
        .iter()
        .filter(|r| r.exchange == exchange)
        .collect();
    if rows.is_empty() {
        return Instruments::Json(Reply::ok(
            json!({"status": "success", "message": "No instruments found", "data": []}),
        ));
    }
    if csv {
        return Instruments::Csv {
            filename: format!("instruments_{}.csv", exchange),
            body: instruments_csv(&rows),
        };
    }
    let data: Vec<Value> = rows.iter().map(|r| row_json(&snap, r, false)).collect();
    Instruments::Json(Reply::ok(json!({
        "status": "success",
        "message": format!("Found {} instruments", data.len()),
        "data": data,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(symbol: &str, name: &str, exchange: &str, expiry: &str, it: &str) -> SymToken {
        SymToken {
            symbol: symbol.into(),
            brsymbol: symbol.into(),
            name: name.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: "1".into(),
            expiry: expiry.into(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: it.into(),
            tick_size: 0.05,
        }
    }

    #[test]
    fn freeze_quantities_come_from_the_nse_table() {
        assert_eq!(freeze_qty_for_option("NIFTY06OCT2622400CE", "NFO"), 1800);
        assert_eq!(
            freeze_qty_for_option("NIFTYNXT5027OCT26FUT", "NFO"),
            freeze_qty("NIFTYNXT50", "NFO")
        );
        assert_eq!(freeze_qty_for_option("NIFTY", "NSE_INDEX"), 0);
        assert_eq!(freeze_qty_for_option("CRUDEOIL19OCT26FUT", "MCX"), 0);
        assert!(freeze_qty("RELIANCE", "NFO") > 0);
    }

    #[test]
    fn search_ranks_like_the_web() {
        let rows = vec![
            row("RPOWER", "RELIANCE POWER", "NSE", "", "EQ"),
            row("RELIANCE", "RELIANCE INDUSTRIES", "NSE", "", "EQ"),
            row("RIIL", "RELIANCE INDUSTRIAL INFRA", "NSE", "", "EQ"),
            row("RELIANCE", "RELIANCE INDUSTRIES", "BSE", "", "EQ"),
        ];
        let got: Vec<&str> = search_rows(&rows, "reliance", Some("NSE"))
            .iter()
            .map(|r| r.symbol.as_str())
            .collect();
        assert_eq!(got, ["RELIANCE", "RIIL", "RPOWER"]);
        // An exchange with no rows searches everything.
        assert_eq!(search_rows(&rows, "RELIANCE", Some("MCX")).len(), 4);
    }

    #[test]
    fn expiries_filter_underlying_and_past_dates() {
        let rows = vec![
            row("NIFTY27OCT26FUT", "NIFTY", "NFO", "27-OCT-26", "FUT"),
            row("NIFTY29SEP26FUT", "NIFTY", "NFO", "29-SEP-26", "FUT"),
            row("NIFTYFPI27OCT26FUT", "NIFTYFPI", "NFO", "27-OCT-26", "FUT"),
            row("NIFTY23NOV26FUT", "NIFTY", "NFO", "23-NOV-26", "FUT"),
            row("NIFTY06OCT2622400CE", "NIFTY", "NFO", "06-OCT-26", "CE"),
        ];
        let today = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        assert_eq!(
            expiry_dates(&rows, "NIFTY", "NFO", true, today),
            ["27-OCT-26", "23-NOV-26"]
        );
        assert_eq!(
            expiry_dates(&rows, "nifty", "NFO", false, today),
            ["06-OCT-26"]
        );
    }

    #[test]
    fn csv_matches_python_csv_module() {
        let mut r = row("A,B", "X \"Y\"", "NSE", "", "EQ");
        r.lot_size = 11250;
        let csv = instruments_csv(&[&r]);
        let mut lines = csv.split("\r\n");
        assert_eq!(lines.next(), Some(INSTRUMENT_COLUMNS));
        assert_eq!(
            lines.next(),
            Some("\"A,B\",\"A,B\",\"X \"\"Y\"\"\",NSE,NSE,1,,0.0,11250,EQ,0.05")
        );
    }
}
