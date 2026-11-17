//! Rows saved under a symbol the master renamed move to its current symbol
//! (`db::sqlite::symbol_renames`, web openalgo#2463): a holding, position,
//! working order or GTT saved as HFCL-BE could otherwise be neither closed
//! (the master no longer lists HFCL-BE) nor netted (an order for HFCL finds
//! nothing under that name).

use super::core::EngineCmd;
use super::types::SbResult;
use super::Sandbox;
use crate::db::sqlite::symbol_renames::{rename_rows, RenameTally, SymbolRename};

/// Every sandbox table that names an instrument by symbol and exchange.
const TABLES: [&str; 5] = [
    "sandbox_orders",
    "sandbox_trades",
    "sandbox_positions",
    "sandbox_holdings",
    "sandbox_gtt",
];

impl Sandbox {
    /// Move every table's rows in one transaction, so an engine decision
    /// sees an instrument under one name; then the engine watches the
    /// instruments under their current names.
    pub(crate) fn rename_symbols(&self, renames: &[SymbolRename]) -> SbResult<RenameTally> {
        let tally = self.core().db.with_tx(|tx| -> rusqlite::Result<_> {
            let mut tally = RenameTally::default();
            for table in TABLES {
                tally.add(rename_rows(tx, table, renames)?);
            }
            Ok(tally)
        })?;
        if tally.moved > 0 {
            self.core().notify_engine(EngineCmd::Rebuild);
        }
        Ok(tally)
    }
}
