//! Saved rows follow an instrument whose OpenAlgo symbol the master renamed
//! (web openalgo#2463).
//!
//! Zerodha's master stored an NSE stock traded outside the EQ series under
//! Kite's tradingsymbol (HFCL-BE, EMBASSY-RR); it now stores the base symbol
//! NSE lists (HFCL, EMBASSY) and keeps the tradingsymbol in `brsymbol`. A
//! row saved under the old symbol (a sandbox holding, a strategy book leg)
//! then names a symbol no order can use, and an order under the new one
//! does not net against it, so the position can be neither closed nor
//! reduced. After a master is installed, the stores that hold such rows move
//! them to the current symbol (`services::master_contract_service`,
//! `renamed_symbols`). The web moves nothing: a recorded deviation.
//!
//! A rename comes from the stored master alone: an NSE row whose `brsymbol`
//! is its `symbol` with a series suffix, where no NSE row has that
//! `brsymbol` as its own symbol. A saved symbol the master still lists is
//! never touched, and moving is idempotent: a second pass finds nothing
//! left under an old symbol, so a row written under one after the first
//! pass (an order in flight across the download) moves at the next.

use crate::brokers::common::master_contract::series_base;
use crate::error::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;

/// One instrument's move: rows saved under `from` on `exchange` belong to
/// `to` now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRename {
    pub exchange: String,
    pub from: String,
    pub to: String,
}

/// What moving did in one store.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RenameTally {
    /// Rows now saved under the current symbol.
    pub moved: usize,
    /// `table: OLD -> NEW` where rows stayed under the old symbol because a
    /// row under the new one already holds the same key (a position saved
    /// under both names): moving would merge two positions.
    pub kept: Vec<String>,
}

impl RenameTally {
    pub fn add(&mut self, other: RenameTally) {
        self.moved += other.moved;
        self.kept.extend(other.kept);
    }
}

/// The renames the stored master implies (see the module doc), once each.
pub fn series_renames(conn: &Connection) -> Result<Vec<SymbolRename>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.symbol, s.brsymbol FROM symtoken s
         WHERE s.exchange = 'NSE' AND s.symbol <> s.brsymbol
           AND NOT EXISTS (SELECT 1 FROM symtoken t
                           WHERE t.exchange = 'NSE' AND t.symbol = s.brsymbol)
         ORDER BY s.brsymbol",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut renames = Vec::new();
    for row in rows {
        let (symbol, brsymbol) = row?;
        // `to` is read from `from`, so one tradingsymbol never has two.
        if series_base(&brsymbol) == Some(symbol.as_str()) {
            renames.push(SymbolRename {
                exchange: "NSE".into(),
                from: brsymbol,
                to: symbol,
            });
        }
    }
    Ok(renames)
}

/// Move `table`'s rows saved under a rename's old symbol to its new one,
/// inside the caller's transaction. `table` is one of the app's own tables
/// with `symbol` and `exchange` columns, never input. A row whose move
/// would break a unique key stays where it is and is named in `kept`.
pub fn rename_rows(
    conn: &Connection,
    table: &'static str,
    renames: &[SymbolRename],
) -> rusqlite::Result<RenameTally> {
    let mut tally = RenameTally::default();
    let mut by_exchange: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for r in renames {
        by_exchange
            .entry(r.exchange.as_str())
            .or_default()
            .insert(r.from.as_str(), r.to.as_str());
    }
    for (exchange, to_by_from) in by_exchange {
        let saved: Vec<String> = {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT symbol FROM {table} WHERE exchange = ?1"
            ))?;
            let rows = stmt.query_map([exchange], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for from in saved {
            let Some(to) = to_by_from.get(from.as_str()) else {
                continue;
            };
            tally.moved += conn.execute(
                &format!(
                    "UPDATE OR IGNORE {table} SET symbol = ?1 WHERE exchange = ?2 AND symbol = ?3"
                ),
                params![to, exchange, from],
            )?;
            let left: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE exchange = ?1 AND symbol = ?2"),
                params![exchange, from],
                |r| r.get(0),
            )?;
            if left > 0 {
                tally.kept.push(format!("{table}: {from} -> {to}"));
            }
        }
    }
    Ok(tally)
}

#[cfg(test)]
mod tests;
