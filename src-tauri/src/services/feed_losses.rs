//! Market data received and what its readers missed (NEW-REL-05,
//! PROD-REL-05), for health (`/health/api/current`, `market_data`) and Run
//! diagnostics.
//!
//! Per broker feed: how its current connection is doing (connected, for
//! how long, the last update, the updates received since it began), and,
//! per reader, the updates it skipped because it had fallen behind the
//! feed's channel, both since that connection began and since OpenAlgo
//! started. Health judges by the skips since start: a reconnect does not
//! undo a price a stop never saw. For the apps on the market data feed
//! (port 8765), since OpenAlgo started: frames a slow app's queue folded
//! into a newer one for the same instrument (it gets the latest price, by
//! design, so this is reported apart and never warns), frames dropped past
//! its queue's cap, and updates the feed server skipped. The strategy
//! module's own missed prices, and its exits waiting to be placed, are in
//! `strategy_ticks`.

use crate::feed::MarketDataSource;
use crate::state::AppState;
use crate::websocket::{Reader, SinceConnect, WebSocketManager};
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// The broker feeds health reports: the market data feed always; the order
/// update socket (other brokers relay order updates on the market feed or
/// have the order book read) and the deeper-book socket (Fyers 50, Dhan
/// 20; the bridge relays it to apps on the feed like the market feed)
/// while they run, or once a reader skipped any of their updates, so that
/// stays listed after the socket stops.
fn feeds(ctx: &AppState) -> Vec<(&'static str, &WebSocketManager)> {
    listed(
        ctx.websocket.as_ref(),
        ctx.runtime.order_ws.as_ref(),
        ctx.runtime.depth_ws.as_ref(),
    )
}

/// The market data feed, then the order update and market depth sockets
/// that run or skipped, each under the name health reports it by.
fn listed<'a>(
    market_data: &'a WebSocketManager,
    order_updates: &'a WebSocketManager,
    market_depth: &'a WebSocketManager,
) -> Vec<(&'static str, &'a WebSocketManager)> {
    let mut out = vec![("market_data", market_data)];
    for (name, feed) in [
        ("order_updates", order_updates),
        ("market_depth", market_depth),
    ] {
        if feed.is_running() || total(&feed.stats().skipped_since_start()) > 0 {
            out.push((name, feed));
        }
    }
    out
}

fn total(skipped: &[(Reader, u64)]) -> u64 {
    skipped.iter().map(|(_, n)| n).sum()
}

fn by_reader(skipped: &[(Reader, u64)]) -> Map<String, Value> {
    skipped
        .iter()
        .map(|(r, n)| (r.name().to_string(), json!(n)))
        .collect()
}

/// One broker feed's entry: the current connection (`now`), and what each
/// reader skipped since it began and since OpenAlgo started.
fn feed_block(
    name: &str,
    connected: bool,
    now: &SinceConnect,
    since_start: &[(Reader, u64)],
    last_update_age: Option<Duration>,
) -> Value {
    json!({
        "name": name,
        "connected": connected,
        "connected_for_s": now.connected_for.filter(|_| connected).map(|d| d.as_secs()),
        "last_update_age_s": last_update_age.map(|d| d.as_secs()),
        "received_since_connect": now.received,
        "skipped_since_connect": by_reader(&now.skipped),
        "skipped_total_since_connect": now.skipped_total(),
        "skipped_since_start": by_reader(since_start),
        "skipped_total_since_start": total(since_start),
    })
}

fn feed_json(name: &str, feed: &WebSocketManager) -> Value {
    let stats = feed.stats();
    let mut v = feed_block(
        name,
        feed.is_connected(),
        &stats.since_connect(),
        &stats.skipped_since_start(),
        stats.last_event_age(),
    );
    v["connection"] = crate::services::feed_connections::json(feed);
    v
}

/// What the feed server did not send its apps since OpenAlgo started.
#[derive(Debug, Clone, Copy, Default)]
struct AppFrames {
    coalesced: u64,
    dropped: u64,
    skipped: u64,
}

/// The block from its parts: `status` is `warn` while any reader of a
/// broker feed skipped an update since OpenAlgo started, or an app was not
/// sent one (dropped or skipped). Frames folded into a newer price for a
/// slow app (`coalesced`) are by design and do not warn.
fn block(broker_feeds: Vec<Value>, apps: AppFrames) -> Value {
    let feed_skips = broker_feeds
        .iter()
        .any(|f| f["skipped_total_since_start"].as_u64().unwrap_or(0) > 0);
    let warn = feed_skips || apps.dropped > 0 || apps.skipped > 0;
    json!({
        "status": if warn { "warn" } else { "pass" },
        "broker_feeds": broker_feeds,
        "feed_clients": {
            "coalesced": apps.coalesced,
            "dropped": apps.dropped,
            "skipped": apps.skipped,
        },
    })
}

/// The `market_data` health block.
pub fn json(ctx: &AppState) -> Value {
    let broker_feeds: Vec<Value> = feeds(ctx).iter().map(|(n, f)| feed_json(n, f)).collect();
    let apps = match ctx.bridge.client_losses() {
        Some(l) => AppFrames {
            coalesced: l.coalesced.load(Ordering::Relaxed),
            dropped: l.dropped.load(Ordering::Relaxed),
            skipped: l.lagged.load(Ordering::Relaxed),
        },
        None => AppFrames::default(),
    };
    block(broker_feeds, apps)
}

