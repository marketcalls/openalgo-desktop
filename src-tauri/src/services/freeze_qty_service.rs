//! Freeze quantities (web `database/qty_freeze_db.py` and the
//! `blueprints/admin.py` freeze routes): the table the Freeze Quantities
//! page edits, in openalgo.db, and [`FreezeLimits`], which every lookup
//! reads: scalping's order-size check and freeze-sized exits, and the
//! `freeze_qty` of the symbol, search, option-symbol and Historify lists.

use super::symbol_service::{builtin_freeze_qty, freeze_underlying};
use crate::db::sqlite::webui::{self, FreezeQty};
use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use crate::state::AppState;
use parking_lot::{Mutex, RwLock};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Arc;

/// The freeze limit of a symbol: the trader's entry from the Freeze
/// Quantities page, else NSE's NFO limit built into the app, else none (0,
/// web `get_freeze_qty`).
///
/// Desktop departure: the web copies NSE's file into its table once and
/// lists those rows on the page. Here NSE's limits stay built in, so a
/// release carries NSE's new limits to every install (75a6d66), and the
/// table holds only the trader's own entries, which take precedence. They
/// apply as soon as the page saves them, as on the web (its cache reload).
#[derive(Clone, Default)]
pub struct FreezeLimits {
    entries: Arc<RwLock<Arc<HashMap<String, i64>>>>,
    /// One reload at a time: each reads the table and swaps the map
    /// together, so of two page changes in flight the later read lands
    /// last and no older map replaces a newer one.
    reload_lock: Arc<Mutex<()>>,
}

impl FreezeLimits {
    /// The limits with the page's entries read from `db`. Unreadable, the
    /// built-in limits apply alone (and the error is logged).
    pub fn loaded(db: &SqliteDb) -> Self {
        let limits = Self::default();
        if let Err(e) = db.conn().and_then(|c| limits.reload(&c)) {
            tracing::error!("Freeze quantities could not be read: {}", e);
        }
        limits
    }

    /// Read the page's entries again.
    pub fn reload(&self, conn: &Connection) -> Result<()> {
        let _one = self.reload_lock.lock();
        let map: HashMap<String, i64> = webui::freeze_list(conn)?
            .into_iter()
            .map(|f| (format!("{}:{}", f.exchange, f.symbol), f.freeze_qty))
            .collect();
        *self.entries.write() = Arc::new(map);
        Ok(())
    }

    /// Web `get_freeze_qty`: the limit for an underlying, 0 when none is
    /// known.
    pub fn qty(&self, symbol: &str, exchange: &str) -> i64 {
        let key = format!("{}:{}", exchange, symbol);
        let entries = self.entries.read().clone();
        entries
            .get(&key)
            .copied()
            .unwrap_or_else(|| builtin_freeze_qty(&key))
    }

    /// Web `get_freeze_qty_for_option`: the limit of an F&O symbol's
    /// underlying.
    pub fn for_option(&self, symbol: &str, exchange: &str) -> i64 {
        freeze_underlying(symbol).map_or(0, |u| self.qty(&u, exchange))
    }
}

/// After a change on the page: lookups follow it at once.
fn changed(ctx: &AppState, conn: &Connection) {
    if let Err(e) = ctx.freeze.reload(conn) {
        tracing::error!("Freeze quantities could not be read after a change: {}", e);
    }
}

/// Every entry.
pub fn list(ctx: &AppState) -> Result<Vec<FreezeQty>> {
    let c = ctx.sqlite.conn()?;
    webui::freeze_list(&c)
}

/// How many entries there are, on one exchange or on all.
pub fn count(ctx: &AppState, exchange: Option<&str>) -> Result<i64> {
    let c = ctx.sqlite.conn()?;
    webui::freeze_count(&c, exchange)
}

/// Add an entry. `None`: the symbol already has one on that exchange.
pub fn add(ctx: &AppState, exchange: &str, symbol: &str, qty: i64) -> Result<Option<FreezeQty>> {
    let c = ctx.sqlite.conn()?;
    let added = webui::freeze_add(&c, exchange, symbol, qty)?;
    changed(ctx, &c);
    Ok(added)
}

/// The entry `id`. An entry that cannot be read reads as missing (the
/// page answers "not found"); `Err` only when the database is unavailable.
pub fn get(ctx: &AppState, id: i64) -> Result<Option<FreezeQty>> {
    let c = ctx.sqlite.conn()?;
    Ok(webui::freeze_get(&c, id).ok().flatten())
}

/// Change the quantity of the entry `id`.
pub fn update(ctx: &AppState, id: i64, qty: i64) -> Result<bool> {
    let c = ctx.sqlite.conn()?;
    let updated = webui::freeze_update(&c, id, qty)?;
    changed(ctx, &c);
    Ok(updated)
}

/// Remove the entry `id`.
pub fn delete(ctx: &AppState, id: i64) -> Result<bool> {
    let c = ctx.sqlite.conn()?;
    let deleted = webui::freeze_delete(&c, id)?;
    changed(ctx, &c);
    Ok(deleted)
}

/// Replace every entry of `exchange` with `rows` (an uploaded exchange
/// file) and answer how many the exchange now has.
pub fn replace_exchange(ctx: &AppState, exchange: &str, rows: &[(String, i64)]) -> Result<i64> {
    let c = ctx.sqlite.conn()?;
    webui::freeze_replace_exchange(&c, exchange, rows)?;
    changed(ctx, &c);
    webui::freeze_count(&c, Some(exchange))
}
