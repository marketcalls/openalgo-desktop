//! The words a node reads when its call is refused or never answered,
//! pinned as whole literals so a changed constant cannot pass with itself
//! (parity CL-26, CL-09).

use super::*;
use crate::agent::tools::testkit::Kit;
use crate::brokers::mock::MockCall;
use crate::services::apikey_service::ApiKeyService;

/// CL-26: a call that got no answer (timed out, cut off, or over the
/// token's budget) may have taken effect, so the node is told an order may
/// have been placed and to check the order book, and the answer is marked
/// unknown. Only a call that was never sent says nothing changed.
#[test]
fn an_unanswered_call_says_an_order_may_have_been_placed() {
    let may_have = "OpenAlgo did not answer in time. If this was an order, it may have been placed: check the order book before running the workflow again.";
    assert_eq!(
        transport_failure(Transport::Timeout),
        json!({"status": "error", "error": may_have, "code": 504, "error_type": "unknown_outcome"})
    );
    for cut_off in [Transport::NoReply, Transport::Budget] {
        assert_eq!(
            transport_failure(cut_off),
            json!({"status": "error", "error": may_have, "code": 500, "error_type": "unknown_outcome"})
        );
    }
    assert_eq!(
        transport_failure(Transport::NotSent),
        json!({"status": "error",
               "error": "The request could not be sent to OpenAlgo, so nothing was changed.",
               "code": 500})
    );
}

/// CL-09: Semi-Auto refuses both of Flow's closes, the scoped one and
/// close-all, in the web's whole sentence with its final full stop
/// (`close_position_service`), and nothing reaches the broker.
#[tokio::test]
async fn semi_auto_refuses_both_closes_in_the_webs_whole_sentence() {
    let kit = Kit::new();
    ApiKeyService::set_order_mode(kit.app(), "semi_auto").unwrap();
    let client = AppFlowClient::new(
        std::sync::Arc::downgrade(kit.app()),
        Secret::new(kit.key.clone()),
    );
    let refused = json!({"status": "error",
        "error": "Close position operation is not allowed in Semi-Auto mode. Please switch to Auto mode to close positions.",
        "code": 403});
    assert_eq!(
        client
            .close_position("SBIN", "NSE", "MIS", "Golden Flow")
            .await,
        refused
    );
    assert_eq!(client.close_all_positions().await, refused);
    let calls = kit.mock.calls();
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, MockCall::CloseAll | MockCall::PlaceOrder(_))),
        "{:?}",
        calls
    );
}
