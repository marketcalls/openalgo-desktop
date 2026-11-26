//! The name `/EODChartData` is asked with (web tradesmart
//! `_index_eod_symbol`, marketcalls/openalgo#2475, 9a12bd0f7).
//!
//! TradeSmart's master lists an NSE index under a trading symbol (`NIFTY
//! INDEX`, `NIFTY BANK`, `FINNIFTY`, `INDIA VIX`) that `/EODChartData`
//! does not take (`Nifty 50`, `Nifty Bank`, `Nifty Fin Service`,
//! `INDIAVIX`), and the file's other name column does not match either
//! (`Nifty Fin Services`), so daily history of every NSE index came back
//! empty but for today's bar built from the quote. `/GetSecurityInfo`, the
//! documented way to resolve an instrument, reports the name by token, so
//! no index name is written into the code (`NorenConfig::eod_index_lookup`).
//!
//! The answer is kept a day per (exchange, token) in a `TtlCache`, as the
//! web's `LockedTTLCache(maxsize=64, ttl=24 h)`, and cleared by
//! `Broker::on_logout` (logout, a broker switch, the daily boundary). A
//! lookup reads the cache's generation before it asks, so an answer that
//! lands after a logout is returned to its caller but not kept
//! (`docs/port/no-keychain.md`, caches). A failed lookup is not kept and
//! asks with the stored broker symbol, as before.

use super::super::mapping::{noren_exchange, text};
use super::super::transport::{stat_ok, Category, Session};
use super::super::NorenBroker;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::QuoteKey;
use crate::cache::TtlCache;
use serde_json::json;
use std::time::{Duration, Instant};

/// Index names by (Noren exchange, token).
pub(crate) struct IndexEodNames(TtlCache<(String, String), String>);

impl Default for IndexEodNames {
    fn default() -> Self {
        // Index names change rarely; a day keeps a rename from sticking
        // past the next session (the web's TTL).
        Self(TtlCache::new(64, Duration::from_secs(24 * 3600)))
    }
}

impl IndexEodNames {
    pub(crate) fn clear(&self) {
        self.0.clear();
    }
}

/// The `sym` name of `row` for `/EODChartData`: the member's fixed index
/// name, else for a daily index history the name `/GetSecurityInfo`
/// reports where the member resolves it, else the broker symbol.
pub(super) async fn eod_symbol(
    b: &NorenBroker,
    s: &Session,
    key: &QuoteKey,
    row: &SymToken,
    daily: bool,
) -> String {
    let fixed = b
        .cfg
        .eod_index_names
        .iter()
        .find(|((exchange, symbol), _)| *exchange == key.exchange && *symbol == key.symbol);
    if let Some((_, name)) = fixed {
        return name.to_string();
    }
    let index = matches!(key.exchange.as_str(), "NSE_INDEX" | "BSE_INDEX");
    if daily && index && b.cfg.eod_index_lookup {
        let exch = noren_exchange(&key.exchange);
        return resolve(b, s, exch, &row.token, row.br_symbol()).await;
    }
    row.br_symbol().to_string()
}

/// The index name `/GetSecurityInfo` reports for `token` on `exch`, from
/// the cache when it holds one, else `fallback`.
async fn resolve(b: &NorenBroker, s: &Session, exch: &str, token: &str, fallback: &str) -> String {
    let names = &b.index_eod_names.0;
    let key = (exch.to_string(), token.to_string());
    let (cached, generation) = names.get_with_generation(&key, Instant::now());
    if let Some(name) = cached {
        return name;
    }
    let asked = b
        .post_raw(
            "/GetSecurityInfo",
            json!({"exch": exch, "token": token}),
            s,
            Category::Data,
        )
        .await;
    match asked {
        Ok(info) if stat_ok(&info) && !text(&info, "tsym").is_empty() => {
            let name = text(&info, "tsym");
            names.fill(key, name.clone(), generation, Instant::now());
            name
        }
        Ok(_) => fallback.to_string(),
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Index name lookup for {}:{} failed ({}); daily history asks for {}",
                exch,
                token,
                e.code(),
                fallback
            );
            fallback.to_string()
        }
    }
}

#[cfg(test)]
mod tests;
