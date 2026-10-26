//! How each broker feed's connection has held up, for health
//! (`/health/api/current`, `market_data.broker_feeds[].connection`) and the
//! Health page's feed connections card: the state now, the connections
//! made, the ones that ended and were made again (and of those, the ones
//! where the broker went quiet), and why and when the last one ended. The
//! counts run per broker (`websocket::FeedStats`): from OpenAlgo's start,
//! or from another broker's feed being replaced by this one.
//!
//! A trader who sees prices lag reads here whether the broker's feed keeps
//! dropping or going quiet (the web has no such view; its feeds log it).

use crate::websocket::{Connection, FeedStatus, WebSocketManager};
use serde_json::{json, Value};

fn state(status: &FeedStatus) -> &'static str {
    match status {
        FeedStatus::Disconnected => "disconnected",
        FeedStatus::Connecting { .. } => "connecting",
        FeedStatus::Connected { .. } => "connected",
        FeedStatus::Reconnecting { .. } => "reconnecting",
        FeedStatus::AuthFailed { .. } => "auth_failed",
    }
}

fn block(status: &FeedStatus, c: &Connection) -> Value {
    json!({
        "broker": c.broker,
        "state": state(status),
        "connects": c.connects,
        "disconnects": c.disconnects,
        "stalls": c.stalls,
        "last_reconnect": c.last_drop.map(|d| json!({
            "reason": d.cause.name(),
            "at": d.at.to_rfc3339(),
        })),
    })
}

/// One feed's `connection` entry.
pub fn json(feed: &WebSocketManager) -> Value {
    block(&feed.status(), &feed.stats().connection())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websocket::DropCause;
    use chrono::TimeZone;

    #[test]
    fn the_entry_names_the_state_counts_and_last_reconnect() {
        let c = Connection {
            broker: Some("kotak"),
            connects: 4,
            disconnects: 3,
            stalls: 1,
            last_drop: Some(crate::websocket::LastDrop {
                cause: DropCause::HeartbeatUnanswered,
                at: chrono::Utc
                    .with_ymd_and_hms(2026, 10, 26, 4, 58, 0)
                    .unwrap(),
            }),
        };
        let status = FeedStatus::Reconnecting {
            broker: "kotak".into(),
            attempt: 1,
            delay_ms: 500,
        };
        assert_eq!(
            block(&status, &c),
            json!({
                "broker": "kotak",
                "state": "reconnecting",
                "connects": 4,
                "disconnects": 3,
                "stalls": 1,
                "last_reconnect": {
                    "reason": "heartbeat_unanswered",
                    "at": "2026-10-26T04:58:00+00:00",
                },
            })
        );
        let idle = json(&WebSocketManager::new());
        assert_eq!(idle["state"], "disconnected");
        assert_eq!(idle["broker"], Value::Null);
        assert_eq!(idle["last_reconnect"], Value::Null);
    }
}
