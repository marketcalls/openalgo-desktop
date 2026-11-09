//! Resolving the order-update stream's bare ids (audit RB-11). The REST
//! lookup itself is pinned end to end by the corpus case
//! `indmoney/order_unknown_id`.

use super::master;
use crate::brokers::indmoney::IndmoneyBroker;

/// Web indmoney_order_adapter.py:60-66: the book is read once per unknown
/// order in 5 s, per order rather than globally, so a burst for one order
/// reads it once and another order is not held up.
#[test]
fn the_book_is_read_once_per_unknown_order_in_five_seconds() {
    let b = IndmoneyBroker::new(master());
    assert!(b.claim_book_read("96057999"));
    assert!(!b.claim_book_read("96057999"));
    assert!(b.claim_book_read("96058000"));
}
