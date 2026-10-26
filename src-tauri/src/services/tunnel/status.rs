//! What the trader sees of the tunnel: one state and one sentence naming
//! the cause and the next step. Never an ngrok error code or a protocol
//! term; those go to the log.

use super::provider::OpenError;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelState {
    Stopped,
    Connecting,
    Online,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TunnelStatus {
    pub state: TunnelState,
    /// The public address, while online.
    pub url: Option<String>,
    pub message: String,
}

/// Why the tunnel is not running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Off {
    /// The switch is off.
    Disabled,
    /// On, but no authtoken is saved.
    NoAuthtoken,
    /// Nobody is signed in: the authtoken cannot be opened (KEY-01).
    SignedOut,
    /// The saved authtoken does not open with this install's key.
    Unreadable,
    /// The settings could not be read.
    SettingsUnreadable,
}

impl TunnelStatus {
    pub fn stopped(why: Off) -> Self {
        let message = match why {
            Off::Disabled => "The tunnel is off.",
            Off::NoAuthtoken => "Add your ngrok authtoken to start the tunnel.",
            Off::SignedOut => "The tunnel starts when you sign in to OpenAlgo.",
            Off::Unreadable => {
                "Your saved ngrok authtoken could not be read. Paste it again and save."
            }
            Off::SettingsUnreadable => {
                "OpenAlgo could not read the tunnel settings. Save them again; if this keeps \
                 happening, restart OpenAlgo."
            }
        };
        Self::new(TunnelState::Stopped, None, message)
    }

    /// Opening a tunnel; `after_loss` when an open one was lost.
    pub fn connecting(after_loss: bool) -> Self {
        let message = if after_loss {
            "The connection to ngrok was lost. Reconnecting."
        } else {
            "Connecting to ngrok."
        };
        Self::new(TunnelState::Connecting, None, message)
    }

    pub fn online(url: &str) -> Self {
        Self {
            state: TunnelState::Online,
            url: Some(url.to_string()),
            message: format!("Online at {}", url),
        }
    }

    /// The tunnel did not open: why, and whether OpenAlgo keeps trying.
    pub fn failed(problem: OpenError, domain: Option<&str>) -> Self {
        let message = match (problem, domain) {
            (OpenError::Authtoken, _) => "ngrok did not accept the authtoken. Copy it again \
                from Your Authtoken in your ngrok dashboard, paste it here and save."
                .to_string(),
            (OpenError::Domain, Some(d)) => format!(
                "Your ngrok account cannot use the domain {}. Check it under Domains in your \
                 ngrok dashboard, or clear the domain to use the address ngrok gives your account.",
                d
            ),
            (OpenError::Domain, None) => "ngrok would not give your account an address. Reserve \
                a domain under Domains in your ngrok dashboard and enter it here."
                .to_string(),
            (OpenError::Account, _) => "ngrok will not start a tunnel for this account. Sign in \
                to your ngrok dashboard to see why (for example, an email still to verify)."
                .to_string(),
            (OpenError::InUse, _) => "Your ngrok tunnel is in use by another ngrok agent, on \
                this or another computer. Stop the other one; OpenAlgo keeps trying."
                .to_string(),
            (OpenError::Unreachable, _) => "Cannot reach ngrok. Check your internet connection \
                and firewall; OpenAlgo keeps trying."
                .to_string(),
            (OpenError::Other, _) => "ngrok did not open the tunnel. OpenAlgo keeps trying; the \
                details are in the log."
                .to_string(),
        };
        Self {
            state: TunnelState::Error,
            url: None,
            message,
        }
    }

    fn new(state: TunnelState, url: Option<String>, message: &str) -> Self {
        Self {
            state,
            url,
            message: message.to_string(),
        }
    }
}
