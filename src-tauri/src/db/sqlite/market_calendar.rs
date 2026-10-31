//! Market calendar (web `database/market_calendar_db.py`): holidays with
//! per-exchange closures and special sessions, and per-exchange trading
//! hours. Serves the admin pages and `/api/v1/market/{holidays,timings}`.
//!
//! Schema: the web's columns (`holiday_date`, `holiday_type`,
//! `exchange_code`, `is_open`, `start_time`/`end_time` epoch ms,
//! `start_offset`/`end_offset` ms from IST midnight) are added to the tables
//! earlier desktop builds created, which keep their own columns so nothing
//! that reads them breaks; writes here fill both.

use crate::db::sqlite::migrations::column_exists;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate, TimeZone, Weekday};
use chrono_tz::Asia::Kolkata;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::HashMap;

/// Web `SUPPORTED_EXCHANGES`, in the web's order (timings are listed in it).
pub const SUPPORTED_EXCHANGES: &[&str] = &[
    "NSE", "BSE", "NFO", "BFO", "MCX", "BCD", "CDS", "NCO", "CRYPTO",
];
pub const HOLIDAY_TYPES: &[&str] = &["TRADING_HOLIDAY", "SETTLEMENT_HOLIDAY", "SPECIAL_SESSION"];
pub const CRYPTO_EXCHANGES: &[&str] = &["CRYPTO"];

/// Web `DEFAULT_MARKET_TIMINGS` (ms from IST midnight).
pub const DEFAULT_TIMINGS: &[(&str, i64, i64)] = &[
    ("NSE", 33_300_000, 55_800_000),
    ("BSE", 33_300_000, 55_800_000),
    ("NFO", 33_300_000, 56_400_000),
    ("BFO", 33_300_000, 56_400_000),
    ("CDS", 32_400_000, 61_200_000),
    ("BCD", 32_400_000, 61_200_000),
    ("MCX", 32_400_000, 86_100_000),
    ("NCO", 32_400_000, 86_100_000),
    ("CRYPTO", 0, 86_399_000),
];

/// Timings earlier desktop builds seeded. A row still holding exactly this
/// value was never edited and moves to the web default; anything else is
/// the trader's own setting and is kept.
const LEGACY_DESKTOP_DEFAULTS: &[(&str, &str, &str)] = &[
    ("NSE", "09:15", "15:30"),
    ("BSE", "09:15", "15:30"),
    ("NFO", "09:15", "15:30"),
    ("MCX", "09:00", "23:30"),
    ("CDS", "09:00", "17:00"),
];

const SEED_JSON: &str = include_str!("market_calendar_seed.json");

#[derive(Debug, Deserialize)]
struct SeedOpen {
    exchange: String,
    start_time: i64,
    end_time: i64,
}

#[derive(Debug, Deserialize)]
struct SeedHoliday {
    date: String,
    description: String,
    holiday_type: String,
    closed: Vec<String>,
    open: Vec<SeedOpen>,
}

pub fn hhmm(offset_ms: i64) -> String {
    format!(
        "{:02}:{:02}",
        offset_ms / 3_600_000,
        (offset_ms % 3_600_000) / 60_000
    )
}

/// `HH:MM` to ms from midnight, with the web's validation (`%H:%M`).
pub fn parse_hhmm(s: &str) -> Option<i64> {
    let t = chrono::NaiveTime::parse_from_str(s, "%H:%M").ok()?;
    use chrono::Timelike;
    Some(t.hour() as i64 * 3_600_000 + t.minute() as i64 * 60_000)
}

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Migration `050_market_calendar`: web columns, backfill from the desktop
/// columns, move untouched legacy timings to the web defaults, seed missing
/// timings and the web's holiday list (a date already present is left as
/// the trader has it).
pub fn migrate(conn: &Connection) -> Result<()> {
    add_column(conn, "market_holidays", "holiday_date", "TEXT")?;
    add_column(
        conn,
        "market_holidays",
        "holiday_type",
        "TEXT NOT NULL DEFAULT 'TRADING_HOLIDAY'",
    )?;
    add_column(conn, "market_holiday_exchanges", "exchange_code", "TEXT")?;
    add_column(
        conn,
        "market_holiday_exchanges",
        "is_open",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(conn, "market_holiday_exchanges", "start_time", "INTEGER")?;
    add_column(conn, "market_holiday_exchanges", "end_time", "INTEGER")?;
    add_column(conn, "market_timings", "exchange_code", "TEXT")?;
    add_column(conn, "market_timings", "start_time", "TEXT")?;
    add_column(conn, "market_timings", "end_time", "TEXT")?;
    add_column(conn, "market_timings", "start_offset", "INTEGER")?;
    add_column(conn, "market_timings", "end_offset", "INTEGER")?;
    conn.execute_batch(
        "UPDATE market_holidays SET holiday_date = COALESCE(holiday_date, date);
         UPDATE market_holidays SET description = '' WHERE description IS NULL;
         UPDATE market_holiday_exchanges SET exchange_code = COALESCE(exchange_code, exchange);
         UPDATE market_timings SET exchange_code = COALESCE(exchange_code, exchange),
             start_time = COALESCE(start_time, market_open),
             end_time = COALESCE(end_time, market_close);
         CREATE INDEX IF NOT EXISTS idx_holiday_date_year ON market_holidays(holiday_date, year);
         CREATE INDEX IF NOT EXISTS idx_holiday_exchange ON market_holiday_exchanges(holiday_id, exchange_code);",
    )?;
    for (ex, open, close) in LEGACY_DESKTOP_DEFAULTS {
        if let Some((_, _, end)) = DEFAULT_TIMINGS.iter().find(|(e, _, _)| e == ex) {
            conn.execute(
                "UPDATE market_timings SET end_time = ?1, market_close = ?1
                 WHERE exchange_code = ?2 AND start_time = ?3 AND end_time = ?4",
                params![hhmm(*end), ex, open, close],
            )?;
        }
    }
    // Offsets from the (possibly backfilled) HH:MM text.
    let rows: Vec<(i64, String, String)> = {
        let mut st = conn.prepare("SELECT id, start_time, end_time FROM market_timings")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        v
    };
    for (id, s, e) in rows {
        if let (Some(so), Some(eo)) = (parse_hhmm(&s), parse_hhmm(&e)) {
            conn.execute(
                "UPDATE market_timings SET start_offset = COALESCE(start_offset, ?1),
                    end_offset = COALESCE(end_offset, ?2) WHERE id = ?3",
                params![so, eo, id],
            )?;
        }
    }
    for (ex, so, eo) in DEFAULT_TIMINGS {
        conn.execute(
            "INSERT INTO market_timings (exchange, market_open, market_close, exchange_code,
                start_time, end_time, start_offset, end_offset)
             SELECT ?1, ?2, ?3, ?1, ?2, ?3, ?4, ?5
             WHERE NOT EXISTS (SELECT 1 FROM market_timings WHERE exchange_code = ?1 OR exchange = ?1)",
            params![ex, hhmm(*so), hhmm(*eo), so, eo],
        )?;
    }
    seed_holidays(conn)?;
    Ok(())
}

