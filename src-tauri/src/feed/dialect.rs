//! Where each broker's web adapter shapes the feed differently: the keys
//! its `market_data.data` carries beside the canonical ones and how it
//! fills `data.mode` (`names`, WS-05 and audit C06), and the depth a Depth
//! subscribe is acknowledged with.
//!
//! The desktop renders one canonical schema for every broker (the names the
//! recorded Zerodha transcript carries: `last_quantity`, `average_price`,
//! `price_change`, `price_change_percent`, `oi` and `open_interest`). The web
//! forwards each broker adapter's own dict, so an SDK script written against
//! the web with, say, Angel reads `data["last_trade_quantity"]`. For each
//! broker the desktop adds every key its web adapter publishes, with the
//! same value, beside the canonical one: no client loses a key and none
//! meets a KeyError. Zerodha's frames stay as recorded.
//!
//! Deliberate deviation from the web: the canonical names always go out, a
//! broker's own names go out from the lowest mode its adapter sends them in
//! upward, and a key whose value the broker did not send is left out with
//! its canonical key rather than sent as 0 (web openalgo#2376).

use super::source::{Mode, DEFAULT_DEPTH};

pub(super) mod names;
pub(crate) use names::{dialect, DataMode, Dialect, Extra, Ltt, Source, Unit};

/// How a broker's web adapter fills `actual_depth`, which the web's proxy
/// puts in a subscribe acknowledgment (`websocket_proxy/server.py`:
/// `response.get("actual_depth", depth_level)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepthAck {
    /// No `actual_depth` (Zerodha, Fyers, Upstox, Kotak and the others in
    /// [`depth_ack`]): the depth asked for, in every mode, whatever is
    /// streamed.
    Echo,
    /// The level it falls back to in Depth mode, the depth asked for
    /// otherwise (Angel, Dhan, Definedge, the XTS family, Motilal, Nubra,
    /// Pocketful, Samco, Tradejini; the default for a broker not listed).
    Fallback,
    /// The level it falls back to in Depth mode, `null` otherwise: Arrow,
    /// both HDFC adapters and Rupeezy send `actual_depth=None` outside
    /// Depth mode.
    FallbackElseNull,
    /// Always 5, in every mode (IIFL Capital).
    Five,
}

fn depth_ack(broker: &str) -> DepthAck {
    match broker {
        "zerodha" | "fyers" | "upstox" | "kotak" | "groww" | "aliceblue" | "shoonya"
        | "flattrade" | "zebu" | "tradesmart" | "firstock" | "fivepaisa" | "indmoney"
        | "mstock" | "paytm" | "deltaexchange" => DepthAck::Echo,
        "arrow" | "hdfcsecurities" | "hdfcsky" | "rupeezy" => DepthAck::FallbackElseNull,
        "iiflcapital" => DepthAck::Five,
        _ => DepthAck::Fallback,
    }
}

/// A subscribe's book depth: the levels streamed, and the depth its
/// acknowledgment names (`None`: JSON `null`), exactly as the broker's web
/// adapter answers (maintainer decision 2026-10-16: like the web). `None`
/// only when the broker streams no level at all.
///
/// Depth mode streams the largest level the broker serves up to the one
/// asked for, else its smallest (the web's `get_fallback_depth_level`
/// defaults to 5); other modes need no book. Because an echoing broker
/// names the depth asked for (20 on Zerodha, which streams 5), a client
/// reads the depth it gets from the frames, not from the acknowledgment.
pub(crate) fn subscribe_depth(
    broker: &str,
    mode: Mode,
    requested: i64,
    supported: &[u8],
) -> Option<(u8, Option<i64>)> {
    let rule = depth_ack(broker);
    if mode != Mode::Depth {
        let named = match rule {
            DepthAck::Echo | DepthAck::Fallback => Some(requested),
            DepthAck::FallbackElseNull => None,
            DepthAck::Five => Some(5),
        };
        return Some((DEFAULT_DEPTH, named));
    }
    let streamed = supported
        .iter()
        .copied()
        .filter(|d| i64::from(*d) <= requested)
        .max()
        .or_else(|| supported.iter().copied().min())?;
    let named = match rule {
        DepthAck::Echo => requested,
        DepthAck::Five => 5,
        DepthAck::Fallback | DepthAck::FallbackElseNull => i64::from(streamed),
    };
    Some((streamed, Some(named)))
}

#[cfg(test)]
mod tests {
    use super::subscribe_depth;
    use crate::feed::Mode;

    /// Maintainer decision 2026-10-16, like the web: Zerodha, whose web
    /// adapter has no `actual_depth`, names the depth asked for in every mode
    /// and streams what it serves.
    #[test]
    fn an_echoing_broker_names_the_depth_asked_for() {
        let depth = |requested, supported: &[u8]| {
            subscribe_depth("zerodha", Mode::Depth, requested, supported)
        };
        assert_eq!(depth(20, &[5]), Some((5, Some(20))));
        assert_eq!(depth(50, &[5]), Some((5, Some(50))));
        assert_eq!(depth(5, &[5]), Some((5, Some(5))));
        assert_eq!(depth(3, &[5]), Some((5, Some(3))));
        assert_eq!(
            subscribe_depth("fyers", Mode::Depth, 50, &[5, 50]),
            Some((50, Some(50)))
        );
        assert_eq!(
            subscribe_depth("zerodha", Mode::Ltp, 20, &[5]),
            Some((5, Some(20)))
        );
    }

    /// Angel's and Dhan's web adapters name the level they fall back to
    /// (the highest served up to the one asked for, else 5).
    #[test]
    fn a_falling_back_broker_names_the_level_streamed() {
        assert_eq!(
            subscribe_depth("angel", Mode::Depth, 20, &[5]),
            Some((5, Some(5)))
        );
        assert_eq!(
            subscribe_depth("dhan", Mode::Depth, 50, &[5, 20]),
            Some((20, Some(20)))
        );
        assert_eq!(
            subscribe_depth("angel", Mode::Depth, 3, &[5]),
            Some((5, Some(5)))
        );
        assert_eq!(
            subscribe_depth("angel", Mode::Quote, 20, &[5]),
            Some((5, Some(20)))
        );
        assert_eq!(subscribe_depth("angel", Mode::Depth, 5, &[]), None);
    }

    /// Arrow, both HDFC adapters and Rupeezy send `actual_depth=None`
    /// outside Depth mode, so the web answers `"depth": null` there; IIFL
    /// Capital always names 5.
    #[test]
    fn arrow_and_hdfc_name_no_depth_outside_depth_mode_and_iifl_capital_five() {
        for broker in ["arrow", "hdfcsecurities", "hdfcsky", "rupeezy"] {
            for mode in [Mode::Ltp, Mode::Quote] {
                assert_eq!(subscribe_depth(broker, mode, 5, &[5]), Some((5, None)));
            }
            assert_eq!(
                subscribe_depth(broker, Mode::Depth, 20, &[5]),
                Some((5, Some(5)))
            );
        }
        for mode in [Mode::Ltp, Mode::Depth] {
            assert_eq!(
                subscribe_depth("iiflcapital", mode, 20, &[5]),
                Some((5, Some(5)))
            );
        }
    }
}
