//! UI-01 and N-02: one analyzer mode, whichever path changes it.
//!
//! The app's switch, an SDK script (`/api/v1/analyzer/toggle`) and an AI
//! client (the MCP `analyzer_toggle` tool) all change the mode in one step
//! under the sandbox's mode lock: the engine follows the mode actually
//! stored before the change answers, and every change announces that mode
//! (`analyzer.mode_changed`, emitted to the pages as Socket.IO
//! `analyzer_mode_changed`), so a page showing Sandbox learns that an AI
//! client switched the server to Live before the trader's next order.

use crate::mcp_support::M;
use axum::http::{Method, StatusCode};
use futures_util::future::join_all;
use openalgo_desktop_lib::events::{Event, Lane, Subscriber, Topic};
use openalgo_desktop_lib::mcp::store::TokenScope;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

/// The modes announced, in order.
#[derive(Default)]
struct Announced(Mutex<Vec<bool>>);

#[async_trait::async_trait]
impl Subscriber for Announced {
    fn name(&self) -> &'static str {
        "test-mode-announcements"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![Topic::AnalyzerModeChanged]
    }
    async fn handle(&self, event: Arc<Event>) {
        if let Event::AnalyzerModeChanged { analyze_mode } = *event {
            self.0.lock().push(analyze_mode);
        }
    }
}

impl Announced {
    /// Wait until `n` announcements arrived (the bus delivers on a task).
    async fn wait_for(&self, n: usize) -> Vec<bool> {
        for _ in 0..400 {
            if self.0.lock().len() >= n {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.0.lock().clone()
    }
}

async fn listening(m: &M) -> Arc<Announced> {
    let announced = Arc::new(Announced::default());
    m.h.ctx.bus.subscribe(announced.clone(), Lane::Critical);
    announced
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_way_to_change_the_mode_announces_the_stored_mode() {
    let m = M::new().await;
    m.h.analyze(false);
    let announced = listening(&m).await;
    let token = m.token(TokenScope::ReadWrite);

    // The app's Live/Analyze switch.
    let (s, v) = m
        .session_json(Method::POST, "/auth/analyzer-toggle", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["analyze_mode"], true);
    assert_eq!(v["data"]["message"], "Switched to Analyze mode");
    assert!(
        m.h.ctx.sandbox.is_engine_running().await,
        "the engine follows the stored mode before the switch answers"
    );
    assert_eq!(announced.wait_for(1).await, vec![true]);

    // An SDK script.
    let (s, v) =
        m.h.post(
            "/api/v1/analyzer/toggle",
            m.h.with_key(json!({"mode": false})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(!m.h.ctx.sandbox.is_engine_running().await);
    assert_eq!(announced.wait_for(2).await, vec![true, false]);

    // An AI client.
    let out = m
        .output(&token, "analyzer_toggle", json!({"mode": true}))
        .await;
    assert_eq!(out["data"]["data"]["analyze_mode"], true, "{}", out);
    assert!(m.h.ctx.sandbox.is_engine_running().await);
    assert_eq!(announced.wait_for(3).await, vec![true, false, true]);

    assert!(m.h.ctx.sqlite.get_analyze_mode().unwrap());
    m.h.shutdown().await;
}

/// N-02: many changes at once, from the switch and from the API. Each flip
/// reads the mode under the change's lock, and the engine and the last
/// announcement end on the mode that is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_changes_leave_the_engine_on_the_stored_mode() {
    let m = M::new().await;
    m.h.analyze(false);
    let announced = listening(&m).await;

    let app = &m;
    let flips = (0..40).map(|_| async move {
        let (s, v) = app
            .session_json(Method::POST, "/auth/analyzer-toggle", None)
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
    });
    join_all(flips).await;
    let stored = m.h.ctx.sqlite.get_analyze_mode().unwrap();
    assert!(
        !stored,
        "an even number of flips leaves the mode where it was"
    );
    assert_eq!(m.h.ctx.sandbox.is_engine_running().await, stored);
    assert_eq!(announced.wait_for(40).await.last(), Some(&stored));

    let h = &m.h;
    let sets = (0..40).map(|i| {
        let body = h.with_key(json!({"mode": i % 2 == 0}));
        async move {
            let (s, v) = h.post("/api/v1/analyzer/toggle", body).await;
            assert_eq!(s, StatusCode::OK, "{}", v);
        }
    });
    join_all(sets).await;
    let stored = m.h.ctx.sqlite.get_analyze_mode().unwrap();
    assert_eq!(m.h.ctx.sandbox.is_engine_running().await, stored);
    assert_eq!(announced.wait_for(80).await.last(), Some(&stored));
    m.h.shutdown().await;
}