/// Insert every web holiday whose date is not in the table yet.
pub fn seed_holidays(conn: &Connection) -> Result<usize> {
    let seed: std::collections::BTreeMap<String, Vec<SeedHoliday>> =
        serde_json::from_str(SEED_JSON)?;
    let mut added = 0;
    for (year, list) in seed {
        let year: i32 = year
            .parse()
            .map_err(|_| AppError::Internal("bad holiday seed".into()))?;
        for h in list {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM market_holidays WHERE holiday_date = ?1 OR date = ?1)",
                [&h.date],
                |r| r.get(0),
            )?;
            if exists {
                continue;
            }
            let id = insert_holiday_row(conn, &h.date, &h.description, &h.holiday_type, year)?;
            for ex in &h.closed {
                insert_exchange_row(conn, id, ex, false, None, None)?;
            }
            for o in &h.open {
                insert_exchange_row(
                    conn,
                    id,
                    &o.exchange,
                    true,
                    Some(o.start_time),
                    Some(o.end_time),
                )?;
            }
            added += 1;
        }
    }
    Ok(added)
}

/// Web `MUHURAT_2026_SESSION`: Diwali Muhurat, Sunday 8 November 2026, as
/// NSE, BSE and MCX published it on 19 October (MCX circular
/// MCX/TRD/590/2026). Trading runs 14:20 to 15:30 IST and equity derivatives
/// to 15:40; NCO is NSE's commodity segment, which the NSE schedule lists.
/// Epoch milliseconds, like every window in the holiday tables.
pub const MUHURAT_2026_DATE: &str = "2026-11-08";
pub const MUHURAT_2026_SESSION: &[(&str, i64, i64)] = &[
    ("NSE", 1_794_127_800_000, 1_794_132_000_000), // 14:20 - 15:30
    ("BSE", 1_794_127_800_000, 1_794_132_000_000),
    ("NFO", 1_794_127_800_000, 1_794_132_600_000), // 14:20 - 15:40
    ("BFO", 1_794_127_800_000, 1_794_132_600_000),
    ("CDS", 1_794_127_800_000, 1_794_132_000_000),
    ("BCD", 1_794_127_800_000, 1_794_132_000_000),
    ("MCX", 1_794_127_800_000, 1_794_132_000_000),
    ("NCO", 1_794_127_800_000, 1_794_132_000_000),
];

/// Web `_MUHURAT_2026_GUESS`: what the 2026 seed carried before the
/// exchanges published the timings, 18:00 to 19:15 with MCX to 00:15 the
/// next morning and no NCO row. A row still holding it moves.
const MUHURAT_2026_GUESS: &[(&str, i64, i64)] = &[
    ("NSE", 1_794_141_000_000, 1_794_145_500_000),
    ("BSE", 1_794_141_000_000, 1_794_145_500_000),
    ("NFO", 1_794_141_000_000, 1_794_145_500_000),
    ("BFO", 1_794_141_000_000, 1_794_145_500_000),
    ("CDS", 1_794_141_000_000, 1_794_145_500_000),
    ("BCD", 1_794_141_000_000, 1_794_145_500_000),
    ("MCX", 1_794_141_000_000, 1_794_163_500_000),
];

/// What moving one exchange of 2026's Muhurat to the published session does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuhuratStep {
    /// The row holds the shipped guess.
    Update,
    /// The row was never shipped (NCO).
    Insert,
    /// Already the published session.
    Current,
    /// Set or removed by the trader; left alone.
    Custom,
}

