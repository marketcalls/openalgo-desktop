//! Alert channels that post to an incoming webhook: Slack and Discord (web
//! `services/alerts/channels/webhook.py`, `slack.py`, `discord.py`).
//!
//! A trader creates an incoming webhook in Slack or Discord and pastes its
//! URL on the channel's page; OpenAlgo then posts each alert to it. No bot
//! and no token to keep alive.
//!
//! **The URL is the credential.** Anybody holding it can post into that
//! channel, so it is treated like a password:
//!
//! - It is accepted only for the provider's own webhook host and path
//!   ([`WebhookKind::validate_url`]), the host matched exactly, so a
//!   lookalike such as `hooks.slack.com.example.net` is refused. The saved
//!   URL is checked again before every post.
//! - The post connects only to a public address of that host, never to a
//!   second lookup (`send`: Flow's httpRequest guard and pinned client), so
//!   a hosts file or DNS answer pointing the provider's name at this
//!   computer or the home network is refused (a desktop addition: the web
//!   relies on the host check alone). Redirects are not followed.
//! - It is stored sealed (`store`), never returned to a page, and never
//!   logged: errors name the provider and the next step, never the URL, and
//!   the Diagnostics error log masks one a library might print
//!   ([`redact_urls`], the web's `utils/url_redaction.py`).
//!
//! **Sends never block an order.** An alert only queues the post on the
//! channel's own lane (`lane`).

mod send;

pub use send::{
    deliver, post, post_to_saved, saved_url, send_test, send_text, status, WebhookHttp,
};

use super::formatting::{AlertMessage, SEPARATOR};
use super::ChannelName;
use crate::security::Secret;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

/// Longest URL accepted. Real webhook URLs are around 120 characters.
pub const MAX_URL_LENGTH: usize = 500;

static SLACK_PATH: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^/services/[A-Za-z0-9]+/[A-Za-z0-9]+/[A-Za-z0-9]+/?$").ok());
static DISCORD_PATH: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^/api(?:/v\d{1,2})?/webhooks/\d{1,20}/[A-Za-z0-9_-]+/?$").ok());
/// A Discord webhook into a forum or thread names the thread.
static DISCORD_QUERY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^thread_id=\d{1,20}$").ok());
/// Characters Discord markdown gives a meaning to.
static DISCORD_MARKDOWN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"([\\*_~`|>#\[\]()-])").ok());

fn full_match(re: &LazyLock<Option<Regex>>, text: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(text))
}

/// A webhook provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebhookKind {
    Slack,
    Discord,
}

/// A URL that is not this channel's webhook. The sentence is for the trader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidWebhookUrl(pub String);

/// A URL that passed [`WebhookKind::validate_url`]. Its `Debug` never
/// prints it.
#[derive(Clone)]
pub struct WebhookUrl {
    kind: WebhookKind,
    url: Secret,
}

impl std::fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WebhookUrl({:?}, <redacted>)", self.kind)
    }
}

impl WebhookUrl {
    pub fn kind(&self) -> WebhookKind {
        self.kind
    }

    pub fn expose(&self) -> &str {
        self.url.expose()
    }
}

impl WebhookKind {
    pub fn channel(self) -> ChannelName {
        match self {
            WebhookKind::Slack => ChannelName::Slack,
            WebhookKind::Discord => ChannelName::Discord,
        }
    }

