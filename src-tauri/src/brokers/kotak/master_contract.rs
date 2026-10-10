//! Kotak scrip master -> OpenAlgo symbol master
//! (web `database/master_contract_db.py`).
//!
//! * File URLs come from `GET {base}/script-details/1.0/masterscrip/file-paths`
//!   (`Authorization: <access token>`), tried on the session `baseUrl`, then
//!   two fixed hosts; failing that, today's dated files on Kotak's CDN are
//!   probed with a one-byte ranged GET.
//! * One CSV per segment; header names are compared with spaces and `;`
//!   removed (NSE cash ships `dStrikePrice;` and `dTickSize `).
//! * Cash: `token = pSymbol`, `symbol = pSymbolName`, `brsymbol =
//!   pTrdSymbol`, brexchange the literal `NSE`/`BSE`; a row without an ISIN
//!   is an index (`NSE_INDEX`/`BSE_INDEX`, type EQ). NSE keeps groups EQ and
//!   BE only. Tick size is paise.
//! * F&O: expiry from the `lExpiryDate` epoch (+315513000 on NSE F&O and
//!   CDS, not on MCX and BSE F&O), strike in paise, `XX` is FUT; a
//!   non-positive `lExpiryDate` is a reference row with no expiry and no type.
//!   Symbol `<name><DDMMMYY>FUT` / `<name><DDMMMYY><strike><CE|PE>`.
//! * `bcs_fo` (BCD) is never fetched and `nse_com` has no processor, as on
//!   the web.
//!
//! Token de-duplication is per exchange (the shared symbol master's rule);
//! the web skips a pSymbol already present on any exchange, which drops BSE
//! rows whose pSymbol happens to equal an NSE one.

use super::{KotakBroker, KotakSession};
use crate::brokers::common::history::IST_OFFSET_SECS;
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{format_expiry, parse_broker_expiry, split_csv_line};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use reqwest::StatusCode;
use serde_json::Value;
use std::time::Duration;

/// Epoch offset Kotak's NSE F&O and CDS expiries carry (1980-01-01).
pub const EXPIRY_OFFSET: i64 = 315_513_000;

/// The files processed, in the web's order.
pub const SEGMENT_FILES: &[&str] = &["NSE_CM", "NSE_FO", "BSE_CM", "CDE_FO", "MCX_FO", "BSE_FO"];

/// Bucket a file URL by its basename (web `get_kotak_master_filepaths`).
pub fn file_key(url: &str) -> Option<&'static str> {
    let name = url.rsplit('/').next().unwrap_or(url).to_ascii_lowercase();
    Some(if name.contains("nse_cm") {
        "NSE_CM"
    } else if name.contains("bse_cm") {
        "BSE_CM"
    } else if name.contains("nse_fo") {
        "NSE_FO"
    } else if name.contains("bse_fo") {
        "BSE_FO"
    } else if name.contains("cde_fo") {
        "CDE_FO"
    } else if name.contains("mcx_fo") {
        "MCX_FO"
    } else if name.contains("nse_com") {
        "NSE_COM"
    } else {
        return None;
    })
}

/// The dated CDN fallback URLs for `date` (`YYYY-MM-DD`).
pub fn fallback_urls(cdn: &str, date: &str) -> Vec<(&'static str, String)> {
    let t = |f: &str| format!("{}/{}/transformed/{}.csv", cdn, date, f);
    let v1 = |f: &str| format!("{}/{}/transformed-v1/{}.csv", cdn, date, f);
    vec![
        ("CDE_FO", t("cde_fo")),
        ("MCX_FO", t("mcx_fo")),
        ("NSE_FO", t("nse_fo")),
        ("BSE_FO", t("bse_fo")),
        ("NSE_COM", t("nse_com")),
        ("BSE_CM", v1("bse_cm-v1")),
        ("NSE_CM", v1("nse_cm-v1")),
    ]
}

/// Header lookup with spaces and `;` removed from the names.
struct Header {
    names: Vec<String>,
}

