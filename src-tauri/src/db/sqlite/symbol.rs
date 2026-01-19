//! Symbol/token management
//!
//! Provides efficient O(1) symbol lookups via in-memory cache.
//! Database operations are used for persistence only - runtime lookups
//! should use the cache in AppState.

use crate::error::Result;
use crate::state::SymbolInfo;
use rusqlite::{params, Connection};

/// Store symbols in database (batch insert with transaction)
/// Uses prepared statements within a transaction for performance
pub fn store_symbols(conn: &mut Connection, symbols: &[SymbolInfo]) -> Result<()> {
    tracing::info!("Storing {} symbols to database...", symbols.len());

    let start = std::time::Instant::now();

    // Start transaction
    let tx = conn.transaction()?;

    // Clear existing symbols
    tx.execute("DELETE FROM symtoken", [])?;
    tracing::debug!("Cleared existing symbols");

    // Use prepared statement for fast inserts
    // Use INSERT OR REPLACE to handle duplicate (exchange, symbol) combinations in source data
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO symtoken (symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        for (idx, symbol) in symbols.iter().enumerate() {
            if idx > 0 && idx % 50000 == 0 {
                tracing::info!("Inserted {} symbols...", idx);
            }

            if let Err(e) = stmt.execute(params![
                &symbol.symbol,
                &symbol.token,
                &symbol.exchange,
                &symbol.name,
                symbol.lot_size,
                symbol.tick_size,
                &symbol.instrument_type,
                &symbol.brsymbol,
                &symbol.brexchange,
            ]) {
                tracing::error!("Failed to insert symbol {}: {:?} - Error: {}", idx, symbol.symbol, e);
                return Err(e.into());
            }
        }
    }

    tracing::info!("Committing transaction...");
    tx.commit()?;

    let elapsed = start.elapsed();
    tracing::info!("Stored {} symbols in database in {:.2}s", symbols.len(), elapsed.as_secs_f64());
    Ok(())
}

/// Load all symbols from database (used to populate cache on startup)
pub fn load_symbols(conn: &Connection) -> Result<Vec<SymbolInfo>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange FROM symtoken",
    )?;

    let symbols = stmt
        .query_map([], |row| {
            Ok(SymbolInfo {
                symbol: row.get(0)?,
                token: row.get(1)?,
                exchange: row.get(2)?,
                name: row.get(3)?,
                lot_size: row.get(4)?,
                tick_size: row.get(5)?,
                instrument_type: row.get(6)?,
                brsymbol: row.get(7)?,
                brexchange: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    tracing::debug!("Loaded {} symbols from database", symbols.len());
    Ok(symbols)
}

/// Get symbol count from database
#[allow(dead_code)]
pub fn count_symbols(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symtoken",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Get symbols by exchange from database
#[allow(dead_code)]
pub fn get_symbols_by_exchange(conn: &Connection, exchange: &str) -> Result<Vec<SymbolInfo>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange
         FROM symtoken
         WHERE exchange = ?1",
    )?;

    let symbols = stmt
        .query_map(params![exchange], |row| {
            Ok(SymbolInfo {
                symbol: row.get(0)?,
                token: row.get(1)?,
                exchange: row.get(2)?,
                name: row.get(3)?,
                lot_size: row.get(4)?,
                tick_size: row.get(5)?,
                instrument_type: row.get(6)?,
                brsymbol: row.get(7)?,
                brexchange: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(symbols)
}

/// Search symbols in database by name/symbol pattern
#[allow(dead_code)]
pub fn search_symbols(
    conn: &Connection,
    query: &str,
    exchange: Option<&str>,
    limit: usize,
) -> Result<Vec<SymbolInfo>> {
    let query_pattern = format!("%{}%", query);
    let limit_i64 = limit as i64;

    if let Some(exch) = exchange {
        let mut stmt = conn.prepare(
            "SELECT symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange
             FROM symtoken
             WHERE (symbol LIKE ?1 OR name LIKE ?1) AND exchange = ?2
             LIMIT ?3",
        )?;
        let symbols = stmt.query_map(params![query_pattern, exch, limit_i64], |row| {
            Ok(SymbolInfo {
                symbol: row.get(0)?,
                token: row.get(1)?,
                exchange: row.get(2)?,
                name: row.get(3)?,
                lot_size: row.get(4)?,
                tick_size: row.get(5)?,
                instrument_type: row.get(6)?,
                brsymbol: row.get(7)?,
                brexchange: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(symbols)
    } else {
        let mut stmt = conn.prepare(
            "SELECT symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange
             FROM symtoken
             WHERE symbol LIKE ?1 OR name LIKE ?1
             LIMIT ?2",
        )?;
        let symbols = stmt.query_map(params![query_pattern, limit_i64], |row| {
            Ok(SymbolInfo {
                symbol: row.get(0)?,
                token: row.get(1)?,
                exchange: row.get(2)?,
                name: row.get(3)?,
                lot_size: row.get(4)?,
                tick_size: row.get(5)?,
                instrument_type: row.get(6)?,
                brsymbol: row.get(7)?,
                brexchange: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(symbols)
    }
}
