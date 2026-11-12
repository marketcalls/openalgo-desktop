//! Posting to a saved Slack or Discord webhook, and what each outcome tells
//! the trader (web `WebhookChannel.post`, `send_text`, `post_to_saved`,
//! `status`, `send_test`).

use super::{WebhookKind, WebhookUrl};
use crate::events::OrderMeta;
use crate::flow::contract::env::{HttpCall, OutboundHttp};
use crate::flow::nodes::http::{client, guard};
use crate::messaging::alerts;
use crate::messaging::channels::{
    formatting, lane, settings, store, ChannelStatus, DeliveryResult, TEST_MESSAGE,
};
use crate::state::AppState;
use parking_lot::RwLock;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// How long one post may take. Both providers answer in well under a
/// second; this only bounds a stalled connection.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The one HTTP client every Slack and Discord post goes through, built on
/// first use: Flow's guarded client (public addresses only, the vetted
/// address pinned, no redirect, no proxy). Tests put a fake in its place.
#[derive(Default)]
pub struct WebhookHttp(RwLock<Option<Arc<dyn OutboundHttp>>>);

impl WebhookHttp {
    fn get(&self) -> Option<Arc<dyn OutboundHttp>> {
        if let Some(http) = self.0.read().clone() {
            return Some(http);
        }
        let mut slot = self.0.write();
        if slot.is_none() {
            match client::GuardedHttp::new() {
                Ok(http) => *slot = Some(Arc::new(http)),
                Err(e) => tracing::error!(
                    "The Slack and Discord HTTP client could not be built: {}",
                    e
                ),
            }
        }
        slot.clone()
    }

    #[cfg(test)]
    pub(crate) fn set(&self, http: Arc<dyn OutboundHttp>) {
        *self.0.write() = Some(http);
    }
}

/// Where the post may connect: a public address of the URL's host, or the
/// sentence for the trader.
async fn destination(http: &dyn OutboundHttp, url: &WebhookUrl) -> Result<SocketAddr, String> {
    let label = url.kind().label();
    let cannot_reach = format!(
        "OpenAlgo could not reach {}. Check this computer's internet connection.",
        label
    );
    let host = ::url::Url::parse(url.expose())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .ok_or_else(|| {
            format!(
                "The saved {label} webhook URL cannot be used. Save it again on the {label} page."
            )
        })?;
    let addresses: Vec<IpAddr> = match http.resolve(&host).await {
        Ok(found) if !found.is_empty() => found,
        _ => {
            tracing::warn!(
                "{} alert not delivered: {} could not be looked up",
                label,
                host
            );
            return Err(cannot_reach);
        }
    };
    if let Some(private) = addresses.iter().find(|a| guard::is_refused(**a)) {
        tracing::warn!(
            "{} alert not delivered: {} resolves to {}, which is not a public address",
            label,
            host,
            private
        );
        return Err(format!(
            "OpenAlgo did not post to {}: this computer looks up its address as one on a private network. Check this computer's DNS settings or hosts file.",
            label
        ));
    }
    let ip = addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .ok_or(cannot_reach)?;
    Ok(SocketAddr::new(ip, 443))
}

/// Post one message and report what happened. Never fails: every outcome
/// is a sentence for the trader, and the log names the cause without the
/// URL.
pub async fn post(ctx: &AppState, url: &WebhookUrl, text: &str) -> DeliveryResult {
    let kind = url.kind();
    let label = kind.label();
    let Some(http) = ctx.messaging.channels.http.get() else {
        return DeliveryResult::failed(format!("The {} message could not be sent.", label));
    };
    let connect_to = match destination(http.as_ref(), url).await {
        Ok(at) => at,
        Err(sentence) => return DeliveryResult::failed(sentence),
    };
    let body = match serde_json::to_vec(&kind.payload(text)) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("{} alert could not be sent: {}", label, e);
            return DeliveryResult::failed(format!("The {} message could not be sent.", label));
        }
    };
    let call = HttpCall {
        method: "POST".into(),
        url: url.expose().to_string(),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: Some(body),
        timeout: TIMEOUT,
        connect_to,
    };
    let reply = match http.send(call).await {
        Ok(reply) => reply,
        Err(reason) if reason == client::TIMED_OUT => {
            tracing::warn!(
                "{} alert not delivered: no answer within {}s",
                label,
                TIMEOUT.as_secs()
            );
            return DeliveryResult::failed(format!(
                "{} did not answer in time. Try again shortly.",
                label
            ));
        }
        Err(_) => {
            // The transport's text can carry the URL, so it is not logged.
            tracing::warn!("{} alert not delivered: the connection failed", label);
            return DeliveryResult::failed(format!(
                "OpenAlgo could not reach {}. Check this computer's internet connection.",
                label
            ));
        }
    };
    if kind.accepted(reply.status) {
        return DeliveryResult::sent(format!("Message sent to {}.", label));
    }
    let said: String = String::from_utf8_lossy(&reply.body)
        .chars()
        .take(200)
        .collect();
    tracing::warn!(
        "{} refused an alert: HTTP {} {:?}",
        label,
        reply.status,
        super::redact_urls(&said)
    );
    DeliveryResult::failed(kind.refusal(reply.status))
}

