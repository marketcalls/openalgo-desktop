//! Blocked addresses (web `traffic_db.IPBan`): bans by hand and automatic ones, with the
//! caps and the addresses that are never banned.

use super::health::{raise_alert, AlertRow};
use super::{cap_text, ts};
use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct BanRow {
    pub ip_address: String,
    pub ban_reason: Option<String>,
    pub ban_count: i64,
    pub banned_at: String,
    pub expires_at: Option<String>,
    pub is_permanent: bool,
    pub created_by: String,
}

pub fn is_loopback_ip(ip: &str) -> bool {
    ip.trim().eq_ignore_ascii_case("localhost")
        || crate::server::addr::parse(ip).is_some_and(|a| a.is_loopback())
}

/// Addresses that are never banned: loopback (the trader's own machine), and
/// the internal identities in the discard-only `100::/64` block: the shared
/// identity of every caller behind a tunnel or proxy (banning it would cut
/// off the trader's own TradingView and Chartink alerts) and the MCP
/// dispatcher (security review S-03). Every real address, however it is
/// written, is checked like any other.
pub fn never_banned(ip: &str) -> bool {
    is_loopback_ip(ip)
        || matches!(
            crate::server::addr::parse(ip),
            Some(std::net::IpAddr::V6(v)) if v.segments()[..4] == [0x100, 0, 0, 0]
        )
}

/// A ban to record ([`ban_ip`]).
#[derive(Debug, Clone, Copy)]
pub struct Ban<'a> {
    pub ip: &'a str,
    pub reason: &'a str,
    /// How long it lasts; `None` (or `permanent`) never expires.
    pub duration_hours: Option<i64>,
    pub permanent: bool,
    /// Who banned it: "manual", "host_ban", "system".
    pub created_by: &'a str,
}

/// Ban (or re-ban) an address. Loopback is never banned. A ban past
/// `repeat_limit` bans of the address is permanent: the Security page's
/// Repeat Limit ("Number of bans before permanent ban") for the bans the
/// trader enters there. Automatic bans pass no limit (`auto_ban`), so they
/// always expire. The web makes the `repeat_limit`-th ban permanent
/// (`ban_count >= repeat_limit`), one ban sooner than its page says, and
/// applies it to automatic bans too.
pub fn ban_ip(
    conn: &Connection,
    ban: Ban<'_>,
    now: DateTime<Utc>,
    repeat_limit: i64,
) -> Result<bool> {
    let Ban {
        ip,
        reason,
        duration_hours,
        permanent,
        created_by,
    } = ban;
    // Stored in the one spelling every check looks up: `::ffff:1.2.3.4`
    // is `1.2.3.4`, IPv6 in its compressed lower-case form.
    let Some(ip) = crate::server::addr::canonical_text(ip) else {
        return Ok(false);
    };
    let ip = ip.as_str();
    if never_banned(ip) {
        return Ok(false);
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT ban_count FROM ip_bans WHERE ip_address = ?1",
            [ip],
            |r| r.get(0),
        )
        .optional()?;
    let count = existing.map(|c| c + 1).unwrap_or(1);
    let permanent = permanent || duration_hours.is_none() || count > repeat_limit.max(1);
    let expires = if permanent {
        None
    } else {
        duration_hours.map(|h| ts(now + Duration::hours(h)))
    };
    conn.execute(
        "INSERT INTO ip_bans (ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(ip_address) DO UPDATE SET ban_reason = excluded.ban_reason,
             ban_count = excluded.ban_count, banned_at = excluded.banned_at,
             expires_at = excluded.expires_at, is_permanent = excluded.is_permanent,
             created_by = excluded.created_by",
        params![
            ip,
            cap_text(reason, 200),
            count,
            ts(now),
            expires,
            permanent,
            cap_text(created_by, 50)
        ],
    )?;
    Ok(true)
}

/// Most automatic bans in force at once. Beyond it no further address is
/// banned automatically; the rate limits and failure budgets still apply.
pub const AUTO_BAN_CAP: i64 = 10_000;

/// How long an automatic ban lasts when the setting gives no duration.
pub const AUTO_BAN_DEFAULT_HOURS: i64 = 24;

