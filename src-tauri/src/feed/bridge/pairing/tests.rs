//! Audit R107-17: pairing is decided inside one broker packet.

use super::*;
use crate::brokers::common::streaming::NormalizedDepth;
use crate::feed::bridge::render::tests::full_tick;

fn book(symbol: &str) -> FeedEvent {
    FeedEvent::Depth(NormalizedDepth {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        ..Default::default()
    })
}

fn kinds(steps: &[Step<'_>]) -> Vec<&'static str> {
    steps
        .iter()
        .map(|s| match s {
            Step::One(FeedEvent::Tick(_)) => "tick",
            Step::One(_) => "book",
            Step::Pair(..) => "pair",
        })
        .collect()
}

/// A full tick pairs with its own instrument's book right behind it in the
/// packet; another instrument's book, a book first, a tick without a price
/// and an index tick do not pair.
#[test]
fn a_full_tick_pairs_with_its_book_right_behind_it() {
    let sbin = FeedEvent::Tick(full_tick("SBIN", "NSE"));
    let tcs = FeedEvent::Tick(full_tick("TCS", "NSE"));
    let mut priceless = full_tick("SBIN", "NSE");
    priceless.mark_absent(QuoteField::Ltp);
    let priceless = FeedEvent::Tick(priceless);
    let index = FeedEvent::Tick(full_tick("NIFTY", "NSE_INDEX"));
    let (sbin_book, tcs_book) = (book("SBIN"), book("TCS"));

    let packet = [&sbin, &sbin_book, &tcs, &tcs_book];
    assert_eq!(kinds(&steps(&packet)), ["pair", "pair"]);
    assert_eq!(kinds(&steps(&[&sbin, &tcs_book])), ["tick", "book"]);
    assert_eq!(kinds(&steps(&[&sbin_book, &sbin])), ["book", "tick"]);
    assert_eq!(kinds(&steps(&[&priceless, &sbin_book])), ["tick", "book"]);
    let index_book = FeedEvent::Depth(NormalizedDepth {
        symbol: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        ..Default::default()
    });
    assert_eq!(kinds(&steps(&[&index, &index_book])), ["tick", "book"]);
    assert_eq!(kinds(&steps(&[&sbin])), ["tick"]);
}
