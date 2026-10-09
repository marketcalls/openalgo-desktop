//! Who is calling (security review S-02, S-03, S-10): the one classifier
//! the HTTP server (`middleware::peer_layer`) and the market data feed's
//! handshake (`feed::server::handshake_source`) both use, with one list of
//! forwarding headers, one `Host` parser and canonical addresses, so the
//! two can never see different callers for the same connection.

use axum::http::{header, HeaderMap};
use std::net::{IpAddr, Ipv4Addr};

/// Where a request comes from. Classified once per request or feed
/// connection by [`classify`] and stored; every per-caller control (rate
/// limits, failure and sign-in budgets, bans, the webhook allowlist, the
/// Remote MCP switch, account setup) reads the stored value, and nothing
/// else reads forwarding headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A program on this computer: a loopback peer (127.0.0.0/8, ::1, or
    /// loopback inside IPv6) that sent no forwarding-type header and names
    /// this computer on the listener's own port. A peer using one of this
    /// machine's LAN addresses is a device on the network, like any other.
    Local,
    /// A device on the network: a socket peer that is not loopback. Its
    /// headers are never read.
    Lan(IpAddr),
    /// A caller behind a tunnel or proxy on this computer: a loopback peer
    /// that sent a forwarding-type header or named any other host. Its
    /// address is unknown: no forwarding header is ever read as one, so
    /// every tunnel caller is one shared identity ([`PROXIED_CALLER`]).
    Tunnel,
}

impl Source {
    /// The address per-caller controls (rate limits, failure budgets,
    /// monitoring and automatic bans) count this caller under: a device on
    /// the network by [`crate::server::addr::budget_key`] (its address;
    /// a foreign IPv6 device by its /64); every tunnel caller shares
    /// [`PROXIED_CALLER`].
    pub fn ip(self) -> IpAddr {
        match self {
            Source::Local => IpAddr::V4(Ipv4Addr::LOCALHOST),
            Source::Lan(ip) => crate::server::addr::budget_key(ip),
            Source::Tunnel => PROXIED_CALLER,
        }
    }

    /// The device's own address, which IP allowlists match and bans are
    /// checked for (with its /64 too, `Monitor::is_banned_peer`): a device
    /// on the network only. This computer and tunnel callers have none.
    pub fn network_address(self) -> Option<IpAddr> {
        match self {
            Source::Lan(ip) => Some(ip),
            _ => None,
        }
    }

    pub fn is_local(self) -> bool {
        self == Source::Local
    }
}

/// Stand-in address for callers that reach the app through a proxy or
/// tunnel on this machine whose address is not known: one shared,
/// non-loopback identity, so they are never trusted as local and never
/// share limits with the trader's own programs. In the discard-only
/// `100::/64` block, like the MCP dispatcher's address.
pub const PROXIED_CALLER: IpAddr = IpAddr::V6(std::net::Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 2));

/// Header names a reverse proxy, tunnel or CDN adds (ngrok, cloudflared,
/// Tailscale Funnel, Fastly, Fly, nginx, Caddy, Envoy), besides every
/// `x-forwarded-*`. A loopback request carrying any of them, whatever its
/// value (empty and repeated ones included), came from somewhere else.
const FORWARDING_HEADERS: [&str; 13] = [
    "forwarded",
    "via",
    "x-real-ip",
    "x-client-ip",
    "x-cluster-client-ip",
    "x-original-forwarded-for",
    "true-client-ip",
    "cf-connecting-ip",
    "cf-ray",
    "fastly-client-ip",
    "fly-client-ip",
    "tailscale-funnel-request",
    "x-envoy-external-address",
];

/// Whether a forwarding-type header is present. Header names are matched
/// case-insensitively (they are stored in lower case).
pub fn forwarded(headers: &HeaderMap) -> bool {
    headers.keys().any(|k| {
        let n = k.as_str();
        n.starts_with("x-forwarded-") || FORWARDING_HEADERS.contains(&n)
    })
}

/// `host[:port]` split into its name (lower case, IPv6 without brackets)
/// and port. `None` for a malformed port or bracket.
pub fn split_authority(host: &str) -> Option<(String, Option<u16>)> {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        let (name, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        return Some((name.to_string(), port));
    }
    match host.rsplit_once(':') {
        Some((name, p)) => Some((name.to_string(), Some(p.parse::<u16>().ok()?))),
        None => Some((host, None)),
    }
}

/// The names this computer answers to on loopback.
pub fn is_loopback_name(name: &str) -> bool {
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// Whether `host` names this computer on loopback on one of the
/// listener's own ports.
fn names_this_computer(host: &str, own_port: &dyn Fn(Option<u16>) -> bool) -> bool {
    split_authority(host).is_some_and(|(name, port)| is_loopback_name(&name) && own_port(port))
}

/// Who is calling. `peer` is the connection's socket peer (`None` for an
/// in-process call with no connection, as in tests); `authority` the
/// request target's authority (HTTP/2 `:authority`, or an absolute-form
/// HTTP/1.1 target); `own_port` says whether a port (or none) is the
/// listener's own: the app's pages for HTTP, the feed's port for the feed.
///
/// * A peer that is not loopback is [`Source::Lan`], by canonical address
///   (one of this machine's own LAN addresses included); its headers are
///   never read (anyone can send them).
/// * A loopback peer (or no connection) is [`Source::Local`] only when no
///   forwarding-type header is present and every host it names (`Host`,
///   exactly once, and the target's authority) is this computer on
///   loopback on the listener's own port. A request with a connection must
///   name one. No list of interfaces ever makes a caller local.
/// * Anything else is [`Source::Tunnel`]: never local, no address, one
///   shared identity.
pub fn classify(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    authority: Option<&str>,
    own_port: &dyn Fn(Option<u16>) -> bool,
) -> Source {
    if let Some(peer) = peer.map(crate::server::addr::canonical) {
        if !peer.is_loopback() {
            return Source::Lan(peer);
        }
    }
    let mut hosts = headers.get_all(header::HOST).iter();
    let host_ok = match (hosts.next(), hosts.next()) {
        (Some(h), None) => h.to_str().is_ok_and(|h| names_this_computer(h, own_port)),
        (None, _) => authority.is_some() || peer.is_none(),
        _ => false,
    };
    let authority_ok = authority.is_none_or(|a| names_this_computer(a, own_port));
    if forwarded(headers) || !host_ok || !authority_ok {
        Source::Tunnel
    } else {
        Source::Local
    }
}
