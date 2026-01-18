//! DuckDB database module for historical data (Historify)

pub mod models;
mod migrations;

use crate::error::Result;
use duckdb::Connection;
use models::MarketDataRow;
use parking_lot::Mutex;
use std::path::Path;

/// DuckDB database wrapper
pub struct DuckDb {
    conn: Mutex<Connection>,
}

impl DuckDb {
    /// Create new DuckDB connection
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        let db = Self {
            conn: Mutex::new(conn),
        };

        // Run migrations
        db.run_migrations()?;

        Ok(db)
    }

    /// Run database migrations
    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn.lock();
        migrations::run_migrations(&conn)
    }

    /// Query market data
    pub fn query_market_data(
        &self,
        symbol: &str,
        exchange: &str,
        timeframe: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<MarketDataRow>> {
        let conn = self.conn.lock();

        let mut stmt = conn.prepare(
            "SELECT timestamp, open, high, low, close, volume
             FROM market_data
             WHERE symbol = ? AND exchange = ? AND timeframe = ?
               AND timestamp >= ? AND timestamp <= ?
             ORDER BY timestamp ASC",
        )?;

        let rows = stmt
            .query_map(
                duckdb::params![symbol, exchange, timeframe, from_date, to_date],
                |row| {
                    Ok(MarketDataRow {
                        timestamp: row.get(0)?,
                        open: row.get(1)?,
                        high: row.get(2)?,
                        low: row.get(3)?,
                        close: row.get(4)?,
                        volume: row.get(5)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Insert market data
    pub fn insert_market_data(
        &self,
        symbol: &str,
        exchange: &str,
        timeframe: &str,
        data: &[MarketDataRow],
    ) -> Result<usize> {
        let mut conn = self.conn.lock();

        let tx = conn.transaction()?;

        let mut stmt = tx.prepare(
            "INSERT INTO market_data (symbol, exchange, timeframe, timestamp, open, high, low, close, volume)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (symbol, exchange, timeframe, timestamp) DO UPDATE SET
               open = excluded.open, high = excluded.high, low = excluded.low,
               close = excluded.close, volume = excluded.volume",
        )?;

        let mut count = 0;
        for row in data {
            stmt.execute(duckdb::params![
                symbol,
                exchange,
                timeframe,
                row.timestamp,
                row.open,
                row.high,
                row.low,
                row.close,
                row.volume,
            ])?;
            count += 1;
        }

        drop(stmt);
        tx.commit()?;

        Ok(count)
    }
}
