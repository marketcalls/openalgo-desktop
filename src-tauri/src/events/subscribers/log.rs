//! Log subscriber (web `subscribers/log_subscriber.py`): live order calls go to
//! `order_logs`, sandbox mode calls to `analyzer_logs`, both in `logs.db`.

use crate::db::sqlite::logs::LogsDb;
use crate::events::{Event, Mode, Subscriber, Topic};
use std::sync::Arc;

pub struct LogSubscriber {
    logs: Arc<LogsDb>,
}

impl LogSubscriber {
    pub fn new(logs: Arc<LogsDb>) -> Self {
        Self { logs }
    }
}

#[async_trait::async_trait]
impl Subscriber for LogSubscriber {
    fn name(&self) -> &'static str {
        "log"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderPlaced,
            Topic::OrderFailed,
            Topic::OrderUnconfirmed,
            Topic::OrderNoAction,
            Topic::OrderModified,
            Topic::OrderModifyFailed,
            Topic::OrderCancelled,
            Topic::OrderCancelFailed,
            Topic::AllOrdersCancelled,
            Topic::PositionClosed,
            Topic::BasketCompleted,
            Topic::SplitCompleted,
            Topic::OptionsCompleted,
            Topic::MultiOrderCompleted,
            Topic::AnalyzerError,
            Topic::GttPlaced,
            Topic::GttFailed,
            Topic::GttModified,
            Topic::GttModifyFailed,
            Topic::GttCancelled,
            Topic::GttCancelFailed,
            Topic::GttTriggered,
            Topic::GttExpired,
        ]
    }

    async fn handle(&self, event: Arc<Event>) {
        let Some(meta) = event.meta().cloned() else {
            return;
        };
        let logs = self.logs.clone();
        // SQLite write off the async workers.
        let res = tokio::task::spawn_blocking(move || match meta.mode {
            Mode::Analyze => {
                logs.insert_analyzer_log(&meta.api_type, &meta.request_data, &meta.response_data)
            }
            Mode::Live => {
                logs.insert_order_log(&meta.api_type, &meta.request_data, &meta.response_data)
            }
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Could not write order log: {}", e),
            Err(e) => tracing::error!("Order log task failed: {}", e),
        }
    }
}
