//! Where an HTTP Request node may send (the web's `_check_http_destination`,
//! `flow_executor_service.py` 3108-3150, lead decision 12).
//!
//! The URL is interpolated from workflow variables, and a webhook puts its
//! caller's JSON into them, so whoever can fire a webhook can steer this
//! request. Every address the host resolves to must be public: Python 3.12's
//! `ipaddress` classes (private, loopback, link-local, reserved, multicast,
//! unspecified), written out here as address blocks because Rust has no
//! `is_private`/`is_reserved` for IPv6, plus 100.64.0.0/10 (carrier-grade
//! NAT, the Tailscale range), which Python counts as public (parity
//! N-httpRequest-07). The request then connects to a vetted address only,
//! never to a second lookup of the name (N-httpRequest-08).

use super::url::urlsplit;
use crate::flow::contract::env::OutboundHttp;
use crate::flow::contract::pyfmt::quote;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// IPv4 blocks refused (address, prefix length).
const V4_REFUSED: &[([u8; 4], u32)] = &[
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([224, 0, 0, 0], 4),
    ([240, 0, 0, 0], 4),
];

/// Globally reachable addresses inside a refused IPv4 block.
const V4_ALLOWED: &[[u8; 4]] = &[[192, 0, 0, 9], [192, 0, 0, 10]];

/// IPv6 blocks refused: the reserved blocks (which hold loopback,
/// unspecified, IPv4-compatible, NAT64 and discard), documentation, 6to4,
/// unique-local, link-local and multicast.
const V6_REFUSED: &[(u128, u32)] = &[
    (0x0000 << 112, 8),
    (0x0100 << 112, 8),
    (0x0200 << 112, 7),
    (0x0400 << 112, 6),
    (0x0800 << 112, 5),
    (0x1000 << 112, 4),
    (0x2001_0db8 << 96, 32),
    (0x2002 << 112, 16),
    (0x3fff << 112, 20),
    (0x4000 << 112, 3),
    (0x6000 << 112, 3),
    (0x8000 << 112, 3),
    (0xa000 << 112, 3),
    (0xc000 << 112, 3),
    (0xe000 << 112, 4),
    (0xf000 << 112, 5),
    (0xf800 << 112, 6),
    (0xfc00 << 112, 7),
    (0xfe00 << 112, 9),
    (0xfe80 << 112, 10),
    (0xff00 << 112, 8),
];

/// IETF protocol assignments (2001::/23), refused except the blocks IANA
/// lists as globally reachable.
const V6_IETF: (u128, u32) = (0x2001 << 112, 23);
const V6_IETF_ALLOWED: &[(u128, u32)] = &[
    ((0x2001_0001 << 96) | 1, 128),
    ((0x2001_0001 << 96) | 2, 128),
    (0x2001_0003 << 96, 32),
    ((0x2001_0004_0112u128) << 80, 48),
    (0x2001_0020 << 96, 28),
    (0x2001_0030 << 96, 28),
];

fn in_v6(addr: u128, (net, len): (u128, u32)) -> bool {
    let mask = if len == 0 {
        0
    } else {
        u128::MAX << (128 - len)
    };
    addr & mask == net & mask
}

fn v4_refused(a: Ipv4Addr) -> bool {
    if V4_ALLOWED.contains(&a.octets()) {
        return false;
    }
    let n = u32::from(a);
    V4_REFUSED.iter().any(|(net, len)| {
        let mask = u32::MAX << (32 - len);
        n & mask == u32::from(Ipv4Addr::from(*net)) & mask
    })
}

fn v6_refused(a: Ipv6Addr) -> bool {
    let n = u128::from(a);
    if V6_REFUSED.iter().any(|b| in_v6(n, *b)) {
        return true;
    }
    in_v6(n, V6_IETF) && !V6_IETF_ALLOWED.iter().any(|b| in_v6(n, *b))
}

/// Whether the node may never send to `ip`. An IPv4-mapped IPv6 address
/// is judged as the IPv4 address it carries, as Python 3.12 does.
pub fn is_refused(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => v4_refused(a),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4_refused(v4),
            None => v6_refused(a),
        },
    }
}

/// A destination that passed the check.
#[derive(Debug, Clone, PartialEq)]
pub struct Vetted {
    /// The only address the request may connect to.
    pub connect_to: SocketAddr,
}

/// A URL the parser cannot read the way the web's check reads it.
pub const UNPARSEABLE: &str = "URL could not be parsed";

/// Whether the WHATWG parser the request is sent with sees the same host
/// the check saw.
fn same_host(url: &str, hostname: &str) -> bool {
    let Ok(parsed) = ::url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host() else {
        return false;
    };
    let ours = match hostname.parse::<IpAddr>() {
        Ok(IpAddr::V6(a)) => ::url::Host::<String>::Ipv6(a),
        Ok(IpAddr::V4(a)) => ::url::Host::Ipv4(a),
        Err(_) => match ::url::Host::parse(hostname) {
            Ok(h) => h,
            Err(_) => return false,
        },
    };
    host.to_owned() == ours
}

/// Check `url` (`_check_http_destination`): `Err` with the reason the
/// node reports after `refusing to send this request: `.
pub async fn check(http: &dyn OutboundHttp, url: &str) -> Result<Vetted, String> {
    let parts = urlsplit(url).map_err(|_| UNPARSEABLE.to_string())?;
    if parts.scheme != "http" && parts.scheme != "https" {
        let shown = if parts.scheme.is_empty() {
            "none"
        } else {
            parts.scheme.as_str()
        };
        return Err(format!(
            "scheme {} is not allowed; use http or https",
            quote(shown)
        ));
    }
    let Some(host) = parts.hostname() else {
        return Err("URL has no host".into());
    };
    // The web lets a bad port raise out of the check and abort the whole
    // run (parity N-httpRequest-19); here only the node fails.
    let port = parts.port().map_err(|_| UNPARSEABLE.to_string())?;
    if !same_host(url, &host) {
        return Err(UNPARSEABLE.into());
    }
    let addresses = http
        .resolve(&host)
        .await
        .map_err(|e| format!("host {} could not be resolved ({})", quote(&host), e))?;
    if addresses.is_empty() {
        return Err(format!(
            "host {} could not be resolved (no address)",
            quote(&host)
        ));
    }
    if let Some(bad) = addresses.iter().find(|a| is_refused(**a)) {
        return Err(format!(
            "host {} resolves to {}, which is not a public address",
            quote(&host),
            bad
        ));
    }
    let ip = addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let default_port = if parts.scheme == "https" { 443 } else { 80 };
    let port = port.filter(|p| *p != 0).unwrap_or(default_port);
    Ok(Vetted {
        connect_to: SocketAddr::new(ip, port),
    })
}