/// The stored Muhurat holiday's id and one step per published exchange.
pub type MuhuratPlan = (i64, Vec<(&'static str, MuhuratStep)>);

/// Web `plan_muhurat_2026`: the holiday's id and one step per exchange of the
/// published session, or `None` when 8 November 2026 is not stored as a
/// special session, which is left to the trader who changed it.
pub fn plan_muhurat_2026(conn: &Connection) -> Result<Option<MuhuratPlan>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM market_holidays
             WHERE COALESCE(holiday_date, date) = ?1 AND holiday_type = 'SPECIAL_SESSION'
             ORDER BY id LIMIT 1",
            [MUHURAT_2026_DATE],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let rows: HashMap<String, Option<(i64, i64)>> = {
        let mut st = conn.prepare(
            "SELECT COALESCE(exchange_code, exchange), is_open, start_time, end_time
             FROM market_holiday_exchanges WHERE holiday_id = ?1 ORDER BY id",
        )?;
        let v = st
            .query_map([id], |r| {
                let window = match (r.get::<_, bool>(1)?, r.get(2)?, r.get(3)?) {
                    (true, Some(s), Some(e)) => Some((s, e)),
                    _ => None,
                };
                Ok((r.get::<_, String>(0)?, window))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        v
    };
    let guess = |ex: &str| {
        MUHURAT_2026_GUESS
            .iter()
            .find(|(g, _, _)| *g == ex)
            .map(|(_, s, e)| (*s, *e))
    };
    let steps = MUHURAT_2026_SESSION
        .iter()
        .map(|(ex, s, e)| {
            let step = match rows.get(*ex) {
                None if guess(ex).is_some() => MuhuratStep::Custom,
                None => MuhuratStep::Insert,
                Some(Some(w)) if *w == (*s, *e) => MuhuratStep::Current,
                Some(Some(w)) if Some(*w) == guess(ex) => MuhuratStep::Update,
                Some(_) => MuhuratStep::Custom,
            };
            (*ex, step)
        })
        .collect();
    Ok(Some((id, steps)))
}

/// Web `migrate_muhurat_2026`: move 2026's Muhurat session from the shipped
/// guess to the published one. Guarded on the guess, so a session the trader
/// set on the Holidays page stays as it is. A fresh install is seeded with
/// the published session and has nothing to move.
pub fn migrate_muhurat_2026(conn: &Connection) -> Result<()> {
    let Some((id, steps)) = plan_muhurat_2026(conn)? else {
        return Ok(());
    };
    let mut changed = 0;
    for (ex, step) in steps {
        let Some((_, start, end)) = MUHURAT_2026_SESSION.iter().find(|(e, _, _)| *e == ex) else {
            continue;
        };
        match step {
            MuhuratStep::Insert => {
                insert_exchange_row(conn, id, ex, true, Some(*start), Some(*end))?;
                changed += 1;
            }
            MuhuratStep::Update => {
                let Some((_, gs, ge)) = MUHURAT_2026_GUESS.iter().find(|(e, _, _)| *e == ex) else {
                    continue;
                };
                changed += conn.execute(
                    "UPDATE market_holiday_exchanges SET start_time = ?1, end_time = ?2
                     WHERE holiday_id = ?3 AND COALESCE(exchange_code, exchange) = ?4
                       AND start_time = ?5 AND end_time = ?6",
                    params![start, end, id, ex, gs, ge],
                )?;
            }
            MuhuratStep::Current | MuhuratStep::Custom => {}
        }
    }
    if changed > 0 {
        tracing::info!(
            "Market Calendar DB: set the 8 November 2026 Muhurat session to the published timings for {} exchange(s)",
            changed
        );
    }
    Ok(())
}

fn insert_holiday_row(
    conn: &Connection,
    date: &str,
    description: &str,
    holiday_type: &str,
    year: i32,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO market_holidays (date, holiday_date, description, holiday_type, year)
         VALUES (?1, ?1, ?2, ?3, ?4)",
        params![date, description, holiday_type, year],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_exchange_row(
    conn: &Connection,
    holiday_id: i64,
    exchange: &str,
    is_open: bool,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO market_holiday_exchanges (holiday_id, exchange, exchange_code, is_open, start_time, end_time)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
        params![holiday_id, exchange, is_open, start, end],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenWindow {
    pub exchange: String,
    pub start_time: i64,
    pub end_time: i64,
}

#[derive(Debug, Clone)]
pub struct Holiday {
    pub id: i64,
    pub date: String,
    pub description: String,
    pub holiday_type: String,
    pub year: i32,
    pub closed: Vec<String>,
    pub open: Vec<OpenWindow>,
}

fn exchanges_for(conn: &Connection, holiday_id: i64) -> Result<(Vec<String>, Vec<OpenWindow>)> {
    let mut st = conn.prepare(
        "SELECT exchange_code, is_open, start_time, end_time FROM market_holiday_exchanges
         WHERE holiday_id = ?1 ORDER BY id",
    )?;
    let rows = st
        .query_map([holiday_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, bool>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut closed = Vec::new();
    let mut open = Vec::new();
    for (ex, is_open, s, e) in rows {
        if is_open {
            open.push(OpenWindow {
                exchange: ex,
                start_time: s.unwrap_or(0),
                end_time: e.unwrap_or(0),
            });
        } else {
            closed.push(ex);
        }
    }
    Ok((closed, open))
}

fn holiday_from_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, String, String, String, i32)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        r.get(3)?,
        r.get(4)?,
    ))
}

pub fn holidays_by_year(conn: &Connection, year: i32) -> Result<Vec<Holiday>> {
    let rows = {
        let mut st = conn.prepare(
            "SELECT id, holiday_date, description, holiday_type, year FROM market_holidays
             WHERE year = ?1 ORDER BY holiday_date, id",
        )?;
        let v = st
            .query_map([year], holiday_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        v
    };
    rows.into_iter()
        .map(|(id, date, description, holiday_type, year)| {
            let (closed, open) = exchanges_for(conn, id)?;
            Ok(Holiday {
                id,
                date,
                description,
                holiday_type,
                year,
                closed,
                open,
            })
        })
        .collect()
}

/// Whether `date` is a market holiday for the market as a whole (web
/// `is_market_holiday(date)` with no exchange). A special session such as
/// Muhurat trading is not one, even at a weekend; otherwise a weekend is,
/// and so is any listed holiday, whichever exchanges it leaves open.
pub fn is_market_holiday(conn: &Connection, date: NaiveDate) -> Result<bool> {
    let holiday = holiday_on(conn, date)?;
    if holiday
        .as_ref()
        .is_some_and(|h| h.holiday_type == "SPECIAL_SESSION")
    {
        return Ok(false);
    }
    Ok(holiday.is_some() || matches!(date.weekday(), Weekday::Sat | Weekday::Sun))
}

fn holiday_on(conn: &Connection, date: NaiveDate) -> Result<Option<Holiday>> {
    let row = conn
        .query_row(
            "SELECT id, holiday_date, description, holiday_type, year FROM market_holidays
             WHERE holiday_date = ?1 ORDER BY id LIMIT 1",
            [date.format("%Y-%m-%d").to_string()],
            holiday_from_row,
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((id, date, description, holiday_type, year)) => {
            let (closed, open) = exchanges_for(conn, id)?;
            Ok(Some(Holiday {
                id,
                date,
                description,
                holiday_type,
                year,
                closed,
                open,
            }))
        }
    }
}

pub fn holiday_years(conn: &Connection) -> Result<Vec<i32>> {
    let mut st = conn.prepare("SELECT DISTINCT year FROM market_holidays ORDER BY year")?;
    let v = st
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<Vec<i32>, _>>()?;
    Ok(v)
}

pub fn holiday_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM market_holidays", [], |r| r.get(0))?)
}

pub struct NewHoliday<'a> {
    pub date: NaiveDate,
    pub description: &'a str,
    pub holiday_type: &'a str,
    pub closed: &'a [String],
    pub open: &'a [OpenWindow],
}

/// Add a holiday. `Ok(None)` when one already exists on that date.
pub fn add_holiday(conn: &Connection, h: &NewHoliday) -> Result<Option<i64>> {
    let date = h.date.format("%Y-%m-%d").to_string();
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM market_holidays WHERE holiday_date = ?1 OR date = ?1)",
        [&date],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(None);
    }
    conn.execute_batch("SAVEPOINT add_holiday")?;
    let r = (|| -> Result<i64> {
        let id = insert_holiday_row(conn, &date, h.description, h.holiday_type, h.date.year())?;
        for ex in h.closed {
            insert_exchange_row(conn, id, ex, false, None, None)?;
        }
        for o in h.open {
            insert_exchange_row(
                conn,
                id,
                &o.exchange,
                true,
                Some(o.start_time),
                Some(o.end_time),
            )?;
        }
        Ok(id)
    })();
    match r {
        Ok(id) => {
            conn.execute_batch("RELEASE add_holiday")?;
            Ok(Some(id))
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO add_holiday; RELEASE add_holiday");
            Err(e)
        }
    }
}

