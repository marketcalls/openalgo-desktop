//! Security master (web `database/master_contract_db.py`).
//!
//! `GET /oapi/v1/security-master` is a public plain CSV (no auth) with the
//! columns `exchange, security_id, instrument_segment, expiry_date,
//! strike_price, option_type, lot_size, tick_size, close_price,
//! exch_security_id, symbol_name, underline_symbol, open_price`.
//!
//! * `exchange` is only NSE / BSE / MCX; NFO / BFO / CDS come from
//!   `instrument_segment`. Indices are EQUITY rows in reserved token bands
//!   (NSE 26000-26999, BSE 999 and below).
//! * token = `exch_security_id` (market data), brsymbol = `security_id`
//!   (orders), brexchange = the parent exchange. `DUMMY<n>` tokens (MF and
//!   NCD placeholders) are dropped.
//! * BSE cash rows with a blank `symbol_name` borrow the NSE name of the
//!   same `security_id`, else use the `security_id`.
//! * Duplicate `(symbol, exchange)` pairs keep the best `_cash_rank` row
//!   (the real equity rather than the issuer's bonds).
//! * Tick sizes are paise when integral (`5` -> 0.05) and rupees when
//!   decimal (`.0025`).

use super::{HdfcSecuritiesBroker, USER_AGENT};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, format_strike, split_csv_line, CsvHeader,
};
use crate::brokers::types::SymbolData;
use crate::error::{AppError, Result};
use chrono::NaiveDate;
use std::collections::HashMap;

/// `(exchange, instrument_segment)` -> OpenAlgo exchange for non-cash rows.
pub const SEGMENT_EXCHANGE: &[((&str, &str), &str)] = &[
    (("NSE", "FUTIDX"), "NFO"),
    (("NSE", "OPTIDX"), "NFO"),
    (("NSE", "FUTSTK"), "NFO"),
    (("NSE", "OPTSTK"), "NFO"),
    (("NSE", "FUTCUR"), "CDS"),
    (("NSE", "OPTCUR"), "CDS"),
    (("BSE", "FUTIDX"), "BFO"),
    (("BSE", "OPTIDX"), "BFO"),
    (("MCX", "FUTCOM"), "MCX"),
    (("MCX", "OPTFUT"), "MCX"),
    (("MCX", "FUTIDX"), "MCX"),
    (("MCX", "OPTIDX"), "MCX"),
];

/// Index display names that differ from the OpenAlgo symbol, plus the NSE
/// index rows named only by `security_id` (web `_INDEX_SYMBOL_OVERRIDES`).
pub const INDEX_SYMBOL_OVERRIDES: &[(&str, &str)] = &[
    ("NIFTYMID50", "NIFTYMIDCAP50"),
    ("MINIFTYEQ", "MINIFTY"),
    ("DEFTYEQ", "DEFTY"),
    ("CNX100EQ", "NIFTY100"),
];

