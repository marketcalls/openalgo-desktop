//! Exports in the web's formats (`historify_db.export_*`): one CSV or
//! tab-separated TXT of stored candles, a ZIP of one CSV per symbol and
//! interval (computed intervals aggregated on the fly), or a Parquet file
//! written by DuckDB's own `COPY ... TO` (zstd by default).
//!
//! Dates and times in exported text are IST. Files are written under the
//! store's work directory and handed to the download route once; a file is
//! removed after it is sent, when it is replaced, and on startup.

use super::db::{self, Bar};
use super::interval;
use super::time::ist_date_time;
use crate::error::Result;
use duckdb::types::Value as Dv;
use duckdb::{params, params_from_iter, Connection};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Exports kept waiting for their download, across sessions.
const MAX_PENDING: usize = 8;

/// Python `repr(float)` for CSV cells.
pub fn py_float(x: f64) -> String {
    if !x.is_finite() {
        return String::new();
    }
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}

/// Web `_sanitize_filename`.
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' => '_',
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') => c,
            _ => '_',
        })
        .filter(|c| *c != '\0')
        .collect()
}

/// SQL string literal.
pub fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ------------------------------------------------------------------ zip

/// A minimal ZIP writer (deflate, no ZIP64): what Python's `zipfile`
/// produces for a handful of CSVs.
pub struct ZipWriter<W: Write> {
    w: W,
    offset: u64,
    central: Vec<u8>,
    count: u16,
}

impl<W: Write> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        Self {
            w,
            offset: 0,
            central: Vec::new(),
            count: 0,
        }
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let crc = crc.sum();
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        enc.write_all(data)?;
        let packed = enc.finish()?;
        let too_big = |n: usize| {
            u32::try_from(n)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file too large"))
        };
        let (csize, usize_) = (too_big(packed.len())?, too_big(data.len())?);
        let name_b = name.as_bytes();
        let name_len = u16::try_from(name_b.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name too long"))?;
        let local_offset = u32::try_from(self.offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "archive too large"))?;
        // DOS date 1980-01-01, time 00:00.
        let (time, date) = (0u16, 33u16);
        let mut h = Vec::with_capacity(30 + name_b.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&8u16.to_le_bytes());
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&csize.to_le_bytes());
        h.extend_from_slice(&usize_.to_le_bytes());
        h.extend_from_slice(&name_len.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name_b);
        self.w.write_all(&h)?;
        self.w.write_all(&packed)?;
        self.offset += (h.len() + packed.len()) as u64;

        let c = &mut self.central;
        c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&8u16.to_le_bytes());
        c.extend_from_slice(&time.to_le_bytes());
        c.extend_from_slice(&date.to_le_bytes());
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&csize.to_le_bytes());
        c.extend_from_slice(&usize_.to_le_bytes());
        c.extend_from_slice(&name_len.to_le_bytes());
        c.extend_from_slice(&[0u8; 12]);
        c.extend_from_slice(&local_offset.to_le_bytes());
        c.extend_from_slice(name_b);
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "too many files"))?;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let cd_offset = u32::try_from(self.offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "archive too large"))?;
        self.w.write_all(&self.central)?;
        let mut e = Vec::with_capacity(22);
        e.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&self.count.to_le_bytes());
        e.extend_from_slice(&self.count.to_le_bytes());
        e.extend_from_slice(&(self.central.len() as u32).to_le_bytes());
        e.extend_from_slice(&cd_offset.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.w.write_all(&e)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// Read back the entries of an archive written by [`ZipWriter`] (or any
/// deflate/stored ZIP without ZIP64). Used by tests and import checks.
pub fn read_zip(bytes: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "not a zip archive");
    let u16_at = |i: usize| -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            bytes
                .get(i..i + 2)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        ))
    };
    let u32_at = |i: usize| -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            bytes
                .get(i..i + 4)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        ))
    };
    let eocd = (0..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(i).ok() == Some(0x0605_4b50))
        .ok_or_else(bad)?;
    let n = u16_at(eocd + 10)? as usize;
    let mut p = u32_at(eocd + 16)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if u32_at(p)? != 0x0201_4b50 {
            return Err(bad());
        }
        let method = u16_at(p + 10)?;
        let csize = u32_at(p + 20)? as usize;
        let name_len = u16_at(p + 28)? as usize;
        let extra = u16_at(p + 30)? as usize;
        let comment = u16_at(p + 32)? as usize;
        let local = u32_at(p + 42)? as usize;
        let name = String::from_utf8_lossy(bytes.get(p + 46..p + 46 + name_len).ok_or_else(bad)?)
            .to_string();
        p += 46 + name_len + extra + comment;
        let lname = u16_at(local + 26)? as usize;
        let lextra = u16_at(local + 28)? as usize;
        let start = local + 30 + lname + lextra;
        let data = bytes.get(start..start + csize).ok_or_else(bad)?;
        let content = match method {
            0 => data.to_vec(),
            8 => {
                let mut v = Vec::new();
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut v)?;
                v
            }
            _ => return Err(bad()),
        };
        out.push((name, content));
    }
    Ok(out)
}

