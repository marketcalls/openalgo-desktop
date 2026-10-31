//! Shared by the trigger tests: every message the code under test logs,
//! and an app with an account and its API key.

use super::*;

/// Every message logged on this thread while the guard lives (a
/// current-thread test runtime keeps its tasks on it).
#[derive(Clone, Default)]
pub(super) struct Logged(pub(super) Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Logged {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{:?}", v);
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0.lock().push(m.0);
    }
}

pub(super) fn logged() -> (tracing::subscriber::DefaultGuard, Logged) {
    use tracing_subscriber::layer::SubscriberExt;
    let l = Logged::default();
    let guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(l.clone()));
    (guard, l)
}

impl Logged {
    pub(super) fn has(&self, line: &str) -> bool {
        self.0.lock().iter().any(|m| m == line)
    }
}

impl Logged {
    pub(super) fn starting_with(&self, prefix: &str) -> Vec<String> {
        self.0
            .lock()
            .iter()
            .filter(|m| m.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// Wait (bounded) until `line` was logged.
pub(super) async fn wait_for(logs: &Logged, line: &str) {
    for _ in 0..500 {
        if logs.has(line) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("never logged {:?}: {:?}", line, logs.0.lock());
}

/// An app with an account and its API key (made before any workflow).
pub(super) fn keyed() -> crate::state::testing::TestCtx {
    let t = app();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    t
}