impl Header {
    fn parse(line: &str) -> Self {
        Self {
            names: split_csv_line(line)
                .into_iter()
                .map(|s| s.trim_start_matches('\u{feff}').replace([' ', ';'], ""))
                .collect(),
        }
    }

    fn idx(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
}

fn bad_format(file: &str, col: &str) -> AppError {
    tracing::error!("Kotak {} scrip master has no '{}' column", file, col);
    AppError::Broker(
        "Kotak's instrument list has an unexpected format. Try downloading the master contract again later."
            .into(),
    )
}

fn f(v: &str) -> Option<f64> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("nan") {
        None
    } else {
        t.parse().ok()
    }
}

/// Strike in a symbol: integral strikes without a fraction.
fn strike_text(strike: f64) -> String {
    if strike.fract() == 0.0 && strike.abs() < 1e15 {
        format!("{}", strike as i64)
    } else {
        format!("{}", strike)
    }
}

/// web `combine_details`.
pub fn combine_details(name: &str, expiry: &str, strike: f64, instrument_type: &str) -> String {
    let base = format!("{}{}", name, expiry.replace('-', ""));
    match instrument_type {
        "FUT" => format!("{}FUT", base),
        "CE" | "PE" => format!("{}{}{}", base, strike_text(strike), instrument_type),
        _ => base,
    }
}

/// Expiry text from Kotak's epoch (`None` for the no-expiry sentinel).
pub fn expiry_from_epoch(raw: i64, offset: bool) -> Option<String> {
    if raw <= 0 {
        return None;
    }
    let secs = if offset { raw + EXPIRY_OFFSET } else { raw };
    chrono::DateTime::from_timestamp(secs, 0).map(|d| format_expiry(d.date_naive()))
}

/// Parse a cash file (`NSE_CM` or `BSE_CM`).
pub fn parse_cash(csv: &str, nse: bool) -> Result<Vec<SymToken>> {
    let file = if nse { "NSE_CM" } else { "BSE_CM" };
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return Ok(Vec::new());
    };
    let h = Header::parse(head);
    let col = |n: &str| h.idx(n).ok_or_else(|| bad_format(file, n));
    let (c_tok, c_desc, c_exp, c_strike, c_lot, c_tick, c_trd, c_name, c_isin) = (
        col("pSymbol")?,
        col("pDesc")?,
        col("pExpiryDate")?,
        col("dStrikePrice")?,
        col("lLotSize")?,
        col("dTickSize")?,
        col("pTrdSymbol")?,
        col("pSymbolName")?,
        col("pISIN")?,
    );
    let c_group = h.idx("pGroup");
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fs = split_csv_line(line);
        let get = |i: usize| fs.get(i).map(|s| s.trim()).unwrap_or("");
        let symbol = get(c_name).to_string();
        let no_isin = {
            let i = get(c_isin);
            i.is_empty() || i.eq_ignore_ascii_case("nan")
        };
        let exchange = if nse {
            let group = c_group.map(get).unwrap_or("");
            if group == "EQ" || group == "BE" {
                "NSE"
            } else if no_isin {
                "NSE_INDEX"
            } else {
                continue;
            }
        } else {
            if symbol.is_empty() {
                continue;
            }
            if no_isin {
                "BSE_INDEX"
            } else {
                "BSE"
            }
        };
        let token = get(c_tok).to_string();
        if token.is_empty() || symbol.is_empty() {
            continue;
        }
        out.push(SymToken {
            symbol,
            brsymbol: get(c_trd).to_string(),
            name: get(c_desc).to_string(),
            exchange: exchange.to_string(),
            brexchange: if nse { "NSE" } else { "BSE" }.to_string(),
            token,
            expiry: parse_broker_expiry(get(c_exp))
                .map(format_expiry)
                .unwrap_or_default(),
            strike: f(get(c_strike)).unwrap_or(0.0),
            lot_size: f(get(c_lot)).map(|v| v as i32).unwrap_or(1),
            instrument_type: "EQ".into(),
            tick_size: f(get(c_tick)).map(|v| v / 100.0).unwrap_or(0.0),
        });
    }
    Ok(out)
}

