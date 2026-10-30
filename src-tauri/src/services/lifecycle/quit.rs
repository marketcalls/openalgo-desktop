//! Quitting with Live work running (ARCH-03): the trader is told what
//! stops watching, with the inventory an update's restart reads
//! (`services::update`), what waits for alerts or schedules, and that the
//! keys lock at quit (KEY-01), and confirms first (maintainer decision
//! 2026-10-16). Nothing is flattened: the positions and broker orders stay
//! where they are.

use crate::services::update::{activity, broker_name, Warning};
use crate::state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};

/// What quitting now would leave unwatched, or `None` when nothing runs
/// and nothing waits for an alert or a schedule.
pub async fn quit_warning(ctx: &AppState) -> Option<Warning> {
    let broker = broker_name(ctx);
    activity(ctx)
        .await
        .quit_warning(broker.as_deref(), waiting(ctx))
}

/// Names shown in one sentence before "and N more".
const NAMES_SHOWN: usize = 5;

/// What waits for an alert or a schedule while OpenAlgo is closed, one
/// sentence per kind. Nothing there is open at the broker; it simply does
/// not run until the next start and sign-in, since the keys are locked at
/// quit (KEY-01).
fn waiting(ctx: &AppState) -> Vec<String> {
    let mut items = Vec::new();
    let mut unread = false;
    match ctx.strategy.store.alert_strategy_names() {
        Ok(n) if !n.is_empty() => items.push(format!(
            "Webhook alerts for {} (TradingView and other senders) are refused until you open OpenAlgo and sign in again.",
            names(&n)
        )),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Quit: could not read the strategies' webhooks: {}", e);
            unread = true;
        }
    }
    match ctx.chartink.store.all() {
        Ok(all) => {
            let active: Vec<String> = all
                .into_iter()
                .filter(|s| s.is_active)
                .map(|s| s.name)
                .collect();
            if !active.is_empty() {
                items.push(format!(
                    "Chartink alerts for {} are refused until you open OpenAlgo and sign in again.",
                    names(&active)
                ));
            }
        }
        Err(e) => {
            tracing::warn!("Quit: could not read the Chartink strategies: {}", e);
            unread = true;
        }
    }
    match ctx
        .sqlite
        .conn()
        .and_then(|c| crate::flow::store::workflows::active_waiting_names(&c))
    {
        Ok(n) if !n.is_empty() => items.push(flow_waiting(&n)),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Quit: could not read the active Flow workflows: {}", e);
            unread = true;
        }
    }
    let starts = scheduled_starts(ctx, &mut unread);
    if !starts.is_empty() {
        items.push(format!(
            "Scheduled starts for {} do not run while OpenAlgo is closed.",
            names(&starts)
        ));
    }
    if unread {
        items.push("OpenAlgo could not check which strategies wait for alerts or schedules. Check them before you quit.".into());
    }
    items
}

/// Active Flow workflows: their schedules, alerts, order watches and
/// webhooks do not run while the app is closed.
fn flow_waiting(active: &[String]) -> String {
    let (one, them) = if active.len() == 1 {
        ("is", "its")
    } else {
        ("are", "their")
    };
    format!(
        "Flow {} {} {} active; {} schedules, price alerts, order watches and webhooks do not run while OpenAlgo is closed.",
        if active.len() == 1 { "workflow" } else { "workflows" },
        names(active),
        one,
        them
    )
}

/// Strategies and OpenScript deployments that start on a schedule.
fn scheduled_starts(ctx: &AppState, unread: &mut bool) -> Vec<String> {
    use crate::strategy::scheduler::JobFunc;
    let mut out: Vec<String> = ctx
        .strategy
        .scheduler
        .jobs()
        .into_iter()
        .filter(|j| j.func == JobFunc::RunScheduledStart)
        .filter_map(
            |j| match ctx.strategy.store.get_strategy_unscoped(j.strategy_id) {
                Ok(row) => row.map(|r| r.name),
                Err(_) => {
                    *unread = true;
                    None
                }
            },
        )
        .collect();
    match ctx
        .sqlite
        .conn()
        .map_err(|e| e.to_string())
        .and_then(|c| crate::trading::runner::store::all_schedules(&c).map_err(|e| e.to_string()))
    {
        Ok(s) => out.extend(s.into_keys()),
        Err(e) => {
            tracing::warn!("Quit: could not read the OpenScript schedules: {}", e);
            *unread = true;
        }
    }
    out
}