/// Delete a holiday; returns its description when it existed.
pub fn delete_holiday(conn: &Connection, id: i64) -> Result<Option<String>> {
    let desc: Option<Option<String>> = conn
        .query_row(
            "SELECT description FROM market_holidays WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(desc) = desc else {
        return Ok(None);
    };
    conn.execute(
        "DELETE FROM market_holiday_exchanges WHERE holiday_id = ?1",
        [id],
    )?;
    conn.execute("DELETE FROM market_holidays WHERE id = ?1", [id])?;
    Ok(Some(desc.unwrap_or_default()))
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub id: Option<i64>,
    pub exchange: String,
    pub start_time: String,
    pub end_time: String,
    pub start_offset: i64,
    pub end_offset: i64,
}

/// Every configured timing, by exchange (web `get_all_market_timings`).
pub fn all_timings(conn: &Connection) -> Result<Vec<Timing>> {
    let mut st = conn.prepare(
        "SELECT id, exchange_code, start_time, end_time, start_offset, end_offset
         FROM market_timings WHERE exchange_code IS NOT NULL ORDER BY exchange_code",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(Timing {
                id: Some(r.get(0)?),
                exchange: r.get(1)?,
                start_time: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                end_time: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                start_offset: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                end_offset: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !rows.is_empty() {
        return Ok(rows);
    }
    Ok(DEFAULT_TIMINGS
        .iter()
        .map(|(e, s, en)| Timing {
            id: None,
            exchange: e.to_string(),
            start_time: hhmm(*s),
            end_time: hhmm(*en),
            start_offset: *s,
            end_offset: *en,
        })
        .collect())
}

/// Update (or create) one exchange's hours. Times are validated by the caller.
pub fn update_timing(conn: &Connection, exchange: &str, start: &str, end: &str) -> Result<()> {
    let ex = exchange.to_ascii_uppercase();
    let (so, eo) = match (parse_hhmm(start), parse_hhmm(end)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err(AppError::Validation(
                "Invalid time format. Use HH:MM".into(),
            ))
        }
    };
    let n = conn.execute(
        "UPDATE market_timings SET start_time = ?1, end_time = ?2, start_offset = ?3, end_offset = ?4,
            market_open = ?1, market_close = ?2
         WHERE exchange_code = ?5",
        params![start, end, so, eo, ex],
    )?;
    if n == 0 {
        conn.execute(
            "INSERT INTO market_timings (exchange, market_open, market_close, exchange_code, start_time,
                end_time, start_offset, end_offset)
             VALUES (?1, ?2, ?3, ?1, ?2, ?3, ?4, ?5)",
            params![ex, start, end, so, eo],
        )?;
    }
    Ok(())
}

