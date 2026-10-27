//! The Profile page's Server Status (docs-35): each part of the running app
//! with its real state, read from what the app already tracks. The web's
//! card listed its Flask app, its WebSocket proxy and its ZeroMQ bus, each
//! always "Running"; the desktop has no Flask or ZeroMQ, and a part that is
//! down says so with the fix.
//!
//! ```text
//! App and API             the HTTP listener (`AppState::server_status`)
//! Market data server      the feed listener for SDK clients (`feed_status`)
//! Broker market data      the broker's price stream (`AppState::websocket`)
//! Broker order updates    its order socket, for brokers that have one
//! Order checks            the order reconciler, and whether a gap is open
//! Health checks           the latest health sample (`health_service`)
//! ```

use super::streams;
use crate::services::health_service as health;
use crate::state::{AppState, ServerStatus};
use crate::websocket::{FeedStatus, WebSocketManager};
use serde::Serialize;

/// How a part is doing; picks the badge colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    /// Working.
    Ok,
    /// On its way (starting, reconnecting) or working with warnings.
    Waiting,
    /// Not working; the detail says why and what to do.
    Problem,
    /// Not in use: no broker is signed in.
    Off,
}

/// One part of the running app, as the Server Status card shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Component {
    pub name: &'static str,
    /// Where a listener listens (`host:port`).
    pub address: Option<String>,
    pub state: State,
    /// The badge text.
    pub label: &'static str,
    /// What the trader reads under it: the cause and the fix for a problem.
    pub detail: Option<String>,
}

/// Every part, in the order the card shows them.
pub(crate) fn server_components(ctx: &AppState) -> Vec<Component> {
    let cfg = ctx.server_config();
    let mut out = vec![
        listener(
            "App and API",
            (&cfg.bind_host, cfg.http_port),
            &ctx.server_status.read(),
        ),
        listener(
            "Market data server",
            (&cfg.bind_host, cfg.ws_port),
            &ctx.feed_status.read(),
        ),
    ];
    out.extend(broker_streams(ctx));
    out.push(order_checks(
        ctx.reconciler.is_running(),
        health::order_facts_failing(ctx),
    ));
    out.push(health_checks(&health::overall(
        &health::latest_sample(ctx),
        ctx.now(),
    )));
    out
}

/// A listener's state. `(host, port)` is the configured address, shown
/// while the listener is not running there.
pub(crate) fn listener(
    name: &'static str,
    (host, port): (&str, u16),
    st: &ServerStatus,
) -> Component {
    let configured = format!("{}:{}", host, port);
    let (address, state, label, detail) = match st {
        ServerStatus::Running { host, port } => {
            (format!("{}:{}", host, port), State::Ok, "Running", None)
        }
        ServerStatus::Starting => (configured, State::Waiting, "Starting", None),
        ServerStatus::PortInUse { port, message } => (
            format!("{}:{}", host, port),
            State::Problem,
            "Port in use",
            Some(message.clone()),
        ),
        ServerStatus::Failed { message } => {
            (configured, State::Problem, "Stopped", Some(message.clone()))
        }
    };
    Component {
        name,
        address: Some(address),
        state,
        label,
        detail,
    }
}

/// The broker's streams: market data always, order updates when the broker
/// sends them over a socket of their own.
fn broker_streams(ctx: &AppState) -> Vec<Component> {
    if ctx.get_broker_session().is_none() {
        return vec![Component {
            name: "Broker market data",
            address: None,
            state: State::Off,
            label: "Not signed in",
            detail: Some("Log in to your broker from Select Broker to stream prices.".into()),
        }];
    }
    let mut out = vec![stream("Broker market data", &ctx.websocket, true)];
    let orders = &ctx.runtime.order_ws;
    if orders.is_running() {
        out.push(stream("Broker order updates", orders, false));
    }
    out
}

fn stream(name: &'static str, feed: &WebSocketManager, market: bool) -> Component {
    let status = feed.status();
    let instruments = market.then(|| feed.instrument_count());
    let (_, detail) = streams::describe(&status, feed.stats().last_event_age(), instruments);
    let (state, label) = stream_state(&status);
    Component {
        name,
        address: None,
        state,
        label,
        detail: Some(detail),
    }
}

fn stream_state(status: &FeedStatus) -> (State, &'static str) {
    match status {
        FeedStatus::Connected { .. } => (State::Ok, "Connected"),
        FeedStatus::Connecting { .. } => (State::Waiting, "Connecting"),
        FeedStatus::Reconnecting { .. } => (State::Waiting, "Reconnecting"),
        FeedStatus::AuthFailed { .. } => (State::Problem, "Refused"),
        FeedStatus::Disconnected => (State::Problem, "Not connected"),
    }
}

/// The order reconciler: whether it runs, and whether it is re-reading the
/// broker's order book after a gap in order updates.
fn order_checks(running: bool, gap_open: bool) -> Component {
    let (state, label, detail) = match (running, gap_open) {
        (false, _) => (
            State::Problem,
            "Stopped",
            "Orders are not being checked against your broker's order book. Quit OpenAlgo and open it again.",
        ),
        (true, true) => (
            State::Waiting,
            "Re-checking",
            crate::services::order_reconciler::ENTRIES_PAUSED,
        ),
        (true, false) => (
            State::Ok,
            "Running",
            "Checks your open orders against your broker's order book every few seconds.",
        ),
    };
    Component {
        name: "Order checks",
        address: None,
        state,
        label,
        detail: Some(detail.into()),
    }
}