    pub fn label(self) -> &'static str {
        self.channel().label()
    }

    /// The exact host names the webhook may live on.
    fn hosts(self) -> &'static [&'static str] {
        match self {
            WebhookKind::Slack => &["hooks.slack.com"],
            WebhookKind::Discord => &[
                "discord.com",
                "discordapp.com",
                "ptb.discord.com",
                "canary.discord.com",
            ],
        }
    }

    /// The longest message the provider takes. Slack accepts 40,000 but
    /// cuts long messages in its client; an alert is never near 4,000.
    pub fn max_length(self) -> usize {
        match self {
            WebhookKind::Slack => 4000,
            WebhookKind::Discord => 2000,
        }
    }

    /// How a valid URL begins, for error messages.
    pub fn url_prefix(self) -> &'static str {
        match self {
            WebhookKind::Slack => "https://hooks.slack.com/services/",
            WebhookKind::Discord => "https://discord.com/api/webhooks/",
        }
    }

    fn not_ours(self) -> InvalidWebhookUrl {
        InvalidWebhookUrl(format!(
            "That is not a {} webhook URL. It should begin with {}",
            self.label(),
            self.url_prefix()
        ))
    }

    /// `url` if it is this channel's webhook (the web's `validate_url`,
    /// which reads it with Python's `urlsplit`).
    pub fn validate_url(self, url: Option<&Value>) -> Result<WebhookUrl, InvalidWebhookUrl> {
        let text = url.and_then(Value::as_str).map(str::trim).unwrap_or("");
        if text.is_empty() {
            return Err(InvalidWebhookUrl(format!(
                "Paste the {} webhook URL.",
                self.label()
            )));
        }
        if text.chars().count() > MAX_URL_LENGTH || text.chars().any(unsafe_char) {
            return Err(self.not_ours());
        }
        if !self.is_ours(text) {
            return Err(self.not_ours());
        }
        Ok(WebhookUrl {
            kind: self,
            url: Secret::new(text),
        })
    }

    fn is_ours(self, url: &str) -> bool {
        let Some(parts) = Parts::split(url) else {
            return false;
        };
        let path_ok = match self {
            WebhookKind::Slack => full_match(&SLACK_PATH, parts.path),
            WebhookKind::Discord => full_match(&DISCORD_PATH, parts.path),
        };
        let query_ok = match (self, parts.query) {
            (_, None) => true,
            (_, Some("")) => true,
            (WebhookKind::Discord, Some(q)) => full_match(&DISCORD_QUERY, q),
            (WebhookKind::Slack, Some(_)) => false,
        };
        parts.scheme.eq_ignore_ascii_case("https")
            && self.hosts().contains(&parts.hostname.as_str())
            && !parts.has_userinfo
            && matches!(parts.port, Port::None | Port::Number(443))
            && parts.fragment.is_none_or(str::is_empty)
            && path_ok
            && query_ok
    }

    /// An order alert as this provider's text, every value escaped.
    pub fn render(self, message: &AlertMessage) -> String {
        let (bold, escape): (&str, fn(&str) -> String) = match self {
            WebhookKind::Slack => ("*", escape_slack),
            WebhookKind::Discord => ("**", escape_discord),
        };
        let mut parts = vec![format!("{bold}{}{bold}", escape(message.title))];
        if let Some(strategy) = &message.strategy {
            parts.push(format!("Strategy: {}", escape(strategy)));
        }
        if let Some(banner) = message.banner {
            parts.push(format!("{bold}{}{bold}", escape(banner)));
            parts.push(SEPARATOR.to_string());
        }
        parts.extend(message.lines.iter().map(|l| escape(l)));
        parts.join("\n")
    }

    /// Text an API caller wrote, made safe to post. Slack's control
    /// characters are escaped; Discord's markdown is kept (the caller wrote
    /// it on purpose) and its mentions are switched off in the payload.
    pub fn render_text(self, text: &str) -> String {
        match self {
            WebhookKind::Slack => escape_slack(text),
            WebhookKind::Discord => text.to_string(),
        }
    }

    /// The JSON body that posts `text`. A Discord post resolves no
    /// mention, so nothing in it can ping `@everyone`, a user or a role.
    pub fn payload(self, text: &str) -> Value {
        match self {
            WebhookKind::Slack => json!({"text": text}),
            WebhookKind::Discord => json!({"content": text, "allowed_mentions": {"parse": []}}),
        }
    }

    /// Whether the provider took the message.
    pub fn accepted(self, status: u16) -> bool {
        match self {
            WebhookKind::Slack => status == 200,
            WebhookKind::Discord => status == 200 || status == 204,
        }
    }

    /// What a refusal means for the trader.
    pub fn refusal(self, status: u16) -> String {
        let label = self.label();
        match status {
            401 | 403 | 404 | 410 => format!(
                "{label} no longer accepts this webhook. It may have been deleted, or its channel archived. Create a new webhook and save it on the {label} page."
            ),
            429 => format!(
                "{label} is limiting how fast messages are posted. Try again in a minute."
            ),
            s if s >= 500 => format!("{label} is having trouble right now. Try again later."),
            _ => format!("{label} refused the message. The OpenAlgo log has the details."),
        }
    }

    /// An alert cut to the provider's limit. Only alerts and Flow's
    /// messages ever reach it; an API caller's too-long text is refused.
    pub fn fit(self, text: &str) -> String {
        let max = self.max_length();
        if text.chars().count() <= max {
            return text.to_string();
        }
        let mut cut: String = text.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    }
}

