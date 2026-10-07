//! Historify service functions (web `services/historify_service.py`): each
//! returns the status code and body the route sends.

use super::db::{self, SymbolReq};
use super::export::{self, ExportSpec, PendingExport};
use super::import::{self, FileKind};
use super::interval;
use super::time::{day_start, ist_date, ist_naive, parse_date};
use super::{Historify, SUPPORTED_EXCHANGES};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::db::duckdb::STORE_UNAVAILABLE;
use crate::error::AppError;
use crate::services::core::Reply;
use crate::services::market_data_service::candle_json;
use crate::services::search_ui_service::FNO_EXCHANGES;
use crate::services::symbol_service::freeze_qty_for_option;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

fn failed(what: &str, e: AppError) -> Reply {
    tracing::error!("Historify {} failed: {}", what, e);
    Reply::error(500, STORE_UNAVAILABLE)
}

fn invalid_exchange() -> Reply {
    Reply::error(
        400,
        format!(
            "Invalid exchange. Supported: {}",
            SUPPORTED_EXCHANGES.join(", ")
        ),
    )
}

fn sorted_fno() -> Vec<&'static str> {
    let mut v = FNO_EXCHANGES.to_vec();
    v.sort();
    v
}

fn invalid_fno() -> Reply {
    Reply::error(
        400,
        format!(
            "Invalid FNO exchange. Supported: {}",
            sorted_fno().join(", ")
        ),
    )
}

/// Web `validate_symbol`: index exchanges are not in the master.
fn validate_symbol(symbols: &SymbolResolver, symbol: &str, exchange: &str) -> Result<(), String> {
    let ex = exchange.to_uppercase();
    if ex == "NSE_INDEX" || ex == "BSE_INDEX" {
        return Ok(());
    }
    if symbols.by_symbol(&ex, &symbol.to_uppercase()).is_none() {
        return Err(format!(
            "Symbol '{}' not found in {} master contract. Please check the symbol name.",
            symbol, exchange
        ));
    }
    Ok(())
}

/// Dates for the chart and export routes (IST midnight).
fn date_bound(s: Option<&str>) -> Result<Option<NaiveDate>, Reply> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => parse_date(s)
            .map(Some)
            .ok_or_else(|| Reply::error(400, "Choose dates in YYYY-MM-DD format.")),
    }
}

impl Historify {
    // ---------------------------------------------------------- watchlist

    pub async fn watchlist(&self) -> Reply {
        match self.db.run(db::watchlist).await {
            Ok(w) => Reply::ok(json!({"status": "success", "count": w.len(), "data": w})),
            Err(e) => failed("reading the watchlist", e),
        }
    }

    pub async fn add_watchlist(
        &self,
        symbols: &SymbolResolver,
        symbol: &str,
        exchange: &str,
        display_name: Option<String>,
    ) -> Reply {
        if symbol.is_empty() || exchange.is_empty() {
            return Reply::error(400, "Symbol and exchange are required");
        }
        if !SUPPORTED_EXCHANGES.contains(&exchange.to_uppercase().as_str()) {
            return invalid_exchange();
        }
        if let Err(m) = validate_symbol(symbols, symbol, exchange) {
            return Reply::error(400, m);
        }
        let (s, e, now) = (
            symbol.to_string(),
            exchange.to_string(),
            ist_naive(self.now()),
        );
        match self
            .db
            .write(move |c| db::watchlist_add(c, &s, &e, display_name.as_deref(), now))
            .await
        {
            Ok(msg) => Reply::ok(json!({"status": "success", "message": msg})),
            Err(e) => failed("adding to the watchlist", e),
        }
    }

    pub async fn remove_watchlist(&self, symbol: &str, exchange: &str) -> Reply {
        if symbol.is_empty() || exchange.is_empty() {
            return Reply::error(400, "Symbol and exchange are required");
        }
        let (s, e) = (symbol.to_string(), exchange.to_string());
        match self
            .db
            .write(move |c| db::watchlist_remove(c, &s, &e))
            .await
        {
            Ok(msg) => Reply::ok(json!({"status": "success", "message": msg})),
            Err(e) => failed("removing from the watchlist", e),
        }
    }