/// "A, B and C"; past [`NAMES_SHOWN`], "A, B, C, D, E and 3 more".
fn names(list: &[String]) -> String {
    let shown: Vec<&str> = list.iter().take(NAMES_SHOWN).map(String::as_str).collect();
    let rest = list.len() - shown.len();
    match (shown.split_last(), rest) {
        (None, _) => String::new(),
        (Some((last, [])), 0) => (*last).to_string(),
        (Some((last, head)), 0) => format!("{} and {}", head.join(", "), last),
        (Some(_), n) => format!("{} and {} more", shown.join(", "), n),
    }
}

/// What to do with a request to close the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRequest {
    /// Close: the trader confirmed, or nothing needed confirming.
    Close,
    /// Keep the window and ask the trader (start the check).
    Ask,
    /// Keep the window: the trader is being asked already.
    Asking,
}

/// One quit check at a time, and the trader's answer.
#[derive(Debug, Default)]
pub struct QuitGate {
    asking: AtomicBool,
    confirmed: AtomicBool,
}

impl QuitGate {
    /// Decide on a request to close the main window.
    pub fn on_close(&self) -> CloseRequest {
        if self.confirmed.load(Ordering::SeqCst) {
            CloseRequest::Close
        } else if self.asking.swap(true, Ordering::SeqCst) {
            CloseRequest::Asking
        } else {
            CloseRequest::Ask
        }
    }

    /// The trader answered (or nothing needed asking): `quit` lets the next
    /// close go through; otherwise the next close asks again.
    pub fn answered(&self, quit: bool) {
        self.confirmed.store(quit, Ordering::SeqCst);
        self.asking.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_flow_workflows_are_named() {
        assert_eq!(
            flow_waiting(&["Gap fade".into()]),
            "Flow workflow Gap fade is active; its schedules, price alerts, order watches and webhooks do not run while OpenAlgo is closed."
        );
        assert_eq!(
            flow_waiting(&["A".into(), "B".into()]),
            "Flow workflows A and B are active; their schedules, price alerts, order watches and webhooks do not run while OpenAlgo is closed."
        );
    }

    #[tokio::test]
    async fn quitting_names_the_active_flow_workflows() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        t.ctx
            .security
            .install(&crate::security::Unlocked::random().unwrap());
        let conn = t.ctx.sqlite.conn().unwrap();
        let nodes =
            serde_json::json!([{"id": "s", "type": "start", "data": {"scheduleType": "daily"}}]);
        let w = crate::flow::store::workflows::create(
            &conn,
            &t.ctx.security,
            &crate::flow::store::workflows::NewWorkflow {
                name: "Opening range",
                description: None,
                nodes: &nodes,
                edges: &serde_json::json!([]),
            },
            t.ctx.now(),
        )
        .unwrap();
        crate::flow::store::workflows::set_active(&conn, w.id, true, t.ctx.now()).unwrap();
        drop(conn);
        assert!(waiting(&t.ctx)
            .iter()
            .any(|i| i.starts_with("Flow workflow Opening range is active;")));
    }

    #[test]
    fn a_long_list_of_names_is_cut_short() {
        let n = |k: usize| (1..=k).map(|i| format!("S{}", i)).collect::<Vec<_>>();
        assert_eq!(names(&n(1)), "S1");
        assert_eq!(names(&n(3)), "S1, S2 and S3");
        assert_eq!(names(&n(5)), "S1, S2, S3, S4 and S5");
        assert_eq!(names(&n(8)), "S1, S2, S3, S4, S5 and 3 more");
    }

    #[test]
    fn one_check_at_a_time_and_a_kept_window_asks_again() {
        let gate = QuitGate::default();
        assert_eq!(gate.on_close(), CloseRequest::Ask);
        // A second close while the dialog is open does not open another.
        assert_eq!(gate.on_close(), CloseRequest::Asking);
        // Kept open: the next close asks again.
        gate.answered(false);
        assert_eq!(gate.on_close(), CloseRequest::Ask);
        // Confirmed: the close goes through, every time.
        gate.answered(true);
        assert_eq!(gate.on_close(), CloseRequest::Close);
        assert_eq!(gate.on_close(), CloseRequest::Close);
    }
}
