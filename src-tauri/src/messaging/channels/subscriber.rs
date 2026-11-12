//! Order alerts on the event bus (web `subscribers/alert_subscriber.py`,
//! which replaced the Telegram and WhatsApp subscribers).
//!
//! Each channel is its own subscriber of the alert topics, as Telegram and
//! WhatsApp were, so each has its own bounded queue and worker on the bus
//! and a channel that fails or stalls never stops another. A subscriber
//! acts only on a topic that has a group (failures and validation errors
//! have none, as before) and only when the trader switched its channel on
//! and routed that group to it. Telegram and WhatsApp then send through
//! their own services; Slack and Discord queue the post on their lane.

use super::{categories, settings, ChannelName};
use crate::events::{Event, Subscriber, Topic};
use crate::state::AppState;
use std::sync::{Arc, Weak};

/// One channel's order alerts.
pub struct ChannelAlerts {
    ctx: Weak<AppState>,
    channel: ChannelName,
}

impl ChannelAlerts {
    pub fn new(ctx: Weak<AppState>, channel: ChannelName) -> Self {
        Self { ctx, channel }
    }
}

#[async_trait::async_trait]
impl Subscriber for ChannelAlerts {
    fn name(&self) -> &'static str {
        self.channel.name()
    }

    fn topics(&self) -> Vec<Topic> {
        categories::alert_topics()
    }

    async fn handle(&self, event: Arc<Event>) {
        let (Some(ctx), Some(meta)) = (self.ctx.upgrade(), event.meta()) else {
            return;
        };
        let Some(category) = categories::category_of(event.topic()) else {
            return;
        };
        if !settings::receives(&ctx, self.channel, category) {
            return;
        }
        match self.channel {
            ChannelName::Telegram => ctx.messaging.telegram.send_order_alert(&ctx, meta).await,
            ChannelName::WhatsApp => ctx.messaging.whatsapp.send_order_alert(&ctx, meta).await,
            ChannelName::Slack | ChannelName::Discord => {
                if let Some(kind) = self.channel.webhook() {
                    super::webhook::deliver(&ctx, kind, meta);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