    pub async fn bulk_remove_watchlist(&self, items: Vec<SymbolReq>) -> Reply {
        if items.is_empty() {
            return Reply::error(400, "No symbols provided");
        }
        let total = items.len();
        match self
            .db
            .write(move |c| db::watchlist_bulk_remove(c, &items))
            .await
        {
            Ok((removed, skipped, fails)) => {
                let mut message = format!("Removed {} symbol(s) from watchlist", removed);
                if skipped > 0 {
                    message.push_str(&format!(", {} not found", skipped));
                }
                if !fails.is_empty() {
                    message.push_str(&format!(", {} failed", fails.len()));
                }
                Reply::ok(json!({
                    "status": "success", "message": message, "removed": removed,
                    "skipped": skipped, "failed": fails, "total": total,
                }))
            }
            Err(e) => failed("removing from the watchlist", e),
        }
    }

    pub async fn bulk_add_watchlist(
        &self,
        symbols: &SymbolResolver,
        items: Vec<SymbolReq>,
    ) -> Reply {
        let total = items.len();
        let mut valid = Vec::new();
        let mut invalid = Vec::new();
        for it in items {
            let (s, e) = (it.symbol.to_uppercase(), it.exchange.to_uppercase());
            if s.is_empty() || e.is_empty() {
                invalid.push(json!({
                    "symbol": if s.is_empty() { "MISSING" } else { s.as_str() },
                    "exchange": if e.is_empty() { "MISSING" } else { e.as_str() },
                    "error": "Missing symbol or exchange",
                }));
                continue;
            }
            if !SUPPORTED_EXCHANGES.contains(&e.as_str()) {
                invalid.push(json!({"symbol": s, "exchange": e, "error": "Invalid exchange"}));
                continue;
            }
            if let Err(m) = validate_symbol(symbols, &s, &e) {
                invalid.push(json!({"symbol": s, "exchange": e, "error": m}));
                continue;
            }
            valid.push(it);
        }
        let now = ist_naive(self.now());
        match self
            .db
            .write(move |c| db::watchlist_bulk_add(c, &valid, now))
            .await
        {
            Ok((added, skipped, mut fails)) => {
                fails.extend(invalid);
                Reply::ok(json!({
                    "status": "success", "added": added, "skipped": skipped,
                    "failed": fails, "total": total,
                }))
            }
            Err(e) => failed("adding to the watchlist", e),
        }
    }

    // ------------------------------------------------------------- reads

    /// Web `get_chart_data`.
    pub async fn chart_data(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start_date: Option<&str>,
        end_date: Option<&str>,
    ) -> Reply {
        let start = match date_bound(start_date) {
            Ok(d) => d.map(day_start),
            Err(r) => return r,
        };
        let end = match date_bound(end_date) {
            Ok(d) => d.map(|d| day_start(d) + 86_400),
            Err(r) => return r,
        };
        let (s, e, i) = (
            symbol.to_string(),
            exchange.to_string(),
            interval.to_string(),
        );
        let bars = match self
            .db
            .run(move |c| db::ohlcv(c, &s, &e, &i, start, end))
            .await
        {
            Ok(b) => b,
            Err(e) => return failed("reading candles", e),
        };
        if bars.is_empty() {
            return Reply::ok(json!({
                "status": "success", "data": [], "count": 0, "message": "No data available",
            }));
        }
        let data: Vec<Value> = bars.iter().map(candle_json).collect();
        Reply::ok(json!({
            "status": "success",
            "symbol": symbol.to_uppercase(),
            "exchange": exchange.to_uppercase(),
            "interval": interval,
            "count": data.len(),
            "data": data,
        }))
    }

