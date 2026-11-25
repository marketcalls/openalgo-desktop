//! Sandbox P&L CSV exports (web `blueprints/sandbox.py`
//! `generate_*_csv` and the `/sandbox/mypnl/export/*` routes), read from
//! `sandbox.db` for the sandbox's user.

use crate::sandbox::clock::display_seconds;
use crate::sandbox::db::{DailyPnlRow, HoldingRow, PositionRow, TradeRow};
use crate::sandbox::types::float;
use crate::sandbox::{Sandbox, SandboxError};
use rusqlite::Connection;
use rust_decimal::Decimal;

/// Which export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Export {
    Daily,
    Positions,
    Holdings,
    Trades,
}

impl Export {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "daily" => Export::Daily,
            "positions" => Export::Positions,
            "holdings" => Export::Holdings,
            "trades" => Export::Trades,
            _ => return None,
        })
    }

    /// File name stem (`sandbox_<stem>_<timestamp>.csv`).
    pub fn stem(&self) -> &'static str {
        match self {
            Export::Daily => "daily_pnl",
            Export::Positions => "positions",
            Export::Holdings => "holdings",
            Export::Trades => "trades",
        }
    }

    /// The web's 404 message when there is nothing to export.
    pub fn empty_message(&self) -> &'static str {
        match self {
            Export::Daily => "No daily P&L data to export",
            Export::Positions => "No positions data to export",
            Export::Holdings => "No holdings data to export",
            Export::Trades => "No trades data to export",
        }
    }
}

/// Python `str(float(x))`.
pub fn py_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}

fn dec(d: Decimal) -> String {
    py_float(float(d))
}

