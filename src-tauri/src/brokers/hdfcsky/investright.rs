//! What HDFC Sky's feed and HDFC Securities' InvestRight feed share. The
//! two are one protocol: the web's `hdfcsky_market_pb2.py` and
//! `hdfcsecurities_market_pb2.py` describe the same messages, and their
//! `*_websocket.py` parsers and `*_adapter.py` normalisers are the same
//! code. Both desktop feeds shape their ticks and read their text frames
//! here.

use crate::brokers::common::streaming::{FeedEvent, FeedMode, NormalizedTick, QuoteField};
use serde_json::Value;

/// What a full packet says beyond the quote fields its tick copies.
pub(crate) struct PacketFacts {
    /// The instrument's subscription mode: an `LTP` scrip's packet is its
    /// price, an `ALL` scrip's the full quote.
    pub subscribed: FeedMode,
    /// An `indexData` packet.
    pub index: bool,
    /// `lowerCircuitLimit` and `upperCircuitLimit` as sent.
    pub band: (f64, f64),
    /// `lastTradeTime` as sent.
    pub ltt: i64,
    /// The packet carries book levels.
    pub book: bool,
}

/// Shape a tick that copies a full packet's quote fields, as both feeds
/// make it: the circuit band (in `extras`, Depth frames name it as the web
/// does, hdfcsky_adapter.py:447-452), the trade time, what the packet
/// carries, and the change from the previous close.
///
/// A last traded price of 0 is one the packet did not send (proto3), an
/// instrument not traded yet: the tick marks it absent, so no LTP or Quote
/// frame shows a zero price (the web publishes ltp 0). Such a packet is no
/// full tick either (mode 2, so the bridge pairs no book with it): its book
/// goes on its own to Depth holders, with ltp 0, as a book before the first
/// quote does.
///
/// A full packet without book levels goes to Quote and LTP holders only
/// (mode 2). The web sends its Depth frame with no `depth` key: proto3
/// cannot tell an empty level list from one not sent, and it may mean a
/// cleared book. On the desktop a full tick would carry the connection's
/// last book to Depth holders, which could show liquidity that is gone; a
/// frame without the book needs a bridge switch (lane wire), and until then
/// a Depth holder gets no frame for such a packet.
pub(crate) fn finish(t: &mut NormalizedTick, p: PacketFacts) {
    t.extras.lower_circuit = limit(p.band.0);
    t.extras.upper_circuit = limit(p.band.1);
    set_trade_time(t, p.ltt);
    mark_carriage(t, p.subscribed, p.index);
    if t.ltp == 0.0 {
        t.mark_absent(QuoteField::Ltp);
    }
    if t.ltp == 0.0 || (!p.book && !p.index) {
        t.mode = t.mode.min(2);
        // A derivative's OI is still in the packet, beyond mode 2.
        if t.mode == 2 && !t.absent.contains(QuoteField::OpenInterest) {
            t.mark_carried(QuoteField::OpenInterest);
        }
    }
    t.derive_change();
}

/// A band limit as sent: proto3 writes 0 for a limit the packet does not
/// carry, and no instrument has a limit of 0.
fn limit(v: f64) -> Option<f64> {
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
/// full packet (mode 3: the quote, the book and the OI), whose OI counts on
/// a derivative segment only, and which reaches Depth holders with or
/// without a book in it, as the web publishes it on the Depth topic. An
/// index packet is a full packet at every mode too, with no volume, order
/// flow or OI.
fn mark_carriage(t: &mut NormalizedTick, subscribed: FeedMode, index: bool) {
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
    t.mode = 3;
    if !has_open_interest(&t.exchange) {
        t.mark_absent(F::OpenInterest);
    }
}

/// `lastTradeTime` in epoch milliseconds. HDFC sends it in epoch seconds
/// (the web's own test pairs `lastTradeTime` 1700000000 with
/// `packetTimestamp` 1700000000000), and every time on the feed is epoch
/// ms (`docs/prompt/websockets-format.md`); the web publishes the seconds
/// unconverted as `ltt`. A value already in milliseconds is kept.
pub(crate) fn trade_time_ms(sent: i64) -> i64 {
    if sent > 1_000_000_000_000 {
        sent
    } else {
        sent.saturating_mul(1000)
    }
}

/// The tick's trade time from the packet's `lastTradeTime`. A packet with
/// none (an index, an instrument not traded yet) has no trade time, and
/// its frames leave `ltt` out: the web falls back to the packet time
/// (`tick.get("ltt", tick.get("timestamp"))`), which no trade happened at.
pub(crate) fn set_trade_time(t: &mut NormalizedTick, sent: i64) {
    if sent > 0 {
        t.last_trade_time_ms = trade_time_ms(sent);
    } else {
        t.mark_absent(QuoteField::LastTradeTime);
    }
}

/// The web's `_AUTH_FAILURE_INDICATORS` (both `*_websocket.py`).
const AUTH_FAILURE: &[&str] = &[
    "401",
    "403",
    "unauthorized",
    "unauthorised",
    "forbidden",
    "invalid token",
    "token expired",
    "session expired",
    "invalid api_key",
];

/// What a text frame says (web `_on_ws_message`: text frames are
/// acknowledgements and errors; neither is market data). An error or
/// failure whose text names a token or auth problem
/// (`_is_fatal_auth_error`) ends the session: the feed stops until the
/// trader logs in again. The web keeps the socket until it closes, then
/// retries with the token in its database and stops when that is
/// unchanged; the desktop's token changes only with a new login, so it
/// stops at once. Any other notice publishes nothing.
pub(crate) fn text_events(text: &str, broker: &str) -> Vec<FeedEvent> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        tracing::debug!("{} feed sent a text frame that is not JSON", broker);
        return Vec::new();
    };
    let status = match v.get("status") {
        Some(Value::String(s)) => s.to_ascii_lowercase(),
        _ => String::new(),
    };
    if status != "error" && status != "failure" {
        tracing::debug!("{} feed notice: {} bytes", broker, text.len());
        return Vec::new();
    }
    let lower = text.to_ascii_lowercase();
    if !AUTH_FAILURE.iter().any(|k| lower.contains(k)) {
        tracing::warn!(
            "{} feed reported an error that is not about the login",
            broker
        );
        return Vec::new();
    }
    tracing::warn!("{} feed refused the login session", broker);
    vec![FeedEvent::AuthFailed(format!(
        "{} ended the live data session. Log in to {} again.",
        broker, broker
    ))]
}