// --------------------------------------------------------------- export

/// An export request after validation.
#[derive(Debug, Clone, Default)]
pub struct ExportSpec {
    /// Empty means every symbol (CSV/TXT: no filter; ZIP/Parquet: catalog).
    pub symbols: Vec<(String, String)>,
    pub intervals: Vec<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// Parquet codec: zstd, snappy, gzip or none.
    pub compression: String,
}

/// `Ok((message, records))` or `Err(trader message)`.
pub type ExportResult = std::result::Result<(String, i64), String>;

fn mb(path: &Path) -> f64 {
    std::fs::metadata(path)
        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

fn symbol_filter(symbols: &[(String, String)], params: &mut Vec<Dv>) -> Option<String> {
    if symbols.is_empty() {
        return None;
    }
    let parts: Vec<&str> = symbols
        .iter()
        .map(|(s, e)| {
            params.push(Dv::Text(s.to_uppercase()));
            params.push(Dv::Text(e.to_uppercase()));
            "(symbol = ? AND exchange = ?)"
        })
        .collect();
    Some(format!("({})", parts.join(" OR ")))
}

/// Web `export_bulk_csv` (`delim = ','`) and `export_to_txt` (`'\t'`):
/// stored candles with the interval filtered directly.
pub fn export_flat(
    c: &Connection,
    path: &Path,
    spec: &ExportSpec,
    delim: char,
) -> Result<ExportResult> {
    let mut params = Vec::new();
    let mut conds = Vec::new();
    if let Some(f) = symbol_filter(&spec.symbols, &mut params) {
        conds.push(f);
    }
    if let Some(i) = spec.intervals.first() {
        conds.push("interval = ?".into());
        params.push(Dv::Text(i.clone()));
    }
    if let Some(s) = spec.start.filter(|v| *v != 0) {
        conds.push("timestamp >= ?".into());
        params.push(Dv::BigInt(s));
    }
    if let Some(e) = spec.end.filter(|v| *v != 0) {
        conds.push("timestamp <= ?".into());
        params.push(Dv::BigInt(e));
    }
    let where_ = if conds.is_empty() {
        "1=1".to_string()
    } else {
        conds.join(" AND ")
    };
    let sql = format!(
        "SELECT symbol, exchange, interval, timestamp, open, high, low, close, volume, oi \
         FROM market_data WHERE {} ORDER BY symbol, exchange, interval, timestamp",
        where_
    );
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query(params_from_iter(params))?;
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    let d = delim.to_string();
    writeln!(
        out,
        "{}",
        [
            "symbol", "exchange", "interval", "date", "time", "open", "high", "low", "close",
            "volume", "oi"
        ]
        .join(&d)
    )?;
    let mut n = 0i64;
    while let Some(r) = rows.next()? {
        let ts: i64 = r.get(3)?;
        let (date, time) = ist_date_time(ts);
        let sym: String = r.get(0)?;
        let exch: String = r.get(1)?;
        let iv: String = r.get(2)?;
        writeln!(
            out,
            "{}",
            [
                sym,
                exch,
                iv,
                date,
                time,
                py_float(r.get(4)?),
                py_float(r.get(5)?),
                py_float(r.get(6)?),
                py_float(r.get(7)?),
                r.get::<_, i64>(8)?.to_string(),
                r.get::<_, Option<i64>>(9)?.unwrap_or(0).to_string(),
            ]
            .join(&d)
        )?;
        n += 1;
    }
    out.flush()?;
    drop(out);
    if n == 0 {
        let _ = std::fs::remove_file(path);
        return Ok(Err("No data matching the criteria".into()));
    }
    Ok(Ok((format!("Exported {} records", n), n)))
}

/// Candles for one symbol and interval the way the ZIP and Parquet
/// exports read them; `None` when the source interval has nothing stored.
fn export_bars(
    c: &Connection,
    sym: &str,
    exch: &str,
    iv: &str,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<Option<Vec<Bar>>> {
    if interval::is_daily_aggregated(iv) {
        if db::count_source(c, sym, exch, "D", start, end)? == 0 {
            tracing::warn!("No D data for {}:{}, skipping {}", sym, exch, iv);
            return Ok(None);
        }
        return Ok(Some(db::aggregated_daily(c, sym, exch, iv, start, end)?));
    }
    if interval::is_intraday_computed(iv) {
        if db::count_source(c, sym, exch, "1m", start, end)? == 0 {
            tracing::warn!("No 1m data for {}:{}, skipping {}", sym, exch, iv);
            return Ok(None);
        }
        let Some(p) = interval::parse(iv) else {
            return Ok(None);
        };
        return Ok(Some(db::aggregated_intraday(
            c, sym, exch, p.minutes, start, end, true,
        )?));
    }
    Ok(Some(db::stored(c, sym, exch, iv, start, end)?))
}

fn export_symbols(c: &Connection, spec: &ExportSpec) -> Result<Vec<(String, String)>> {
    if spec.symbols.is_empty() {
        db::catalog_symbols(c)
    } else {
        Ok(spec
            .symbols
            .iter()
            .map(|(s, e)| (s.to_uppercase(), e.to_uppercase()))
            .collect())
    }
}

/// Web `export_to_zip`: `SYMBOL_EXCHANGE_INTERVAL.csv` per pair.
pub fn export_zip(c: &Connection, path: &Path, spec: &ExportSpec) -> Result<ExportResult> {
    let symbols = export_symbols(c, spec)?;
    if symbols.is_empty() {
        return Ok(Err("No symbols found to export".into()));
    }
    let intervals = if spec.intervals.is_empty() {
        vec!["D".to_string()]
    } else {
        spec.intervals.clone()
    };
    let mut zip = ZipWriter::new(BufWriter::new(std::fs::File::create(path)?));
    let (mut total, mut skipped) = (0i64, 0usize);
    let written = (|| -> Result<()> {
        for (sym, exch) in &symbols {
            for iv in &intervals {
                let Some(bars) = export_bars(c, sym, exch, iv, spec.start, spec.end)? else {
                    skipped += 1;
                    continue;
                };
                if bars.is_empty() {
                    continue;
                }
                let mut csv = String::from("date,time,open,high,low,close,volume,oi\n");
                for b in &bars {
                    let (d, t) = ist_date_time(b.timestamp);
                    csv.push_str(&format!(
                        "{},{},{},{},{},{},{},{}\n",
                        d,
                        t,
                        py_float(b.open),
                        py_float(b.high),
                        py_float(b.low),
                        py_float(b.close),
                        b.volume,
                        b.oi
                    ));
                }
                let name = format!(
                    "{}_{}_{}.csv",
                    sanitize_filename(sym),
                    sanitize_filename(exch),
                    sanitize_filename(iv)
                );
                zip.add(&name, csv.as_bytes())?;
                total += bars.len() as i64;
            }
        }
        Ok(())
    })();
    let finished = zip.finish();
    if let Err(e) = written {
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    drop(finished?);
    if total == 0 {
        let _ = std::fs::remove_file(path);
        return Ok(Err(if skipped > 0 {
            format!(
                "No data exported. Missing 1m data for computed intervals: {} symbol(s)",
                skipped
            )
        } else {
            "No data matching the criteria".into()
        }));
    }
    let mut msg = format!("Exported {} records ({:.2} MB)", total, mb(path));
    if skipped > 0 {
        msg.push_str(&format!(
            ". Note: {} computed interval(s) skipped due to missing 1m data.",
            skipped
        ));
    }
    Ok(Ok((msg, total)))
}

/// Web `export_to_parquet`: every row in one file, aggregated like the ZIP,
/// with columns symbol, exchange, interval, timestamp, open, high, low,
/// close, volume, oi, datetime. Written with DuckDB `COPY ... TO`.
pub fn export_parquet(c: &Connection, path: &Path, spec: &ExportSpec) -> Result<ExportResult> {
    let symbols = export_symbols(c, spec)?;
    if symbols.is_empty() {
        return Ok(Err("No symbols found to export".into()));
    }
    let iv = spec
        .intervals
        .first()
        .cloned()
        .unwrap_or_else(|| "D".to_string());
    c.execute_batch(
        "CREATE OR REPLACE TEMP TABLE hfy_export (symbol VARCHAR, exchange VARCHAR, \
         interval VARCHAR, timestamp BIGINT, open DOUBLE, high DOUBLE, low DOUBLE, \
         close DOUBLE, volume BIGINT, oi BIGINT)",
    )?;
    let r = (|| -> Result<ExportResult> {
        let (mut total, mut skipped) = (0i64, 0usize);
        for (sym, exch) in &symbols {
            let Some(bars) = export_bars(c, sym, exch, &iv, spec.start, spec.end)? else {
                skipped += 1;
                continue;
            };
            let mut app = c.appender_to_catalog_and_db("hfy_export", "temp", "main")?;
            for b in &bars {
                app.append_row(params![
                    sym,
                    exch,
                    iv,
                    b.timestamp,
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume,
                    b.oi
                ])?;
            }
            app.flush()?;
            total += bars.len() as i64;
        }
        if total == 0 {
            return Ok(Err(if skipped > 0 {
                format!(
                    "No data exported. Missing source data for computed interval: {} symbol(s)",
                    skipped
                )
            } else {
                "No data matching the criteria".into()
            }));
        }
        let codec = match spec.compression.as_str() {
            "snappy" => "snappy",
            "gzip" => "gzip",
            "none" => "uncompressed",
            _ => "zstd",
        };
        c.execute_batch(&format!(
            "COPY (SELECT symbol, exchange, interval, timestamp, open, high, low, close, volume, \
             oi, make_timestamp(timestamp * 1000000) AS datetime FROM temp.main.hfy_export \
             ORDER BY symbol, exchange, interval, timestamp) TO {} (FORMAT PARQUET, COMPRESSION {})",
            sql_str(&path.to_string_lossy()),
            sql_str(codec)
        ))?;
        let mut msg = format!("Exported {} records ({:.2} MB)", total, mb(path));
        if skipped > 0 {
            msg.push_str(&format!(
                ". Note: {} symbol(s) skipped due to missing source data.",
                skipped
            ));
        }
        Ok(Ok((msg, total)))
    })();
    let _ = c.execute_batch("DROP TABLE IF EXISTS temp.main.hfy_export");
    match r {
        Ok(Ok(v)) => Ok(Ok(v)),
        other => {
            let _ = std::fs::remove_file(path);
            other
        }
    }
}

/// Web `get_export_preview`.
pub fn preview(c: &Connection, spec: &ExportSpec) -> Result<serde_json::Value> {
    let mut params = Vec::new();
    let mut conds = Vec::new();
    if let Some(f) = symbol_filter(&spec.symbols, &mut params) {
        conds.push(f);
    }
    if let Some(i) = spec.intervals.first() {
        conds.push("interval = ?".into());
        params.push(Dv::Text(i.clone()));
    }
    if let Some(s) = spec.start.filter(|v| *v != 0) {
        conds.push("timestamp >= ?".into());
        params.push(Dv::BigInt(s));
    }
    if let Some(e) = spec.end.filter(|v| *v != 0) {
        conds.push("timestamp <= ?".into());
        params.push(Dv::BigInt(e));
    }
    let where_ = if conds.is_empty() {
        "1=1".to_string()
    } else {
        conds.join(" AND ")
    };
    let (n, syms, exchs, ivs, first, last): (i64, i64, i64, i64, Option<i64>, Option<i64>) = c
        .query_row(
            &format!(
                "SELECT COUNT(*), COUNT(DISTINCT symbol), COUNT(DISTINCT exchange), \
                 COUNT(DISTINCT interval), MIN(timestamp), MAX(timestamp) FROM market_data WHERE {}",
                where_
            ),
            params_from_iter(params),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    if n == 0 {
        return Ok(serde_json::json!({
            "total_records": 0, "symbol_count": 0, "exchange_count": 0, "interval_count": 0,
            "first_date": null, "last_date": null,
            "estimated_size_csv_mb": 0, "estimated_size_parquet_mb": 0,
        }));
    }
    let date = |t: Option<i64>| t.filter(|v| *v != 0).map(super::time::ist_date);
    Ok(serde_json::json!({
        "total_records": n,
        "symbol_count": syms,
        "exchange_count": exchs,
        "interval_count": ivs,
        "first_date": date(first),
        "last_date": date(last),
        "estimated_size_csv_mb": round2(n as f64 * 100.0 / (1024.0 * 1024.0)),
        "estimated_size_parquet_mb": round2(n as f64 * 20.0 / (1024.0 * 1024.0)),
    }))
}

// -------------------------------------------------------------- samples

/// The import sample rows (web `download_sample`).
const SAMPLE: [(&str, f64, f64, f64, f64, i64); 5] = [
    ("2024-01-01", 100.0, 103.0, 99.5, 102.5, 10000),
    ("2024-01-02", 102.5, 104.0, 101.0, 101.0, 12000),
    ("2024-01-03", 101.0, 103.5, 100.5, 103.0, 11000),
    ("2024-01-04", 103.0, 105.0, 102.5, 104.5, 15000),
    ("2024-01-05", 104.5, 106.0, 103.5, 105.5, 13000),
];

pub fn sample_csv() -> String {
    let mut s = String::from("date,time,open,high,low,close,volume,oi\n");
    for (d, o, h, l, c, v) in SAMPLE {
        s.push_str(&format!(
            "{},09:15:00,{},{},{},{},{},0\n",
            d,
            py_float(o),
            py_float(h),
            py_float(l),
            py_float(c),
            v
        ));
    }
    s
}

/// The same rows as a zstd Parquet file, written by an in-memory DuckDB.
pub fn sample_parquet(work_dir: &Path) -> Result<Vec<u8>> {
    std::fs::create_dir_all(work_dir)?;
    let path = work_dir.join(format!("sample_{}.parquet", uuid::Uuid::new_v4().simple()));
    let rows: Vec<String> = SAMPLE
        .iter()
        .map(|(d, o, h, l, c, v)| {
            format!(
                "('{}', '09:15:00', {}::DOUBLE, {}::DOUBLE, {}::DOUBLE, {}::DOUBLE, {}::BIGINT, 0::BIGINT)",
                d, o, h, l, c, v
            )
        })
        .collect();
    let r = (|| -> Result<Vec<u8>> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES {}) t(date, time, open, high, low, close, volume, oi)) \
             TO {} (FORMAT PARQUET, COMPRESSION 'zstd')",
            rows.join(", "),
            sql_str(&path.to_string_lossy())
        ))?;
        drop(conn);
        Ok(std::fs::read(&path)?)
    })();
    let _ = std::fs::remove_file(&path);
    r
}

