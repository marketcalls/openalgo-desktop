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