    /// `/api/v1/history` with `source=db` (web `get_history_from_db`).
    pub async fn history_from_db(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Reply {
        let from = day_start(start);
        let to = day_start(end) + 86_399;
        let (s, e, i) = (
            symbol.to_string(),
            exchange.to_string(),
            interval.to_string(),
        );
        match self
            .db
            .run(move |c| db::ohlcv(c, &s, &e, &i, Some(from), Some(to)))
            .await
        {
            Ok(bars) if bars.is_empty() => Reply::error(
                404,
                format!(
                    "No data found for {}:{} interval {} in local database. Download data first using Historify.",
                    symbol, exchange, interval
                ),
            ),
            Ok(bars) => Reply::ok(json!({
                "status": "success",
                "data": bars.iter().map(candle_json).collect::<Vec<_>>(),
            })),
            Err(e) => {
                tracing::error!("Reading local history failed: {}", e);
                Reply::error(500, "Could not read the local Historify database. Try again.")
            }
        }
    }

    pub async fn catalog(&self) -> Reply {
        match self.db.run(db::catalog).await {
            Ok(c) => Reply::ok(json!({"status": "success", "count": c.len(), "data": c})),
            Err(e) => failed("reading the catalog", e),
        }
    }

    pub async fn catalog_metadata(&self) -> Reply {
        match self.db.run(db::catalog_with_metadata).await {
            Ok(c) => Reply::ok(json!({"status": "success", "count": c.len(), "data": c})),
            Err(e) => failed("reading the catalog", e),
        }
    }

    /// Web `get_catalog_grouped_service`.
    pub async fn catalog_grouped(&self, group_by: &str) -> Reply {
        let rows = match self.db.run(db::catalog_with_metadata).await {
            Ok(r) => r,
            Err(e) => return failed("reading the catalog", e),
        };
        let mut groups = serde_json::Map::new();
        for r in rows {
            let key = if group_by == "underlying" {
                r["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| r["symbol"].as_str())
                    .unwrap_or("Unknown")
                    .to_string()
            } else {
                r["exchange"].as_str().unwrap_or("Unknown").to_string()
            };
            if let Value::Array(a) = groups.entry(key).or_insert_with(|| json!([])) {
                a.push(r);
            }
        }
        Reply::ok(json!({
            "status": "success", "group_by": group_by,
            "group_count": groups.len(), "data": Value::Object(groups),
        }))
    }

    /// Web `get_symbol_data_info`.
    pub async fn symbol_info(&self, symbol: &str, exchange: &str, interval: Option<&str>) -> Reply {
        let (s, e) = (symbol.to_uppercase(), exchange.to_uppercase());
        match interval {
            Some(i) => {
                let (s2, e2, i2) = (s.clone(), e.clone(), i.to_string());
                match self.db.run(move |c| db::data_range(c, &s2, &e2, &i2)).await {
                    Ok(Some((f, l, n))) => {
                        let mut data =
                            json!({"first_timestamp": f, "last_timestamp": l, "record_count": n});
                        if let (Some(f), Some(l)) = (f, l) {
                            data["first_date"] = json!(ist_date(f));
                            data["last_date"] = json!(ist_date(l));
                        }
                        Reply::ok(
                            json!({"status": "success", "symbol": s, "exchange": e, "interval": i, "data": data}),
                        )
                    }
                    Ok(None) => Reply::ok(json!({
                        "status": "success", "symbol": s, "exchange": e, "interval": i,
                        "data": null, "message": "No data available",
                    })),
                    Err(err) => failed("reading the catalog", err),
                }
            }
            None => match self.db.run(db::catalog).await {
                Ok(c) => {
                    let mine: Vec<Value> = c
                        .into_iter()
                        .filter(|r| r["symbol"] == s.as_str() && r["exchange"] == e.as_str())
                        .collect();
                    Reply::ok(
                        json!({"status": "success", "symbol": s, "exchange": e, "intervals": mine}),
                    )
                }
                Err(err) => failed("reading the catalog", err),
            },
        }
    }

    /// Web `get_historify_intervals`.
    pub fn historify_intervals(&self) -> Reply {
        let mut all: Vec<&str> = interval::STORAGE_INTERVALS.to_vec();
        all.extend_from_slice(interval::COMPUTED_INTERVALS);
        Reply::ok(json!({
            "status": "success",
            "storage_intervals": interval::sorted(interval::STORAGE_INTERVALS),
            "computed_intervals": interval::sorted(interval::COMPUTED_INTERVALS),
            "all_intervals": interval::sorted(&all),
            "description": {
                "storage": "These intervals are downloaded and stored in the database",
                "computed": "These intervals are computed from 1-minute data on-the-fly",
            },
        }))
    }

    pub fn exchanges(&self) -> Reply {
        Reply::ok(json!({"status": "success", "data": SUPPORTED_EXCHANGES}))
    }

    /// Web `get_stats`.
    pub async fn stats(&self) -> Reply {
        let path = self.db.path().to_path_buf();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let (total, symbols, watch) = match self.db.run(db::stats).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Reading Historify statistics failed: {}", e);
                (0, 0, 0)
            }
        };
        Reply::ok(json!({"status": "success", "data": {
            "database_path": path.to_string_lossy(),
            "database_size_mb": ((size as f64 / (1024.0 * 1024.0)) * 100.0).round() / 100.0,
            "total_records": total,
            "total_symbols": symbols,
            "watchlist_count": watch,
        }}))
    }