fn offsets(conn: &Connection) -> Result<Vec<(String, i64, i64)>> {
    let t = all_timings(conn)?;
    Ok(t.into_iter()
        .map(|t| (t.exchange, t.start_offset, t.end_offset))
        .collect())
}

fn offset_for(offs: &[(String, i64, i64)], ex: &str) -> Option<(i64, i64)> {
    offs.iter()
        .find(|(e, _, _)| e == ex)
        .map(|(_, s, e)| (*s, *e))
        .or_else(|| {
            DEFAULT_TIMINGS
                .iter()
                .find(|(e, _, _)| *e == ex)
                .map(|(_, s, e)| (*s, *e))
        })
}

/// Epoch ms of IST midnight for `date`.
pub fn ist_midnight_ms(date: NaiveDate) -> i64 {
    Kolkata
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap_or_default())
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// Trading windows for `date` (web `get_market_timings_for_date`): special
/// sessions, settlement holidays, trading holidays with open exchanges,
/// weekends (crypto only), then the configured hours.
pub fn timings_for_date(conn: &Connection, date: NaiveDate) -> Result<Vec<OpenWindow>> {
    let midnight = ist_midnight_ms(date);
    let offs = offsets(conn)?;
    let normal = |list: &[&str]| -> Vec<OpenWindow> {
        list.iter()
            .filter_map(|ex| {
                offset_for(&offs, ex).map(|(s, e)| OpenWindow {
                    exchange: ex.to_string(),
                    start_time: midnight + s,
                    end_time: midnight + e,
                })
            })
            .collect()
    };
    if let Some(h) = holiday_on(conn, date)? {
        return Ok(match h.holiday_type.as_str() {
            "SPECIAL_SESSION" => h.open,
            "SETTLEMENT_HOLIDAY" => normal(SUPPORTED_EXCHANGES),
            _ => {
                let all_closed = SUPPORTED_EXCHANGES
                    .iter()
                    .all(|e| h.closed.iter().any(|c| c == e));
                if all_closed && h.open.is_empty() {
                    Vec::new()
                } else {
                    h.open
                }
            }
        });
    }
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return Ok(normal(CRYPTO_EXCHANGES));
    }
    Ok(normal(SUPPORTED_EXCHANGES))
}

/// One exchange's trading window on a date, epoch ms (web
/// `get_effective_session_window`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionWindow {
    pub start_ms: i64,
    pub end_ms: i64,
    /// A special session or a holiday's own window, not the usual hours.
    pub is_special: bool,
}

/// What the holiday table says about one exchange on one date.
enum CalendarDay {
    Window(SessionWindow),
    Closed,
    Usual,
}

/// Steps 2-5 of [`effective_session_window`].
fn calendar_day(conn: &Connection, date: NaiveDate, exchange: &str) -> Result<CalendarDay> {
    let day = date.format("%Y-%m-%d").to_string();
    let first_of = |kind: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row(
                "SELECT id FROM market_holidays WHERE holiday_date = ?1 AND holiday_type = ?2
                 ORDER BY id LIMIT 1",
                params![day, kind],
                |r| r.get(0),
            )
            .optional()?)
    };
    if let Some(id) = first_of("SPECIAL_SESSION")? {
        let row: Option<(Option<i64>, Option<i64>)> = conn
            .query_row(
                "SELECT start_time, end_time FROM market_holiday_exchanges
                 WHERE holiday_id = ?1 AND exchange_code = ?2 AND is_open = 1 ORDER BY id LIMIT 1",
                params![id, exchange],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((Some(start_ms), Some(end_ms))) = row {
            return Ok(CalendarDay::Window(SessionWindow {
                start_ms,
                end_ms,
                is_special: true,
            }));
        }
    }
    if let Some(id) = first_of("TRADING_HOLIDAY")? {
        let row: Option<(bool, Option<i64>, Option<i64>)> = conn
            .query_row(
                "SELECT is_open, start_time, end_time FROM market_holiday_exchanges
                 WHERE holiday_id = ?1 AND exchange_code = ?2 ORDER BY id LIMIT 1",
                params![id, exchange],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((false, _, _)) => return Ok(CalendarDay::Closed),
            Some((true, Some(start_ms), Some(end_ms))) => {
                return Ok(CalendarDay::Window(SessionWindow {
                    start_ms,
                    end_ms,
                    is_special: true,
                }))
            }
            // Open without times, or not listed on the holiday: the usual
            // hours, as the web reads it.
            _ => {}
        }
    }
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return Ok(CalendarDay::Closed);
    }
    Ok(CalendarDay::Usual)
}

