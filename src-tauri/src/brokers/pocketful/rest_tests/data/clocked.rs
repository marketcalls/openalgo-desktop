//! Data cases that need a short deadline: the one deadline a batch of
//! quotes has (desktop: the web has none).

use super::{master, plain_sentence, session};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::pocketful::data::{quote_each, UNANSWERED};
use crate::brokers::pocketful::PocketfulBroker;
use crate::brokers::types::*;
use std::collections::HashSet;
use std::time::Duration;

/// A host that takes connections and never answers: a broker that stopped
/// answering. The task is aborted when the guard goes.
struct Silent {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Silent {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        Self { base, task }
    }

    fn broker(&self, symbols: SymbolResolver) -> PocketfulBroker {
        PocketfulBroker::with_base_url(symbols, &self.base, "ws://127.0.0.1:9", &self.base)
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One batch of quotes has one deadline (desktop): the symbols it
/// leaves unasked say so, never a quote of zeros, and a chain counts their
/// open interest as missing.
#[tokio::test]
async fn a_batch_past_its_deadline_answers_each_symbol_left_unasked() {
    let host = Silent::start().await;
    let b = host.broker(master());
    let keys = [
        QuoteKey::new("NSE", "IDEA"),
        QuoteKey::new("NSE", "RELIANCE"),
    ];
    let started = std::time::Instant::now();

    let (quotes, oi_missing) = quote_each(&b, &session(), &keys, Duration::from_millis(300))
        .await
        .unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let shape: Vec<(&str, bool, Option<&str>)> = quotes
        .iter()
        .map(|q| (q.symbol.as_str(), q.data.is_some(), q.error.as_deref()))
        .collect();
    assert_eq!(
        shape,
        [
            ("IDEA", false, Some(UNANSWERED)),
            ("RELIANCE", false, Some(UNANSWERED)),
        ]
    );
    plain_sentence(UNANSWERED);
    assert_eq!(
        oi_missing,
        HashSet::from(["IDEA".to_string(), "RELIANCE".to_string()])
    );
}
