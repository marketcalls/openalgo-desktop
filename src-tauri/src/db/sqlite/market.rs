//! Market holidays and timings management
//!
//! Provides CRUD operations for market holidays and trading session timings.

use crate::error::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ============================================================================
// Market Holiday Types
// ============================================================================

/// Market holiday entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketHoliday {
    pub id: i64,
    pub date: String,
    pub description: Option<String>,
    pub year: i32,
    pub exchanges: Vec<String>,
}

/// Create request for a market holiday
#[derive(Debug, Clone, Deserialize)]
pub struct CreateHolidayRequest {
    pub date: String,
    pub description: Option<String>,
    pub exchanges: Vec<String>,
}

// ============================================================================
// Market Holiday Functions
// ============================================================================

/// Create a new market holiday
pub fn create_holiday(conn: &Connection, req: &CreateHolidayRequest) -> Result<MarketHoliday> {
    // Extract year from date (format: YYYY-MM-DD)
    let year: i32 = req.date[..4].parse().unwrap_or(2024);

    conn.execute(
        "INSERT INTO market_holidays (date, description, year) VALUES (?1, ?2, ?3)",
        params![req.date, req.description, year],
    )?;

    let id = conn.last_insert_rowid();

    // Insert exchange associations
    for exchange in &req.exchanges {
        conn.execute(
            "INSERT INTO market_holiday_exchanges (holiday_id, exchange) VALUES (?1, ?2)",
            params![id, exchange],
        )?;
    }

    tracing::info!("Created market holiday: {} ({})", req.date, id);

    Ok(MarketHoliday {
        id,
        date: req.date.clone(),
        description: req.description.clone(),
        year,
        exchanges: req.exchanges.clone(),
    })
}

