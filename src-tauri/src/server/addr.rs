//! One parser for every network address the server reads from text: the
//! caller address a configured tunnel header reports, and the addresses
//! stored in or checked against the ban list. Two parsers that disagree are
//! a way around whichever check uses the stricter one.

use std::net::{IpAddr, SocketAddr};

/// `ip` in its one canonical form: an IPv4 address carried in IPv6
/// (`::ffff:1.2.3.4`, what a dual-stack listener reports for an IPv4
/// caller) is the IPv4 address itself.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// The address a device on the network is counted and banned under, given
/// this machine's own addresses (`own`): an IPv4 address itself; an IPv6
/// address inside a /64 one of this machine's interfaces holds, or a
/// link-local one (`fe80::/10`), itself too, since every device on the
/// trader's own network shares that prefix; any other IPv6 address by its
/// /64, which one device can rotate through at will.
pub fn budget_key_with(ip: IpAddr, own: &[IpAddr]) -> IpAddr {
    let ip = canonical(ip);
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    // The app's internal identities (`100::/64` to `100:0:0:2::/64`: the MCP
    // dispatcher, the tunnel, per-credential and overflow keys) are
    // identities, not devices: each keeps its own key.
    let s = v6.segments();
    if s[0] == 0x100 && s[1] == 0 && s[2] == 0 && s[3] <= 2 {
        return ip;
    }
    let prefix = |a: &std::net::Ipv6Addr| {
        let s = a.segments();
        std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0)
    };
    let link_local = v6.segments()[0] & 0xffc0 == 0xfe80;
    let ours = own
        .iter()
        .any(|o| matches!(o, IpAddr::V6(o) if prefix(o) == prefix(&v6)));
    if link_local || ours {
        ip
    } else {
        IpAddr::V6(prefix(&v6))
    }
}

/// The aggregate a caller key also counts against, in every limit
/// (`ratelimit::RateLimiter`): a per-address IPv6 key (a device inside one
/// of this machine's own prefixes, or a link-local one) counts against its
/// /64 (all of `fe80::/10` against one), so one device rotating through the
/// prefix is capped in total. Pure: it does not depend on which prefixes
/// are known as ours, so a stale list never loses the aggregate. `None` for
/// IPv4, a foreign /64 key, the internal identities and loopback. The
/// aggregate is written as the /64 with an all-ones interface identifier,
/// an anycast address no device sends from.
pub fn aggregate_key(key: IpAddr) -> Option<IpAddr> {
    let IpAddr::V6(v6) = canonical(key) else {
        return None;
    };
    let s = v6.segments();
    let internal = s[0] == 0x100 && s[1] == 0 && s[2] == 0 && s[3] <= 2;
    let network_key = s[4..] == [0, 0, 0, 0];
    if internal || network_key || is_aggregate(key) || v6.is_loopback() {
        return None;
    }
    let p = if s[0] & 0xffc0 == 0xfe80 {
        [0xfe80, 0, 0, 0]
    } else {
        [s[0], s[1], s[2], s[3]]
    };
    Some(IpAddr::V6(std::net::Ipv6Addr::new(
        p[0], p[1], p[2], p[3], 0xffff, 0xffff, 0xffff, 0xffff,
    )))
}

/// Whether `key` is an aggregate written by [`aggregate_key`].
pub fn is_aggregate(key: IpAddr) -> bool {
    matches!(key, IpAddr::V6(v) if v.segments()[4..] == [0xffff, 0xffff, 0xffff, 0xffff])
}

/// [`budget_key_with`] with this machine's addresses as they are now. An
/// IPv6 address whose prefix the last read does not hold as ours is keyed
/// by its /64 (failing toward aggregation); before that, the interfaces
/// are read again if the last read is more than a second old, in case the
/// machine has just taken that prefix.
pub fn budget_key(ip: IpAddr) -> IpAddr {
    let key = budget_key_with(ip, &own_addresses_cached());
    if matches!(key, IpAddr::V6(_)) && key != canonical(ip) {
        return budget_key_with(ip, &own_addresses_fresh(std::time::Duration::from_secs(1)));
    }
    key
}

