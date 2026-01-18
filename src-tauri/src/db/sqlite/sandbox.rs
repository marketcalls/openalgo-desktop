//! Sandbox (paper trading) management
//!
//! Provides paper trading functionality with simulated order execution,
//! position tracking, holdings, and funds management.

use crate::db::sqlite::models::{SandboxFunds, SandboxHolding, SandboxOrder, SandboxPosition};
use crate::error::Result;
use rusqlite::{params, Connection};
use uuid::Uuid;

/// Get sandbox positions
pub fn get_positions(conn: &Connection) -> Result<Vec<SandboxPosition>> {
    let mut stmt = conn.prepare(
        "SELECT id, symbol, exchange, product, quantity, average_price, ltp, pnl, created_at, updated_at
         FROM sandbox_positions WHERE quantity != 0 ORDER BY symbol",
    )?;

    let positions = stmt
        .query_map([], |row| {
            Ok(SandboxPosition {
                id: row.get(0)?,
                symbol: row.get(1)?,
                exchange: row.get(2)?,
                product: row.get(3)?,
                quantity: row.get(4)?,
                average_price: row.get(5)?,
                ltp: row.get(6)?,
                pnl: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(positions)
}

/// Get sandbox orders
pub fn get_orders(conn: &Connection) -> Result<Vec<SandboxOrder>> {
    let mut stmt = conn.prepare(
        "SELECT id, order_id, symbol, exchange, side, quantity, price, order_type, product, status,
                filled_quantity, average_price, created_at, updated_at
         FROM sandbox_orders ORDER BY created_at DESC LIMIT 100",
    )?;

    let orders = stmt
        .query_map([], |row| {
            Ok(SandboxOrder {
                id: row.get(0)?,
                order_id: row.get(1)?,
                symbol: row.get(2)?,
                exchange: row.get(3)?,
                side: row.get(4)?,
                quantity: row.get(5)?,
                price: row.get(6)?,
                order_type: row.get(7)?,
                product: row.get(8)?,
                status: row.get(9)?,
                filled_quantity: row.get(10)?,
                average_price: row.get(11)?,
                created_at: row.get(12)?,
                updated_at: row.get(13)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(orders)
}

/// Place a sandbox order
pub fn place_order(
    conn: &Connection,
    symbol: &str,
    exchange: &str,
    side: &str,
    quantity: i32,
    price: f64,
    order_type: &str,
    product: &str,
) -> Result<SandboxOrder> {
    let order_id = format!("SB{}", Uuid::new_v4().to_string().replace("-", "")[..12].to_uppercase());

    // For market orders, simulate immediate fill
    let (status, filled_qty, avg_price) = if order_type == "MARKET" {
        ("complete", quantity, price)
    } else {
        ("pending", 0, 0.0)
    };

    conn.execute(
        "INSERT INTO sandbox_orders (order_id, symbol, exchange, side, quantity, price, order_type, product, status, filled_quantity, average_price)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![order_id, symbol, exchange, side, quantity, price, order_type, product, status, filled_qty, avg_price],
    )?;

    // Update position if order is filled
    if status == "complete" {
        update_position(conn, symbol, exchange, side, quantity, price, product)?;
    }

    let id = conn.last_insert_rowid();

    Ok(SandboxOrder {
        id,
        order_id,
        symbol: symbol.to_string(),
        exchange: exchange.to_string(),
        side: side.to_string(),
        quantity,
        price,
        order_type: order_type.to_string(),
        product: product.to_string(),
        status: status.to_string(),
        filled_quantity: filled_qty,
        average_price: avg_price,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Update position after order fill
fn update_position(
    conn: &Connection,
    symbol: &str,
    exchange: &str,
    side: &str,
    quantity: i32,
    price: f64,
    product: &str,
) -> Result<()> {
    let qty_change = if side == "BUY" { quantity } else { -quantity };

    // Get existing position
    let existing = conn.query_row(
        "SELECT quantity, average_price FROM sandbox_positions WHERE exchange = ? AND symbol = ? AND product = ?",
        rusqlite::params![exchange, symbol, product],
        |row| Ok((row.get::<_, i32>(0)?, row.get::<_, f64>(1)?)),
    );

    match existing {
        Ok((current_qty, current_avg)) => {
            let new_qty = current_qty + qty_change;

            // Calculate new average price
            let new_avg = if new_qty == 0 {
                0.0
            } else if (current_qty >= 0 && qty_change > 0) || (current_qty <= 0 && qty_change < 0) {
                // Adding to position
                ((current_qty.abs() as f64 * current_avg) + (quantity as f64 * price))
                    / (current_qty.abs() + quantity) as f64
            } else {
                // Reducing position - keep existing average
                current_avg
            };

            conn.execute(
                "UPDATE sandbox_positions SET quantity = ?, average_price = ?, updated_at = datetime('now')
                 WHERE exchange = ? AND symbol = ? AND product = ?",
                rusqlite::params![new_qty, new_avg, exchange, symbol, product],
            )?;
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Create new position
            conn.execute(
                "INSERT INTO sandbox_positions (symbol, exchange, product, quantity, average_price, ltp)
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![symbol, exchange, product, qty_change, price, price],
            )?;
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

/// Reset sandbox
pub fn reset(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM sandbox_orders", [])?;
    conn.execute("DELETE FROM sandbox_positions", [])?;
    conn.execute("DELETE FROM sandbox_trades", [])?;
    conn.execute("DELETE FROM sandbox_holdings", [])?;
    conn.execute("UPDATE sandbox_funds SET available_cash = 1000000, used_margin = 0, total_value = 1000000", [])?;
    conn.execute("DELETE FROM sandbox_daily_pnl", [])?;
    tracing::info!("Sandbox reset completed");
    Ok(())
}

/// Get sandbox holdings
pub fn get_holdings(conn: &Connection) -> Result<Vec<SandboxHolding>> {
    let mut stmt = conn.prepare(
        "SELECT id, symbol, exchange, quantity, average_price, ltp, pnl, created_at, updated_at
         FROM sandbox_holdings WHERE quantity > 0 ORDER BY symbol",
    )?;

    let holdings = stmt
        .query_map([], |row| {
            Ok(SandboxHolding {
                id: row.get(0)?,
                symbol: row.get(1)?,
                exchange: row.get(2)?,
                quantity: row.get(3)?,
                average_price: row.get(4)?,
                ltp: row.get(5)?,
                pnl: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(holdings)
}

/// Get sandbox funds
pub fn get_funds(conn: &Connection) -> Result<SandboxFunds> {
    let funds = conn.query_row(
        "SELECT available_cash, used_margin, total_value, updated_at FROM sandbox_funds WHERE id = 1",
        [],
        |row| {
            Ok(SandboxFunds {
                available_cash: row.get(0)?,
                used_margin: row.get(1)?,
                total_value: row.get(2)?,
                updated_at: row.get(3)?,
            })
        },
    )?;

    Ok(funds)
}

/// Update LTP for a position and recalculate P&L
pub fn update_position_ltp(
    conn: &Connection,
    exchange: &str,
    symbol: &str,
    ltp: f64,
) -> Result<()> {
    // Update position LTP and calculate P&L
    conn.execute(
        "UPDATE sandbox_positions
         SET ltp = ?1, pnl = (quantity * (?1 - average_price)), updated_at = datetime('now')
         WHERE exchange = ?2 AND symbol = ?3",
        params![ltp, exchange, symbol],
    )?;

    // Update holdings LTP and calculate P&L
    conn.execute(
        "UPDATE sandbox_holdings
         SET ltp = ?1, pnl = (quantity * (?1 - average_price)), updated_at = datetime('now')
         WHERE exchange = ?2 AND symbol = ?3",
        params![ltp, exchange, symbol],
    )?;

    Ok(())
}

/// Update funds (margin used, etc.)
pub fn update_funds(
    conn: &Connection,
    available_cash: Option<f64>,
    used_margin: Option<f64>,
) -> Result<SandboxFunds> {
    if let Some(cash) = available_cash {
        conn.execute(
            "UPDATE sandbox_funds SET available_cash = ?1, updated_at = datetime('now') WHERE id = 1",
            params![cash],
        )?;
    }

    if let Some(margin) = used_margin {
        conn.execute(
            "UPDATE sandbox_funds SET used_margin = ?1, updated_at = datetime('now') WHERE id = 1",
            params![margin],
        )?;
    }

    // Recalculate total value
    conn.execute(
        "UPDATE sandbox_funds SET total_value = available_cash + used_margin, updated_at = datetime('now') WHERE id = 1",
        [],
    )?;

    get_funds(conn)
}

/// Cancel a sandbox order
pub fn cancel_order(conn: &Connection, order_id: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE sandbox_orders SET status = 'cancelled', updated_at = datetime('now')
         WHERE order_id = ?1 AND status = 'pending'",
        params![order_id],
    )?;

    Ok(rows > 0)
}
