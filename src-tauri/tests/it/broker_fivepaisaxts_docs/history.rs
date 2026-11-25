//! 5paisa XTS asks for history a year at a time (web 19f993c92,
//! marketcalls/openalgo#2490).

use super::fake::{broker, history, token};
use openalgo_desktop_lib::brokers::Broker;

/// Web 19f993c92: up to 365 days per call, whole days, the next window one
/// second after the last, so no day between two windows is lost and a long
/// range takes few calls.
#[tokio::test]
async fn history_is_asked_a_year_at_a_time() {
    let (b, fake, _) = broker().await;
    b.get_history(&token(), &history("1m", (2024, 1, 1), (2025, 2, 4)))
        .await
        .unwrap();
    let windows: Vec<String> = fake
        .lines("ohlc|")
        .iter()
        .map(|l| l.splitn(3, '|').nth(2).unwrap_or("").to_string())
        .collect();
    assert_eq!(
        windows,
        [
            "Jan 01 2024 000000|Dec 30 2024 235959|60",
            "Dec 31 2024 000000|Feb 04 2025 235959|60",
        ]
    );
}