/// An automatic ban (the 404 and invalid-key trackers). It applies to a
/// device on the network only, keyed as it is counted (an IPv4 address,
/// an IPv6 /64): never this computer, the internal identities (the tunnel
/// identity among them) or exactly one of this machine's own addresses
/// (`own`; the trader's own browser on the LAN address). Devices sharing
/// the machine's IPv6 prefix are keyed one by one, so each is banned on its
/// own. It always expires, is never
/// made permanent by repetition, never shortens a ban already in force (a
/// ban entered by hand included), and at most `cap` are in force at once.
pub fn auto_ban(
    conn: &Connection,
    ip: &str,
    reason: &str,
    duration_hours: Option<i64>,
    now: DateTime<Utc>,
    own: &[std::net::IpAddr],
    cap: i64,
) -> Result<bool> {
    let Some(addr) = crate::server::addr::parse(ip) else {
        return Ok(false);
    };
    let text = crate::server::addr::canonical(addr).to_string();
    if never_banned(&text) {
        return Ok(false);
    }
    // This machine's own addresses, exactly (the trader's own browser on
    // the LAN address). Other devices in its IPv6 prefix are keyed and
    // banned one by one.
    let exact = crate::server::addr::canonical(addr);
    if own
        .iter()
        .any(|o| crate::server::addr::canonical(*o) == exact)
    {
        return Ok(false);
    }
    let in_force: Option<(bool, Option<String>)> = conn
        .query_row(
            "SELECT is_permanent, expires_at FROM ip_bans WHERE ip_address = ?1",
            [&text],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((permanent, expires)) = in_force {
        if permanent || expires.is_some_and(|e| e > ts(now)) {
            return Ok(false);
        }
    } else {
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ip_bans WHERE created_by = 'system'
             AND (is_permanent = 1 OR expires_at > ?1)",
            [ts(now)],
            |r| r.get(0),
        )?;
        if active >= cap {
            // Never make room by lifting a ban in force: keep them all,
            // add none, and tell the trader.
            raise_alert(
                conn,
                &AlertRow {
                    alert_type: "ban_list_full".into(),
                    severity: "warn".into(),
                    metric_name: "automatic_bans".into(),
                    metric_value: active as f64,
                    threshold_value: cap as f64,
                    message: "Too many addresses are banned automatically, so no more are \
                              being added. Review the Security page."
                        .into(),
                    ..Default::default()
                },
                now,
            )?;
            return Ok(false);
        }
    }
    let hours = duration_hours.unwrap_or(AUTO_BAN_DEFAULT_HOURS).max(1);
    let ban = Ban {
        ip: &text,
        reason,
        duration_hours: Some(hours),
        permanent: false,
        created_by: "system",
    };
    let banned = ban_ip(conn, ban, now, i64::MAX)?;
    if banned {
        if let Some(net) = crate::server::addr::aggregate_key(addr) {
            alert_on_banned_network(conn, net, now)?;
        }
    }
    Ok(banned)
}

/// Devices banned in one of this machine's own IPv6 networks before the
/// trader is told: such a network is never banned as a whole (every device
/// at home shares it), so many banned devices in it raise a health alert
/// instead.
pub const BANNED_DEVICES_ALERT: usize = 10;

/// The /64 an aggregate key stands for, written as its prefix.
fn network_prefix(net: std::net::IpAddr) -> String {
    match net {
        std::net::IpAddr::V6(v) => {
            let s = v.segments();
            std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0).to_string()
        }
        v4 => v4.to_string(),
    }
}

fn alert_on_banned_network(
    conn: &Connection,
    net: std::net::IpAddr,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut st = conn.prepare(
        "SELECT ip_address FROM ip_bans WHERE created_by = 'system'
         AND (is_permanent = 1 OR expires_at > ?1)",
    )?;
    let banned = st
        .query_map([ts(now)], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|ip| crate::server::addr::parse(&ip))
        .filter(|ip| crate::server::addr::aggregate_key(*ip) == Some(net))
        .count();
    if banned >= BANNED_DEVICES_ALERT {
        raise_alert(
            conn,
            &AlertRow {
                alert_type: "banned_devices_on_network".into(),
                severity: "warn".into(),
                metric_name: "banned_devices_on_network".into(),
                metric_value: banned as f64,
                threshold_value: BANNED_DEVICES_ALERT as f64,
                message: format!(
                    "{} devices on your network ({}/64) were blocked for repeated bad requests. \
                     One of your devices may be misconfigured or compromised; check the \
                     Security page.",
                    banned,
                    network_prefix(net)
                ),
                ..Default::default()
            },
            now,
        )?;
    }
    Ok(())
}

pub fn unban_ip(conn: &Connection, ip: &str) -> Result<bool> {
    let canonical = crate::server::addr::canonical_text(ip).unwrap_or_else(|| ip.to_string());
    Ok(conn.execute(
        "DELETE FROM ip_bans WHERE ip_address IN (?1, ?2)",
        params![canonical, ip],
    )? > 0)
}

pub fn all_bans(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<BanRow>> {
    conn.execute(
        "DELETE FROM ip_bans WHERE is_permanent = 0 AND expires_at IS NOT NULL AND expires_at < ?1",
        [ts(now)],
    )?;
    let mut st = conn.prepare(
        "SELECT ip_address, ban_reason, ban_count, banned_at, expires_at, is_permanent, created_by
         FROM ip_bans ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(BanRow {
                ip_address: r.get(0)?,
                ban_reason: r.get(1)?,
                ban_count: r.get(2)?,
                banned_at: r.get(3)?,
                expires_at: r.get(4)?,
                is_permanent: r.get(5)?,
                created_by: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