    // ----------------------------------------------------------- deletes

    pub async fn delete_data(
        &self,
        symbol: &str,
        exchange: &str,
        interval: Option<String>,
    ) -> Reply {
        if symbol.is_empty() || exchange.is_empty() {
            return Reply::error(400, "Symbol and exchange are required");
        }
        let (s, e) = (symbol.to_string(), exchange.to_string());
        match self
            .db
            .write(move |c| db::delete_market_data(c, &s, &e, interval.as_deref()))
            .await
        {
            Ok(msg) => {
                tracing::info!("{}", msg);
                Reply::ok(json!({"status": "success", "message": msg}))
            }
            Err(e) => failed("deleting data", e),
        }
    }

    pub async fn bulk_delete(&self, items: Vec<SymbolReq>) -> Reply {
        if items.is_empty() {
            return Reply::error(400, "No symbols provided");
        }
        let total = items.len();
        match self.db.write(move |c| db::bulk_delete(c, &items)).await {
            Ok((deleted, skipped, fails)) => {
                let mut message = format!("Deleted {} symbol(s)", deleted);
                if skipped > 0 {
                    message.push_str(&format!(", {} had no data", skipped));
                }
                if !fails.is_empty() {
                    message.push_str(&format!(", {} failed", fails.len()));
                }
                Reply::ok(json!({
                    "status": "success", "message": message, "deleted": deleted,
                    "skipped": skipped, "failed": fails, "total": total,
                }))
            }
            Err(e) => failed("deleting data", e),
        }
    }

    // ------------------------------------------------------------ upload

    /// Web `upload_csv_data` / `upload_parquet_data` on a saved file.
    pub async fn upload(
        &self,
        path: &Path,
        kind: FileKind,
        symbol: &str,
        exchange: &str,
        interval: &str,
    ) -> Reply {
        if symbol.is_empty() || exchange.is_empty() || interval.is_empty() {
            return Reply::error(400, "Symbol, exchange, and interval are required");
        }
        if !SUPPORTED_EXCHANGES.contains(&exchange.to_uppercase().as_str()) {
            return invalid_exchange();
        }
        if !interval::VALID_UPLOAD_INTERVALS.contains(&interval) {
            return Reply::error(
                400,
                format!(
                    "Invalid interval. Supported: {}",
                    interval::sorted(interval::VALID_UPLOAD_INTERVALS).join(", ")
                ),
            );
        }
        let p = path.to_path_buf();
        let parsed = match self.db.run(move |c| import::read_file(c, &p, kind)).await {
            Ok(Ok(p)) => p,
            Ok(Err(m)) => return Reply::error(400, m),
            Err(e) => {
                tracing::error!("Reading an uploaded file failed: {}", e);
                return Reply::error(
                    400,
                    "The file could not be read. Check its format and try again.",
                );
            }
        };
        let dropped = parsed.dropped;
        let (s, e, i, now) = (
            symbol.to_string(),
            exchange.to_string(),
            interval.to_string(),
            ist_naive(self.now()),
        );
        let records = match self
            .db
            .write(move |c| db::upsert_bars(c, &s, &e, &i, &parsed.bars, now))
            .await
        {
            Ok(n) => n,
            Err(e) => return failed("saving an upload", e),
        };
        let mut message = format!("Imported {} records", records);
        if dropped > 0 {
            message.push_str(&format!(" ({} rows skipped due to invalid data)", dropped));
        }
        tracing::info!(
            "{} import: {} for {}:{}:{}",
            if kind == FileKind::Csv {
                "CSV"
            } else {
                "Parquet"
            },
            message,
            symbol,
            exchange,
            interval
        );
        Reply::ok(json!({
            "status": "success", "message": message, "symbol": symbol.to_uppercase(),
            "exchange": exchange.to_uppercase(), "interval": interval, "records": records,
        }))
    }

    // ------------------------------------------------------------ export

