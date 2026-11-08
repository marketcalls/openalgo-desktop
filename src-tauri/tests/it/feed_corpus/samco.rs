//! Samco: the broadcast feed (`brokers::samco::streaming::SamcoFeed`, the
//! `quote` and `quote2` streams on one socket), and order updates from
//! polling the order book (Samco has no order socket: web
//! `_POLLING_BROKERS`), read from the fake broker API by Samco's own
//! `OrderPoller`.
//!
//! The market feed knows NIFTY's streaming id (`-23`), as it does once a
//! REST index quote of NIFTY has run in the session (`brokers/samco/data.rs`
//! `index_details`); any other index starts without one (audit RB-05). The
//! broker's own `create_feed` shares a cache only a REST quote fills, which
//! a case cannot run before the feed starts, so the feed is built here
//! with the cache it would have then.

use super::harness::{corpus_cases, Env, Feeds};
use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::samco::streaming::SamcoFeed;
use openalgo_desktop_lib::brokers::samco::{ListingIds, SamcoBroker};
use openalgo_desktop_lib::brokers::types::AuthToken;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Samco's order book, empty until a case serves another.
const ORDER_BOOK: &str = "/order/orderBook";

/// Percent-encoded as Samco's login returns it; the feed sends it decoded.
const SESSION: &str = "sess%2Btoken";

fn feeds(env: &Env) -> Feeds {
    // The broker wires the bridge as a session does (its depth levels and
    // exchange refusals) and owns the order poller.
    let broker = Arc::new(SamcoBroker::with_urls(
        env.symbols(),
        env.http_url(),
        format!("{}/master", env.http_url()),
        env.url("main"),
    ));
    let auth = AuthToken::new(SESSION);
    let listing_ids = Arc::new(Mutex::new(ListingIds::default()));
    listing_ids.lock().insert("NSE_INDEX", "NIFTY", "-23");
    let feeds = Feeds::market(SamcoFeed::new(&env.url("main"), SESSION, listing_ids))
        .with_broker(broker.clone(), &auth);
    if !env.orders() {
        return feeds;
    }
    env.serve_json(
        ORDER_BOOK,
        json!({"status": "Success", "orderBookDetails": []}),
    );
    let updates = broker
        .start_order_updates(&auth, Duration::from_secs(1))
        .expect("Samco's order poller");
    feeds.with_order_feed(OrderFeed::Stream(updates))
}

corpus_cases!(
    "samco",
    feeds,
    [
        quote_mode,
        ltp_mode,
        depth_quote_then_book,
        depth_book_first_future,
        reconnect_forgets_state,
        quote_and_depth_clients,
        index_ltp_and_quote,
        order_book_polling,
    ]
);