// ------------------------------------------------------- pending files

/// A finished export waiting for its download.
#[derive(Debug, Clone)]
pub struct PendingExport {
    pub path: PathBuf,
    pub mime: &'static str,
    pub filename: String,
}

/// One pending export per browser session, at most [`MAX_PENDING`].
#[derive(Default)]
pub struct ExportSlots {
    slots: Mutex<VecDeque<(String, PendingExport)>>,
}

impl ExportSlots {
    pub fn put(&self, session: &str, e: PendingExport) {
        let mut evicted = Vec::new();
        {
            let mut s = self.slots.lock();
            if let Some(i) = s.iter().position(|(k, _)| k == session) {
                if let Some((_, old)) = s.remove(i) {
                    evicted.push(old);
                }
            }
            s.push_back((session.to_string(), e));
            while s.len() > MAX_PENDING {
                if let Some((_, old)) = s.pop_front() {
                    evicted.push(old);
                }
            }
        }
        for old in evicted {
            let _ = std::fs::remove_file(&old.path);
        }
    }

    pub fn take(&self, session: &str) -> Option<PendingExport> {
        let mut s = self.slots.lock();
        let i = s.iter().position(|(k, _)| k == session)?;
        s.remove(i).map(|(_, e)| e)
    }

    pub fn len(&self) -> usize {
        self.slots.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove every pending file.
    pub fn clear(&self) {
        let all: Vec<_> = self.slots.lock().drain(..).collect();
        for (_, e) in all {
            let _ = std::fs::remove_file(&e.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_round_trip() {
        let mut z = ZipWriter::new(Vec::new());
        z.add("A_NSE_D.csv", b"date,time\n2024-01-01,00:00:00\n")
            .unwrap();
        z.add("B_NSE_5m.csv", &vec![b'x'; 10_000]).unwrap();
        let bytes = z.finish().unwrap();
        let files = read_zip(&bytes).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "A_NSE_D.csv");
        assert_eq!(files[0].1, b"date,time\n2024-01-01,00:00:00\n");
        assert_eq!(files[1].1.len(), 10_000);
    }

    #[test]
    fn python_formatting() {
        assert_eq!(py_float(100.0), "100.0");
        assert_eq!(py_float(102.5), "102.5");
        assert_eq!(py_float(0.05), "0.05");
        assert_eq!(sanitize_filename("M&M/NSE"), "M_M_NSE");
        assert!(sample_csv().starts_with("date,time,open,high,low,close,volume,oi\n2024-01-01,09:15:00,100.0,103.0,99.5,102.5,10000,0\n"));
    }
}