/// The latest health sample: its overall status and how old it is.
fn health_checks(o: &health::Overall) -> Component {
    let age = o.sample_age_s.map(|s| {
        let s = std::time::Duration::from_secs(u64::try_from(s).unwrap_or(0));
        format!("Last check {} ago.", streams::ago(s))
    });
    let with_age = |hint: &str| match &age {
        Some(a) => format!("{} {}", a, hint),
        None => hint.to_string(),
    };
    let (state, label, detail) = match o.status.as_str() {
        "pass" => (State::Ok, "Healthy", age.clone()),
        "warn" => (
            State::Waiting,
            "Warnings",
            Some(with_age("See the Health page for what to check.")),
        ),
        "fail" => (
            State::Problem,
            "Failing",
            Some(with_age("See the Health page for what failed.")),
        ),
        _ => (State::Problem, "Unknown", o.reason.clone()),
    };
    Component {
        name: "Health checks",
        address: None,
        state,
        label,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::BrokerRegistry;
    use chrono::Utc;

    fn names(c: &[Component]) -> Vec<&'static str> {
        c.iter().map(|c| c.name).collect()
    }

    #[test]
    fn listeners_report_their_real_state() {
        let running = ServerStatus::Running {
            host: "127.0.0.1".into(),
            port: 5000,
        };
        let c = listener("App and API", ("127.0.0.1", 5000), &running);
        assert_eq!(
            (c.state, c.label, c.address.as_deref()),
            (State::Ok, "Running", Some("127.0.0.1:5000"))
        );
        let taken = ServerStatus::PortInUse {
            port: 8765,
            message: "Port 8765 is already used by another program.".into(),
        };
        let c = listener("Market data server", ("0.0.0.0", 8765), &taken);
        assert_eq!((c.state, c.label), (State::Problem, "Port in use"));
        assert_eq!(c.address.as_deref(), Some("0.0.0.0:8765"));
        assert_eq!(
            c.detail.as_deref(),
            Some("Port 8765 is already used by another program.")
        );
        let failed = ServerStatus::Failed {
            message: "OpenAlgo cannot use port 80.".into(),
        };
        let c = listener("App and API", ("127.0.0.1", 80), &failed);
        assert_eq!(
            (c.state, c.address.as_deref()),
            (State::Problem, Some("127.0.0.1:80"))
        );
        let c = listener("App and API", ("127.0.0.1", 5000), &ServerStatus::Starting);
        assert_eq!(c.state, State::Waiting);
    }

    #[test]
    fn broker_streams_name_each_state() {
        let b = || "zerodha".to_string();
        assert_eq!(
            stream_state(&FeedStatus::Connected { broker: b() }).0,
            State::Ok
        );
        assert_eq!(
            stream_state(&FeedStatus::Reconnecting {
                broker: b(),
                attempt: 2,
                delay_ms: 500
            }),
            (State::Waiting, "Reconnecting")
        );
        assert_eq!(
            stream_state(&FeedStatus::AuthFailed {
                broker: b(),
                message: "x".into()
            }),
            (State::Problem, "Refused")
        );
        assert_eq!(stream_state(&FeedStatus::Disconnected).0, State::Problem);
    }

    #[test]
    fn order_checks_and_health_say_what_is_wrong() {
        assert_eq!(order_checks(true, false).state, State::Ok);
        let gap = order_checks(true, true);
        assert_eq!((gap.state, gap.label), (State::Waiting, "Re-checking"));
        assert_eq!(order_checks(false, false).state, State::Problem);

        let overall = |status: &str, age: Option<i64>, reason: Option<&str>| health::Overall {
            status: status.into(),
            sample_age_s: age,
            reason: reason.map(Into::into),
        };
        let ok = health_checks(&overall("pass", Some(30), None));
        assert_eq!(
            (ok.state, ok.detail.as_deref()),
            (State::Ok, Some("Last check 30 s ago."))
        );
        assert_eq!(
            health_checks(&overall("warn", Some(5), None)).state,
            State::Waiting
        );
        assert_eq!(
            health_checks(&overall("fail", None, None)).state,
            State::Problem
        );
        let unknown = health_checks(&overall("unknown", None, Some("No health sample yet.")));
        assert_eq!(
            (unknown.state, unknown.detail.as_deref()),
            (State::Problem, Some("No health sample yet."))
        );
    }

    #[tokio::test]
    async fn every_part_the_desktop_runs_and_nothing_it_does_not() {
        let t = crate::state::testing::build(BrokerRegistry::new(), Utc::now());
        *t.ctx.server_status.write() = ServerStatus::Running {
            host: "127.0.0.1".into(),
            port: 5000,
        };
        let parts = server_components(&t.ctx);
        assert_eq!(
            names(&parts),
            [
                "App and API",
                "Market data server",
                "Broker market data",
                "Order checks",
                "Health checks"
            ]
        );
        // No broker is signed in, so its stream is off, not "Running".
        assert_eq!(
            (parts[2].state, parts[2].label),
            (State::Off, "Not signed in")
        );
        assert_eq!(
            parts[3].state,
            State::Ok,
            "the reconciler starts with the app"
        );
        let text = serde_json::to_string(&parts).unwrap();
        for web_only in ["Flask", "ZeroMQ", "5555"] {
            assert!(!text.contains(web_only), "{}", web_only);
        }
        t.ctx.shutdown().await;
    }
}
