//! Runtime configuration. Every setting lives in the `settings` row of the
//! main database and is edited in the app; there is no `.env`.
//!
//! The only setting read from the environment is the development override
//! `OPENALGO_DESKTOP_DEV_PORTS=1`, which forces the HTTP listener to 5500 and
//! the WebSocket listener to 8766 without touching the stored settings. It
//! exists so development on a machine that also runs OpenAlgo web
//! (5000/8765) never binds those ports. Debug builds use the development
//! ports unconditionally. The app and the `mcp` subcommand read it through
//! the one parser here (`dev_ports_from`).
//!
//! Two other variables are read, neither a setting: `OPENALGO_MCP_TOKEN`,
//! by the `mcp` subcommand alone (the AI client sets it; see
//! `mcp::stdio`), and `APPIMAGE` on Linux, set by the AppImage runtime to
//! the file being run, so the MCP client configuration names that file.

use crate::error::{AppError, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

pub const DEFAULT_HTTP_PORT: u16 = 5000;
pub const DEFAULT_WS_PORT: u16 = 8765;
pub const DEV_HTTP_PORT: u16 = 5500;
pub const DEV_WS_PORT: u16 = 8766;
pub const DEV_PORTS_ENV: &str = "OPENALGO_DESKTOP_DEV_PORTS";

/// Request body limit for every route.
pub const BODY_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServerConfig {
    pub http_port: u16,
    pub ws_port: u16,
    pub bind_host: String,
    /// Public URL the broker redirects to (ngrok etc.), when set.
    pub host_server: Option<String>,
    pub ngrok_allow: bool,
    pub websocket_url: Option<String>,
    pub active_broker: Option<String>,
    pub redirect_url: Option<String>,
    /// Daily broker-session boundary in IST (web SESSION_EXPIRY_TIME).
    pub session_expiry_hour: u32,
    pub session_expiry_minute: u32,
    pub dev_ports: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_port: DEFAULT_HTTP_PORT,
            ws_port: DEFAULT_WS_PORT,
            bind_host: "127.0.0.1".into(),
            host_server: None,
            ngrok_allow: false,
            websocket_url: None,
            active_broker: None,
            redirect_url: None,
            session_expiry_hour: 3,
            session_expiry_minute: 0,
            dev_ports: false,
        }
    }
}

