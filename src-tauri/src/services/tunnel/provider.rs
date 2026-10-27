//! What opens a tunnel. The app's provider is ngrok's SDK
//! ([`super::ngrok_sdk::NgrokProvider`]); tests use a fake, so nothing in them
//! reaches the network.

use crate::security::Secret;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

/// One caller's connection through the tunnel: a byte stream carrying
/// HTTP/1.1, served like a connection to the local listener.
pub trait TunnelIo: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> TunnelIo for T {}

/// What a tunnel is opened with. `Debug` prints the authtoken redacted
/// (`Secret`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub authtoken: Secret,
    /// The domain on the trader's ngrok account (the free one under
    /// Domains in the ngrok dashboard). Required: the tunnel never opens on
    /// any other address, so the Host Server URL, the webhook addresses and
    /// the broker redirect address never change (maintainer, 2026-10-27).
    pub domain: String,
}

/// Why a tunnel did not open. Each is told to the trader in their words
/// (`status::problem_message`); the detail goes to the log where it
/// happened, never with the authtoken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// ngrok refused the authtoken: malformed, invalid or revoked.
    Authtoken,
    /// The account may not use the reserved domain (not reserved on it,
    /// reserved by another account, a paid-plan feature, a bad name).
    Domain,
    /// The account cannot start a tunnel now (suspended, email not
    /// verified).
    Account,
    /// The account's tunnel or domain is in use elsewhere (another ngrok
    /// agent, or a session of this app ngrok has not yet closed).
    InUse,
    /// ngrok opened the tunnel on another address than the domain; it is
    /// closed at once, never served.
    OtherAddress,
    /// ngrok could not be reached (network, firewall, DNS, TLS).
    Unreachable,
    /// Anything else ngrok answered.
    Other,
}

impl OpenError {
    /// Whether trying again later can succeed without the trader changing
    /// a setting. A refused authtoken or domain does not: retrying a wrong
    /// token only spends the account's attempts.
    pub fn retries(self) -> bool {
        matches!(self, Self::InUse | Self::Unreachable | Self::Other)
    }
}

/// Opens tunnels.
#[async_trait]
pub trait TunnelProvider: Send + Sync + 'static {
    async fn open(&self, request: &OpenRequest) -> Result<Box<dyn OpenTunnel>, OpenError>;
}

/// A tunnel that is open.
#[async_trait]
pub trait OpenTunnel: Send + 'static {
    /// The public address callers use (`https://...`).
    fn url(&self) -> String;
    /// The next caller's connection; `None` once the tunnel is lost.
    async fn accept(&mut self) -> Option<Box<dyn TunnelIo>>;
    /// End the tunnel and its session with ngrok.
    async fn close(self: Box<Self>);
}