/// Index row -> OpenAlgo index symbol: upper case without spaces, from the
/// display name or else the `security_id`, then the override table.
pub fn classify_index_symbol(display: &str, security_id: &str) -> String {
    let squash = |s: &str| -> String {
        s.to_ascii_uppercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let mut key = squash(display);
    if key.is_empty() {
        key = squash(security_id);
    }
    INDEX_SYMBOL_OVERRIDES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or(key)
}

/// Lower wins: the real equity among rows sharing a display name.
pub fn cash_rank(security_id: &str) -> u8 {
    let code = security_id.to_ascii_uppercase();
    let equity = code.ends_with("EQNR") || code.ends_with("EQTT");
    if code.len() == 10 && equity {
        0
    } else if equity {
        1
    } else if !(code.ends_with("NR") || code.ends_with("TT")) {
        2
    } else {
        3
    }
}

/// Paise when integral, rupees when the text has a decimal point.
pub fn tick_size(raw: &str) -> f64 {
    let v: f64 = raw.trim().parse().unwrap_or(0.0);
    if raw.contains('.') {
        v
    } else {
        v / 100.0
    }
}

fn bad_format() -> AppError {
    AppError::Broker(
        "The HDFC Securities instrument list has an unexpected format. Try downloading the master contract again later."
            .into(),
    )
}

struct Raw {
    brexchange: String,
    segment: String,
    security_id: String,
    expiry: String,
    strike: String,
    option_type: String,
    lot: String,
    tick: String,
    token: String,
    symbol_name: String,
}

/// Parse the security-master CSV into master rows (web
/// `process_security_master`).
pub fn parse_security_master(text: &str) -> Result<Vec<SymbolData>> {
    let mut lines = text.lines();
    let header = CsvHeader::parse(lines.next().ok_or_else(bad_format)?);
    let col = |n: &str| header.index(n).ok_or_else(bad_format);
    let (c_ex, c_sid, c_seg, c_exp, c_strike, c_opt, c_lot, c_tick, c_tok, c_name) = (
        col("exchange")?,
        col("security_id")?,
        col("instrument_segment")?,
        col("expiry_date")?,
        col("strike_price")?,
        col("option_type")?,
        col("lot_size")?,
        col("tick_size")?,
        col("exch_security_id")?,
        col("symbol_name")?,
    );
    let raws: Vec<Raw> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f = split_csv_line(l);
            let g = |i: usize| f.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
            Raw {
                brexchange: g(c_ex).to_ascii_uppercase(),
                segment: g(c_seg).to_ascii_uppercase(),
                security_id: g(c_sid),
                expiry: g(c_exp),
                strike: g(c_strike),
                option_type: g(c_opt).to_ascii_uppercase(),
                lot: g(c_lot),
                tick: g(c_tick),
                token: g(c_tok),
                symbol_name: g(c_name),
            }
        })
        .collect();

    // NSE cash names by security_id, for BSE rows with a blank name.
    let mut nse_names: HashMap<&str, &str> = HashMap::new();
    for r in &raws {
        if r.segment == "EQUITY" && r.brexchange == "NSE" && !r.symbol_name.is_empty() {
            nse_names
                .entry(r.security_id.as_str())
                .or_insert(r.symbol_name.as_str());
        }
    }

    // (rank, original index, row)
    let mut kept: Vec<(u8, usize, SymbolData)> = Vec::new();
    for (idx, r) in raws.iter().enumerate() {
        let Ok(token_num) = r.token.parse::<f64>() else {
            continue;
        };
        if r.security_id.is_empty() || !token_num.is_finite() {
            continue;
        }
        let is_cash = r.segment == "EQUITY";
        let mut exchange = if is_cash {
            r.brexchange.clone()
        } else {
            SEGMENT_EXCHANGE
                .iter()
                .find(|((e, s), _)| *e == r.brexchange && *s == r.segment)
                .map(|(_, oa)| (*oa).to_string())
                .unwrap_or_default()
        };
        let is_nse_index =
            is_cash && r.brexchange == "NSE" && (26000.0..=26999.0).contains(&token_num);
        let is_bse_index = is_cash && r.brexchange == "BSE" && token_num <= 999.0;
        if is_nse_index {
            exchange = "NSE_INDEX".into();
        } else if is_bse_index {
            exchange = "BSE_INDEX".into();
        }
        // COM / UNDCUR underlyings are not tradable.
        if exchange.is_empty() {
            continue;
        }
        let expiry = NaiveDate::parse_from_str(&r.expiry, "%Y-%m-%d")
            .map(format_expiry)
            .unwrap_or_default();
        let compact = expiry.replace('-', "");
        // Futures carry placeholder strikes (-0.01, -1e-7, 0).
        let strike = r.strike.parse::<f64>().unwrap_or(0.0).max(0.0) + 0.0;
        let lot = r.lot.parse::<f64>().map(|v| v as i64).unwrap_or(0);
        let lot = if lot > 0 && lot < 100_000_000 {
            lot as i32
        } else {
            1
        };
        let is_option = matches!(r.option_type.as_str(), "CE" | "PE");
        let is_future = !is_option && !is_cash && !expiry.is_empty();
        let is_deriv = is_option || is_future;

        let cash_symbol = if !r.symbol_name.is_empty() {
            r.symbol_name.clone()
        } else if let Some(n) = nse_names.get(r.security_id.as_str()) {
            (*n).to_string()
        } else {
            r.security_id.clone()
        };
        let symbol = if is_nse_index || is_bse_index {
            classify_index_symbol(&r.symbol_name, &r.security_id)
        } else if is_option {
            format!(
                "{}{}{}{}",
                r.symbol_name,
                compact,
                format_strike(strike),
                r.option_type
            )
        } else if is_future {
            format!("{}{}FUT", r.symbol_name, compact)
        } else {
            cash_symbol
        };
        if symbol.is_empty() {
            continue;
        }
        let instrument_type = if is_option {
            r.option_type.clone()
        } else if is_future {
            "FUT".into()
        } else {
            "EQ".into()
        };
        let name = if is_deriv {
            r.symbol_name.clone()
        } else {
            symbol.clone()
        };
        let rank = if is_deriv {
            0
        } else {
            cash_rank(&r.security_id)
        };
        kept.push((
            rank,
            idx,
            SymbolData {
                symbol,
                brsymbol: r.security_id.clone(),
                name,
                exchange,
                brexchange: r.brexchange.clone(),
                token: r.token.clone(),
                expiry,
                strike,
                lot_size: lot,
                instrument_type,
                tick_size: tick_size(&r.tick),
            },
        ));
    }

    // Keep the best-ranked row per (symbol, exchange), then file order.
    let mut best: HashMap<(String, String), (u8, usize)> = HashMap::new();
    for (rank, idx, row) in &kept {
        let k = (row.symbol.clone(), row.exchange.clone());
        let e = best.entry(k).or_insert((*rank, *idx));
        if (*rank, *idx) < *e {
            *e = (*rank, *idx);
        }
    }
    let out: Vec<SymbolData> = kept
        .into_iter()
        .filter(|(_, idx, row)| {
            best.get(&(row.symbol.clone(), row.exchange.clone()))
                .is_some_and(|(_, i)| i == idx)
        })
        .map(|(_, _, row)| row)
        .collect();
    if out.is_empty() {
        return Err(AppError::Broker(
            "The HDFC Securities instrument list was empty. Your existing master contract was kept; try again later."
                .into(),
        ));
    }
    Ok(out)
}

pub async fn download(b: &HdfcSecuritiesBroker) -> Result<Vec<SymbolData>> {
    let resp = b
        .http
        .get(&b.urls.master)
        .header("User-Agent", USER_AGENT)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| super::redact(e.into()))?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "HDFC Securities security master download refused"
        );
        return Err(AppError::Broker(
            "HDFC Securities did not send the instrument list. Try downloading the master contract again shortly."
                .into(),
        ));
    }
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    let text = String::from_utf8_lossy(&bytes);
    let rows = parse_security_master(&text)?;
    tracing::info!(
        "HDFC Securities master contract: {} instruments",
        rows.len()
    );
    Ok(rows)
}
