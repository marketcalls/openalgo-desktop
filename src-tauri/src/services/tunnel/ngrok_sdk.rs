//! ngrok's official SDK (`ngrok` crate) as the tunnel provider, in
//! process: one session with ngrok's service and one HTTPS endpoint whose
//! connections come back to this app (`server::tunnel` serves them).
//!
//! The SDK reconnects a lost session itself, with a delay that grows
//! without a cap (`ExponentialBackoff::from_millis(50)`: 50 ms, 2.5 s,
//! 125 s, then hours). Its connector is told to give up instead, so a lost
//! session ends the tunnel and the supervisor opens a new one with its own
//! capped backoff (`supervisor`).

use super::provider::{OpenError, OpenRequest, OpenTunnel, TunnelIo, TunnelProvider};
use async_trait::async_trait;
use futures_util::StreamExt;
use ngrok::config::TunnelBuilder;
use ngrok::session::{ConnectError, IoStream};
use ngrok::tunnel::{AcceptError, EndpointInfo, HttpTunnel, TunnelCloser};
use std::sync::Arc;

/// Opens tunnels through ngrok's service.
#[derive(Default)]
pub struct NgrokProvider {
    /// ngrok's server address, replaced only by tests (a local socket).
    server: Option<String>,
}

impl NgrokProvider {
    #[cfg(test)]
    pub(crate) fn at(server: String) -> Self {
        Self {
            server: Some(server),
        }
    }
}

/// The session's connector: the first connection as ngrok makes it, and
/// no reconnect (see the module comment).
async fn connect_once(
    host: String,
    port: u16,
    tls: Arc<rustls::ClientConfig>,
    lost: Option<AcceptError>,
) -> Result<Box<dyn IoStream>, ConnectError> {
    if lost.is_some() {
        return Err(ConnectError::Canceled);
    }
    ngrok::session::default_connect(host, port, tls, None).await
}

#[async_trait]
impl TunnelProvider for NgrokProvider {
    async fn open(&self, request: &OpenRequest) -> Result<Box<dyn OpenTunnel>, OpenError> {
        let mut builder = ngrok::Session::builder();
        builder
            .authtoken(request.authtoken.expose())
            .client_info(
                "openalgo-desktop",
                env!("CARGO_PKG_VERSION"),
                None::<String>,
            )
            .connector(connect_once);
        if let Some(server) = &self.server {
            if builder.server_addr(server.as_str()).is_err() {
                return Err(OpenError::Other);
            }
        }
        let session = builder
            .connect()
            .await
            .map_err(|e| connect_problem(&e, request))?;
        let mut endpoint = session.http_endpoint();
        // Named, so ngrok is not sent the machine's host name and program
        // path (the SDK's default description of where traffic goes).
        endpoint.forwards_to("OpenAlgo Desktop");
        if let Some(domain) = &request.domain {
            endpoint.domain(domain.as_str());
        }
        let tunnel = endpoint
            .listen()
            .await
            .map_err(|e| problem("open the tunnel", &e, request))?;
        Ok(Box::new(NgrokTunnel { session, tunnel }))
    }
}

struct NgrokTunnel {
    session: ngrok::Session,
    tunnel: HttpTunnel,
}

#[async_trait]
impl OpenTunnel for NgrokTunnel {
    fn url(&self) -> String {
        self.tunnel.url().to_string()
    }

    async fn accept(&mut self) -> Option<Box<dyn TunnelIo>> {
        match self.tunnel.next().await {
            Some(Ok(conn)) => Some(Box::new(conn)),
            Some(Err(e)) => {
                tracing::info!("The ngrok tunnel ended: {}", e);
                None
            }
            None => None,
        }
    }

    async fn close(mut self: Box<Self>) {
        if let Err(e) = self.tunnel.close().await {
            tracing::debug!("Closing the ngrok tunnel: {}", e);
        }
        if let Err(e) = self.session.close().await {
            tracing::debug!("Closing the ngrok session: {}", e);
        }
    }
}

/// What a failed session start means for the trader.
fn connect_problem(e: &ConnectError, request: &OpenRequest) -> OpenError {
    match e {
        ConnectError::Tcp(_) | ConnectError::Tls(_) | ConnectError::ProxyConnect(_) => {
            tracing::warn!("Could not reach ngrok: {}", e);
            OpenError::Unreachable
        }
        _ => problem("sign in to ngrok", e, request),
    }
}

/// Classify an answer from ngrok and log it, never with the authtoken: the
/// answer to a malformed token repeats it.
fn problem(step: &str, e: &dyn ngrok::Error, request: &OpenRequest) -> OpenError {
    let code = e.error_code().unwrap_or("none");
    let kind = by_code(e.error_code());
    if kind == OpenError::Authtoken {
        tracing::warn!("ngrok refused the authtoken ({}) at: {}", code, step);
    } else {
        tracing::warn!(
            "ngrok did not {} ({}): {}",
            step,
            code,
            redacted(&e.msg(), request.authtoken.expose())
        );
    }
    kind
}

/// ngrok's error codes (ngrok.com/docs/errors/reference), by what the
/// trader can do about them.
pub(super) fn by_code(code: Option<&str>) -> OpenError {
    let Some(n) = code
        .and_then(|c| c.trim().strip_prefix("ERR_NGROK_"))
        .and_then(|n| n.parse::<u32>().ok())
    else {
        return OpenError::Other;
    };
    match n {
        // Malformed, a v1 token, invalid, revoked; a verified account and
        // authtoken required.
        105 | 106 | 107 | 300 | 4018 => OpenError::Authtoken,
        // Suspended accounts; an email still to verify.
        103 | 123 | 205 | 247 | 338 => OpenError::Account,
        // Not reserved on the account, reserved by another, a paid-plan
        // name, a name the token may not bind, another region, bad
        // characters.
        307 | 309 | 313 | 314 | 315 | 316 | 317 | 318 | 319 | 320 | 321 | 322 | 326 => {
            OpenError::Domain
        }
        // The account's agent sessions or this endpoint are in use.
        108 | 334 | 348 | 349 => OpenError::InUse,
        _ => OpenError::Other,
    }
}

/// `msg` without the authtoken and without the line ngrok uses to repeat
/// one.
fn redacted(msg: &str, authtoken: &str) -> String {
    msg.lines()
        .filter(|l| !l.to_ascii_lowercase().contains("your authtoken"))
        .map(|l| {
            if authtoken.is_empty() {
                l.to_string()
            } else {
                l.replace(authtoken, "[REDACTED]")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