/// The saved webhook URL, if there is one and it is still this channel's.
/// An error is a database that could not be read.
pub fn saved_url(ctx: &AppState, kind: WebhookKind) -> crate::error::Result<Option<WebhookUrl>> {
    let conn = ctx.sqlite.conn()?;
    let Some(url) = store::read_webhook_url(&conn, &ctx.security, kind.channel().name())? else {
        return Ok(None);
    };
    let stored = serde_json::Value::String(url.expose().to_string());
    match kind.validate_url(Some(&stored)) {
        Ok(valid) => Ok(Some(valid)),
        Err(_) => {
            tracing::warn!(
                "The saved {} webhook is not a {} URL; ignoring it",
                kind.label(),
                kind.label()
            );
            Ok(None)
        }
    }
}

/// How the channel stands, for the /alerts page.
pub fn status(ctx: &AppState, kind: WebhookKind) -> crate::error::Result<ChannelStatus> {
    let label = kind.label();
    let stored = {
        let conn = ctx.sqlite.conn()?;
        store::load(&conn, kind.channel().name())?
    };
    if !stored.is_some_and(|s| s.webhook_saved) {
        return Ok(ChannelStatus {
            configured: false,
            ready: false,
            detail: format!("No webhook is saved yet. Add one on the {} page.", label),
        });
    }
    if saved_url(ctx, kind)?.is_none() {
        // Saved, but it no longer opens or is not this provider's URL.
        return Ok(ChannelStatus {
            configured: false,
            ready: false,
            detail: format!(
                "The saved webhook can no longer be used. Save it again on the {} page.",
                label
            ),
        });
    }
    Ok(ChannelStatus {
        configured: true,
        ready: true,
        detail: format!("Webhook saved. Alerts post to your {} channel.", label),
    })
}

/// Send the test message now.
pub async fn send_test(
    ctx: &Arc<AppState>,
    kind: WebhookKind,
) -> crate::error::Result<DeliveryResult> {
    let Some(url) = saved_url(ctx, kind)? else {
        return Ok(DeliveryResult::failed(format!(
            "Save a {} webhook first, then send a test.",
            kind.label()
        )));
    };
    Ok(post(ctx, &url, &kind.fit(&kind.render_text(TEST_MESSAGE))).await)
}

/// Post text an API caller wrote, now (`wait`) or on the channel's lane.
/// The text was already checked against the provider's limit.
pub async fn send_text(
    ctx: &Arc<AppState>,
    kind: WebhookKind,
    text: &str,
    wait: bool,
) -> crate::error::Result<DeliveryResult> {
    let label = kind.label();
    let Some(url) = saved_url(ctx, kind)? else {
        return Ok(DeliveryResult::failed(format!(
            "No {label} webhook is saved. Add one on the {label} page in OpenAlgo."
        )));
    };
    let rendered = kind.render_text(text);
    if wait {
        return Ok(post(ctx, &url, &rendered).await);
    }
    if queue(ctx, kind, rendered) {
        return Ok(DeliveryResult::sent("Notification queued."));
    }
    Ok(DeliveryResult::failed(format!(
        "Too many {} messages are waiting to be sent. Try again shortly.",
        label
    )))
}

/// Queue a post of `text` to whatever webhook is saved when it runs.
fn queue(ctx: &Arc<AppState>, kind: WebhookKind, text: String) -> bool {
    lane::submit(ctx, kind, move |ctx| async move {
        post_to_saved(&ctx, kind, &text).await;
    })
}

/// Post to the webhook saved now, if the channel is still on: what every
/// queued post runs. Both are read when it runs, not when it was queued,
/// so a channel switched off, or a webhook removed or replaced, while the
/// post waited is never posted to.
pub async fn post_to_saved(ctx: &AppState, kind: WebhookKind, text: &str) -> DeliveryResult {
    let label = kind.label();
    if !settings::settings_for(ctx, kind.channel()).enabled {
        tracing::info!("{} alert not sent: {} was switched off", label, label);
        return DeliveryResult::failed(format!("{} alerts are turned off.", label));
    }
    let url = match saved_url(ctx, kind) {
        Ok(Some(url)) => url,
        Ok(None) => {
            tracing::info!("{} alert not sent: no usable webhook is saved", label);
            return DeliveryResult::failed(format!("No {} webhook is saved.", label));
        }
        Err(e) => {
            tracing::error!(
                "{} alert not sent: the saved webhook could not be read: {}",
                label,
                e
            );
            return DeliveryResult::failed(format!("The {} message could not be sent.", label));
        }
    };
    post(ctx, &url, text).await
}

/// One order alert, queued on the channel's lane; never waits on the
/// network. A full lane drops it (logged by the lane).
pub fn deliver(ctx: &Arc<AppState>, kind: WebhookKind, meta: &OrderMeta) {
    let message = formatting::build_message(
        &meta.api_type,
        &meta.request_data,
        &meta.response_data,
        alerts::is_analyze(meta),
        &alerts::local_time(ctx),
    );
    queue(ctx, kind, kind.fit(&kind.render(&message)));
}
