//! How many instruments each broker's market data feed carries per
//! connection and how many connections it opens for them (audit C07), in
//! one place. The web pools every broker the same way
//! (`websocket_proxy/connection_manager.ConnectionPool`: 1,000 a
//! connection, up to 3, `base_adapter.py:24-32`); the desktop keeps that
//! wherever the broker allows it, and lowers it where the broker's own
//! limit, or another socket the session opens on the same account, would
//! make a further connection evict one, refuse its subscriptions, or never
//! connect. The web pools those brokers past their limits: recorded
//! deviations, each with its source.

use crate::websocket::PoolLimits;

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
        // PiConnect allows one WebSocket session per user and token (web
        // broker/flattrade/streaming/flattrade_adapter.py:48-55); order
        // updates ride that socket here.
        "flattrade" => PoolLimits::single(1000),
        // Market Data Feed V3 allows two connections on the Standard plan
        // (web broker/upstox/streaming/upstox_adapter.py:37-46: "the pool
        // default of 3 is one too many").
        "upstox" => PoolLimits {
            per_connection: 1000,
            connections: 2,
        },
        // 300 instruments a connection, refused past it (web
        // hdfcsky_adapter.py:239-254), three sockets a key, one of which
        // the REST quote snapshots open for a moment (hdfcsky_mapping.py:
        // 43-46, api/data.py:440-490). The web never opens a second
        // connection (its pool waits for 1,000), so it tops out at 300.
        "hdfcsky" => PoolLimits {
            per_connection: 300,
            connections: 2,
        },
        // Three sockets a key, one for the REST quote snapshots
        // (hdfcsecurities_mapping.py:43-46).
        "hdfcsecurities" => PoolLimits {
            per_connection: 1000,
            connections: 2,
        },
        // Three sockets a client code, one of them the order socket (web
        // broker/angel/streaming/angel_order_adapter.py:5-8).
        "angel" => PoolLimits {
            per_connection: 1000,
            connections: 2,
        },
        // XTS subscriptions belong to the market-data session, not the
        // socket: every socket of a session streams all of them, so a
        // second socket adds no room and doubles every tick. One socket
        // carries the web pool's 3,000.
        id if XTS.contains(&id) => PoolLimits::single(3000),
        _ => PoolLimits::WEB_DEFAULT,
    }
}

/// The deeper-book socket's room: Dhan's 20 levels stream for at most 50
/// instruments (web broker/dhan/streaming/dhan_mapping.py:100, refused past
/// it with "Maximum 50 subscriptions allowed for 20-depth").
pub(crate) fn depth_limits(broker: &str) -> PoolLimits {
    match broker {
        "dhan" => PoolLimits::single(50),
        _ => PoolLimits::single(3000),
    }
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
        assert_eq!(pool_limits("fivepaisa").connections, 1);
        assert_eq!(pool_limits("hdfcsky").per_connection, 300);
        assert_eq!(pool_limits("compositedge"), PoolLimits::single(3000));
        assert_eq!(depth_limits("dhan"), PoolLimits::single(50));
    }
}