/// The trading window of `exchange` on `date`, `None` when it is shut
/// (web `get_effective_session_window`, the one answer the Flow
/// market-hours gate reads): CRYPTO always open; a special session for the
/// exchange; a trading holiday that closes it, or keeps it open with its
/// own times; a weekend; else the configured hours. A trading holiday that
/// does not list the exchange leaves it open with its usual hours (unlike
/// [`timings_for_date`]); any read failure counts as shut.
pub fn effective_session_window(
    conn: &Connection,
    date: NaiveDate,
    exchange: &str,
) -> Option<SessionWindow> {
    if exchange.is_empty() {
        return None;
    }
    let ex = exchange.to_uppercase();
    let midnight = ist_midnight_ms(date);
    // The web falls back to its defaults when the timings cannot be read.
    let offs = offsets(conn).unwrap_or_default();
    let usual = || {
        offset_for(&offs, &ex).map(|(s, e)| SessionWindow {
            start_ms: midnight + s,
            end_ms: midnight + e,
            is_special: false,
        })
    };
    if CRYPTO_EXCHANGES.contains(&ex.as_str()) {
        return usual();
    }
    match calendar_day(conn, date, &ex) {
        Ok(CalendarDay::Window(w)) => Some(w),
        Ok(CalendarDay::Closed) => None,
        Ok(CalendarDay::Usual) => usual(),
        Err(e) => {
            tracing::debug!("effective session window for {} {} failed: {}", date, ex, e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&c).unwrap();
        c
    }

    #[test]
    fn seed_matches_the_web_and_is_idempotent() {
        let c = legacy_db();
        migrate(&c).unwrap();
        assert_eq!(holiday_count(&c).unwrap(), 31);
        assert_eq!(seed_holidays(&c).unwrap(), 0);
        let h = holidays_by_year(&c, 2026).unwrap();
        assert_eq!(h.len(), 17);
        assert_eq!(h[0].date, "2026-01-15");
        assert_eq!(h[0].open[0].exchange, "MCX");
    }

    #[test]
    fn a_populated_legacy_calendar_keeps_the_traders_holidays() {
        let c = legacy_db();
        c.execute(
            "INSERT INTO market_holidays (date, description, year) VALUES ('2026-01-26', 'My Republic Day', 2026)",
            [],
        )
        .unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO market_holidays (date, description, year) VALUES ('2026-12-28', NULL, 2026)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO market_holiday_exchanges (holiday_id, exchange) VALUES (?1, 'NSE')",
            [id],
        )
        .unwrap();
        migrate(&c).unwrap();
        let h = holidays_by_year(&c, 2026).unwrap();
        // 17 web holidays, one of them already present, plus the trader's own.
        assert_eq!(h.len(), 18);
        let mine = h.iter().find(|x| x.date == "2026-01-26").unwrap();
        assert_eq!(mine.description, "My Republic Day");
        assert_eq!(mine.closed, vec!["NSE".to_string()]);
        assert_eq!(mine.holiday_type, "TRADING_HOLIDAY");
        assert!(h
            .iter()
            .any(|x| x.date == "2026-12-28" && x.description.is_empty()));
        // The legacy columns still read the same rows.
        let legacy: String = c
            .query_row(
                "SELECT exchange FROM market_holiday_exchanges WHERE holiday_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy, "NSE");
    }

    #[test]
    fn untouched_legacy_timings_move_to_web_defaults_and_edits_are_kept() {
        let c = legacy_db();
        c.execute(
            "UPDATE market_timings SET market_open = '09:20' WHERE exchange = 'NSE'",
            [],
        )
        .unwrap();
        migrate(&c).unwrap();
        let t = all_timings(&c).unwrap();
        let get = |e: &str| t.iter().find(|x| x.exchange == e).unwrap().clone();
        assert_eq!(get("NSE").start_time, "09:20");
        assert_eq!(get("NSE").start_offset, 33_600_000);
        assert_eq!(get("NFO").end_time, "15:40");
        assert_eq!(get("MCX").end_time, "23:55");
        assert_eq!(get("CRYPTO").end_offset, 86_399_000);
        assert_eq!(t.len(), 9);
    }

    /// Web test/test_sandbox_pnl_snapshot_days.py (its Indian-market cases)
    /// and `is_market_holiday`'s special-session rule.
    #[test]
    fn market_holidays_as_the_web_reads_them() {
        let c = legacy_db();
        migrate(&c).unwrap();
        let day = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        // The dates reported in web issue #876: Friday trades, the weekend
        // does not.
        assert!(!is_market_holiday(&c, day(2, 6)).unwrap());
        assert!(is_market_holiday(&c, day(2, 7)).unwrap());
        assert!(is_market_holiday(&c, day(2, 8)).unwrap());
        // Weekday holidays, including one that keeps MCX's evening session.
        assert!(is_market_holiday(&c, day(1, 26)).unwrap());
        assert!(is_market_holiday(&c, day(10, 20)).unwrap());
        // Muhurat trading on a Sunday is a special session, not a holiday.
        assert!(!is_market_holiday(&c, day(11, 8)).unwrap());
    }

    #[test]
    fn weekday_weekend_and_holiday_windows() {
        let c = legacy_db();
        migrate(&c).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let w = timings_for_date(&c, d).unwrap();
        assert_eq!(w.len(), 9);
        assert_eq!(w[0].start_time, 1_790_826_300_000);
        let sat = timings_for_date(&c, NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()).unwrap();
        assert_eq!(sat.len(), 1);
        assert_eq!(sat[0].exchange, "CRYPTO");
        let republic = timings_for_date(&c, NaiveDate::from_ymd_opt(2026, 1, 26).unwrap()).unwrap();
        assert!(republic.is_empty() || republic.iter().all(|w| w.exchange != "NSE"));
    }

    /// N-marketHours-05/06: the web's `get_effective_session_window` order,
    /// including the case `timings_for_date` answers differently (a trading
    /// holiday that does not list the exchange leaves it open).
    #[test]
    fn effective_windows_follow_the_web_calendar() {
        let c = legacy_db();
        migrate(&c).unwrap();
        let day = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        let at = |m, d, hhmm: &str| ist_midnight_ms(day(m, d)) + parse_hhmm(hhmm).unwrap();
        let usual = |m, d, s: &str, e: &str| {
            Some(SessionWindow {
                start_ms: at(m, d, s),
                end_ms: at(m, d, e),
                is_special: false,
            })
        };
        assert_eq!(
            effective_session_window(&c, day(10, 1), "nse"),
            usual(10, 1, "09:15", "15:30")
        );
        assert_eq!(
            effective_session_window(&c, day(10, 1), "NFO"),
            usual(10, 1, "09:15", "15:40")
        );
        assert_eq!(
            effective_session_window(&c, day(10, 1), "MCX"),
            usual(10, 1, "09:00", "23:55")
        );
        // Weekend: shut, CRYPTO aside.
        assert_eq!(effective_session_window(&c, day(10, 3), "NSE"), None);
        assert!(effective_session_window(&c, day(10, 3), "CRYPTO").is_some());
        // A holiday closing the exchange; one keeping MCX open with its times.
        assert_eq!(effective_session_window(&c, day(1, 26), "NSE"), None);
        assert_eq!(
            effective_session_window(&c, day(1, 15), "MCX"),
            Some(SessionWindow {
                start_ms: 1_768_476_600_000,
                end_ms: 1_768_501_500_000,
                is_special: true,
            })
        );
        // Not listed on the holiday: open with the usual hours (the web's
        // gate), where timings_for_date leaves it out.
        assert_eq!(
            effective_session_window(&c, day(1, 26), "NCO"),
            usual(1, 26, "09:00", "23:55")
        );
        assert!(timings_for_date(&c, day(1, 26))
            .unwrap()
            .iter()
            .all(|w| w.exchange != "NCO"));
        // A Sunday special session opens the listed exchanges, NCO among
        // them since the published Muhurat session.
        let muhurat = effective_session_window(&c, day(11, 8), "NSE").unwrap();
        assert!(muhurat.is_special);
        assert_eq!(muhurat.start_ms, 1_794_127_800_000);
        assert_eq!(
            effective_session_window(&c, day(11, 8), "NCO").map(|w| (w.start_ms, w.end_ms)),
            Some((1_794_127_800_000, 1_794_132_000_000))
        );
        // The trader's own hours, and an exchange with none.
        update_timing(&c, "NSE", "09:20", "15:25").unwrap();
        assert_eq!(
            effective_session_window(&c, day(10, 1), "NSE"),
            usual(10, 1, "09:20", "15:25")
        );
        assert_eq!(effective_session_window(&c, day(10, 1), "XYZ"), None);
        assert_eq!(effective_session_window(&c, day(10, 1), ""), None);
    }

    /// The 8 November 2026 rows as (exchange, window), `None` for a closed row.
    fn muhurat_rows(c: &Connection) -> HashMap<String, Option<(i64, i64)>> {
        let mut st = c
            .prepare(
                "SELECT e.exchange_code, e.is_open, e.start_time, e.end_time
                 FROM market_holiday_exchanges e JOIN market_holidays h ON h.id = e.holiday_id
                 WHERE h.holiday_date = '2026-11-08'",
            )
            .unwrap();
        let v = st
            .query_map([], |r| {
                let open: bool = r.get(1)?;
                let window = if open {
                    Some((r.get(2)?, r.get(3)?))
                } else {
                    None
                };
                Ok((r.get::<_, String>(0)?, window))
            })
            .unwrap()
            .collect::<std::result::Result<HashMap<_, _>, _>>()
            .unwrap();
        v
    }

    fn published() -> HashMap<String, Option<(i64, i64)>> {
        MUHURAT_2026_SESSION
            .iter()
            .map(|(ex, s, e)| (ex.to_string(), Some((*s, *e))))
            .collect()
    }

    /// An install seeded before the timings were published: the guess, and
    /// no NCO row.
    fn as_shipped(c: &Connection) {
        for (ex, s, e) in MUHURAT_2026_GUESS {
            c.execute(
                "UPDATE market_holiday_exchanges SET start_time = ?1, end_time = ?2
                 WHERE exchange_code = ?3 AND holiday_id =
                    (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08')",
                params![s, e, ex],
            )
            .unwrap();
        }
        c.execute(
            "DELETE FROM market_holiday_exchanges WHERE exchange_code = 'NCO' AND holiday_id =
                (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08')",
            [],
        )
        .unwrap();
    }

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn steps(c: &Connection) -> HashMap<&'static str, MuhuratStep> {
        plan_muhurat_2026(c)
            .unwrap()
            .unwrap()
            .1
            .into_iter()
            .collect()
    }

    /// Web test/test_migrate_muhurat_2026.py::test_the_published_session_is_what_the_module_carries:
    /// 14:20 to 15:30 IST, NFO and BFO to 15:40.
    #[test]
    fn the_published_muhurat_session_is_what_the_module_carries() {
        let at = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .unwrap()
                .with_timezone(&Kolkata)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        };
        for (ex, s, e) in MUHURAT_2026_SESSION {
            assert_eq!(at(*s), "2026-11-08 14:20", "{ex}");
            let close = if matches!(*ex, "NFO" | "BFO") {
                "15:40"
            } else {
                "15:30"
            };
            assert_eq!(at(*e), format!("2026-11-08 {close}"), "{ex}");
        }
        assert_eq!(MUHURAT_2026_SESSION.len(), 8);
        assert_eq!(MUHURAT_2026_DATE, "2026-11-08");
    }

    /// Web test/test_migrate_muhurat_2026.py::test_a_new_install_is_seeded_with_the_published_session.
    #[test]
    fn a_new_install_is_seeded_with_the_published_muhurat_session() {
        let c = legacy_db();
        migrate(&c).unwrap();
        assert_eq!(muhurat_rows(&c), published());
        assert!(steps(&c).values().all(|s| *s == MuhuratStep::Current));
        let w = effective_session_window(&c, day(11, 8), "NSE").unwrap();
        assert_eq!(
            (w.start_ms, w.end_ms),
            (1_794_127_800_000, 1_794_132_000_000)
        );
    }

    /// Web test/test_migrate_muhurat_2026.py::test_an_existing_install_moves_from_the_guess
    /// and test_rerunning_changes_nothing.
    #[test]
    fn an_existing_install_moves_from_the_muhurat_guess_once() {
        let c = legacy_db();
        migrate(&c).unwrap();
        as_shipped(&c);
        let mut plan = steps(&c);
        assert_eq!(plan.remove("NCO"), Some(MuhuratStep::Insert));
        assert!(plan.values().all(|s| *s == MuhuratStep::Update));
        let before = effective_session_window(&c, day(11, 8), "MCX").unwrap();
        assert_eq!(before.end_ms, 1_794_163_500_000, "the guessed 00:15 close");

        migrate_muhurat_2026(&c).unwrap();
        assert_eq!(muhurat_rows(&c), published());
        let after = effective_session_window(&c, day(11, 8), "MCX").unwrap();
        assert_eq!(
            (after.start_ms, after.end_ms),
            (1_794_127_800_000, 1_794_132_000_000)
        );

        migrate_muhurat_2026(&c).unwrap();
        assert_eq!(muhurat_rows(&c), published());
        assert!(steps(&c).values().all(|s| *s == MuhuratStep::Current));
    }

    /// Web test/test_migrate_muhurat_2026.py::test_what_an_admin_set_is_left_alone.
    #[test]
    fn a_muhurat_session_the_trader_set_is_left_alone() {
        let c = legacy_db();
        migrate(&c).unwrap();
        as_shipped(&c);
        let own = (1_794_142_800_000_i64, 1_794_146_400_000_i64); // 18:30 - 19:30 IST
        c.execute(
            "UPDATE market_holiday_exchanges SET start_time = ?1, end_time = ?2 WHERE exchange_code = 'NSE'
                AND holiday_id = (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08')",
            params![own.0, own.1],
        )
        .unwrap();
        c.execute(
            "UPDATE market_holiday_exchanges SET is_open = 0, start_time = NULL, end_time = NULL
             WHERE exchange_code = 'CDS'
                AND holiday_id = (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08')",
            [],
        )
        .unwrap();
        c.execute(
            "DELETE FROM market_holiday_exchanges WHERE exchange_code = 'MCX'
                AND holiday_id = (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08')",
            [],
        )
        .unwrap();
        let plan = steps(&c);
        for ex in ["NSE", "CDS", "MCX"] {
            assert_eq!(plan[ex], MuhuratStep::Custom, "{ex}");
        }

        migrate_muhurat_2026(&c).unwrap();
        let rows = muhurat_rows(&c);
        assert_eq!(rows["NSE"], Some(own));
        assert_eq!(rows["CDS"], None);
        assert!(!rows.contains_key("MCX"));
        assert_eq!(rows["NFO"], Some((1_794_127_800_000, 1_794_132_600_000)));
        assert_eq!(rows["NCO"], Some((1_794_127_800_000, 1_794_132_000_000)));
        // A special session opens only the exchanges it lists.
        assert_eq!(effective_session_window(&c, day(11, 8), "MCX"), None);
    }

    /// Web test/test_migrate_muhurat_2026.py::test_a_removed_special_session_is_not_recreated.
    #[test]
    fn a_removed_muhurat_session_is_not_recreated() {
        let c = legacy_db();
        migrate(&c).unwrap();
        c.execute_batch(
            "DELETE FROM market_holiday_exchanges WHERE holiday_id =
                (SELECT id FROM market_holidays WHERE holiday_date = '2026-11-08');
             DELETE FROM market_holidays WHERE holiday_date = '2026-11-08';",
        )
        .unwrap();
        assert!(plan_muhurat_2026(&c).unwrap().is_none());
        migrate_muhurat_2026(&c).unwrap();
        assert!(muhurat_rows(&c).is_empty());
    }

    /// Web test/test_migrate_muhurat_2026.py::test_the_app_applies_it_at_startup and
    /// test_the_migration_is_registered: the app's own migrations move an install
    /// on the guess when it next starts.
    #[test]
    fn the_app_moves_the_muhurat_guess_when_it_starts() {
        let c = legacy_db();
        migrate(&c).unwrap();
        as_shipped(&c);
        crate::db::sqlite::migrations::run_migrations(&c).unwrap();
        assert_eq!(muhurat_rows(&c), published());
    }
}