    /// Web `bulk_export` (format, symbols, interval/intervals, dates,
    /// compression). The file waits in `exports` under `session`.
    pub async fn export_bulk(&self, session: &str, body: &serde_json::Map<String, Value>) -> Reply {
        let mut format = body
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("csv")
            .to_lowercase();
        let symbols = parse_symbol_list(body.get("symbols"));
        let single = body
            .get("interval")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut compression = body
            .get("compression")
            .and_then(Value::as_str)
            .unwrap_or("zstd")
            .to_string();
        if !["zstd", "snappy", "gzip", "none"].contains(&compression.as_str()) {
            compression = "zstd".into();
        }
        let intervals: Option<Vec<String>> = match body.get("intervals") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => {
                if a.is_empty() {
                    return Reply::error(400, "At least one interval must be specified");
                }
                let mut seen = Vec::new();
                for v in a {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if !seen.contains(&s) {
                        seen.push(s);
                    }
                }
                let invalid: Vec<String> = seen
                    .iter()
                    .filter(|i| interval::parse(i).is_none())
                    .map(|i| format!("'{}'", i))
                    .collect();
                if !invalid.is_empty() {
                    return Reply::error(
                        400,
                        format!("Invalid intervals: [{}]", invalid.join(", ")),
                    );
                }
                Some(seen)
            }
            Some(_) => return Reply::error(400, "intervals must be an array"),
        };
        let start = match date_bound(body.get("start_date").and_then(Value::as_str)) {
            Ok(d) => d.map(day_start),
            Err(r) => return r,
        };
        let end = match date_bound(body.get("end_date").and_then(Value::as_str)) {
            Ok(d) => d.map(|d| day_start(d) + 86_400),
            Err(r) => return r,
        };

        let stamp = ist_naive(self.now()).format("%Y%m%d_%H%M%S").to_string();
        let export_id = &uuid::Uuid::new_v4().simple().to_string()[..10];
        let base = if symbols.len() == 1 {
            format!(
                "historify_{}_{}_{}",
                export::sanitize_filename(&symbols[0].0),
                stamp,
                export_id
            )
        } else {
            format!("historify_export_{}_{}", stamp, export_id)
        };
        let has_computed = intervals
            .as_ref()
            .is_some_and(|v| v.iter().any(|i| interval::is_custom(i)));
        if intervals.as_ref().is_some_and(|v| v.len() > 1)
            || (has_computed && (format == "csv" || format == "txt"))
        {
            format = "zip".into();
        }
        let (ext, mime) = match format.as_str() {
            "parquet" => (".parquet", "application/octet-stream"),
            "zip" => (".zip", "application/zip"),
            "txt" => (".txt", "text/plain"),
            _ => (".csv", "text/csv"),
        };
        let filename = format!("{}{}", base, ext);
        let dir = match self.work_dir() {
            Ok(d) => d.to_path_buf(),
            Err(e) => {
                tracing::error!("Could not create the export folder: {}", e);
                return Reply::error(
                    500,
                    "Could not prepare the export file. Check free disk space.",
                );
            }
        };
        let path = dir.join(&filename);
        let spec = ExportSpec {
            symbols,
            intervals: match (intervals, single) {
                (Some(v), _) => v,
                (None, Some(i)) => vec![i],
                (None, None) => Vec::new(),
            },
            start,
            end,
            compression,
        };
        let (p, fmt) = (path.clone(), format.clone());
        let r = self
            .db
            .run(move |c| match fmt.as_str() {
                "parquet" => export::export_parquet(c, &p, &spec),
                "zip" => export::export_zip(c, &p, &spec),
                "txt" => export::export_flat(c, &p, &spec, '\t'),
                _ => export::export_flat(c, &p, &spec, ','),
            })
            .await;
        match r {
            Ok(Ok((message, count))) => {
                self.exports.put(
                    session,
                    PendingExport {
                        path,
                        mime,
                        filename: filename.clone(),
                    },
                );
                Reply::ok(json!({
                    "status": "success", "message": message, "record_count": count,
                    "filename": filename,
                }))
            }
            Ok(Err(m)) => Reply::error(400, m),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                tracing::error!("Historify export failed: {}", e);
                Reply::error(
                    500,
                    "The export could not be written. Check free disk space and try again.",
                )
            }
        }
    }

    /// Web `get_export_preview`.
    pub async fn export_preview(&self, body: &serde_json::Map<String, Value>) -> Reply {
        let start = match date_bound(body.get("start_date").and_then(Value::as_str)) {
            Ok(d) => d.map(day_start),
            Err(r) => return r,
        };
        let end = match date_bound(body.get("end_date").and_then(Value::as_str)) {
            Ok(d) => d.map(|d| day_start(d) + 86_400),
            Err(r) => return r,
        };
        let spec = ExportSpec {
            symbols: parse_symbol_list(body.get("symbols")),
            intervals: body
                .get("interval")
                .and_then(Value::as_str)
                .map(|s| vec![s.to_string()])
                .unwrap_or_default(),
            start,
            end,
            compression: String::new(),
        };
        match self.db.run(move |c| export::preview(c, &spec)).await {
            Ok(p) => Reply::ok(json!({"status": "success", "data": p})),
            Err(e) => failed("previewing an export", e),
        }
    }

    // ---------------------------------------------------------- metadata

    /// Web `enrich_and_save_metadata` from the symbol master.
    pub async fn enrich_metadata(&self, symbols: &SymbolResolver, items: Vec<SymbolReq>) -> Reply {
        if items.is_empty() {
            return Reply::error(400, "No symbols provided");
        }
        let rows: Vec<Value> = items
            .iter()
            .map(|it| {
                let (s, e) = (it.symbol.to_uppercase(), it.exchange.to_uppercase());
                match symbols.by_symbol(&e, &s) {
                    Some(r) => json!({
                        "symbol": r.symbol, "exchange": r.exchange, "name": r.name,
                        "expiry": r.expiry, "strike": r.strike, "lotsize": r.lot_size,
                        "instrumenttype": r.instrument_type, "tick_size": r.tick_size,
                    }),
                    None => json!({"symbol": s, "exchange": e}),
                }
            })
            .collect();
        let now = ist_naive(self.now());
        match self
            .db
            .write(move |c| db::upsert_metadata(c, &rows, now))
            .await
        {
            Ok(n) => Reply::ok(json!({
                "status": "success", "message": format!("Enriched metadata for {} symbols", n),
                "count": n,
            })),
            Err(e) => failed("saving metadata", e),
        }
    }
}

