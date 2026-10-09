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
