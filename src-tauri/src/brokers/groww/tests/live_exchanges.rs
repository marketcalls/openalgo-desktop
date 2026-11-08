//! The Groww part of web openalgo#2341 (`f5805220b`; the web commit has
//! no Groww test): indices stream on the SDK's index topic, and an
//! exchange Groww's live feed does not publish is refused, never sent.
//!
//! The web's other index fix (its dispatcher stripped `_INDEX` from the
//! exchange, so no subscription matched an index tick) has no
//! counterpart: the desktop routes a message by its NATS sid.

use super::*;
use crate::brokers::Broker;

/// growwapi `FeedConstants.get_live_index_topic`:
/// `/ld/indices/{nse|bse}/price.{token}`, NSE indices with the symbol as
/// the token; Groww publishes no index under `/ld/eq`. Depth on an index
/// falls back to its price.
#[test]
fn indices_stream_on_the_index_topic() {
    assert_eq!(
        subjects(&sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Quote)),
        ["/ld/indices/nse/price.NIFTY"]
    );
    assert_eq!(
        subjects(&sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Depth)),
        ["/ld/indices/nse/price.NIFTY"]
    );
    assert_eq!(
        subjects(&sub("SENSEX", "BSE_INDEX", "1", FeedMode::Ltp)),
        ["/ld/indices/bse/price.1"]
    );
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    let frames = f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Ltp)]);
    assert_eq!(
        text(&frames[0]),
        "SUB /ld/indices/nse/price.NIFTY 1\r\nPING\r\n"
    );
}

/// Groww's feed publishes NSE and BSE cash, F&O and indices only: a feed
/// subscribe on any other exchange is refused with a message for the
/// trader (the web's adapter answers `EXCHANGE_NOT_SUPPORTED` with the
/// same text), and the feed never sends it.
#[test]
fn an_exchange_groww_does_not_stream_is_refused() {
    let b = GrowwBroker::new(master());
    for ex in ["MCX", "NCO", "NCDEX", "CDS", "BCD"] {
        assert_eq!(
            b.feed_refusal(ex).as_deref(),
            Some(
                format!(
                    "Groww does not provide live streaming for {}. Live prices are available for NSE, BSE, NFO, BFO and indices.",
                    ex
                )
                .as_str()
            ),
            "{}",
            ex
        );
    }
    for ex in ["NSE", "BSE", "NFO", "BFO", "NSE_INDEX", "BSE_INDEX"] {
        assert_eq!(b.feed_refusal(ex), None, "{}", ex);
    }
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    let mcx = sub("CRUDEOIL19NOV26FUT", "MCX", "472780", FeedMode::Quote);
    assert!(f.subscribe_frames(&[mcx]).is_empty());
    assert_eq!(f.instrument_count(), 0);
}
