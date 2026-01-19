//! Symbol/token management
//!
//! Provides efficient O(1) symbol lookups via in-memory cache.
//! Database operations are used for persistence only - runtime lookups
//! should use the cache in AppState.

use crate::error::Result;
use crate::state::SymbolInfo;
use rusqlite::{params, Connection};

/// Store symbols in database (batch insert with transaction)
/// Uses chunked inserts for performance with large datasets (100k+ symbols)
pub fn store_symbols(conn: &mut Connection, symbols: &[SymbolInfo]) -> Result<()> {
    tracing::info!("Storing {} symbols to database...", symbols.len());

    let tx = conn.transaction()?;

    // Optimize for bulk insert
    tx.execute_batch("PRAGMA synchronous = OFF; PRAGMA journal_mode = MEMORY;")?;

    // Clear existing symbols
    tx.execute("DELETE FROM symtoken", [])?;

    // Use chunked batch inserts for better performance
    // SQLite supports up to 999 variables per statement, we use 9 per row = 111 rows per batch
    const CHUNK_SIZE: usize = 100;

    for (chunk_idx, chunk) in symbols.chunks(CHUNK_SIZE).enumerate() {
        if chunk_idx > 0 && chunk_idx % 1000 == 0 {
            tracing::debug!("Inserted {} symbols...", chunk_idx * CHUNK_SIZE);
        }

        // Build multi-row INSERT statement
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let base = i * 9;
                format!(
                    "(?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                    base + 1, base + 2, base + 3, base + 4, base + 5,
                    base + 6, base + 7, base + 8, base + 9
                )
            })
            .collect();

        let sql = format!(
            "INSERT INTO symtoken (symbol, token, exchange, name, lot_size, tick_size, instrument_type, brsymbol, brexchange) VALUES {}",
            placeholders.join(", ")
        );

        // Collect all parameters for the chunk
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(chunk.len() * 9);
        for s in chunk {
            params_vec.push(Box::new(s.symbol.clone()));
            params_vec.push(Box::new(s.token.clone()));
            params_vec.push(Box::new(s.exchange.clone()));
            params_vec.push(Box::new(s.name.clone()));
            params_vec.push(Box::new(s.lot_size));
            params_vec.push(Box::new(s.tick_size));
            params_vec.push(Box::new(s.instrument_type.clone()));
            params_vec.push(Box::new(s.brsymbol.clone()));
            params_vec.push(Box::new(s.brexchange.clone()));
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        tx.execute(&sql, params_refs.as_slice())?;
    }

    // Restore normal settings
    tx.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA journal_mode = WAL;")?;

    tx.commit()?;

    tracing::info!("Stored {} symbols in database", symbols.len());
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
