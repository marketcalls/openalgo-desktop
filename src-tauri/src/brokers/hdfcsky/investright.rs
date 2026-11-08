//! What HDFC Sky's feed and HDFC Securities' InvestRight feed share. The
//! two are one protocol: the web's `hdfcsky_market_pb2.py` and
//! `hdfcsecurities_market_pb2.py` describe the same messages, and their
//! `*_websocket.py` parsers and `*_adapter.py` normalisers are the same
//! code. Both desktop feeds shape their ticks here.

use crate::brokers::common::streaming::{FeedMode, NormalizedTick, QuoteField};

/// A band limit as sent: proto3 writes 0 for a limit the packet does not
/// carry, and no instrument has a limit of 0.
pub(crate) fn limit(v: f64) -> Option<f64> {
    (v != 0.0).then_some(v)
}

/// Whether instruments of this OpenAlgo exchange have open interest: the
/// derivative segments. A cash equity's packet has no OI, and proto3 then
/// reads as 0 (the web publishes that 0).
fn has_open_interest(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO" | "CDS" | "MCX")
}

/// Say what a tick's packet carries (audit C03 to C05). The subscription
/// type decides the packet: an `LTP` scrip gets its price and trade time,
/// with the previous close only when one is sent; an `ALL` scrip gets the
/// full packet, whose OI counts on a derivative segment only. An index
/// packet is a full packet at every mode, Depth included (the web publishes
/// it on the Depth topic too), with no volume, order flow or OI.
pub(crate) fn mark_carriage(t: &mut NormalizedTick, subscribed: FeedMode, index: bool) {
    use QuoteField as F;
    if subscribed == FeedMode::Ltp {
        t.mode = 1;
        if t.close > 0.0 {
            t.mark_carried(F::Close);
            t.mark_carried(F::Change);
        }
        return;
    }
    if index {
        t.mode = 3;
        for f in [
            F::Volume,
            F::LastQuantity,
            F::AveragePrice,
            F::TotalBuyQuantity,
            F::TotalSellQuantity,
            F::OpenInterest,
        ] {
            t.mark_absent(f);
        }
        return;
    }
    t.mode = 2;
    if has_open_interest(&t.exchange) {
        t.mark_carried(F::OpenInterest);
    }
}