/// Get all holidays for a year
pub fn get_holidays_by_year(conn: &Connection, year: i32) -> Result<Vec<MarketHoliday>> {
    let mut stmt = conn.prepare(
        "SELECT id, date, description, year FROM market_holidays WHERE year = ?1 ORDER BY date",
    )?;

    let holidays: Vec<(i64, String, Option<String>, i32)> = stmt
        .query_map(params![year], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Get exchanges for each holiday
    let mut result = Vec::new();
    for (id, date, description, year) in holidays {
        let exchanges = get_holiday_exchanges(conn, id)?;
        result.push(MarketHoliday {
            id,
            date,
            description,
            year,
            exchanges,
        });
    }

    Ok(result)
}

/// Get all holidays for an exchange
pub fn get_holidays_by_exchange(conn: &Connection, exchange: &str, year: Option<i32>) -> Result<Vec<MarketHoliday>> {
    let sql = if year.is_some() {
        r#"
        SELECT h.id, h.date, h.description, h.year
        FROM market_holidays h
        INNER JOIN market_holiday_exchanges e ON h.id = e.holiday_id
        WHERE e.exchange = ?1 AND h.year = ?2
        ORDER BY h.date
        "#
    } else {
        r#"
        SELECT h.id, h.date, h.description, h.year
        FROM market_holidays h
        INNER JOIN market_holiday_exchanges e ON h.id = e.holiday_id
        WHERE e.exchange = ?1
        ORDER BY h.date
        "#
    };

    let mut stmt = conn.prepare(sql)?;
    let holidays: Vec<(i64, String, Option<String>, i32)> = if let Some(y) = year {
        stmt.query_map(params![exchange, y], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    } else {
        stmt.query_map(params![exchange], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    let mut result = Vec::new();
    for (id, date, description, year) in holidays {
        let exchanges = get_holiday_exchanges(conn, id)?;
        result.push(MarketHoliday {
            id,
            date,
            description,
            year,
            exchanges,
        });
    }

    Ok(result)
}

/// Check if a date is a holiday for an exchange
pub fn is_holiday(conn: &Connection, exchange: &str, date: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM market_holidays h
        INNER JOIN market_holiday_exchanges e ON h.id = e.holiday_id
        WHERE e.exchange = ?1 AND h.date = ?2
        "#,
        params![exchange, date],
        |row| row.get(0),
    )?;

    Ok(count > 0)
}

fn get_holiday_exchanges(conn: &Connection, holiday_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT exchange FROM market_holiday_exchanges WHERE holiday_id = ?1",
    )?;

    let exchanges: Vec<String> = stmt
        .query_map(params![holiday_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(exchanges)
}

/// Delete a market holiday
pub fn delete_holiday(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute("DELETE FROM market_holidays WHERE id = ?1", params![id])?;
    Ok(rows > 0)
}

// ============================================================================
// Market Timings Types
// ============================================================================

/// Market timing entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketTiming {
    pub id: i64,
    pub exchange: String,
    pub pre_open_start: Option<String>,
    pub pre_open_end: Option<String>,
    pub market_open: String,
    pub market_close: String,
    pub post_close_end: Option<String>,
}

/// Update request for market timing
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTimingRequest {
    pub pre_open_start: Option<String>,
    pub pre_open_end: Option<String>,
    pub market_open: Option<String>,
    pub market_close: Option<String>,
    pub post_close_end: Option<String>,
}

// ============================================================================
// Market Timing Functions
// ============================================================================

/// Get all market timings
pub fn get_all_timings(conn: &Connection) -> Result<Vec<MarketTiming>> {
    let mut stmt = conn.prepare(
        "SELECT id, exchange, pre_open_start, pre_open_end, market_open, market_close, post_close_end
         FROM market_timings ORDER BY exchange",
    )?;

    let timings = stmt
        .query_map([], |row| {
            Ok(MarketTiming {
                id: row.get(0)?,
                exchange: row.get(1)?,
                pre_open_start: row.get(2)?,
                pre_open_end: row.get(3)?,
                market_open: row.get(4)?,
                market_close: row.get(5)?,
                post_close_end: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(timings)
}

/// Get timing for a specific exchange
pub fn get_timing_by_exchange(conn: &Connection, exchange: &str) -> Result<Option<MarketTiming>> {
    let result = conn.query_row(
        "SELECT id, exchange, pre_open_start, pre_open_end, market_open, market_close, post_close_end
         FROM market_timings WHERE exchange = ?1",
        params![exchange],
        |row| {
            Ok(MarketTiming {
                id: row.get(0)?,
                exchange: row.get(1)?,
                pre_open_start: row.get(2)?,
                pre_open_end: row.get(3)?,
                market_open: row.get(4)?,
                market_close: row.get(5)?,
                post_close_end: row.get(6)?,
            })
        },
    );

    match result {
        Ok(timing) => Ok(Some(timing)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Update market timing for an exchange
pub fn update_timing(conn: &Connection, exchange: &str, req: &UpdateTimingRequest) -> Result<MarketTiming> {
    // Build dynamic update query
    let mut updates = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(ref val) = req.pre_open_start {
        updates.push("pre_open_start = ?");
        params_vec.push(Box::new(val.clone()));
    }
    if let Some(ref val) = req.pre_open_end {
        updates.push("pre_open_end = ?");
        params_vec.push(Box::new(val.clone()));
    }
    if let Some(ref val) = req.market_open {
        updates.push("market_open = ?");
        params_vec.push(Box::new(val.clone()));
    }
    if let Some(ref val) = req.market_close {
        updates.push("market_close = ?");
        params_vec.push(Box::new(val.clone()));
    }
    if let Some(ref val) = req.post_close_end {
        updates.push("post_close_end = ?");
        params_vec.push(Box::new(val.clone()));
    }

    if !updates.is_empty() {
        params_vec.push(Box::new(exchange.to_string()));
        let sql = format!(
            "UPDATE market_timings SET {} WHERE exchange = ?",
            updates.join(", ")
        );
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, params_refs.as_slice())?;
    }

    get_timing_by_exchange(conn, exchange)?
        .ok_or_else(|| crate::error::AppError::NotFound(format!("Exchange {} not found", exchange)))
}

/// Create a new market timing
pub fn create_timing(conn: &Connection, timing: &MarketTiming) -> Result<MarketTiming> {
    conn.execute(
        r#"
        INSERT INTO market_timings (exchange, pre_open_start, pre_open_end, market_open, market_close, post_close_end)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            timing.exchange,
            timing.pre_open_start,
            timing.pre_open_end,
            timing.market_open,
            timing.market_close,
            timing.post_close_end
        ],
    )?;

    let id = conn.last_insert_rowid();
    tracing::info!("Created market timing for exchange: {}", timing.exchange);

    Ok(MarketTiming {
        id,
        exchange: timing.exchange.clone(),
        pre_open_start: timing.pre_open_start.clone(),
        pre_open_end: timing.pre_open_end.clone(),
        market_open: timing.market_open.clone(),
        market_close: timing.market_close.clone(),
        post_close_end: timing.post_close_end.clone(),
    })
}

/// Check if market is currently open
pub fn is_market_open(conn: &Connection, exchange: &str) -> Result<bool> {
    let timing = match get_timing_by_exchange(conn, exchange)? {
        Some(t) => t,
        None => return Ok(false),
    };

    // Get current time in IST
    let now = chrono::Utc::now();
    let ist = chrono_tz::Asia::Kolkata;
    let now_ist = now.with_timezone(&ist);
    let current_time = now_ist.format("%H:%M").to_string();

    // Check if current time is between market_open and market_close
    Ok(current_time >= timing.market_open && current_time <= timing.market_close)
}
