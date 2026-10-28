//! Analyzer (sandbox mode) request log for the Analyzer page (web
//! `blueprints/analyzer.py` and `utils/api_analyzer.get_analyzer_stats`),
//! read from `logs.db` `analyzer_logs`.

use crate::db::sqlite::logs::redact;
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use rusqlite::params;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::Arc;

/// Rows one page or export reads at most (newest first).
pub const MAX_ROWS: i64 = 10_000;

const TS: &str = "%Y-%m-%dT%H:%M:%SZ";

/// A row as stored: api_type, request, response, created_at.
pub type LogRow = (String, String, String, String);

fn ist_midnight_utc(d: NaiveDate) -> String {
    d.and_hms_opt(0, 0, 0)
        .and_then(|n| Kolkata.from_local_datetime(&n).single())
        .map(|t| t.with_timezone(&Utc).format(TS).to_string())
        .unwrap_or_default()
}

/// UTC bounds of an IST date range (today when neither is given).
pub fn range(
    today: NaiveDate,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> (String, String) {
    let (from, to) = match (start, end) {
        (None, None) => (Some(today), Some(today)),
        other => other,
    };
    (
        from.map(ist_midnight_utc).unwrap_or_else(|| "0000".into()),
        to.map(|d| ist_midnight_utc(d + Duration::days(1)))
            .unwrap_or_else(|| "9999".into()),
    )
}

fn rows_between(ctx: &AppState, from: &str, to: &str) -> Result<Vec<LogRow>> {
    let conn = ctx.logs.conn()?;
    let mut stmt = conn.prepare(
        "SELECT api_type, request_data, response_data, created_at FROM analyzer_logs
         WHERE created_at >= ?1 AND created_at < ?2 ORDER BY created_at DESC, id DESC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![from, to, MAX_ROWS], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn parse_obj(s: &str) -> Value {
    match serde_json::from_str::<Value>(s) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

fn get_or(v: &Value, k: &str, d: Value) -> Value {
    v.get(k).cloned().unwrap_or(d)
}

/// Web `format_request`.
fn format_request(row: &LogRow) -> Value {
    let (api_type, req, resp, created) = row;
    let request = redact(parse_obj(req));
    let response = parse_obj(resp);
    let timestamp = DateTime::parse_from_rfc3339(created)
        .map(|t| {
            t.with_timezone(&Kolkata)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| created.clone());
    let is_error = response.get("status").and_then(Value::as_str) == Some("error");
    let mut out = json!({
        "timestamp": timestamp,
        "api_type": api_type,
        "source": get_or(&request, "strategy", json!("Unknown")),
        "request_data": request,
        "response_data": response,
        "analysis": {
            "issues": is_error,
            "error": get_or(&response, "message", Value::Null),
            "error_type": if is_error { "error" } else { "success" },
            "warnings": get_or(&response, "warnings", json!([])),
        },
    });
    let request = out["request_data"].clone();
    let Some(m) = out.as_object_mut() else {
        return out;
    };
    if api_type == "placeorder" || api_type == "placesmartorder" {
        m.insert(
            "symbol".into(),
            get_or(&request, "symbol", json!("Unknown")),
        );
        m.insert(
            "exchange".into(),
            get_or(&request, "exchange", json!("Unknown")),
        );
        m.insert(
            "action".into(),
            get_or(&request, "action", json!("Unknown")),
        );
        m.insert("quantity".into(), get_or(&request, "quantity", json!(0)));
        m.insert(
            "price_type".into(),
            get_or(&request, "pricetype", json!("Unknown")),
        );
        m.insert(
            "product_type".into(),
            get_or(&request, "product", json!("Unknown")),
        );
        if api_type == "placesmartorder" {
            m.insert(
                "position_size".into(),
                get_or(&request, "position_size", json!(0)),
            );
        }
    } else if api_type == "cancelorder" {
        m.insert(
            "orderid".into(),
            get_or(&request, "orderid", json!("Unknown")),
        );
    }
    out
}

/// The issue kinds, in the order the web counts them.
const ISSUE_KINDS: [&str; 5] = [
    "rate_limit",
    "invalid_symbol",
    "missing_quantity",
    "invalid_exchange",
    "other",
];

/// Web `get_analyzer_stats` over the last 24 hours as the page consumes it:
/// every request in the window, read row by row, not only the newest
/// `MAX_ROWS` the page lists (OBS-05).
fn stats(ctx: &AppState, now: DateTime<Utc>) -> Result<Value> {
    let from = (now - Duration::hours(24)).format(TS).to_string();
    let conn = ctx.logs.conn()?;
    let mut stmt = conn.prepare(
        "SELECT request_data, response_data FROM analyzer_logs
         WHERE created_at >= ?1 ORDER BY created_at DESC, id DESC",
    )?;
    let mut acc = StatsAcc::default();
    let mut rows = stmt.query([&from])?;
    while let Some(r) = rows.next()? {
        acc.add(
            &parse_obj(&r.get::<_, String>(0)?),
            &parse_obj(&r.get::<_, String>(1)?),
        );
    }
    Ok(acc.into_json())
}

/// The running totals of [`stats`]. Sources and symbols keep the order
/// they were first seen in (newest first), each listed once.
#[derive(Default)]
struct StatsAcc {
    total: usize,
    sources: Vec<String>,
    seen_sources: HashSet<String>,
    symbols: Vec<Value>,
    seen_symbols: HashSet<String>,
    issues: [i64; 5],
}

impl StatsAcc {
    fn add(&mut self, request: &Value, response: &Value) {
        self.total += 1;
        let source = match request.get("strategy") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => "Unknown".into(),
        };
        if self.seen_sources.insert(source.clone()) {
            self.sources.push(source);
        }
        if let Some(sym) = request.get("symbol") {
            if self.seen_symbols.insert(sym.to_string()) {
                self.symbols.push(sym.clone());
            }
        }
        if response.get("status").and_then(Value::as_str) == Some("error") {
            let msg = response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            self.issues[issue_kind(&msg)] += 1;
        }
    }

    fn into_json(self) -> Value {
        let by_type: Map<String, Value> = ISSUE_KINDS
            .iter()
            .zip(self.issues)
            .map(|(k, c)| ((*k).to_string(), json!(c)))
            .collect();
        json!({
            "total_requests": self.total,
            "issues": {"total": self.issues.iter().sum::<i64>(), "by_type": by_type},
            "symbols": self.symbols,
            "sources": self.sources,
        })
    }
}

/// Web `get_analyzer_stats`'s classification of an error message.
fn issue_kind(lowercase_message: &str) -> usize {
    let m = lowercase_message;
    if m.contains("rate limit") {
        0
    } else if m.contains("invalid symbol") {
        1
    } else if m.contains("quantity") {
        2
    } else if m.contains("exchange") {
        3
    } else {
        4
    }
}

/// The Analyzer page's data: the requests in `from..to` (newest
/// `MAX_ROWS`) and the last 24 hours' statistics, read off the async
/// workers and handed to `render` there (which answers with the page's
/// JSON text, so up to 10,000 requests are not serialized on the async
/// worker either). Statistics that cannot be read are empty, as on the
/// web.
pub async fn page<T, F>(ctx: &Arc<AppState>, from: String, to: String, render: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Vec<Value>, Value) -> Result<T> + Send + 'static,
{
    ctx.report(move |c| {
        let requests = rows_between(c, &from, &to)?
            .iter()
            .map(format_request)
            .collect();
        let stats = stats(c, c.now()).unwrap_or_else(|e| {
            tracing::error!("Analyzer stats failed: {}", e);
            StatsAcc::default().into_json()
        });
        render(requests, stats)
    })
    .await
}

/// The requests in `from..to` as the export's CSV, read and formatted off
/// the async workers.
pub async fn export(ctx: &Arc<AppState>, from: String, to: String) -> Result<String> {
    ctx.report(move |c| {
        let requests: Vec<Value> = rows_between(c, &from, &to)?
            .iter()
            .map(format_request)
            .collect();
        Ok(csv(&requests))
    })
    .await
}

/// Python `str()` of a value for the CSV.
fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn csv_row(fields: &[String]) -> String {
    let mut line = fields
        .iter()
        .map(|f| {
            if f.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

/// Web `generate_csv`.
fn csv(requests: &[Value]) -> String {
    let headers = [
        "Timestamp",
        "API Type",
        "Source",
        "Symbol",
        "Exchange",
        "Action",
        "Quantity",
        "Price Type",
        "Product Type",
        "Status",
        "Error Message",
    ];
    let mut out = csv_row(&headers.map(String::from));
    for r in requests {
        let issues = r["analysis"]["issues"].as_bool().unwrap_or(false);
        out.push_str(&csv_row(&[
            cell(r.get("timestamp")),
            cell(r.get("api_type")),
            cell(r.get("source")),
            cell(r.get("symbol")),
            cell(r.get("exchange")),
            cell(r.get("action")),
            cell(r.get("quantity")),
            cell(r.get("price_type")),
            cell(r.get("product_type")),
            if issues { "Error" } else { "Success" }.to_string(),
            cell(r["analysis"].get("error")),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_web() {
        let row: LogRow = (
            "placesmartorder".into(),
            json!({"symbol": "SBIN", "strategy": "s1", "apikey": "k", "position_size": 5})
                .to_string(),
            json!({"status": "error", "message": "Invalid quantity"}).to_string(),
            "2026-10-05T04:30:00Z".into(),
        );
        let v = format_request(&row);
        assert_eq!(v["timestamp"], "2026-10-05 10:00:00");
        assert_eq!(v["source"], "s1");
        assert_eq!(v["exchange"], "Unknown");
        assert_eq!(v["position_size"], 5);
        assert_eq!(v["analysis"]["error_type"], "error");
        assert!(v["request_data"].get("apikey").is_none());
        let c = csv(&[v]);
        assert!(c.starts_with("Timestamp,API Type"));
        assert!(c.contains("Error,Invalid quantity\r\n"));
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert_eq!(
            range(today, None, None),
            ("2026-10-04T18:30:00Z".into(), "2026-10-05T18:30:00Z".into())
        );
    }
}