/// `[{symbol, exchange}, ...]` from a request body (non-objects dropped).
pub fn parse_symbol_list(v: Option<&Value>) -> Vec<(String, String)> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|i| {
                let s = i.get("symbol")?.as_str()?.trim().to_string();
                let e = i.get("exchange")?.as_str()?.trim().to_string();
                Some((s, e))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `[{symbol, exchange, display_name?}, ...]` keeping malformed entries as
/// empty requests (so they are reported as failed, like the web).
pub fn parse_symbol_reqs(v: Option<&Value>) -> Vec<SymbolReq> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .map(|i| {
                let s = |k: &str| {
                    i.get(k)
                        .and_then(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default()
                };
                SymbolReq {
                    symbol: s("symbol"),
                    exchange: s("exchange"),
                    display_name: i
                        .get("display_name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ----------------------------------------------------------------- F&O

fn fno_row(r: &SymToken) -> Value {
    json!({
        "symbol": r.symbol, "brsymbol": r.brsymbol, "name": r.name, "exchange": r.exchange,
        "brexchange": r.brexchange, "token": r.token, "expiry": r.expiry, "strike": r.strike,
        "lotsize": r.lot_size, "instrumenttype": r.instrument_type, "tick_size": r.tick_size,
        "freeze_qty": freeze_qty_for_option(&r.symbol, &r.exchange),
    })
}

/// Web `get_fno_underlyings` (distinct `name` per exchange).
pub fn fno_underlyings(rows: &[SymToken], exchange: Option<&str>) -> Reply {
    let ex = exchange.map(str::to_uppercase);
    if let Some(e) = &ex {
        if !FNO_EXCHANGES.contains(&e.as_str()) {
            return invalid_fno();
        }
    }
    let set: BTreeSet<&str> = rows
        .iter()
        .filter(|r| ex.as_deref().is_none_or(|e| r.exchange == e))
        .map(|r| r.name.as_str())
        .filter(|n| !n.is_empty() && !n.contains("NSETEST") && !n.contains("BSETEST"))
        .collect();
    let list: Vec<&str> = set.into_iter().collect();
    Reply::ok(json!({
        "status": "success", "count": list.len(), "data": list,
        "exchange": ex.unwrap_or_else(|| "ALL".into()),
    }))
}

fn expiry_key(e: &str) -> NaiveDate {
    NaiveDate::parse_from_str(e, "%d-%b-%y")
        .or_else(|_| NaiveDate::parse_from_str(e, "%d-%b-%Y"))
        .unwrap_or(NaiveDate::MAX)
}

/// Web `get_fno_expiries`.
pub fn fno_expiries(rows: &[SymToken], underlying: &str, exchange: &str) -> Reply {
    let ex = exchange.to_uppercase();
    if !FNO_EXCHANGES.contains(&ex.as_str()) {
        return invalid_fno();
    }
    let und = underlying.to_uppercase();
    let set: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.exchange == ex && r.name.eq_ignore_ascii_case(&und) && !r.expiry.is_empty())
        .map(|r| r.expiry.as_str())
        .collect();
    let mut list: Vec<&str> = set.into_iter().collect();
    list.sort_by_key(|e| expiry_key(e));
    Reply::ok(json!({
        "status": "success", "count": list.len(), "data": list,
        "underlying": und, "exchange": ex,
    }))
}

/// Filters of the F&O chain.
#[derive(Debug, Clone, Default)]
pub struct ChainQuery {
    pub underlying: String,
    pub exchange: String,
    pub expiry: Option<String>,
    pub instrumenttype: Option<String>,
    pub strike_min: Option<f64>,
    pub strike_max: Option<f64>,
    pub limit: usize,
}

/// Web `fno_search_symbols_db` without a text query: ordered by symbol.
fn chain_rows<'a>(rows: &'a [SymToken], q: &ChainQuery, inst: Option<&str>) -> Vec<&'a SymToken> {
    let und = q.underlying.to_uppercase();
    let mut v: Vec<&SymToken> = rows
        .iter()
        .filter(|r| r.exchange == q.exchange)
        .filter(|r| r.name.eq_ignore_ascii_case(&und))
        .filter(|r| q.expiry.as_deref().is_none_or(|e| r.expiry == e.trim()))
        .filter(|r| match inst.map(|t| t.trim().to_uppercase()).as_deref() {
            Some("FUT") => r.symbol.to_uppercase().ends_with("FUT"),
            Some("CE") => r.symbol.to_uppercase().ends_with("CE"),
            Some("PE") => r.symbol.to_uppercase().ends_with("PE"),
            _ => true,
        })
        .filter(|r| q.strike_min.is_none_or(|m| r.strike >= m))
        .filter(|r| q.strike_max.is_none_or(|m| r.strike <= m))
        .collect();
    v.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    if q.limit > 0 {
        v.truncate(q.limit);
    }
    v
}

/// Web `get_fno_chain`.
pub fn fno_chain(rows: &[SymToken], q: &ChainQuery) -> Reply {
    if !FNO_EXCHANGES.contains(&q.exchange.as_str()) {
        return invalid_fno();
    }
    let data: Vec<Value> = chain_rows(rows, q, q.instrumenttype.as_deref())
        .into_iter()
        .map(fno_row)
        .collect();
    Reply::ok(json!({
        "status": "success", "count": data.len(), "data": data,
        "underlying": q.underlying.to_uppercase(), "exchange": q.exchange,
        "expiry": q.expiry, "instrumenttype": q.instrumenttype,
    }))
}

/// Web `get_option_chain_symbols`: calls then puts.
pub fn fno_options(rows: &[SymToken], q: &ChainQuery) -> Reply {
    if !FNO_EXCHANGES.contains(&q.exchange.as_str()) {
        return invalid_fno();
    }
    let q = ChainQuery {
        limit: 2000,
        ..q.clone()
    };
    let ce: Vec<Value> = chain_rows(rows, &q, Some("CE"))
        .into_iter()
        .map(fno_row)
        .collect();
    let pe: Vec<Value> = chain_rows(rows, &q, Some("PE"))
        .into_iter()
        .map(fno_row)
        .collect();
    let (nc, np) = (ce.len(), pe.len());
    let mut all = ce;
    all.extend(pe);
    Reply::ok(json!({
        "status": "success", "count": all.len(), "ce_count": nc, "pe_count": np, "data": all,
        "underlying": q.underlying.to_uppercase(), "exchange": q.exchange, "expiry": q.expiry,
    }))
}
