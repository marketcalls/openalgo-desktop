//! After a master is installed, rows saved under a symbol it renamed move to
//! the current symbol (`db::sqlite::symbol_renames`, web openalgo#2463):
//! Zerodha's master stores HFCL-BE as HFCL now.
//!
//! Moved: what holds a quantity no running owner keeps in memory, so the
//! position can still be closed and netted: the sandbox's orders, trades,
//! positions, holdings and GTTs, and the strategy book's legs and order
//! tags. Not moved: rows a running owner holds in memory as their identity
//! (scalping stops, Strategy module and OpenScript runs, Action Center
//! queued orders) and lists the trader keeps (watchlists, Chartink
//! mappings, Historify); the broker-integration skill names them, with
//! what the trader does about each.
//!
//! Runs after every install, the stored master's included, so a pass that
//! did not finish (the app quit, a store was busy) is made up at the next;
//! and once the download claim is released, so it never holds up the next
//! download.

use crate::db::sqlite::symbol_renames::{series_renames, RenameTally};
use crate::error::{AppError, Result};
use crate::state::AppState;
use std::sync::Arc;

/// Move the saved rows on the blocking pool. Never fails the install: a
/// store that could not be moved is logged and tried again next time.
pub(super) async fn carry_over(ctx: &Arc<AppState>) {
    let ctx = ctx.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || move_saved_rows(&ctx)).await {
        tracing::warn!("Moving rows saved under renamed symbols stopped: {}", e);
    }
}

fn move_saved_rows(ctx: &AppState) {
    let renames = match ctx.sqlite.conn().and_then(|c| series_renames(&c)) {
        Ok(r) if r.is_empty() => return,
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Renamed symbols could not be read from the master: {}", e);
            return;
        }
    };
    let sandbox = ctx
        .sandbox
        .rename_symbols(&renames)
        .map_err(|e| AppError::Internal(e.message));
    report("sandbox", sandbox);
    report("strategy book", ctx.strategy.book.rename_symbols(&renames));
}

fn report(store: &str, moved: Result<RenameTally>) {
    match moved {
        Ok(t) => {
            if t.moved > 0 {
                tracing::info!(
                    "Moved {} {} rows saved under a renamed symbol to its current symbol",
                    t.moved,
                    store
                );
            }
            for kept in t.kept {
                tracing::warn!(
                    "Kept {} rows under the old symbol ({}): the current symbol already has the same position",
                    store,
                    kept
                );
            }
        }
        Err(e) => tracing::warn!(
            "{} rows saved under renamed symbols were not moved: {}",
            store,
            e
        ),
    }
}

#[cfg(test)]
mod tests;