/// Parse a derivatives file. `exchange` is the OpenAlgo exchange; NFO and
/// CDS carry the epoch offset; MCX and BFO drop rows without an option type.
pub fn parse_derivatives(csv: &str, file: &str, exchange: &str) -> Result<Vec<SymToken>> {
    let offset = matches!(exchange, "NFO" | "CDS");
    let drop_no_type = matches!(exchange, "MCX" | "BFO");
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return Ok(Vec::new());
    };
    let h = Header::parse(head);
    let col = |n: &str| h.idx(n).ok_or_else(|| bad_format(file, n));
    let (c_tok, c_name, c_exp, c_strike, c_lot, c_tick, c_trd, c_seg, c_opt) = (
        col("pSymbol")?,
        col("pSymbolName")?,
        col("lExpiryDate")?,
        col("dStrikePrice")?,
        col("lLotSize")?,
        col("dTickSize")?,
        col("pTrdSymbol")?,
        col("pExchSeg")?,
        col("pOptionType")?,
    );
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fs = split_csv_line(line);
        let get = |i: usize| fs.get(i).map(|s| s.trim()).unwrap_or("");
        let opt_raw = fs.get(c_opt).map(|s| s.as_str()).unwrap_or("");
        if drop_no_type && (opt_raw.is_empty() || opt_raw.eq_ignore_ascii_case("nan")) {
            continue;
        }
        let token = get(c_tok).to_string();
        if token.is_empty() {
            continue;
        }
        let name = get(c_name).to_string();
        let raw_exp = f(get(c_exp)).unwrap_or(0.0) as i64;
        let strike = f(get(c_strike)).map(|v| v / 100.0).unwrap_or(0.0);
        let (expiry, itype) = match expiry_from_epoch(raw_exp, offset) {
            Some(e) => (e, opt_raw.replace("XX", "FUT").trim().to_string()),
            // A row that never expires is no contract: no expiry, no type.
            None => (String::new(), String::new()),
        };
        out.push(SymToken {
            symbol: combine_details(&name, &expiry, strike, &itype),
            brsymbol: get(c_trd).to_string(),
            name,
            exchange: exchange.to_string(),
            brexchange: get(c_seg).to_string(),
            token,
            expiry,
            strike,
            lot_size: f(get(c_lot)).map(|v| v as i32).unwrap_or(1),
            instrument_type: itype,
            tick_size: f(get(c_tick)).map(|v| v / 100.0).unwrap_or(0.0),
        });
    }
    Ok(out)
}

/// Parse one downloaded file by its key.
pub fn parse_file(key: &str, csv: &str) -> Result<Vec<SymToken>> {
    match key {
        "NSE_CM" => parse_cash(csv, true),
        "BSE_CM" => parse_cash(csv, false),
        "NSE_FO" => parse_derivatives(csv, key, "NFO"),
        "CDE_FO" => parse_derivatives(csv, key, "CDS"),
        "MCX_FO" => parse_derivatives(csv, key, "MCX"),
        "BSE_FO" => parse_derivatives(csv, key, "BFO"),
        _ => Ok(Vec::new()),
    }
}

/// `{"data": {"filesPaths": [...]}}` -> `(key, url)` pairs.
pub fn parse_file_paths(v: &Value) -> Option<Vec<(&'static str, String)>> {
    let list = v.get("data")?.get("filesPaths")?.as_array()?;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for u in list.iter().filter_map(Value::as_str) {
        if let Some(k) = file_key(u) {
            out.retain(|(x, _)| *x != k);
            out.push((k, u.to_string()));
        }
    }
    Some(out)
}