/// What a trader reads about a reader that fell behind.
fn reader_words(r: Reader) -> &'static str {
    match r {
        Reader::Strategy => "strategies",
        Reader::Scalping => "scalping stops",
        Reader::Sandbox => "sandbox orders",
        Reader::FeedClients => "apps on the market data feed",
        Reader::OrderRelay => "order updates",
    }
}

/// The diagnostics sentence about a connected feed's updates since it
/// connected; empty before the first.
pub fn sentence(s: &SinceConnect) -> String {
    if s.received == 0 {
        return String::new();
    }
    let updates = if s.received == 1 { "update" } else { "updates" };
    let skipped = s.skipped_total();
    if skipped == 0 {
        return format!(
            "{} {} since it connected, none missed.",
            s.received, updates
        );
    }
    let who: Vec<String> = s
        .skipped
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(r, n)| format!("{} {}", reader_words(*r), n))
        .collect();
    format!(
        "{} {} since it connected; {} reached a reader that had fallen behind and were skipped ({}). Each acts on the next update.",
        s.received,
        updates,
        skipped,
        who.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn since(received: u64, skipped: &[(Reader, u64)]) -> SinceConnect {
        SinceConnect {
            connected_for: Some(Duration::from_secs(60)),
            received,
            skipped: Reader::ALL
                .iter()
                .map(|r| {
                    let n = skipped.iter().find(|(x, _)| x == r).map(|(_, n)| *n);
                    (*r, n.unwrap_or(0))
                })
                .collect(),
        }
    }

    #[test]
    fn the_sentence_names_who_skipped_and_how_many() {
        assert_eq!(sentence(&since(0, &[])), "");
        assert_eq!(
            sentence(&since(12_345, &[])),
            "12345 updates since it connected, none missed."
        );
        assert_eq!(
            sentence(&since(
                500,
                &[(Reader::Scalping, 30), (Reader::FeedClients, 10)]
            )),
            "500 updates since it connected; 40 reached a reader that had fallen behind and \
             were skipped (scalping stops 30, apps on the market data feed 10). Each acts on \
             the next update."
        );
    }

    fn readers(skipped: &[(Reader, u64)]) -> Vec<(Reader, u64)> {
        since(0, skipped).skipped
    }

    /// PROD-REL-05: the block judged by the skips since the feed connected,
    /// so a reconnect turned a feed whose readers had missed prices back to
    /// "pass" and its counts to zero.
    #[test]
    fn a_reconnect_does_not_hide_what_a_reader_skipped() {
        let feed = feed_block(
            "market_data",
            true,
            &since(40, &[]),
            &readers(&[(Reader::Scalping, 7), (Reader::Strategy, 2)]),
            Some(Duration::from_secs(1)),
        );
        let v = block(vec![feed], AppFrames::default());
        assert_eq!(v["status"], "warn", "{}", v);
        let f = &v["broker_feeds"][0];
        assert_eq!(f["skipped_total_since_connect"], 0, "{}", f);
        assert_eq!(f["skipped_since_start"]["scalping"], 7, "{}", f);
        assert_eq!(f["skipped_since_start"]["strategy"], 2, "{}", f);
        assert_eq!(f["skipped_since_start"]["sandbox"], 0, "{}", f);
        assert_eq!(f["skipped_total_since_start"], 9, "{}", f);
        assert_eq!(f["received_since_connect"], 40, "{}", f);
    }

    /// A slow app getting the newest price instead of every update is by
    /// design: reported on its own, never as a loss. A frame dropped or
    /// skipped for an app is a loss.
    #[test]
    fn folded_frames_are_reported_apart_and_do_not_warn() {
        let quiet = || feed_block("market_data", true, &since(10, &[]), &readers(&[]), None);
        let folded = AppFrames {
            coalesced: 120,
            ..AppFrames::default()
        };
        let v = block(vec![quiet()], folded);
        assert_eq!(v["status"], "pass", "{}", v);
        assert_eq!(v["feed_clients"]["coalesced"], 120, "{}", v);
        let dropped = AppFrames {
            dropped: 1,
            ..folded
        };
        assert_eq!(block(vec![quiet()], dropped)["status"], "warn");
    }

    /// PROD-REL-05: the bridge relays the deeper-book socket to apps on the
    /// feed and counts what it skipped against that socket, but health
    /// listed only the market data and order update feeds, so those skips
    /// never reached it.
    #[test]
    fn a_depth_feed_that_skipped_is_listed() {
        let (market, orders, depth) = (
            WebSocketManager::new(),
            WebSocketManager::new(),
            WebSocketManager::new(),
        );
        let names = |l: &[(&'static str, &WebSocketManager)]| -> Vec<&'static str> {
            l.iter().map(|(n, _)| *n).collect()
        };
        assert_eq!(names(&listed(&market, &orders, &depth)), ["market_data"]);

        depth.stats().note_skipped(Reader::FeedClients, 4);
        let feeds = listed(&market, &orders, &depth);
        assert_eq!(names(&feeds), ["market_data", "market_depth"]);
        let v = block(
            feeds.iter().map(|(n, f)| feed_json(n, f)).collect(),
            AppFrames::default(),
        );
        assert_eq!(v["status"], "warn", "{}", v);
        let depth_block = &v["broker_feeds"][1];
        assert_eq!(depth_block["name"], "market_depth", "{}", depth_block);
        assert_eq!(
            depth_block["skipped_since_start"]["feed_clients"], 4,
            "{}",
            depth_block
        );
    }
}