/// This machine's own interface addresses (IPv4 inside IPv6 as IPv4).
pub fn own_addresses() -> Vec<IpAddr> {
    let nets = sysinfo::Networks::new_with_refreshed_list();
    let mut v: Vec<IpAddr> = nets
        .list()
        .values()
        .flat_map(|d| d.ip_networks().iter().map(|n| n.addr))
        .map(canonical)
        .filter(|ip| !ip.is_unspecified())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// How long a read of this machine's addresses is used: an address the
/// machine gives up is no longer treated as its own after this.
pub const OWN_ADDRESSES_TTL: std::time::Duration = std::time::Duration::from_secs(30);

type OwnCache = Option<(std::time::Instant, std::sync::Arc<Vec<IpAddr>>)>;

/// The cached list at `now`, read again with `fetch` once it is older than
/// `max_age`.
pub fn own_addresses_at(
    cache: &mut OwnCache,
    now: std::time::Instant,
    max_age: std::time::Duration,
    fetch: impl FnOnce() -> Vec<IpAddr>,
) -> std::sync::Arc<Vec<IpAddr>> {
    if let Some((at, v)) = cache.as_ref() {
        if now.saturating_duration_since(*at) < max_age {
            return v.clone();
        }
    }
    let v = std::sync::Arc::new(fetch());
    *cache = Some((now, v.clone()));
    v
}

/// [`own_addresses`], read again at most every [`OWN_ADDRESSES_TTL`].
/// Used to key and to spare this machine's own addresses, never to trust
/// a caller: only a loopback peer is this computer.
pub fn own_addresses_cached() -> std::sync::Arc<Vec<IpAddr>> {
    own_addresses_fresh(OWN_ADDRESSES_TTL)
}

static OWN_CACHE: parking_lot::Mutex<OwnCache> = parking_lot::Mutex::new(None);

/// [`own_addresses`], read again if the last read is older than `max_age`.
pub fn own_addresses_fresh(max_age: std::time::Duration) -> std::sync::Arc<Vec<IpAddr>> {
    own_addresses_at(
        &mut OWN_CACHE.lock(),
        std::time::Instant::now(),
        max_age,
        own_addresses,
    )
}

/// Whether `ip` is exactly one of this machine's own addresses (never
/// banned), as of the last read.
pub fn is_own_address(ip: IpAddr) -> bool {
    let ip = canonical(ip);
    !ip.is_unspecified() && own_addresses_cached().contains(&ip)
}

/// An address written as text, in any of the forms a header, a log or a
/// person may use (`1.2.3.4`, `1.2.3.4:5000`, `2001:DB8::1`, `[2001:db8::1]`,
/// `[2001:db8::1]:443`, `::ffff:1.2.3.4`), as its canonical address. `None`
/// for anything else, including host names and zone ids.
pub fn parse(text: &str) -> Option<IpAddr> {
    let t = text.trim();
    let ip = if let Some(rest) = t.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        if !(after.is_empty()
            || after
                .strip_prefix(':')
                .is_some_and(|p| p.parse::<u16>().is_ok()))
        {
            return None;
        }
        inner.parse::<IpAddr>().ok()?
    } else if let Ok(ip) = t.parse::<IpAddr>() {
        ip
    } else {
        t.parse::<SocketAddr>().ok()?.ip()
    };
    Some(canonical(ip))
}

/// The canonical text of an address given as text (what the ban list
/// stores and is looked up by).
pub fn canonical_text(text: &str) -> Option<String> {
    parse(text).map(|ip| ip.to_string())
}

#[cfg(test)]
mod tests {
    /// IPv6 devices inside this machine's own prefix (and link-local ones)
    /// are told apart; a foreign /64 is one device; IPv4 is per address.
    #[test]
    fn keys_tell_our_own_networks_devices_apart() {
        let own: Vec<IpAddr> = vec!["2001:db8:aa:bb::5".parse().unwrap()];
        let key = |s: &str| budget_key_with(s.parse().unwrap(), &own);
        // Two devices in our /64: two identities, each its own address.
        assert_eq!(
            key("2001:db8:aa:bb::6"),
            "2001:db8:aa:bb::6".parse::<IpAddr>().unwrap()
        );
        assert_ne!(key("2001:db8:aa:bb::6"), key("2001:db8:aa:bb::7"));
        // Link-local devices too.
        assert_ne!(key("fe80::1"), key("fe80::2"));
        // A foreign /64 collapses to one identity.
        assert_eq!(key("2001:db8:cc:dd::1"), key("2001:db8:cc:dd:ffff::9"));
        assert_eq!(
            key("2001:db8:cc:dd::1"),
            "2001:db8:cc:dd::".parse::<IpAddr>().unwrap()
        );
        // IPv4, plain or inside IPv6, per address.
        assert_eq!(
            key("::ffff:192.168.1.9"),
            "192.168.1.9".parse::<IpAddr>().unwrap()
        );
        assert_ne!(key("192.168.1.9"), key("192.168.1.10"));
    }

