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
        filled_quantity: Some(filled_qty),
        average_price: Some(avg_price),
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

/// Sandbox configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SandboxConfig {
    pub starting_capital: f64,
    pub reset_day: String,
    pub reset_time: String,
    pub order_check_interval: i32,
    pub mtm_update_interval: i32,
    pub nse_mis_leverage: f64,
    pub nfo_mis_leverage: f64,
    pub cds_mis_leverage: f64,
    pub mcx_mis_leverage: f64,
    pub nse_cnc_leverage: f64,
    pub nfo_nrml_leverage: f64,
    pub cds_nrml_leverage: f64,
    pub mcx_nrml_leverage: f64,
    pub nse_square_off_time: String,
    pub nfo_square_off_time: String,
    pub cds_square_off_time: String,
    pub mcx_square_off_time: String,
}

/// Get sandbox configuration
pub fn get_config(conn: &Connection) -> Result<SandboxConfig> {
    let config = conn.query_row(
        "SELECT starting_capital, reset_day, reset_time, order_check_interval, mtm_update_interval,
                nse_mis_leverage, nfo_mis_leverage, cds_mis_leverage, mcx_mis_leverage,
                nse_cnc_leverage, nfo_nrml_leverage, cds_nrml_leverage, mcx_nrml_leverage,
                nse_square_off_time, nfo_square_off_time, cds_square_off_time, mcx_square_off_time
         FROM sandbox_config WHERE id = 1",
        [],
        |row| {
            Ok(SandboxConfig {
                starting_capital: row.get(0)?,
                reset_day: row.get(1)?,
                reset_time: row.get(2)?,
                order_check_interval: row.get(3)?,
                mtm_update_interval: row.get(4)?,
                nse_mis_leverage: row.get(5)?,
                nfo_mis_leverage: row.get(6)?,
                cds_mis_leverage: row.get(7)?,
                mcx_mis_leverage: row.get(8)?,
                nse_cnc_leverage: row.get(9)?,
                nfo_nrml_leverage: row.get(10)?,
                cds_nrml_leverage: row.get(11)?,
                mcx_nrml_leverage: row.get(12)?,
                nse_square_off_time: row.get(13)?,
                nfo_square_off_time: row.get(14)?,
                cds_square_off_time: row.get(15)?,
                mcx_square_off_time: row.get(16)?,
            })
        },
    )?;

    Ok(config)
}

/// Update sandbox configuration
pub fn update_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    // Validate key is a valid column name
    let valid_keys = [
        "starting_capital", "reset_day", "reset_time", "order_check_interval", "mtm_update_interval",
        "nse_mis_leverage", "nfo_mis_leverage", "cds_mis_leverage", "mcx_mis_leverage",
        "nse_cnc_leverage", "nfo_nrml_leverage", "cds_nrml_leverage", "mcx_nrml_leverage",
        "nse_square_off_time", "nfo_square_off_time", "cds_square_off_time", "mcx_square_off_time",
    ];

    if !valid_keys.contains(&key) {
        return Err(crate::error::AppError::Validation(format!("Invalid config key: {}", key)));
    }

    // Update the config - use parameterized value but column name is validated above
    let sql = format!(
        "UPDATE sandbox_config SET {} = ?1, updated_at = datetime('now') WHERE id = 1",
        key
    );
    conn.execute(&sql, params![value])?;

    Ok(())
}

/// Sandbox trade record
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxTrade {
    pub id: i64,
    pub order_id: String,
    pub trade_id: String,
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub quantity: i32,
    pub price: f64,
    pub created_at: String,
}

/// Get sandbox trades
pub fn get_trades(conn: &Connection) -> Result<Vec<SandboxTrade>> {
    let mut stmt = conn.prepare(
        "SELECT id, order_id, trade_id, symbol, exchange, side, quantity, price, created_at
         FROM sandbox_trades ORDER BY created_at DESC LIMIT 100",
    )?;

    let trades = stmt
        .query_map([], |row| {
            Ok(SandboxTrade {
                id: row.get(0)?,
                order_id: row.get(1)?,
                trade_id: row.get(2)?,
                symbol: row.get(3)?,
                exchange: row.get(4)?,
                side: row.get(5)?,
                quantity: row.get(6)?,
                price: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(trades)
}

/// Daily P&L record
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxDailyPnl {
    pub id: i64,
    pub date: String,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_pnl: f64,
    pub portfolio_value: f64,
    pub created_at: String,
}

/// Get sandbox daily P&L history
pub fn get_daily_pnl(conn: &Connection) -> Result<Vec<SandboxDailyPnl>> {
    let mut stmt = conn.prepare(
        "SELECT id, date, realized_pnl, unrealized_pnl, total_pnl, portfolio_value, created_at
         FROM sandbox_daily_pnl ORDER BY date DESC LIMIT 30",
    )?;

    let pnl = stmt
        .query_map([], |row| {
            Ok(SandboxDailyPnl {
                id: row.get(0)?,
                date: row.get(1)?,
                realized_pnl: row.get(2)?,
                unrealized_pnl: row.get(3)?,
                total_pnl: row.get(4)?,
                portfolio_value: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(pnl)
}

/// Consolidated P&L summary
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxPnlSummary {
    pub today_realized_pnl: f64,
    pub positions_unrealized_pnl: f64,
    pub holdings_unrealized_pnl: f64,
    pub today_total_mtm: f64,
    pub all_time_realized_pnl: f64,
    pub portfolio_value: f64,
}

/// Consolidated P&L data
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxPnlData {
    pub summary: SandboxPnlSummary,
    pub daily_pnl: Vec<SandboxDailyPnl>,
    pub positions: Vec<SandboxPosition>,
    pub holdings: Vec<SandboxHolding>,
    pub trades: Vec<SandboxTrade>,
}

/// Get consolidated sandbox P&L data
pub fn get_pnl_data(conn: &Connection) -> Result<SandboxPnlData> {
    // Get positions, holdings, trades
    let positions = get_positions(conn)?;
    let holdings = get_holdings(conn)?;
    let trades = get_trades(conn)?;
    let daily_pnl = get_daily_pnl(conn)?;
    let funds = get_funds(conn)?;

    // Calculate unrealized P&L from positions
    let positions_unrealized_pnl: f64 = positions.iter()
        .map(|p| p.pnl)
        .sum();

    // Calculate unrealized P&L from holdings
    let holdings_unrealized_pnl: f64 = holdings.iter()
        .map(|h| h.pnl)
        .sum();

    // Calculate today's realized P&L from today's trades
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_realized_pnl = daily_pnl.iter()
        .find(|p| p.date.starts_with(&today))
        .map(|p| p.realized_pnl)
        .unwrap_or(0.0);

    // Calculate all-time realized P&L
    let all_time_realized_pnl: f64 = daily_pnl.iter()
        .map(|p| p.realized_pnl)
        .sum();

    let summary = SandboxPnlSummary {
        today_realized_pnl,
        positions_unrealized_pnl,
        holdings_unrealized_pnl,
        today_total_mtm: today_realized_pnl + positions_unrealized_pnl,
        all_time_realized_pnl,
        portfolio_value: funds.total_value,
    };

    Ok(SandboxPnlData {
        summary,
        daily_pnl,
        positions,
        holdings,
        trades,
    })
}
