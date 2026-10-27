//! The broker's streams on Run diagnostics (DIA-01): whether the market
//! data and the order updates are connected, and how long ago the last
//! update came. A session object alone ("Broker session: Connected") says
//! nothing about either.

use crate::state::AppState;
use crate::websocket::{FeedStatus, WebSocketManager};
use serde_json::{json, Value};
use std::time::Duration;

/// The stream checks for the signed-in broker session: market data always,
/// order updates when this broker sends them over a socket of their own
/// (others relay them, or the order book is read from the broker).
pub(super) fn checks(ctx: &AppState) -> Vec<Value> {
    if ctx.get_broker_session().is_none() {
        return Vec::new();
    }
    let mut out = vec![check("Market data from the broker", &ctx.websocket, true)];
    let orders = &ctx.runtime.order_ws;
    if orders.is_running() {
        out.push(check("Order updates from the broker", orders, false));
    }
    out
}

fn check(name: &str, feed: &WebSocketManager, market: bool) -> Value {
    let instruments = market.then(|| feed.instrument_count());
    let (ok, mut detail) = describe(&feed.status(), feed.stats().last_event_age(), instruments);
    // What arrived since it connected and what its readers skipped
    // (NEW-REL-05).
    let since = crate::services::feed_losses::sentence(&feed.stats().since_connect());
    if ok && !since.is_empty() {
        detail = format!("{} {}", detail, since);
    }
    json!({"name": name, "ok": ok, "ms": Value::Null, "detail": detail})
}

/// Whether the stream is up, and what a trader reads about it. The
/// broker's own refusal text is never shown (it goes to the log where it
/// arrived).
pub(super) fn describe(
    status: &FeedStatus,
    age: Option<Duration>,
    instruments: Option<usize>,
) -> (bool, String) {
    match status {
        FeedStatus::Connected { .. } => {
            let last = match age {
                Some(a) => format!("last update {} ago", ago(a)),
                None => "no update yet".to_string(),
            };
            let detail = match instruments {
                Some(0) => format!("Connected, no instruments subscribed; {}.", last),
                Some(1) => format!("Connected, 1 instrument subscribed; {}.", last),
                Some(n) => format!("Connected, {} instruments subscribed; {}.", n, last),
                None => format!("Connected; {}.", last),
            };
            (true, detail)
        }
        FeedStatus::Connecting { attempt, .. } | FeedStatus::Reconnecting { attempt, .. } => (
            false,
            format!(
                "Reconnecting to the broker (attempt {}). Prices and order updates resume once it connects.",
                attempt
            ),
        ),
        FeedStatus::AuthFailed { .. } => (
            false,
            "The broker refused the session. Sign in to your broker again from the Broker page."
                .to_string(),
        ),
        FeedStatus::Disconnected => (
            false,
            "Not connected. If this lasts, sign in to your broker again from the Broker page."
                .to_string(),
        ),
    }
}

/// "12 s", "3 min", "2 h".
pub(super) fn ago(a: Duration) -> String {
    let s = a.as_secs();
    if s < 120 {
        format!("{} s", s)
    } else if s < 2 * 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h", s / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> FeedStatus {
        FeedStatus::Connected {
            broker: "zerodha".into(),
        }
    }

    #[test]
    fn a_connected_stream_names_its_last_update() {
        assert_eq!(
            describe(&connected(), Some(Duration::from_secs(3)), Some(4)),
            (
                true,
                "Connected, 4 instruments subscribed; last update 3 s ago.".to_string()
            )
        );
        assert_eq!(
            describe(&connected(), None, Some(0)),
            (
                true,
                "Connected, no instruments subscribed; no update yet.".to_string()
            )
        );
        assert_eq!(
            describe(&connected(), Some(Duration::from_secs(600)), None),
            (true, "Connected; last update 10 min ago.".to_string())
        );
        assert_eq!(ago(Duration::from_secs(3 * 3600)), "3 h");
    }

    #[test]
    fn a_stream_that_is_not_up_says_what_to_do_and_never_the_brokers_text() {
        let refused = FeedStatus::AuthFailed {
            broker: "zerodha".into(),
            message: "TokenException: access_token=abc123 invalid".into(),
        };
        let (ok, detail) = describe(&refused, None, Some(1));
        assert!(!ok);
        assert!(detail.contains("Sign in to your broker again"));
        assert!(!detail.contains("abc123") && !detail.contains("Token"));
        let (ok, detail) = describe(
            &FeedStatus::Reconnecting {
                broker: "zerodha".into(),
                attempt: 3,
                delay_ms: 2000,
            },
            Some(Duration::from_secs(40)),
            Some(1),
        );
        assert!(!ok);
        assert!(detail.starts_with("Reconnecting to the broker (attempt 3)."));
        assert!(!describe(&FeedStatus::Disconnected, None, None).0);
    }
}
