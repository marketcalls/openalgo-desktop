//! The Latency page's figures for one group of endpoints (web
//! `latency_db.get_latency_stats`, `_Totals`, `_group_condition`,
//! `_totals_by_tag`): sums rather than averages are read from SQL, so the
//! totals of several endpoints add up (an average of averages is wrong
//! whenever the endpoints were not called equally often, which they never
//! are). Which tag is in which group is [`super::latency_groups`].

use super::latency_groups::{self as groups, ALL, OTHER};
use crate::error::Result;
use rusqlite::{params, Connection};

/// Which records a read covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatencyFilter {
    /// Every record.
    All,
    /// Records stored under one of these tags.
    Tags(Vec<&'static str>),
    /// Records under none of these tags, or under no tag.
    NotTags(Vec<&'static str>),
}

impl LatencyFilter {
    /// A group's records (web `_group_condition`): every record for
    /// [`ALL`] (and a key that is no group), the records in no group for
    /// [`OTHER`], else the group's tags with their legacy spellings.
    pub fn for_group(group_key: &str) -> Self {
        match group_key {
            OTHER => Self::NotTags(groups::known_tags()),
            k if k == ALL || groups::get_group(k).is_none() => Self::All,
            k => Self::Tags(groups::tags_in(k)),
        }
    }

    /// The SQL condition. The tags come from the group table, never from a
    /// request, and are checked to be plain words before they are inlined.
    pub(super) fn sql(&self) -> String {
        let list = |tags: &[&str]| {
            tags.iter()
                .filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .map(|t| format!("'{t}'"))
                .collect::<Vec<_>>()
                .join(",")
        };
        match self {
            Self::All => "1".into(),
            Self::Tags(t) if t.is_empty() => "0".into(),
            Self::Tags(t) => format!("order_type IN ({})", list(t)),
            Self::NotTags(t) => format!(
                "(order_type IS NULL OR order_type = '' OR order_type NOT IN ({}))",
                list(t)
            ),
        }
    }
}

/// Running totals for a set of records (web `_Totals`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LatencySums {
    pub count: i64,
    pub failed: i64,
    pub rtt: f64,
    pub overhead: f64,
    pub total: f64,
    pub under_100: i64,
    pub under_150: i64,
    pub under_200: i64,
    pub under_250: i64,
    pub under_400: i64,
}

impl LatencySums {
    pub fn add(&mut self, o: &Self) {
        self.count += o.count;
        self.failed += o.failed;
        self.rtt += o.rtt;
        self.overhead += o.overhead;
        self.total += o.total;
        self.under_100 += o.under_100;
        self.under_150 += o.under_150;
        self.under_200 += o.under_200;
        self.under_250 += o.under_250;
        self.under_400 += o.under_400;
    }
}

const SUMS: &str = "COUNT(*), COALESCE(SUM(status = 'FAILED'), 0),
    COALESCE(SUM(rtt_ms), 0), COALESCE(SUM(overhead_ms), 0),
    COALESCE(SUM(total_latency_ms), 0), COALESCE(SUM(total_latency_ms < 100), 0),
    COALESCE(SUM(total_latency_ms < 150), 0), COALESCE(SUM(total_latency_ms < 200), 0),
    COALESCE(SUM(total_latency_ms < 250), 0), COALESCE(SUM(total_latency_ms < 400), 0)";

fn sums_at(r: &rusqlite::Row, at: usize) -> rusqlite::Result<LatencySums> {
    Ok(LatencySums {
        count: r.get(at)?,
        failed: r.get(at + 1)?,
        rtt: r.get(at + 2)?,
        overhead: r.get(at + 3)?,
        total: r.get(at + 4)?,
        under_100: r.get(at + 5)?,
        under_150: r.get(at + 6)?,
        under_200: r.get(at + 7)?,
        under_250: r.get(at + 8)?,
        under_400: r.get(at + 9)?,
    })
}

/// Sums per stored tag, from one pass over the table (no tag is `None`).
pub fn sums_by_tag(conn: &Connection) -> Result<Vec<(Option<String>, LatencySums)>> {
    let sql = format!(
        "SELECT NULLIF(order_type, ''), {SUMS} FROM order_latency GROUP BY NULLIF(order_type, '')"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, sums_at(r, 1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Sums per broker of the records `filter` picks, by broker name.
pub fn broker_sums(
    conn: &Connection,
    filter: &LatencyFilter,
) -> Result<Vec<(String, LatencySums)>> {
    let sql = format!(
        "SELECT broker, {SUMS} FROM order_latency WHERE broker IS NOT NULL AND {}
         GROUP BY broker ORDER BY broker",
        filter.sql()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, sums_at(r, 1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One record's total time, with its tag and broker, for percentiles.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentTotal {
    pub tag: Option<String>,
    pub broker: Option<String>,
    pub total_ms: f64,
}

/// The total times of the records `filter` picks since `since` (a stored
/// timestamp).
pub fn recent_totals(
    conn: &Connection,
    since: &str,
    filter: &LatencyFilter,
) -> Result<Vec<RecentTotal>> {
    let sql = format!(
        "SELECT NULLIF(order_type, ''), broker, total_latency_ms FROM order_latency
         WHERE timestamp >= ?1 AND total_latency_ms IS NOT NULL AND {}",
        filter.sql()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(params![since], |r| {
            Ok(RecentTotal {
                tag: r.get(0)?,
                broker: r.get(1)?,
                total_ms: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// A broker's records and their rtt (count, average, min, max), for its
/// histogram (web `get_histogram_data`, every record of the broker).
pub fn broker_rtt(conn: &Connection, broker: &str) -> Result<(i64, f64, f64, f64)> {
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(AVG(COALESCE(rtt_ms, 0)), 0),
                COALESCE(MIN(COALESCE(rtt_ms, 0)), 0), COALESCE(MAX(COALESCE(rtt_ms, 0)), 0)
         FROM order_latency WHERE broker = ?1",
        params![broker],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}
