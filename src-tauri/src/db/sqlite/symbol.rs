//! Symbol/token management

use crate::error::Result;
use crate::state::SymbolInfo;
use rusqlite::Connection;

/// Store symbols in database
pub fn store_symbols(conn: &mut Connection, symbols: &[SymbolInfo]) -> Result<()> {
    let tx = conn.transaction()?;

    // Clear existing symbols
    tx.execute("DELETE FROM symtoken", [])?;

    // Insert new symbols
    let mut stmt = tx.prepare(
        "INSERT INTO symtoken (symbol, token, exchange, name, lot_size, tick_size, instrument_type)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;

    for symbol in symbols {
        stmt.execute(rusqlite::params![
            symbol.symbol,
            symbol.token,
            symbol.exchange,
            symbol.name,
            symbol.lot_size,
            symbol.tick_size,
            symbol.instrument_type,
        ])?;
    }

    drop(stmt);
    tx.commit()?;

    Ok(())
}

/// Load all symbols from database
pub fn load_symbols(conn: &Connection) -> Result<Vec<SymbolInfo>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, token, exchange, name, lot_size, tick_size, instrument_type FROM symtoken",
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
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(symbols)
}
