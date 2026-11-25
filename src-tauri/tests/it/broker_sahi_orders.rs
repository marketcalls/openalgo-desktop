//! Sahi's order stream as the broker builds it (web openalgo#2474): an
//! order socket from the session's token on `/v1/stream/orders`, with the
//! Bearer token and the User-Agent Sahi's CloudFront requires on the
//! upgrade; without a session there is no stream. The frames it carries
//! are pinned by `brokers::sahi::streaming::orders` and the feed corpus
//! (`sahi/order_lifecycle`).

use openalgo_desktop_lib::brokers::common::streaming::OrderFeed;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::sahi::SahiBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;

/// Web test_sahi_order_adapter.py::test_factory_without_session_returns_none,
/// ::test_factory_builds_adapter_from_stored_token and
/// ::test_handshake_targets_order_stream_with_bearer_and_user_agent.
#[test]
fn the_order_feed_is_the_order_stream_and_needs_a_session() {
    let b = SahiBroker::new(SymbolResolver::new());
    assert!(b.capabilities().order_feed);
    assert!(b.create_order_feed(&AuthToken::new(" ")).is_err());
    let Ok(OrderFeed::Socket(feed)) = b.create_order_feed(&AuthToken::new("jwt-token")) else {
        panic!("Sahi's order feed is its order stream");
    };
    assert_eq!(feed.broker(), "sahi");
    let req = feed.ws_request().expect("the order stream's upgrade");
    assert_eq!(
        req.uri().to_string(),
        "wss://direct.sahi.com/v1/stream/orders"
    );
    assert_eq!(req.headers()["authorization"], "Bearer jwt-token");
    assert_eq!(req.headers()["user-agent"], "openalgo-sahi/1.0");
}
