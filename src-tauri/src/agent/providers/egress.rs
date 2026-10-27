//! Where the Agent's provider requests may connect: never the link-local
//! block, where the cloud metadata service answers.
//!
//! The endpoint guard (`base_url`) reads the text a trader saves; it cannot
//! see a name that resolves to 169.254.169.254 or a provider that redirects
//! there, and on a cloud machine (Windows Server on AWS, say) that address
//! hands out the machine's own credentials. So the Agent's HTTP client
//! (`run::client::http_client`) follows no redirect and resolves names
//! through [`GuardedResolver`]: the system lookup, then every address in
//! 169.254.0.0/16, fe80::/10 or fd00:ec2::/32 dropped, after unwrapping the
//! IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`) and NAT64
//! (`64:ff9b::a.b.c.d`) spellings. A name with nothing left fails with
//! [`EndpointRefused`], which the run and the model test show as the
//! guard's sentence. Loopback and private addresses stay reachable: a model
//! on the trader's own computer or network (Ollama, LM Studio) is the point
//! of an endpoint, as on the web.
//!
//! An address written as a number in the URL is not looked up at all; the
//! endpoint guard refuses those ranges in the text (one rule: [`is_refused`]).

use super::base_url::METADATA;
use reqwest_agent::dns::{Addrs, Name, Resolve, Resolving};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;

/// The IPv4 address an IPv6 spelling carries: mapped, compatible (not the
/// unspecified or loopback address, which are IPv6's own) or NAT64.
pub fn unwrap_v4(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(a) = ip else { return ip };
    if let Some(v4) = a.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let s = a.segments();
    let tail = Ipv4Addr::new(
        (s[6] >> 8) as u8,
        (s[6] & 0xff) as u8,
        (s[7] >> 8) as u8,
        (s[7] & 0xff) as u8,
    );
    let compatible = s[..6].iter().all(|x| *x == 0) && !a.is_unspecified() && !a.is_loopback();
    let nat64 = s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0);
    if compatible || nat64 {
        IpAddr::V4(tail)
    } else {
        ip
    }
}

/// Whether a provider request may never reach this address: link-local
/// IPv4 (169.254.0.0/16, the metadata service among it), link-local IPv6
/// (fe80::/10) and AWS's IPv6 metadata block (fd00:ec2::/32).
pub fn is_refused(ip: IpAddr) -> bool {
    match unwrap_v4(ip) {
        IpAddr::V4(a) => a.octets()[..2] == [169, 254],
        IpAddr::V6(a) => {
            let s = a.segments();
            s[0] & 0xffc0 == 0xfe80 || (s[0] == 0xfd00 && s[1] == 0x0ec2)
        }
    }
}

/// A name that resolved only to refused addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointRefused;

impl std::fmt::Display for EndpointRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(METADATA)
    }
}

impl std::error::Error for EndpointRefused {}

/// The name the run's error reading gives a refused endpoint.
pub const REFUSED: &str = "EndpointRefused";

/// Whether an error, or anything it was caused by, is [`EndpointRefused`].
fn refused_in(mut e: Option<&(dyn std::error::Error + 'static)>) -> bool {
    while let Some(err) = e {
        if err.is::<EndpointRefused>() || err.to_string().contains(METADATA) {
            return true;
        }
        e = err.source();
    }
    false
}

/// Whether a provider request failed because its endpoint was refused.
pub fn refused(err: &genai::Error) -> bool {
    use genai::webc::Error as W;
    match err {
        genai::Error::WebModelCall { webc_error, .. }
        | genai::Error::WebAdapterCall { webc_error, .. } => match webc_error {
            W::Reqwest(r) => refused_in(Some(r as &(dyn std::error::Error + 'static))),
            other => other.to_string().contains(METADATA),
        },
        genai::Error::WebStream { error, .. } => {
            let inner: &(dyn std::error::Error + 'static) = error.as_ref();
            refused_in(Some(inner))
        }
        other => other.to_string().contains(METADATA),
    }
}

/// A lookup in progress: the addresses a name resolves to.
pub type Looking = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;

/// A name lookup (the system's; a test's).
pub trait Lookup: Send + Sync + 'static {
    fn lookup(&self, host: String) -> Looking;
}

/// The operating system's lookup (`getaddrinfo`, off the async runtime).
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: String) -> Looking {
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect()) })
    }
}

/// The Agent's resolver: a lookup, then the refused ranges dropped.
pub struct GuardedResolver<L: Lookup = SystemLookup>(pub L);

impl GuardedResolver {
    /// On the system's lookup.
    pub fn system() -> GuardedResolver {
        GuardedResolver(SystemLookup)
    }
}

/// The addresses a request may use, or the refusal when none is left.
pub fn keep_allowed(found: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, EndpointRefused> {
    let total = found.len();
    let kept: Vec<SocketAddr> = found.into_iter().filter(|a| !is_refused(a.ip())).collect();
    if kept.is_empty() && total > 0 {
        return Err(EndpointRefused);
    }
    Ok(kept)
}

impl<L: Lookup> Resolve for GuardedResolver<L> {
    fn resolve(&self, name: Name) -> Resolving {
        let looking = self.0.lookup(name.as_str().to_string());
        Box::pin(async move {
            let found = looking.await?;
            let kept = keep_allowed(found).inspect_err(|_| {
                tracing::warn!(
                    "An Agent provider address resolved only to a refused range; not connecting"
                );
            })?;
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}
