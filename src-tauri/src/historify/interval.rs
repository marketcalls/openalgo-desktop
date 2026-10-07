//! Historify's interval grammar (web `historify_db.parse_interval` and
//! friends). Case matters: lowercase `m` is minutes, uppercase `M` months.

/// Intervals physically stored (web `STORAGE_INTERVALS`).
pub const STORAGE_INTERVALS: &[&str] = &["1m", "D"];
/// Standard intervals aggregated from 1m on the fly (web `COMPUTED_INTERVALS`).
pub const COMPUTED_INTERVALS: &[&str] = &["5m", "15m", "30m", "1h"];

/// Intervals an uploaded file may be stored under (web `VALID_INTERVALS`).
pub const VALID_UPLOAD_INTERVALS: &[&str] = &[
    "1s", "5s", "10s", "15s", "30s", "1m", "2m", "3m", "5m", "10m", "15m", "20m", "30m", "45m",
    "1h", "2h", "3h", "4h", "D", "1D", "W", "1W", "M", "1M",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Intraday,
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    pub kind: Kind,
    /// The number in front of the unit (1 for a bare letter).
    pub value: u32,
    /// Minutes, for intraday intervals.
    pub minutes: u32,
    /// Months, for monthly, quarterly and yearly intervals.
    pub months: u32,
}

/// Web `parse_interval`.
pub fn parse(interval: &str) -> Option<Parsed> {
    let s = interval.trim();
    let p = |kind, value, minutes, months| {
        Some(Parsed {
            kind,
            value,
            minutes,
            months,
        })
    };
    match s {
        "D" => return p(Kind::Daily, 1, 0, 0),
        "W" => return p(Kind::Weekly, 1, 0, 0),
        "M" => return p(Kind::Monthly, 1, 0, 1),
        "Q" => return p(Kind::Quarterly, 1, 0, 3),
        "Y" => return p(Kind::Yearly, 1, 0, 12),
        _ => {}
    }
    let unit = s.chars().last()?;
    let digits = &s[..s.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u32 = digits.parse().ok()?;
    if value == 0 {
        return None;
    }
    match unit {
        'm' => p(Kind::Intraday, value, value, 0),
        'h' => p(Kind::Intraday, value, value.checked_mul(60)?, 0),
        'D' => p(Kind::Daily, value, 0, 0),
        'W' => p(Kind::Weekly, value, 0, 0),
        'M' => p(Kind::Monthly, value, 0, value),
        'Q' => p(Kind::Quarterly, value, 0, value.checked_mul(3)?),
        'Y' => p(Kind::Yearly, value, 0, value.checked_mul(12)?),
        _ => None,
    }
}

pub fn is_storage(interval: &str) -> bool {
    STORAGE_INTERVALS.contains(&interval)
}

/// Web `is_custom_interval`: an intraday interval computed from 1m.
pub fn is_custom(interval: &str) -> bool {
    if is_storage(interval) {
        return false;
    }
    matches!(parse(interval), Some(p) if p.kind == Kind::Intraday)
}

/// Web `is_daily_aggregated_interval`: W, M, Q, Y and their multiples.
pub fn is_daily_aggregated(interval: &str) -> bool {
    matches!(
        parse(interval).map(|p| p.kind),
        Some(Kind::Weekly | Kind::Monthly | Kind::Quarterly | Kind::Yearly)
    )
}

/// Intraday computed (standard or custom).
pub fn is_intraday_computed(interval: &str) -> bool {
    COMPUTED_INTERVALS.contains(&interval) || is_custom(interval)
}

/// Python `sorted()` of a set of strings (lexicographic).
pub fn sorted(v: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// Market open, seconds from IST midnight (web `EXCHANGE_MARKET_OPEN_SECONDS`).
pub fn market_open_seconds(exchange: &str) -> i64 {
    match exchange.to_ascii_uppercase().as_str() {
        "CDS" | "BCD" | "MCX" => 32_400,
        _ => 33_300,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_matches_the_web() {
        assert_eq!(parse("25m").unwrap().minutes, 25);
        assert_eq!(parse("2h").unwrap().minutes, 120);
        assert_eq!(parse("M").unwrap().kind, Kind::Monthly);
        assert_eq!(parse("3M").unwrap().months, 3);
        assert_eq!(parse("2Q").unwrap().months, 6);
        assert_eq!(parse("1D").unwrap().kind, Kind::Daily);
        assert!(parse("MO").is_none());
        assert!(parse("0m").is_none());
        assert!(parse("").is_none());
        assert!(parse("m").is_none());
        assert!(is_custom("25m"));
        assert!(!is_custom("1m"));
        assert!(!is_custom("D"));
        assert!(is_daily_aggregated("W"));
        assert!(is_daily_aggregated("2Y"));
        assert!(!is_daily_aggregated("2D"));
        assert_eq!(sorted(COMPUTED_INTERVALS), vec!["15m", "1h", "30m", "5m"]);
    }
}
