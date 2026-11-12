//! Alerts: telling a trader, on the channels they chose, what their orders
//! did (web `services/alerts/`, marketcalls/openalgo#2435 and #2440).
//!
//! How an alert travels, all on the event bus:
//!
//! 1. An order service publishes an order event (`order.placed` and so on).
//! 2. Each channel is its own bus subscriber (`subscriber`), with its own
//!    bounded queue and worker, so a slow Telegram never holds up Slack. It
//!    maps the event to an alert group (`categories`) and acts only when
//!    the trader switched the channel on and routed that group to it
//!    (`settings`). The web publishes one `alert.raised` event and fans it
//!    out from one callback because its bus shares one cap among all
//!    callbacks; the desktop's bus gives each subscriber its own queue, so
//!    the extra hop would buy nothing.
//! 3. Telegram and WhatsApp send through their own services (`bots`), as
//!    before. Slack and Discord queue the post on their own bounded lane
//!    (`webhook`, `lane`) and never wait on the network in the subscriber.
//!    Slack, Discord and WhatsApp share one formatter (`formatting`).
//!
//! The /alerts page (`server::routes::alert_channels`) reads and changes
//! the same settings; the `/api/v1/slack/notify` and `/discord/notify`
//! endpoints and the Flow nodes post through `notify`, under the same rules.
//!
//! What lives where:
//!
//! | File | What |
//! | --- | --- |
//! | `categories.rs` | the six event groups and their topics |
//! | `store.rs` | the `alert_channels` table (migration 102), the sealed webhook |
//! | `settings.rs` | each channel's switch and groups, cached, writes serialised |
//! | `formatting.rs` | the channel-neutral alert text |
//! | `bots.rs` | Telegram and WhatsApp: status, test message, WhatsApp text |
//! | `webhook/` | Slack and Discord: the URL rules, the post, its answers |
//! | `lane.rs` | each webhook channel's bounded queue of posts |
//! | `notify.rs` | the rules for a caller's own text (REST, Flow) |
//! | `subscriber.rs` | the bus subscribers |

pub mod bots;
pub mod categories;
pub mod formatting;
pub mod lane;
pub mod notify;
pub mod settings;
pub mod store;
pub mod subscriber;
pub mod webhook;

use crate::state::AppState;
use std::sync::Arc;

/// The text of every test message, whichever channel sends it.
pub const TEST_MESSAGE: &str =
    "OpenAlgo test alert. If you can read this, alerts from your OpenAlgo reach this channel.";

/// Every alert channel, in the order the /alerts page shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelName {
    Telegram,
    WhatsApp,
    Slack,
    Discord,
}

impl ChannelName {
    pub const ALL: [ChannelName; 4] = [
        ChannelName::Telegram,
        ChannelName::WhatsApp,
        ChannelName::Slack,
        ChannelName::Discord,
    ];

    /// Stable identifier, used in URLs and stored settings.
    pub fn name(self) -> &'static str {
        match self {
            ChannelName::Telegram => "telegram",
            ChannelName::WhatsApp => "whatsapp",
            ChannelName::Slack => "slack",
            ChannelName::Discord => "discord",
        }
    }

    /// The name a trader reads.
    pub fn label(self) -> &'static str {
        match self {
            ChannelName::Telegram => "Telegram",
            ChannelName::WhatsApp => "WhatsApp",
            ChannelName::Slack => "Slack",
            ChannelName::Discord => "Discord",
        }
    }

    /// The page where the channel is set up.
    pub fn setup_path(self) -> &'static str {
        match self {
            ChannelName::Telegram => "/telegram",
            ChannelName::WhatsApp => "/whatsapp",
            ChannelName::Slack => "/slack",
            ChannelName::Discord => "/discord",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The webhook provider, for a channel that posts to a saved URL.
    pub fn webhook(self) -> Option<webhook::WebhookKind> {
        match self {
            ChannelName::Slack => Some(webhook::WebhookKind::Slack),
            ChannelName::Discord => Some(webhook::WebhookKind::Discord),
            ChannelName::Telegram | ChannelName::WhatsApp => None,
        }
    }

    pub fn uses_webhook(self) -> bool {
        self.webhook().is_some()
    }

    /// Whether a channel nobody has changed is on. Telegram and WhatsApp
    /// are, so alerts keep flowing exactly as they did before the /alerts
    /// page existed; a webhook channel starts off and is switched on when
    /// its webhook is saved.
    pub fn enabled_by_default(self) -> bool {
        !self.uses_webhook()
    }
}

/// How a channel stands, for the /alerts page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatus {
    /// Set up enough to send at all: a bot token saved, a device paired, a
    /// webhook saved.
    pub configured: bool,
    /// Would deliver an alert raised right now.
    pub ready: bool,
    /// One sentence for the trader saying why, and what to do if not.
    pub detail: String,
}

/// What came of one send the trader is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryResult {
    /// Whether the channel accepted the message.
    pub delivered: bool,
    /// One sentence for the trader. On failure it names the cause and the
    /// next step, never a status code.
    pub message: String,
}

impl DeliveryResult {
    pub fn sent(message: impl Into<String>) -> Self {
        Self {
            delivered: true,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            delivered: false,
            message: message.into(),
        }
    }
}

/// The alert channels' own state on the app context: the settings cache
/// and its per-channel writer locks, Slack's and Discord's lanes and their
/// HTTP client.
#[derive(Default)]
pub struct AlertChannels {
    pub(crate) settings: settings::SettingsState,
    pub(crate) lanes: lane::Lanes,
    pub(crate) http: webhook::WebhookHttp,
}

/// How a channel stands right now. An error is a database that could not
/// be read.
pub fn status(ctx: &AppState, channel: ChannelName) -> crate::error::Result<ChannelStatus> {
    match channel.webhook() {
        Some(kind) => webhook::status(ctx, kind),
        None if channel == ChannelName::Telegram => bots::telegram_status(ctx),
        None => Ok(bots::whatsapp_status(ctx)),
    }
}

/// Send a test message now and say whether it arrived. An error is a
/// database that could not be read.
pub async fn send_test(
    ctx: &Arc<AppState>,
    channel: ChannelName,
) -> crate::error::Result<DeliveryResult> {
    match channel.webhook() {
        Some(kind) => webhook::send_test(ctx, kind).await,
        None if channel == ChannelName::Telegram => bots::telegram_test(ctx).await,
        None => bots::whatsapp_test(ctx).await,
    }
}

#[cfg(test)]
pub(crate) mod testkit;