async fn file_paths(b: &KotakBroker, s: &KotakSession) -> Vec<(&'static str, String)> {
    let mut bases = vec![s.base_url.clone()];
    bases.extend(b.scrip_fallback_bases.iter().cloned());
    for base in bases {
        let resp = b
            .http
            .get(format!(
                "{}/script-details/1.0/masterscrip/file-paths",
                base
            ))
            .header("Authorization", &s.access_token)
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Kotak file-paths call failed: {}",
                    crate::brokers::common::redact::url_safe_error(&e)
                );
                continue;
            }
        };
        if resp.status() != StatusCode::OK {
            tracing::warn!(status = resp.status().as_u16(), "Kotak file-paths refused");
            continue;
        }
        if let Ok(v) = resp.json::<Value>().await {
            if let Some(list) = parse_file_paths(&v) {
                return list;
            }
        }
    }
    // Dated CDN fallback, probed with a one-byte ranged GET (HEAD loops on
    // redirects there).
    let today = (chrono::Utc::now() + chrono::Duration::seconds(IST_OFFSET_SECS))
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let mut ok = Vec::new();
    for (key, url) in fallback_urls(&b.scrip_cdn, &today) {
        match b
            .http
            .get(&url)
            .header("Range", "bytes=0-0")
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) if matches!(r.status().as_u16(), 200 | 206) => {
                drop(r);
                ok.push((key, url));
            }
            Ok(r) => tracing::warn!(
                status = r.status().as_u16(),
                "Kotak CDN file missing: {}",
                key
            ),
            Err(e) => tracing::warn!(
                "Kotak CDN probe failed for {}: {}",
                key,
                crate::brokers::common::redact::url_safe_error(&e)
            ),
        }
    }
    ok
}

pub async fn download(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<SymToken>> {
    let s = KotakSession::parse(auth)?;
    let paths = file_paths(b, &s).await;
    if paths.is_empty() {
        return Err(AppError::Broker(
            "Kotak did not provide the instrument lists. Try downloading the master contract again later."
                .into(),
        ));
    }
    // All or nothing (MC-02, a hardening over the web, which keeps whatever
    // segments it got): a partial master would replace a complete one and
    // the smart rule would then skip the download for the rest of the day.
    let mut rows = Vec::new();
    for key in SEGMENT_FILES {
        let Some((_, url)) = paths.iter().find(|(k, _)| k == key) else {
            if REQUIRED_SEGMENTS.contains(key) {
                tracing::warn!("Kotak scrip master file missing: {}", key);
                return Err(segment_failed(key));
            }
            // Kotak lists no currency or commodity file on some days.
            tracing::info!("Kotak lists no {} file today; skipped", key);
            continue;
        };
        let resp = match b.http.get(url).timeout(DOWNLOAD_TIMEOUT).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(
                    status = r.status().as_u16(),
                    "Kotak {} download refused",
                    key
                );
                return Err(segment_failed(key));
            }
            Err(e) => {
                tracing::warn!(
                    "Kotak {} download failed: {}",
                    key,
                    crate::brokers::common::redact::url_safe_error(&e)
                );
                return Err(segment_failed(key));
            }
        };
        let text = resp.text().await.map_err(|e| {
            tracing::warn!(
                "Kotak {} download failed: {}",
                key,
                crate::brokers::common::redact::url_safe_error(&e)
            );
            segment_failed(key)
        })?;
        match parse_file(key, &text) {
            Ok(r) if !r.is_empty() => {
                tracing::info!("Kotak {}: {} instruments", key, r.len());
                rows.extend(r);
            }
            Ok(_) => {
                tracing::warn!("Kotak {} file had no instruments", key);
                return Err(segment_failed(key));
            }
            Err(e) => {
                tracing::error!("Kotak {} could not be processed: {}", key, e.code());
                return Err(segment_failed(key));
            }
        }
    }
    tracing::info!("Kotak master contract parsed: {} instruments", rows.len());
    Ok(rows)
}

/// Segments every Kotak master must have; CDE_FO and MCX_FO may be absent
/// from the day's listing, but once listed they must download and parse.
pub const REQUIRED_SEGMENTS: &[&str] = &["NSE_CM", "NSE_FO", "BSE_CM", "BSE_FO"];

fn segment_failed(key: &str) -> AppError {
    AppError::Broker(format!(
        "Kotak's {} instrument list could not be downloaded. Your existing symbols were kept; try the download again later.",
        key
    ))
}
