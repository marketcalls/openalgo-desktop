//! The transaction cost report (web `sandbox/charge_manager.py`
//! `parse_report_range`, `cost_report`, `cost_report_trades`): charges paid
//! in a range of IST days, as totals, by segment, by day and by trade.
//!
//! The totals and both breakdowns cover every trade in the range, summed in
//! exact decimals (the web sums in SQLite, as floats); only the trade list
//! is cut, most recent first.

use crate::sandbox::charges::{classify_segment, paisa, Charges, CHARGE_COLUMNS, SEGMENTS};
use crate::sandbox::clock::display_seconds;
use crate::sandbox::db::TradeRow;
use crate::sandbox::types::float;
use chrono::{Duration, NaiveDate};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Trades listed on the report page. The CSV has no limit.
pub(crate) const REPORT_TRADE_LIMIT: usize = 200;

/// Days the report covers when no range is asked for.
const REPORT_DEFAULT_DAYS: i64 = 30;

/// A trade on an exchange the rate card has no segment for.
const OTHER: (&str, &str) = ("other", "Other");

/// A report's first and last IST day, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// The report's two date fields as a range (web `parse_report_range`).
/// `today` is the IST date: trades are stamped with the IST wall clock and
/// the report counts IST days, so a machine on UTC must not shift it (web
/// c38c5810f). With no dates, the last 30 days; the error is a sentence
/// for the trader.
pub(crate) fn parse_report_range(
    from: Option<&str>,
    to: Option<&str>,
    today: NaiveDate,
) -> Result<ReportRange, String> {
    let parse = |text: Option<&str>| -> Result<Option<NaiveDate>, String> {
        let text = text.unwrap_or("").trim();
        if text.is_empty() {
            return Ok(None);
        }
        NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| "Dates must look like 2026-11-23 (year, month, day).".to_string())
    };
    let to = parse(to)?.unwrap_or(today);
    let from = parse(from)?.unwrap_or(to - Duration::days(REPORT_DEFAULT_DAYS - 1));
    if from > to {
        return Err("The start date is after the end date.".to_string());
    }
    Ok(ReportRange { from, to })
}

/// Every trade of `user` in the range, oldest first.
pub(crate) fn trades_in(
    conn: &Connection,
    user: &str,
    range: ReportRange,
) -> rusqlite::Result<Vec<TradeRow>> {
    let start = range.from.format("%Y-%m-%d").to_string();
    let end = (range.to + Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM sandbox_trades WHERE user_id = ?1 AND trade_timestamp >= ?2
           AND trade_timestamp < ?3 ORDER BY trade_timestamp ASC, id ASC",
        TradeRow::COLUMNS
    ))?;
    let rows = stmt.query_map(params![user, start, end], TradeRow::from_row)?;
    rows.collect()
}

/// Price times quantity, to the paisa (the report's turnover carries no
/// contract value, as the web's does not).
fn turnover(t: &TradeRow) -> Decimal {
    t.price * Decimal::from(t.quantity.abs())
}

/// `(key, label)` of the segment a trade was charged under.
fn segment_of(t: &TradeRow) -> (&'static str, &'static str) {
    classify_segment(&t.exchange, &t.product, &t.symbol)
        .map(|s| (s.key, s.label))
        .unwrap_or(OTHER)
}

/// One trade with its charges, as the report and the CSV list it (web
/// `trade_row`).
pub(crate) fn trade_row(t: &TradeRow) -> Value {
    let (key, label) = segment_of(t);
    let mut row = json!({
        "tradeid": t.tradeid,
        "orderid": t.orderid,
        "timestamp": display_seconds(&t.trade_timestamp),
        "symbol": t.symbol,
        "exchange": t.exchange,
        "product": t.product,
        "action": t.action,
        "quantity": t.quantity,
        "price": float(t.price),
        "turnover": float(paisa(turnover(t))),
        "segment": key,
        "segment_label": label,
        "strategy": t.strategy.clone().unwrap_or_default(),
    });
    if let (Value::Object(m), Value::Object(c)) = (&mut row, t.charges.as_json()) {
        m.extend(c);
    }
    row
}

#[derive(Default)]
struct Bucket {
    trades: i64,
    turnover: Decimal,
    charges: Charges,
}

impl Bucket {
    fn add(&mut self, trades: i64, turnover: Decimal, charges: &Charges) {
        self.trades += trades;
        self.turnover += turnover;
        self.charges = self.charges + *charges;
    }

    fn json(&self, head: &[(&str, Value)]) -> Value {
        let mut m = Map::new();
        for (k, v) in head {
            m.insert((*k).to_string(), v.clone());
        }
        m.insert("trades".into(), json!(self.trades));
        m.insert("turnover".into(), json!(float(self.turnover)));
        for (name, value) in CHARGE_COLUMNS.iter().zip(self.charges.columns()) {
            m.insert((*name).to_string(), json!(float(value)));
        }
        Value::Object(m)
    }
}

/// The report (web `cost_report`). Grouped as the web groups it (day,
/// exchange, product, option or not), each group's turnover to the paisa,
/// then added into the totals, the day and the segment.
pub(crate) fn cost_report(
    conn: &Connection,
    user: &str,
    range: ReportRange,
    limit: usize,
) -> rusqlite::Result<Value> {
    let trades = trades_in(conn, user, range)?;
    type GroupKey = (String, String, String, bool);
    let mut groups: BTreeMap<GroupKey, (Bucket, (&str, &str))> = BTreeMap::new();
    for t in &trades {
        let sym = t.symbol.to_ascii_uppercase();
        let key = (
            t.trade_timestamp.chars().take(10).collect::<String>(),
            t.exchange.clone(),
            t.product.clone(),
            sym.ends_with("CE") || sym.ends_with("PE"),
        );
        let entry = groups
            .entry(key)
            .or_insert_with(|| (Bucket::default(), segment_of(t)));
        entry.0.add(1, turnover(t), &t.charges);
    }
    let mut totals = Bucket::default();
    let mut by_day: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_segment: BTreeMap<&str, (Bucket, &str)> = BTreeMap::new();
    for ((day, ..), (g, (seg, label))) in &groups {
        let turnover = paisa(g.turnover);
        totals.add(g.trades, turnover, &g.charges);
        by_day
            .entry(day.clone())
            .or_default()
            .add(g.trades, turnover, &g.charges);
        by_segment
            .entry(*seg)
            .or_insert_with(|| (Bucket::default(), *label))
            .0
            .add(g.trades, turnover, &g.charges);
    }
    let rank = |k: &str| {
        SEGMENTS
            .iter()
            .position(|s| s.key == k)
            .unwrap_or(SEGMENTS.len())
    };
    let mut segments: Vec<(&str, (Bucket, &str))> = by_segment.into_iter().collect();
    segments.sort_by_key(|(k, _)| rank(k));
    let listed: Vec<Value> = trades.iter().rev().take(limit).map(trade_row).collect();
    let trades_listed = listed.len();
    Ok(json!({
        "from": range.from.format("%Y-%m-%d").to_string(),
        "to": range.to.format("%Y-%m-%d").to_string(),
        "totals": totals.json(&[]),
        "by_segment": segments
            .iter()
            .map(|(k, (b, label))| b.json(&[("segment", json!(k)), ("label", json!(label))]))
            .collect::<Vec<_>>(),
        "by_day": by_day
            .iter()
            .rev()
            .map(|(d, b)| b.json(&[("date", json!(d))]))
            .collect::<Vec<_>>(),
        "trades": listed,
        "trades_listed": trades_listed,
        "trades_total": totals.trades,
    }))
}