/// Development builds always use the development ports; release builds only
/// when `OPENALGO_DESKTOP_DEV_PORTS=1` is set.
pub fn dev_ports_enabled() -> bool {
    dev_ports_from(
        std::env::var(DEV_PORTS_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// Whether the development ports apply, from the override's value and
/// whether this is a debug build. The one parser of the override: the app
/// and the `mcp` subcommand must agree on the port.
pub fn dev_ports_from(value: Option<&str>, debug_build: bool) -> bool {
    debug_build || matches!(value, Some("1") | Some("true") | Some("TRUE"))
}

impl ServerConfig {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut c = conn.query_row(
            "SELECT http_port, ws_port, bind_host, host_server, ngrok_allow, websocket_url,
                    active_broker, redirect_url, auto_logout_hour, auto_logout_minute
             FROM settings WHERE id = 1",
            [],
            |r| {
                Ok(ServerConfig {
                    http_port: r.get::<_, Option<u16>>(0)?.unwrap_or(DEFAULT_HTTP_PORT),
                    ws_port: r.get::<_, Option<u16>>(1)?.unwrap_or(DEFAULT_WS_PORT),
                    bind_host: r
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| "127.0.0.1".into()),
                    host_server: r.get::<_, Option<String>>(3)?.filter(|s| !s.is_empty()),
                    ngrok_allow: r.get::<_, i64>(4)? != 0,
                    websocket_url: r.get::<_, Option<String>>(5)?.filter(|s| !s.is_empty()),
                    active_broker: r.get::<_, Option<String>>(6)?.filter(|s| !s.is_empty()),
                    redirect_url: r.get::<_, Option<String>>(7)?.filter(|s| !s.is_empty()),
                    session_expiry_hour: r.get::<_, u32>(8)?,
                    session_expiry_minute: r.get::<_, u32>(9)?,
                    dev_ports: false,
                })
            },
        )?;
        if dev_ports_enabled() {
            c.http_port = DEV_HTTP_PORT;
            c.ws_port = DEV_WS_PORT;
            c.dev_ports = true;
        }
        Ok(c)
    }

    pub fn is_loopback(&self) -> bool {
        matches!(self.bind_host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Base URL the broker redirects back to.
    pub fn public_base_url(&self) -> String {
        match (&self.host_server, self.ngrok_allow) {
            (Some(h), true) => h.trim_end_matches('/').to_string(),
            _ => format!("http://127.0.0.1:{}", self.http_port),
        }
    }

    /// Redirect URL registered with the broker: the stored value when the
    /// trader set one, otherwise `<base>/<broker>/callback` (byte-identical
    /// to the web convention).
    pub fn redirect_url_for(&self, broker: &str) -> String {
        if let Some(r) = &self.redirect_url {
            if r.ends_with(&format!("/{}/callback", broker)) {
                return r.clone();
            }
        }
        format!("{}/{}/callback", self.public_base_url(), broker)
    }
}

/// Validate a bind host: loopback names, `0.0.0.0`, or an IP literal.
pub fn validate_bind_host(host: &str) -> Result<()> {
    if matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        Ok(())
    } else {
        Err(AppError::Validation(
            "Enter an IP address such as 127.0.0.1 for this computer only.".into(),
        ))
    }
}

/// Fields the in-app settings page may change.
#[derive(Debug, Default)]
pub struct ServerConfigUpdate {
    pub http_port: Option<u16>,
    pub ws_port: Option<u16>,
    pub bind_host: Option<String>,
    pub host_server: Option<String>,
    pub ngrok_allow: Option<bool>,
    pub websocket_url: Option<String>,
    pub active_broker: Option<String>,
    pub redirect_url: Option<String>,
}

pub fn save(conn: &Connection, u: &ServerConfigUpdate) -> Result<()> {
    if let Some(h) = &u.bind_host {
        validate_bind_host(h)?;
    }
    for p in [u.http_port, u.ws_port].into_iter().flatten() {
        if p < 1024 {
            return Err(AppError::Validation(
                "Choose a port number between 1024 and 65535.".into(),
            ));
        }
    }
    conn.execute(
        "UPDATE settings SET
            http_port = COALESCE(?1, http_port),
            ws_port = COALESCE(?2, ws_port),
            bind_host = COALESCE(?3, bind_host),
            host_server = COALESCE(?4, host_server),
            ngrok_allow = COALESCE(?5, ngrok_allow),
            websocket_url = COALESCE(?6, websocket_url),
            active_broker = COALESCE(?7, active_broker),
            redirect_url = COALESCE(?8, redirect_url),
            updated_at = datetime('now')
         WHERE id = 1",
        params![
            u.http_port,
            u.ws_port,
            u.bind_host,
            u.host_server,
            u.ngrok_allow,
            u.websocket_url,
            u.active_broker,
            u.redirect_url
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_url_matches_web_convention() {
        let c = ServerConfig::default();
        assert_eq!(
            c.redirect_url_for("zerodha"),
            "http://127.0.0.1:5000/zerodha/callback"
        );
        let c = ServerConfig {
            host_server: Some("https://abc.ngrok.app/".into()),
            ngrok_allow: true,
            ..Default::default()
        };
        assert_eq!(
            c.redirect_url_for("fyers"),
            "https://abc.ngrok.app/fyers/callback"
        );
    }

    #[test]
    fn bind_host_validation() {
        assert!(validate_bind_host("127.0.0.1").is_ok());
        assert!(validate_bind_host("0.0.0.0").is_ok());
        assert!(validate_bind_host("192.168.1.5").is_ok());
        assert!(validate_bind_host("evil.example.com").is_err());
    }
}