/// A character no webhook URL contains: whitespace, a backslash, or a
/// control character.
fn unsafe_char(c: char) -> bool {
    c.is_whitespace() || c == '\\' || (c as u32) < 0x20 || c as u32 == 0x7F
}

/// Escape Slack's three control characters (`&` first, or it escapes the
/// others), so `<!channel>` and `<url|text>` show as text.
pub fn escape_slack(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `text` shown literally in Discord markdown.
pub fn escape_discord(text: &str) -> String {
    match DISCORD_MARKDOWN.as_ref() {
        Some(re) => re.replace_all(text, r"\$1").into_owned(),
        None => text.to_string(),
    }
}

/// A URL's port as Python's `urlsplit(...).port` reads it.
#[derive(Debug, PartialEq, Eq)]
enum Port {
    None,
    Number(u16),
    /// Not a number from 0 to 65535: Python raises.
    Invalid,
}

/// The parts of a URL, split the way Python's `urlsplit` splits a
/// `scheme://netloc/path?query#fragment` URL.
struct Parts<'a> {
    scheme: &'a str,
    hostname: String,
    has_userinfo: bool,
    port: Port,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl<'a> Parts<'a> {
    fn split(url: &'a str) -> Option<Parts<'a>> {
        let (scheme, rest) = url.split_once("://")?;
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, Some(f)),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        let (netloc, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (userinfo, host_port) = match netloc.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, netloc),
        };
        if host_port.starts_with('[') {
            // An IPv6 literal is never a provider's host.
            return None;
        }
        // Python 3.12 takes ASCII digits only (no sign), 0 to 65535.
        let (host, port) = match host_port.split_once(':') {
            None => (host_port, Port::None),
            Some((h, "")) => (h, Port::None),
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => {
                (h, p.parse::<u16>().map_or(Port::Invalid, Port::Number))
            }
            Some((h, _)) => (h, Port::Invalid),
        };
        Some(Parts {
            scheme,
            hostname: host.to_ascii_lowercase(),
            has_userinfo: userinfo.is_some(),
            port,
            path,
            query,
            fragment,
        })
    }
}

/// Text with every Slack or Discord webhook credential masked (the web's
/// `_ALERT_WEBHOOK` rule in `utils/url_redaction.py`): the whole path after
/// the fixed prefix is the credential. OpenAlgo never logs one on purpose;
/// this catches one a library puts in an error.
pub fn redact_urls(text: &str) -> String {
    static WEBHOOK: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(
            r"(?i)(?P<prefix>hooks\.slack\.com(?::\d+)?/services/|(?:ptb\.|canary\.)?discord(?:app)?\.com(?::\d+)?/api(?:/v\d{1,2})?/webhooks/)[^\s?#'\x22<>]+",
        )
        .ok()
    });
    match WEBHOOK.as_ref() {
        Some(re) if re.is_match(text) => re.replace_all(text, "${prefix}<redacted>").into_owned(),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests;
