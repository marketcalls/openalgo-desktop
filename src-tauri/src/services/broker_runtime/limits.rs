//! How many instruments each broker's market data feed carries per
//! connection and how many connections it opens for them (audit C07), in
//! one place. The web pools every broker the same way, with no broker
//! opting out (`websocket_proxy/connection_manager.ConnectionPool`: 1,000 a
//! connection, up to 3, `base_adapter.py:24-32`, `broker_factory.py:184`);
//! the desktop keeps that wherever the broker allows it, and lowers it
//! where the broker's own documented limit, or another socket the session
//! opens on the same key, would make a further connection evict one,
//! refuse its subscriptions, or never connect. The web pools those
//! brokers past their limits: recorded deviations, each with its source
//! (web a1b9e3250).

use crate::websocket::PoolLimits;
use std::time::Duration;

/// The Symphony XTS members.
const XTS: [&str; 7] = [
    "fivepaisaxts",
    "jainamxts",
    "compositedge",
    "rmoney",
    "ibulls",
    "wisdom",
    "iifl",
];

/// The market data pool for `broker`.
pub(crate) fn pool_limits(broker: &str) -> PoolLimits {
    match broker {
        // One feed per token and client code: a second socket evicts the
        // first, and both then flap (web services/order_update_service.py:
        // 89-97, broker/fivepaisa/streaming/fivepaisa_order_adapter.py:7-18).
        "fivepaisa" => PoolLimits::single(1000),
        // PiConnect allows one WebSocket session per user and token: another
        // one evicts it (web broker/flattrade/streaming/flattrade_adapter.py:
        // 46-55, issue #1965); order updates ride that socket here.
        "flattrade" => PoolLimits::single(1000),
        // Market Data Feed V3: two connections per user on the Standard plan
        // (web broker/upstox/streaming/upstox_adapter.py:36-47: "the generic
        // default of 3 is ONE TOO MANY for a Standard account").
        "upstox" => PoolLimits {
            per_connection: 1000,
            connections: 2,
        },
        // "Up to 300 instruments per connection and up to three simultaneous
        // connections per API key" (web hdfcsky_mapping.py:42-46), refused
        // past 300 (hdfcsky_adapter.py:239-254); the REST quote snapshots
        // open a feed socket of their own for a moment (`hdfcsky::data`,
        // web api/data.py:440-490), so the feed keeps one free. The web's
        // pool never opens a second connection (it waits for 1,000), so it
        // tops out at 300.
        "hdfcsky" => PoolLimits {
            per_connection: 300,
            connections: 2,
        },
        // "Up to 1500 instruments on a single connection and up to three
        // simultaneous connections per API key" (web
        // hdfcsecurities_mapping.py:42-46); one is kept for the REST quote
        // snapshots' sockets, as for HDFC Sky. The web pool's 1,000 a
        // connection stays.
        "hdfcsecurities" => PoolLimits {
            per_connection: 1000,
            connections: 2,
        },
        // "Per Sahi docs": 1,000 instruments a connection and five
        // connections a token (web broker/sahi/streaming/sahi_mapping.py:
        // 67-69); the web's adapter refuses the 1,001st with
        // MAX_SYMBOLS_REACHED "Sahi allows 1000 instruments per connection"
        // (sahi_adapter.py:221-225), which its pool, moving on at 1,000,
        // never reaches. The feed's own bound is the same number
        // (`sahi::streaming::MAX_INSTRUMENTS`), so no instrument past it is
        // acknowledged and then dropped; the three connections stay within
        // the five.
        "sahi" => PoolLimits {
            per_connection: crate::brokers::sahi::streaming::MAX_INSTRUMENTS,
            connections: 3,
        },
        // XTS subscribes over REST with the market-data session's token
        // (web broker/fivepaisaxts/streaming/fivepaisaxts_websocket.py:27-29),
        // so the subscriptions belong to the session, not a socket: every
        // socket of the session streams all of them, a second one adds no
        // room and doubles every tick. One socket carries the web pool's
        // 3,000.
        id if XTS.contains(&id) => PoolLimits::single(3000),
        _ => PoolLimits::WEB_DEFAULT,
    }
}

