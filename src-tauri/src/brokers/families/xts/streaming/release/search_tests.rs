//! Review X1: the time an admission may spend searching a refused batch.

use super::*;
use crate::brokers::common::http;
use crate::brokers::families::xts::streaming::{run_command, SubscriptionTarget};
use serde_json::json;
use tokio::sync::mpsc;

/// A subscription endpoint that refuses everything it is sent.
async fn refusing_endpoint() -> (String, Arc<Mutex<usize>>) {
    let calls: Arc<Mutex<usize>> = Arc::default();
    let seen = calls.clone();
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move || {
            let seen = seen.clone();
            async move {
                *seen.lock() += 1;
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    r#"{"type":"error","description":"not allowed"}"#,
                )
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a local port");
    let url = format!("http://{}/", l.local_addr().expect("bound"));
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (url, calls)
}

/// Out of time, what is left of a refused batch is answered without
/// another call: the broker refused the batch, so it is not streaming.
#[tokio::test]
async fn a_search_out_of_time_answers_the_rest_as_not_checked() {
    let (url, calls) = refusing_endpoint().await;
    let instruments: Vec<Value> = (0..50)
        .map(|i| json!({"exchangeSegment": 1, "exchangeInstrumentID": i}))
        .collect();
    let (to, answer) = oneshot::channel();
    let waited = Waited(Arc::new(Mutex::new(Answer {
        kind: Kind::Admission,
        search_until: Some(Instant::now()),
        commands_left: 1,
        instruments: Vec::new(),
        refused: Vec::new(),
        to: Some(to),
    })));
    let cmd = Command {
        subscribe: true,
        code: 1512,
        instruments,
        waited: Some(waited.clone()),
        then_release: None,
    };
    let client = http::client();
    let target = SubscriptionTarget {
        broker: "t",
        http: &client,
        url: &url,
        token: "tok",
        gap: std::time::Duration::ZERO,
        split_duplicates: false,
    };
    let (snapshots, _rx) = mpsc::channel(1);
    let report = run_command(&target, &cmd, &snapshots, &mut true).await;
    assert_eq!(*calls.lock(), 1, "only the batch itself");
    assert_eq!(report.refused.len(), 50);
    assert!(report.refused.iter().all(|(_, why)| why == NOT_CHECKED));
    waited.ran(&report, false);
    assert!(answer.await.unwrap().is_empty(), "no instrument is known");
}
