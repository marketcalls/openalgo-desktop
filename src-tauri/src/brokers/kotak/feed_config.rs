//! Kotak feed host lookup (web `streaming/kotak_feed_config.py`): the
//! market-data host each data centre streams from, looked up at login and
//! before a resumed session's first connect. The legacy HSM protocol is not
//! read: a data centre that selects it streams from SFeed
//! ([`feed_url_from_config`]), as on the web.

use super::KotakBroker;
use serde_json::Value;
use std::time::Duration;

pub const DEFAULT_SFEED_URL: &str = "wss://sfeed.kotaksecurities.com/apifeed";

/// `https://` -> `wss://`, a bare host gets `wss://` (web
/// `_to_websocket_scheme`).
pub fn to_wss(url: &str) -> String {
    let u = url.trim();
    if let Some(r) = u.strip_prefix("https://") {
        format!("wss://{}", r)
    } else if let Some(r) = u.strip_prefix("http://") {
        format!("ws://{}", r)
    } else if u.starts_with("wss://") || u.starts_with("ws://") {
        u.to_string()
    } else {
        format!("wss://{}", u)
    }
}

/// The market-data URL a config answer gives a data centre, and the
/// source (`sh` SFeed, `ks` cdtstream). The legacy HSM source (`hs`) falls
/// back to the default SFeed host with no source, and says so, as on the
/// web (`kotak_feed_config.py`): OpenAlgo reads only the SFeed protocol,
/// and nothing has selected HSM since September 2026.
pub fn feed_url_from_config(configs: &Value, data_center: &str) -> (Option<String>, String) {
    let get = |k: String| {
        configs
            .get(&k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let Some(source) = get(format!("{}_broadcast_source", data_center)) else {
        return (None, DEFAULT_SFEED_URL.to_string());
    };
    if source == "hs" {
        tracing::warn!(
            "Kotak data centre {} selects the legacy HSM feed, which OpenAlgo cannot read. Streaming from SFeed instead; if quotes do not arrive, this is why.",
            data_center
        );
        // No source is recorded: the default SFeed host is what streams, as
        // the warning says. The web recorded "hs" and so built its HSM
        // client after all, until openalgo#2376.
        return (None, DEFAULT_SFEED_URL.to_string());
    }
    let url = get(format!("{}_{}_broadcast_endpoint", data_center, source))
        .map(|u| to_wss(&u))
        .unwrap_or_else(|| DEFAULT_SFEED_URL.to_string());
    (Some(source), url)
}

/// Look up and remember the feed URL for `data_center` (web
/// `fetch_feed_config`, 5 s budget). Any failure leaves the default.
pub async fn resolve_feed_url(b: &KotakBroker, data_center: &str) -> String {
    let url = lookup_feed_url(&b.http, &b.feed_config_url, data_center).await;
    *b.feed_url.lock() = Some((data_center.to_string(), url.clone()));
    url
}

/// The feed URL the config service names for `data_center`, else the
/// default.
pub(super) async fn lookup_feed_url(
    http: &reqwest::Client,
    config_url: &str,
    data_center: &str,
) -> String {
    if data_center.is_empty() {
        DEFAULT_SFEED_URL.to_string()
    } else {
        let resp = http
            .get(config_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) => {
                    let configs = v
                        .get("data")
                        .and_then(|d| d.get("configs"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let (source, url) = feed_url_from_config(&configs, data_center);
                    tracing::info!(
                        "Kotak data centre {} streams from source {}",
                        data_center,
                        source.as_deref().unwrap_or("default")
                    );
                    url
                }
                Err(_) => DEFAULT_SFEED_URL.to_string(),
            },
            Ok(r) => {
                tracing::warn!(status = r.status().as_u16(), "Kotak feed config refused");
                DEFAULT_SFEED_URL.to_string()
            }
            Err(e) => {
                tracing::warn!(
                    "Kotak feed config lookup failed: {}",
                    crate::brokers::common::redact::url_safe_error(&e)
                );
                DEFAULT_SFEED_URL.to_string()
            }
        }
    }
}

/// Resolve the data-centre feed host before connecting (a session resumed
/// in this run has not been through the login that looks it up).
pub(super) struct FeedLookup {
    pub(super) http: reqwest::Client,
    pub(super) config_url: String,
    pub(super) data_center: String,
    /// The broker's cache, filled once resolved.
    pub(super) cache: std::sync::Arc<parking_lot::Mutex<Option<(String, String)>>>,
}

/// The remembered feed URL for `data_center`, else the default.
pub fn cached_feed_url(b: &KotakBroker, data_center: &str) -> String {
    match b.feed_url.lock().as_ref() {
        Some((dc, url)) if dc == data_center => url.clone(),
        _ => DEFAULT_SFEED_URL.to_string(),
    }
}
