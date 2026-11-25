//! Funds (web `api/funds.py`, marketcalls/openalgo#2467). The margin
//! calculator is in `margin.rs`.
//!
//! `GET /api/v2/funds/view?client_id=<id>&type=all` (Pocketful docs,
//! "Funds"; the SDK's `api.funds`) answers a label and value table,
//! `data.values` as `[label, amount]` pairs with amounts as text
//! ("1,250.50"). The rows read, by the label the docs give them:
//!
//! * available cash: `Available`, the funds available in the account now;
//! * collateral: `DP Pledge Collateral` plus `Pool Pledge Collateral`, the
//!   funds obtained against pledged securities. The `... Collateral
//!   Benefit` rows describe the same pledge from another angle, so they are
//!   not added on top;
//! * utilised debits: `Net Margin`, the balance required after all open
//!   positions, which is the margin in use.
//!
//! The answer carries no M2M. Realised and unrealised M2M come from the
//! net-wise position book, each row split by `mapping::position_pnl_split`,
//! the split the position book itself reports, so the dashboard and the
//! position book agree. A book that cannot be read leaves both at zero and
//! the funds still stand, as on the web (and as the Noren family's
//! position-book M2M).
//!
//! Funds that cannot be read are an error, never a zero balance (BR-03): a
//! session Pocketful refuses is expired, a refusal reads as Pocketful's
//! reason, and a success without the table is unreadable. The web answers
//! `{}` for each, which its dashboard and session resume read as a dead
//! session; the desktop's resume reads any error that way.

use super::client::{Answer, Lane, Request};
use super::{mapping, orders, session_expired, PocketfulBroker};
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::types::{AuthToken, Funds};
use crate::error::Result;
use serde_json::Value;
use std::collections::BTreeMap;

const FUNDS: &str = "/api/v2/funds/view";

const AVAILABLE: &str = "Available";
const COLLATERAL_ROWS: [&str; 2] = ["DP Pledge Collateral", "Pool Pledge Collateral"];
const USED: &str = "Net Margin";

/// Amounts by label, the last row of a label winning (web `_rows`).
type Table = BTreeMap<String, f64>;

/// The account's funds (web `get_margin_data`).
pub async fn get_funds(b: &PocketfulBroker, auth: &AuthToken) -> Result<Funds> {
    // Checked first, so a session without its account sends nothing.
    let client_id = b.client_id(auth)?;
    let req = Request::get(FUNDS)
        .query("client_id", client_id)
        .query("type", "all");
    let answer = b
        .send(Lane::Data, auth, req)
        .await
        .inspect_err(|e| tracing::warn!("Pocketful funds request failed: {}", e.code()))?;
    let table = funds_table(&answer)?;
    let (realised, unrealised) = position_m2m(b, auth).await;
    Ok(funds_from(&table, realised, unrealised))
}

/// The funds table of a success answer, or why there is none.
fn funds_table(a: &Answer) -> Result<Table> {
    if a.session_refused() {
        tracing::warn!(
            status = a.status.as_u16(),
            "Pocketful refused the session for the funds"
        );
        return Err(session_expired());
    }
    if !a.ok() {
        return Err(orders::refused(a, "funds", false));
    }
    let Some(values) = a
        .payload()
        .and_then(|d| d.get("values"))
        .and_then(Value::as_array)
    else {
        return Err(book_read::unreadable("pocketful", Book::Funds));
    };
    let table = table_of(values);
    let missing: Vec<&str> = [AVAILABLE, USED]
        .into_iter()
        .chain(COLLATERAL_ROWS)
        .filter(|label| !table.contains_key(*label))
        .collect();
    if !missing.is_empty() {
        // Labels only, never amounts: what a first live run needs to see if
        // Pocketful's labels differ from its documentation.
        tracing::warn!(
            "Pocketful funds rows missing {:?}; rows received: {:?}",
            missing,
            table.keys().collect::<Vec<_>>()
        );
    }
    Ok(table)
}

/// `[label, amount]` rows as a table; a row that is not a pair is skipped.
fn table_of(values: &[Value]) -> Table {
    values
        .iter()
        .filter_map(Value::as_array)
        .filter(|row| row.len() >= 2)
        .map(|row| (label(&row[0]), amount(&row[1])))
        .collect()
}

/// web `str(row[0]).strip()`.
fn label(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

/// An amount as Pocketful writes it, thousands separators included; 0 when
/// it does not read (web `_to_float`).
fn amount(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.replace(',', "").trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
    .unwrap_or(0.0)
}

/// (realised, unrealised) across the net-wise position book; zero when it
/// cannot be read (web `_position_m2m`). Each row is split on its own: a
/// position closed in part has booked P&L on the matched quantity and open
/// P&L on the rest.
async fn position_m2m(b: &PocketfulBroker, auth: &AuthToken) -> (f64, f64) {
    match orders::read_positions(b, auth).await {
        Ok(rows) => rows
            .iter()
            .map(mapping::position_pnl_split)
            .fold((0.0, 0.0), |(realised, unrealised), (booked, open)| {
                (realised + booked, unrealised + open)
            }),
        Err(e) => {
            tracing::warn!(
                "Pocketful position book could not be read for M2M: {}",
                e.code()
            );
            (0.0, 0.0)
        }
    }
}

fn funds_from(table: &Table, realised: f64, unrealised: f64) -> Funds {
    let row = |label: &str| table.get(label).copied().unwrap_or(0.0);
    let used = row(USED);
    Funds {
        available_cash: row(AVAILABLE),
        collateral: COLLATERAL_ROWS.into_iter().map(row).sum(),
        used_margin: used,
        utilised_debits: used,
        m2m_realized: realised,
        m2m_unrealized: unrealised,
        ..Default::default()
    }
}