    /// A per-address IPv6 key counts against its /64 aggregate (all of
    /// link-local against one); a foreign /64 key, IPv4, loopback and the
    /// internal identities have none.
    #[test]
    fn per_address_ipv6_keys_have_an_aggregate() {
        let agg = |s: &str| aggregate_key(s.parse().unwrap());
        let net = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert_eq!(
            agg("2001:db8:aa:bb::6"),
            net("2001:db8:aa:bb:ffff:ffff:ffff:ffff")
        );
        assert_eq!(agg("2001:db8:aa:bb::7"), agg("2001:db8:aa:bb::6"));
        assert_eq!(agg("fe80::1"), net("fe80::ffff:ffff:ffff:ffff"));
        assert_eq!(agg("fe80:0:0:5::1"), agg("fe80::1"), "one for link-local");
        assert_eq!(agg("febf::1"), agg("fe80::1"));
        for none in [
            "2001:db8:cc:dd::",
            "192.168.1.9",
            "::1",
            "100::2",
            "100::1",
            "100:0:0:1::5",
            "100:0:0:2::",
            "2001:db8:aa:bb:ffff:ffff:ffff:ffff",
        ] {
            assert_eq!(agg(none), None, "{}", none);
        }
    }

    /// After an interface change, the address the machine gave up is no
    /// longer treated as its own once the short-lived read expires, and
    /// its prefix is no longer keyed as ours.
    #[test]
    fn a_given_up_address_stops_being_ours() {
        let old: IpAddr = "2001:db8:aa:bb::5".parse().unwrap();
        let new: IpAddr = "2001:db8:ee:ff::5".parse().unwrap();
        let mut cache = None;
        let t0 = std::time::Instant::now();
        let ttl = OWN_ADDRESSES_TTL;
        let first = own_addresses_at(&mut cache, t0, ttl, || vec![old]);
        assert!(first.contains(&old));
        // Within the time to live the read is kept.
        let ten = t0 + std::time::Duration::from_secs(10);
        let kept = own_addresses_at(&mut cache, ten, ttl, || vec![new]);
        assert!(kept.contains(&old));
        // After it, the new interfaces are read.
        let later = t0 + ttl + std::time::Duration::from_secs(1);
        let fresh = own_addresses_at(&mut cache, later, ttl, || vec![new]);
        assert!(!fresh.contains(&old) && fresh.contains(&new));
        let device: IpAddr = "2001:db8:aa:bb::9".parse().unwrap();
        assert_eq!(
            budget_key_with(device, &fresh),
            "2001:db8:aa:bb::".parse::<IpAddr>().unwrap(),
            "the old prefix is foreign now"
        );
    }

    use super::*;

    #[test]
    fn one_address_in_every_spelling_is_one_address() {
        for s in [
            "1.2.3.4",
            " 1.2.3.4 ",
            "1.2.3.4:5000",
            "::ffff:1.2.3.4",
            "[::ffff:1.2.3.4]:80",
        ] {
            assert_eq!(canonical_text(s).as_deref(), Some("1.2.3.4"), "{}", s);
        }
        for s in [
            "2001:db8::1",
            "2001:DB8::1",
            "2001:0db8:0000:0000:0000:0000:0000:0001",
            "[2001:db8::1]",
            "[2001:db8::1]:443",
        ] {
            assert_eq!(canonical_text(s).as_deref(), Some("2001:db8::1"), "{}", s);
        }
        for s in [
            "",
            "example.com",
            "1.2.3",
            "fe80::1%en0",
            "[2001:db8::1]x",
            "1.2.3.4:99999",
        ] {
            assert_eq!(parse(s), None, "{}", s);
        }
    }
}