/// Web `sanitize_csv_value`: formula-leading text gets a quote prefix
/// (a leading `-` is left alone for negative numbers).
pub fn sanitize(v: &str) -> String {
    match v.chars().next() {
        Some('=' | '+' | '@' | '\t' | '\r') => format!("'{}", v),
        _ => v.to_string(),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn line(out: &mut String, fields: &[String]) {
    out.push_str(
        &fields
            .iter()
            .map(|f| csv_field(f))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
}

fn headers(out: &mut String, h: &[&str]) {
    line(out, &h.iter().map(|s| s.to_string()).collect::<Vec<_>>());
}

/// The daily P&L history, newest first.
fn daily_csv(conn: &Connection, user: &str) -> Result<Option<String>, SandboxError> {
    let mut out = String::new();
    let mut stmt =
        conn.prepare("SELECT * FROM sandbox_daily_pnl WHERE user_id = ?1 ORDER BY date DESC")?;
    let rows = stmt
        .query_map([user], DailyPnlRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    headers(
        &mut out,
        &[
            "Date",
            "Realized P&L",
            "Positions Unrealized",
            "Holdings Unrealized",
            "Total Unrealized",
            "Total MTM",
            "Portfolio Value",
            "Charges",
            "Net MTM",
        ],
    );
    for r in rows {
        line(
            &mut out,
            &[
                sanitize(&r.date),
                dec(r.realized_pnl),
                dec(r.positions_unrealized_pnl),
                dec(r.holdings_unrealized_pnl),
                dec(r.positions_unrealized_pnl + r.holdings_unrealized_pnl),
                dec(r.total_mtm),
                dec(r.portfolio_value),
                dec(r.charges),
                dec(r.total_mtm - r.charges),
            ],
        );
    }
    Ok(Some(out))
}

/// The positions, most recently updated first.
fn positions_csv(conn: &Connection, user: &str) -> Result<Option<String>, SandboxError> {
    let mut out = String::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM sandbox_positions WHERE user_id = ?1 ORDER BY updated_at DESC, id DESC",
        PositionRow::COLUMNS
    ))?;
    let rows = stmt
        .query_map([user], PositionRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    headers(
        &mut out,
        &[
            "Symbol",
            "Exchange",
            "Product",
            "Quantity",
            "Average Price",
            "LTP",
            "Unrealized P&L",
            "Today Realized P&L",
            "All-Time Realized P&L",
            "Margin Blocked",
            "Status",
            "Last Updated",
        ],
    );
    for p in rows {
        let open = p.quantity != 0;
        line(
            &mut out,
            &[
                sanitize(&p.symbol),
                sanitize(&p.exchange),
                sanitize(p.product.as_str()),
                p.quantity.to_string(),
                dec(p.average_price),
                dec(p.ltp.unwrap_or(Decimal::ZERO)),
                dec(if open { p.pnl } else { Decimal::ZERO }),
                dec(p.today_realized_pnl),
                dec(p.accumulated_realized_pnl),
                dec(p.margin_blocked),
                if open { "Open" } else { "Closed" }.to_string(),
                display_seconds(&p.updated_at),
            ],
        );
    }
    Ok(Some(out))
}

/// The holdings, most recently updated first.
fn holdings_csv(conn: &Connection, user: &str) -> Result<Option<String>, SandboxError> {
    let mut out = String::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM sandbox_holdings WHERE user_id = ?1 ORDER BY updated_at DESC, id DESC",
        HoldingRow::COLUMNS
    ))?;
    let rows = stmt
        .query_map([user], HoldingRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    headers(
        &mut out,
        &[
            "Symbol",
            "Exchange",
            "Quantity",
            "Average Price",
            "LTP",
            "Unrealized P&L",
            "P&L %",
            "Settlement Date",
        ],
    );
    for h in rows {
        line(
            &mut out,
            &[
                sanitize(&h.symbol),
                sanitize(&h.exchange),
                h.quantity.to_string(),
                dec(h.average_price),
                dec(h.ltp.unwrap_or(Decimal::ZERO)),
                dec(h.pnl),
                dec(h.pnl_percent),
                h.settlement_date.chars().take(10).collect(),
            ],
        );
    }
    Ok(Some(out))
}

/// Column headings of the transaction cost report's CSV, with the report
/// row keys they print (web `REPORT_CSV_COLUMNS`, openalgo#2496).
const COST_REPORT_COLUMNS: [(&str, &str); 19] = [
    ("Timestamp", "timestamp"),
    ("Trade ID", "tradeid"),
    ("Order ID", "orderid"),
    ("Symbol", "symbol"),
    ("Exchange", "exchange"),
    ("Product", "product"),
    ("Segment", "segment_label"),
    ("Action", "action"),
    ("Quantity", "quantity"),
    ("Price", "price"),
    ("Turnover", "turnover"),
    ("Brokerage", "brokerage"),
    ("STT/CTT", "stt"),
    ("Exchange Charges", "exchange_charges"),
    ("SEBI Charges", "sebi_charges"),
    ("Stamp Duty", "stamp_duty"),
    ("GST", "gst"),
    ("Total Charges", "total_charges"),
    ("Strategy", "strategy"),
];

/// The transaction cost report's trades as CSV, as Python's `csv` writes
/// them: text through [`sanitize`], whole numbers as integers, floats as
/// `repr`.
pub fn cost_report_csv(rows: &[serde_json::Value]) -> String {
    use serde_json::Value;
    let mut out = String::new();
    headers(&mut out, &COST_REPORT_COLUMNS.map(|(h, _)| h));
    for row in rows {
        let cells: Vec<String> = COST_REPORT_COLUMNS
            .iter()
            .map(|(_, key)| match row.get(*key) {
                Some(Value::String(s)) => sanitize(s),
                Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
                Some(Value::Number(n)) => py_float(n.as_f64().unwrap_or(0.0)),
                _ => String::new(),
            })
            .collect();
        line(&mut out, &cells);
    }
    out
}

/// The trades, newest first.
fn trades_csv(conn: &Connection, user: &str) -> Result<Option<String>, SandboxError> {
    let mut out = String::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM sandbox_trades WHERE user_id = ?1 ORDER BY trade_timestamp DESC, id DESC",
        TradeRow::COLUMNS
    ))?;
    let rows = stmt
        .query_map([user], TradeRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    headers(
        &mut out,
        &[
            "Trade ID",
            "Order ID",
            "Symbol",
            "Exchange",
            "Action",
            "Quantity",
            "Price",
            "Product",
            "Strategy",
            "Timestamp",
            "Charges",
        ],
    );
    for t in rows {
        line(
            &mut out,
            &[
                sanitize(&t.tradeid),
                sanitize(&t.orderid),
                sanitize(&t.symbol),
                sanitize(&t.exchange),
                sanitize(&t.action),
                t.quantity.to_string(),
                dec(t.price),
                sanitize(&t.product),
                sanitize(t.strategy.as_deref().unwrap_or("")),
                display_seconds(&t.trade_timestamp),
                dec(t.charges.total()),
            ],
        );
    }
    Ok(Some(out))
}

/// The CSV for one export, or `None` when there are no rows.
pub async fn export(sb: &Sandbox, which: Export) -> Result<Option<String>, SandboxError> {
    let user = sb.user_id().to_string();
    let db = sb.db().clone();
    tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| -> Result<Option<String>, SandboxError> {
            match which {
                Export::Daily => daily_csv(conn, &user),
                Export::Positions => positions_csv(conn, &user),
                Export::Holdings => holdings_csv(conn, &user),
                Export::Trades => trades_csv(conn, &user),
            }
        })
    })
    .await
    .map_err(|e| SandboxError::internal(format!("export task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_print_like_python() {
        assert_eq!(py_float(100.0), "100.0");
        assert_eq!(py_float(-2.5), "-2.5");
        assert_eq!(sanitize("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(sanitize("-12.5"), "-12.5");
        assert_eq!(Export::parse("daily"), Some(Export::Daily));
        assert_eq!(Export::parse("x"), None);
    }
}
