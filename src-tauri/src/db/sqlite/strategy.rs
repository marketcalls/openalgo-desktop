//! Strategy management

use crate::db::sqlite::models::Strategy;
use crate::error::{AppError, Result};
use rusqlite::Connection;

/// Get all strategies
pub fn get_strategies(conn: &Connection) -> Result<Vec<Strategy>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, webhook_id, exchange, symbol, product, quantity, enabled, created_at, updated_at
         FROM strategies ORDER BY created_at DESC",
    )?;

    let strategies = stmt
        .query_map([], |row| {
            Ok(Strategy {
                id: row.get(0)?,
                name: row.get(1)?,
                webhook_id: row.get(2)?,
                exchange: row.get(3)?,
                symbol: row.get(4)?,
                product: row.get(5)?,
                quantity: row.get(6)?,
                enabled: row.get::<_, i32>(7)? == 1,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(strategies)
}

/// Create a new strategy
pub fn create_strategy(conn: &Connection, strategy: &Strategy) -> Result<Strategy> {
    conn.execute(
        "INSERT INTO strategies (name, webhook_id, exchange, symbol, product, quantity, enabled)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            strategy.name,
            strategy.webhook_id,
            strategy.exchange,
            strategy.symbol,
            strategy.product,
            strategy.quantity,
            strategy.enabled as i32,
        ],
    )?;

    let id = conn.last_insert_rowid();

    // Return the created strategy
    get_strategy_by_id(conn, id)
}

/// Get strategy by ID
fn get_strategy_by_id(conn: &Connection, id: i64) -> Result<Strategy> {
    conn.query_row(
        "SELECT id, name, webhook_id, exchange, symbol, product, quantity, enabled, created_at, updated_at
         FROM strategies WHERE id = ?",
        [id],
        |row| {
            Ok(Strategy {
                id: row.get(0)?,
                name: row.get(1)?,
                webhook_id: row.get(2)?,
                exchange: row.get(3)?,
                symbol: row.get(4)?,
                product: row.get(5)?,
                quantity: row.get(6)?,
                enabled: row.get::<_, i32>(7)? == 1,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            AppError::NotFound(format!("Strategy not found: {}", id))
        }
        _ => e.into(),
    })
}

/// Update a strategy
pub fn update_strategy(
    conn: &Connection,
    id: i64,
    name: Option<String>,
    exchange: Option<String>,
    symbol: Option<String>,
    product: Option<String>,
    quantity: Option<i32>,
    enabled: Option<bool>,
) -> Result<Strategy> {
    let mut updates = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(n) = name {
        updates.push("name = ?");
        params.push(Box::new(n));
    }
    if let Some(e) = exchange {
        updates.push("exchange = ?");
        params.push(Box::new(e));
    }
    if let Some(s) = symbol {
        updates.push("symbol = ?");
        params.push(Box::new(s));
    }
    if let Some(p) = product {
        updates.push("product = ?");
        params.push(Box::new(p));
    }
    if let Some(q) = quantity {
        updates.push("quantity = ?");
        params.push(Box::new(q));
    }
    if let Some(e) = enabled {
        updates.push("enabled = ?");
        params.push(Box::new(e as i32));
    }

    if updates.is_empty() {
        return get_strategy_by_id(conn, id);
    }

    updates.push("updated_at = datetime('now')");

    let sql = format!(
        "UPDATE strategies SET {} WHERE id = ?",
        updates.join(", ")
    );

    params.push(Box::new(id));

    let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, params_refs.as_slice())?;

    get_strategy_by_id(conn, id)
}

/// Delete a strategy
pub fn delete_strategy(conn: &Connection, id: i64) -> Result<()> {
    let rows = conn.execute("DELETE FROM strategies WHERE id = ?", [id])?;

    if rows == 0 {
        return Err(AppError::NotFound(format!("Strategy not found: {}", id)));
    }

    Ok(())
}

/// Get strategy by webhook_id (for webhook handler)
/// Returns the handler's Strategy type which has all fields needed for webhook processing
pub fn get_strategy_by_webhook_id(
    conn: &Connection,
    webhook_id: &str,
) -> Result<Option<crate::webhook::handlers::Strategy>> {
    let result = conn.query_row(
        "SELECT id, name, webhook_id, enabled FROM strategies WHERE webhook_id = ?",
        [webhook_id],
        |row| {
            Ok(crate::webhook::handlers::Strategy {
                id: row.get(0)?,
                name: row.get(1)?,
                webhook_id: row.get(2)?,
                is_active: row.get::<_, i32>(3)? == 1,
                // TODO: Add these fields to strategies table in a future migration
                is_intraday: false,  // Default to positional
                trading_mode: "BOTH".to_string(),  // Default to both directions
                start_time: Some("09:15".to_string()),
                end_time: Some("15:15".to_string()),
                squareoff_time: Some("15:25".to_string()),
            })
        },
    );

    match result {
        Ok(strategy) => Ok(Some(strategy)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get symbol mapping for a strategy
/// Currently uses the strategy's direct symbol/exchange/quantity fields
/// TODO: Implement proper strategy_symbol_mappings table for multi-symbol strategies
pub fn get_symbol_mapping(
    conn: &Connection,
    strategy_id: i64,
    symbol: &str,
) -> Result<Option<crate::webhook::handlers::SymbolMapping>> {
    // For now, get from the strategy's own symbol field
    // In future, this should query strategy_symbol_mappings table
    let result = conn.query_row(
        "SELECT symbol, exchange, quantity, product FROM strategies WHERE id = ? AND symbol = ?",
        rusqlite::params![strategy_id, symbol],
        |row| {
            Ok(crate::webhook::handlers::SymbolMapping {
                symbol: row.get(0)?,
                exchange: row.get(1)?,
                quantity: row.get(2)?,
                product_type: row.get(3)?,
            })
        },
    );

    match result {
        Ok(mapping) => Ok(Some(mapping)),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Try without symbol filter (single-symbol strategy)
            let result2 = conn.query_row(
                "SELECT symbol, exchange, quantity, product FROM strategies WHERE id = ?",
                [strategy_id],
                |row| {
                    Ok(crate::webhook::handlers::SymbolMapping {
                        symbol: row.get(0)?,
                        exchange: row.get(1)?,
                        quantity: row.get(2)?,
                        product_type: row.get(3)?,
                    })
                },
            );
            match result2 {
                Ok(mapping) => Ok(Some(mapping)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}
