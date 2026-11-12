//! The groups of order events a trader routes to each alert channel (web
//! `services/alerts/categories.py`).
//!
//! The /alerts page shows one row per group and one column per channel. A
//! group is the smallest thing a trader decides about: "tell me on Slack
//! when a GTT fires" is a sensible choice, "tell me about gtt.modified but
//! not gtt.placed" is not one anybody makes.
//!
//! Only events that report something that happened are here. Failures and
//! validation errors (`order.failed`, `analyzer.error`, the desktop's
//! `order.unconfirmed`) have never been sent to Telegram or WhatsApp,
//! because a script retrying a bad order turns every chat into a stream of
//! rejections. They stay off every channel.

use crate::events::Topic;

/// One group of order events, as the /alerts page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    /// Stable identifier stored with each channel's settings.
    pub id: &'static str,
    /// What the trader reads.
    pub label: &'static str,
    /// One sentence on what the group covers.
    pub description: &'static str,
    /// The event-bus topics that belong to the group.
    pub topics: &'static [Topic],
}

/// Every group, in the order the page shows them.
pub const CATEGORIES: &[Category] = &[
    Category {
        id: "orders_placed",
        label: "Orders placed",
        description: "New orders, and smart orders that found nothing to change.",
        topics: &[Topic::OrderPlaced, Topic::OrderNoAction],
    },
    Category {
        id: "orders_modified",
        label: "Orders modified",
        description: "A change to the price or quantity of an open order.",
        topics: &[Topic::OrderModified],
    },
    Category {
        id: "orders_cancelled",
        label: "Orders cancelled",
        description: "One order cancelled, or every open order cancelled at once.",
        topics: &[Topic::OrderCancelled, Topic::AllOrdersCancelled],
    },
    Category {
        id: "positions_closed",
        label: "Positions closed",
        description: "One position or every open position squared off.",
        topics: &[Topic::PositionClosed],
    },
    Category {
        id: "batch_orders",
        label: "Basket, split and options orders",
        description: "Orders sent together, with how many went through.",
        topics: &[
            Topic::BasketCompleted,
            Topic::SplitCompleted,
            Topic::OptionsCompleted,
            Topic::MultiOrderCompleted,
        ],
    },
    Category {
        id: "gtt_orders",
        label: "GTT orders",
        description: "GTT orders placed, modified, cancelled, triggered or expired.",
        topics: &[
            Topic::GttPlaced,
            Topic::GttModified,
            Topic::GttCancelled,
            Topic::GttTriggered,
            Topic::GttExpired,
        ],
    },
];

/// Whether `id` names a group.
pub fn is_known(id: &str) -> bool {
    CATEGORIES.iter().any(|c| c.id == id)
}

/// The group an order topic belongs to. A topic in no group raises no
/// alert.
pub fn category_of(topic: Topic) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find(|c| c.topics.contains(&topic))
        .map(|c| c.id)
}

/// The order topics that raise an alert, for the subscribers to register.
pub fn alert_topics() -> Vec<Topic> {
    CATEGORIES
        .iter()
        .flat_map(|c| c.topics.iter().copied())
        .collect()
}

/// Every group id, in page order.
pub fn all_ids() -> impl Iterator<Item = &'static str> {
    CATEGORIES.iter().map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Web test/test_alerts_routing.py::test_the_same_topics_raise_an_alert_as_before
    #[test]
    fn the_same_topics_raise_an_alert_as_before() {
        let previously: BTreeSet<&str> = [
            "order.placed",
            "order.no_action",
            "order.modified",
            "order.cancelled",
            "orders.all_cancelled",
            "position.closed",
            "basket.completed",
            "split.completed",
            "options.completed",
            "multiorder.completed",
            "gtt.placed",
            "gtt.modified",
            "gtt.cancelled",
            "gtt.triggered",
            "gtt.expired",
        ]
        .into_iter()
        .collect();
        let now: BTreeSet<&str> = alert_topics().iter().map(Topic::as_str).collect();
        assert_eq!(now, previously);
        assert_eq!(alert_topics().len(), previously.len(), "no topic twice");
    }

    /// Web test/test_alerts_routing.py::test_failures_and_engine_events_raise_no_alert
    #[test]
    fn failures_and_engine_events_raise_no_alert() {
        for topic in [
            Topic::OrderFailed,
            Topic::OrderModifyFailed,
            Topic::OrderCancelFailed,
            Topic::AnalyzerError,
            Topic::GttFailed,
            Topic::OrderUpdate,
            Topic::SandboxOrderFilled,
            Topic::OrderUnconfirmed,
        ] {
            assert_eq!(category_of(topic), None, "{}", topic.as_str());
        }
    }

    /// Web test/test_alerts_routing.py::test_each_alerting_topic_has_its_group
    #[test]
    fn each_alerting_topic_has_its_group() {
        for (topic, group) in [
            (Topic::OrderPlaced, "orders_placed"),
            (Topic::OrderNoAction, "orders_placed"),
            (Topic::OrderModified, "orders_modified"),
            (Topic::OrderCancelled, "orders_cancelled"),
            (Topic::AllOrdersCancelled, "orders_cancelled"),
            (Topic::PositionClosed, "positions_closed"),
            (Topic::BasketCompleted, "batch_orders"),
            (Topic::SplitCompleted, "batch_orders"),
            (Topic::OptionsCompleted, "batch_orders"),
            (Topic::MultiOrderCompleted, "batch_orders"),
            (Topic::GttTriggered, "gtt_orders"),
        ] {
            assert_eq!(category_of(topic), Some(group), "{}", topic.as_str());
        }
        assert_eq!(
            all_ids().collect::<Vec<_>>(),
            [
                "orders_placed",
                "orders_modified",
                "orders_cancelled",
                "positions_closed",
                "batch_orders",
                "gtt_orders"
            ]
        );
    }
}