/// The deeper-book socket's room: Dhan's 20 levels stream for at most 50
/// instruments, refused past it with "Maximum 50 subscriptions allowed for
/// 20-depth" (web broker/dhan/streaming/dhan_mapping.py:99-100,
/// dhan_adapter.py:389-396).
pub(crate) fn depth_limits(broker: &str) -> PoolLimits {
    match broker {
        "dhan" => PoolLimits::single(50),
        _ => PoolLimits::single(3000),
    }
}

/// How the deeper-book socket's broker words a subscribe past its room,
/// where its web adapter refuses it before the pool does: Dhan's
/// SUBSCRIPTION_LIMIT (dhan_adapter.py:395). Others: the pool's words.
pub(crate) fn depth_room_refusal(broker: &str) -> Option<&'static str> {
    match broker {
        "dhan" => Some("Maximum 50 subscriptions allowed for 20-depth"),
        _ => None,
    }
}

/// How long a deep book may stay silent before the instrument falls back
/// to the main feed's levels, for a broker whose web adapter does so (audit
/// P-10): Dhan's 20 levels, 30 s from the subscription and after the last
/// book (web broker/dhan/streaming/dhan_adapter.py:409-412, 972-1066). Fyers'
/// 50-level TBT book has no such fallback on the web.
pub(crate) fn deep_book_timeout(broker: &str) -> Option<Duration> {
    (broker == "dhan").then_some(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every broker has a pool, the web's unless its own limits are lower,
    /// and no pool is larger than the web's 3,000 instruments.
    #[test]
    fn every_broker_has_a_pool_no_larger_than_the_webs() {
        for id in crate::brokers::catalog::ALL_BROKERS {
            let l = pool_limits(id);
            assert!(l.connections >= 1 && l.per_connection >= 1, "{id}");
            assert!(l.total() <= PoolLimits::WEB_DEFAULT.total(), "{id}");
        }
        assert_eq!(pool_limits("zerodha"), PoolLimits::WEB_DEFAULT);
        assert_eq!(pool_limits("angel"), PoolLimits::WEB_DEFAULT);
        assert_eq!(pool_limits("fivepaisa").connections, 1);
        assert_eq!(pool_limits("hdfcsky").per_connection, 300);
        assert_eq!(pool_limits("sahi").per_connection, 1000);
        assert_eq!(pool_limits("sahi").total(), 3000);
        assert_eq!(pool_limits("compositedge"), PoolLimits::single(3000));
        assert_eq!(depth_limits("dhan"), PoolLimits::single(50));
        assert_eq!(
            depth_room_refusal("dhan"),
            Some("Maximum 50 subscriptions allowed for 20-depth")
        );
        assert_eq!(depth_room_refusal("fyers"), None);
        assert_eq!(deep_book_timeout("dhan"), Some(Duration::from_secs(30)));
        assert_eq!(deep_book_timeout("fyers"), None);
    }

    /// A feed that bounds its own connection (and logs what it leaves out)
    /// is never handed more than that bound, so no instrument is
    /// acknowledged and then dropped (audit C02).
    #[test]
    fn no_pool_hands_a_connection_more_than_its_feed_takes() {
        use crate::brokers::{iiflcapital, kotak, rupeezy, sahi};
        for (id, bound) in [
            ("sahi", sahi::streaming::MAX_INSTRUMENTS),
            ("rupeezy", rupeezy::streaming::MAX_INSTRUMENTS),
            ("kotak", kotak::streaming::MAX_SUBSCRIPTIONS),
            ("iiflcapital", iiflcapital::streaming::MAX_INSTRUMENTS),
        ] {
            assert!(pool_limits(id).per_connection <= bound, "{id}");
        }
    }
}
