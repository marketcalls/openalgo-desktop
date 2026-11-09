//! Audit C01 for XTS: a release goes over REST, so the PUT's answer is the
//! release's answer. A refused PUT reaches the waiting caller (it was only
//! logged) and is not sent again behind its back; the caller's retry is
//! sent once and answered.

use super::super::{FeedSource, XtsFeed};
use crate::brokers::common::streaming::{BrokerFeed, FeedMode, FeedSubscription};
use axum::routing::put;
use axum::Router;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

fn sbin() -> FeedSubscription {
    FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "3045".into(),
        brsymbol: "SBIN-EQ".into(),
        brexchange: "NSECM".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

/// The 5paisa XTS subscription endpoint, answering each PUT (1-based) with
/// the scripted status and body, success otherwise. Returns the base URL
/// and the number of PUTs seen.
async fn endpoint(script: Vec<(usize, u16, &'static str)>) -> (String, Arc<Mutex<usize>>) {
    let calls: Arc<Mutex<usize>> = Arc::default();
    let seen = calls.clone();
    let script = Arc::new(script);
    let app = Router::new().route(
        "/apimarketdata/instruments/subscription",
        put(move || {
            let (seen, script) = (seen.clone(), script.clone());
            async move {
                let n = {
                    let mut g = seen.lock();
                    *g += 1;
                    *g
                };
                match script.iter().find(|(k, _, _)| *k == n) {
                    Some((_, status, body)) => (
                        axum::http::StatusCode::from_u16(*status).unwrap_or_default(),
                        body.to_string(),
                    ),
                    None => (
                        axum::http::StatusCode::OK,
                        r#"{"type":"success","result":{}}"#.to_string(),
                    ),
                }
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a local port");
    let base = format!("http://{}", l.local_addr().expect("bound"));
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (base, calls)
}

fn feed(base: String) -> XtsFeed {
    let mut f = XtsFeed::new(
        &crate::brokers::fivepaisaxts::CONFIG,
        crate::brokers::common::http::client(),
        base,
        None,
        FeedSource::stored("tok", "uid"),
    );
    f.insert(sbin());
    f
}

/// What the broker answered the release of SBIN with.
async fn release(f: &mut XtsFeed) -> Vec<(String, String)> {
    let (frames, answer) = f.release_frames(&[sbin()]);
    assert!(frames.is_empty(), "XTS releases over REST, not the socket");
    let answer = answer.expect("the broker answers an XTS release itself");
    tokio::time::timeout(Duration::from_secs(5), answer)
        .await
        .expect("answered in time")
        .expect("answered")
        .into_iter()
        .map(|(s, why)| (s.symbol, why))
        .collect()
}

#[tokio::test]
async fn a_refused_put_reaches_the_caller_and_its_retry_is_sent_once() {
    let (base, calls) = endpoint(vec![(
        1,
        400,
        r#"{"type":"error","description":"Request could not be processed"}"#,
    )])
    .await;
    let mut f = feed(base);

    let refused = release(&mut f).await;
    assert_eq!(refused.len(), 1, "{:?}", refused);
    assert_eq!(refused[0].0, "SBIN");
    assert!(
        refused[0].1.contains("Request could not be processed")
            && refused[0].1.contains("stays subscribed"),
        "{}",
        refused[0].1
    );
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert_eq!(
        *calls.lock(),
        1,
        "a waited release is not sent again on its own"
    );

    assert!(release(&mut f).await.is_empty(), "the retry is released");
    assert_eq!(*calls.lock(), 2);
}

/// A server error is answered too (the background retry is for releases
/// nobody waits on), and XTS saying the instrument is not subscribed is a
/// release: it holds nothing.
#[tokio::test]
async fn an_unanswered_put_is_refused_and_not_subscribed_is_released() {
    let (base, calls) = endpoint(vec![
        (1, 503, r#"{"type":"error","description":"busy"}"#),
        (
            2,
            400,
            r#"{"type":"error","description":"Instrument not subscribed"}"#,
        ),
    ])
    .await;
    let mut f = feed(base);
    let refused = release(&mut f).await;
    assert_eq!(refused.len(), 1, "{:?}", refused);
    assert!(refused[0].1.contains("did not answer"), "{}", refused[0].1);
    assert!(release(&mut f).await.is_empty());
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert_eq!(
        *calls.lock(),
        2,
        "a refused release is not sent again on its own"
    );
}

fn infy() -> FeedSubscription {
    FeedSubscription {
        symbol: "INFY".into(),
        token: "1594".into(),
        brsymbol: "INFY-EQ".into(),
        ..sbin()
    }
}

/// The subscription endpoint answering each POST: refused with the XTS
/// error body when the batch names `refused_id`, else success. Returns
/// the base URL and the number of POSTs seen.
async fn subscribe_endpoint(refused_id: &'static str) -> (String, Arc<Mutex<usize>>) {
    let calls: Arc<Mutex<usize>> = Arc::default();
    let seen = calls.clone();
    let app = Router::new().route(
        "/apimarketdata/instruments/subscription",
        axum::routing::post(move |body: String| {
            let seen = seen.clone();
            async move {
                *seen.lock() += 1;
                let refused = body.contains(&format!("\"exchangeInstrumentID\":{refused_id}"));
                if refused {
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        r#"{"type":"error","description":"instrument not allowed"}"#.to_string(),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        r#"{"type":"success","result":{"listQuotes":[]}}"#.to_string(),
                    )
                }
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a local port");
    let base = format!("http://{}", l.local_addr().expect("bound"));
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (base, calls)
}

/// What the broker answered the admission of `subs` with.
async fn admit(f: &mut XtsFeed, subs: &[FeedSubscription]) -> Vec<(String, String)> {
    let (frames, answer) = f.admission_frames(subs);
    assert!(
        frames.is_empty(),
        "XTS subscribes over REST, not the socket"
    );
    let answer = answer.expect("the broker answers an XTS subscribe itself");
    tokio::time::timeout(Duration::from_secs(10), answer)
        .await
        .expect("answered in time")
        .expect("answered")
        .into_iter()
        .map(|(s, why)| (s.symbol, why))
        .collect()
}

/// Review R107-03: a definitive REST refusal of a subscribe reaches the
/// admission, in the broker's words; the web logs it and acknowledges a
/// symbol that never streams.
#[tokio::test]
async fn a_refused_subscribe_reaches_its_admission() {
    let (base, calls) = subscribe_endpoint("3045").await;
    let mut f = feed(base);
    assert_eq!(
        admit(&mut f, &[sbin()]).await,
        vec![(
            "SBIN".to_string(),
            "The broker refused to stream this instrument (instrument not allowed).".to_string()
        )]
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(*calls.lock(), 1, "a definitive refusal is not sent again");
}

/// Review R107-03: one refused instrument in a batch does not refuse the
/// others: the batch is asked again one by one, and only the refused one
/// is answered so.
#[tokio::test]
async fn a_mixed_batch_refuses_only_the_refused_instrument() {
    let (base, calls) = subscribe_endpoint("1594").await;
    let mut f = feed(base);
    let refused = admit(&mut f, &[sbin(), infy()]).await;
    assert_eq!(refused.len(), 1, "{:?}", refused);
    assert_eq!(refused[0].0, "INFY");
    assert_eq!(*calls.lock(), 3, "the batch, then each instrument");
}

fn numbered(i: usize) -> FeedSubscription {
    FeedSubscription {
        symbol: format!("S{i:02}"),
        token: (5000 + i).to_string(),
        brsymbol: format!("S{i:02}-EQ"),
        ..sbin()
    }
}

/// Review X1: one refused instrument out of 50 is found in about a dozen
/// calls, within the manager's wait; one by one it took 25 s and the wait
/// ran out, so every item was answered as a success.
#[tokio::test]
async fn a_refused_batch_is_searched_by_halves() {
    let (base, calls) = subscribe_endpoint("5017").await;
    let mut f = feed(base);
    let subs: Vec<FeedSubscription> = (0..50).map(numbered).collect();
    let refused = admit(&mut f, &subs).await;
    assert_eq!(refused.len(), 1, "{:?}", refused);
    assert_eq!(refused[0].0, "S17");
    assert!(*calls.lock() <= 14, "{} calls", *calls.lock());
}
