//! Shared plumbing for the options tools (OI tracker, OI profile, GEX, IV
//! smile and chart, gamma density, straddles, volatility surface, Strategy
//! Builder charts): where an underlying is quoted, the nearest future, the
//! history fetch and the bounded fan-out that keeps a tool inside the
//! broker's rate limits.
//!
//! A tool that needs many history calls goes through [`fan_out`] with the
//! process-wide [`history_gate`]: at most [`HISTORY_CONCURRENCY`] calls in
//! flight and starts spaced by [`HISTORY_SPACING`], shared by every tool and
//! every open page, so two charts loading at once still respect the limit.
//! Quotes go through the broker's batched `multiquotes`.

use super::core::{BrokerHandle, Reply};
use super::market_data_service;
use super::options_service::{near_future_symbol, option_exchange, parse_compact_expiry};
use crate::analytics::series::Bar;
use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::types::Quote;
use crate::state::AppState;
use chrono::NaiveDate;
use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};
use tokio::time::Instant;

/// History calls in flight at once, across all tools.
pub const HISTORY_CONCURRENCY: usize = 3;
/// Minimum gap between two history calls starting (about three a second,
/// the tightest common broker history limit).
pub const HISTORY_SPACING: Duration = Duration::from_millis(340);

/// A concurrency limit plus a minimum spacing between starts.
pub struct Gate {
    limit: usize,
    permits: Semaphore,
    spacing: Duration,
    last_start: Mutex<Option<Instant>>,
}

impl Gate {
    pub fn new(limit: usize, spacing: Duration) -> Self {
        let limit = limit.max(1);
        Gate {
            limit,
            permits: Semaphore::new(limit),
            spacing,
            last_start: Mutex::new(None),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Wait for a slot and for the spacing since the previous start.
    async fn admit(&self) -> Option<SemaphorePermit<'_>> {
        let permit = self.permits.acquire().await.ok();
        if !self.spacing.is_zero() {
            let mut last = self.last_start.lock().await;
            if let Some(t) = *last {
                let since = t.elapsed();
                if since < self.spacing {
                    tokio::time::sleep(self.spacing - since).await;
                }
            }
            *last = Some(Instant::now());
        }
        permit
    }
}

/// The gate every tool's history fan-out shares.
pub fn history_gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate::new(HISTORY_CONCURRENCY, HISTORY_SPACING))
}

/// Run `f` over `items` through `gate`, results in input order. Nothing is
/// spawned: the futures live inside the caller's request and are dropped
/// with it.
pub async fn fan_out<I, O, F, Fut>(gate: &Gate, items: Vec<I>, f: F) -> Vec<O>
where
    F: Fn(I) -> Fut,
    Fut: Future<Output = O>,
{
    stream::iter(items)
        .map(|i| {
            let fut = f(i);
            async move {
                let _permit = gate.admit().await;
                fut.await
            }
        })
        .buffered(gate.limit())
        .collect()
        .await
}

// ------------------------------------------------------------ exchanges

pub const NO_SPOT: &[&str] = &["MCX", "CDS", "BCD", "NCDEX", "NCO"];
pub const CRYPTO: &str = "CRYPTO";

const NSE_INDEX_TOOLS: &[&str] = &[
    "NIFTY",
    "BANKNIFTY",
    "FINNIFTY",
    "MIDCPNIFTY",
    "NIFTYNXT50",
    "NIFTYIT",
    "NIFTYPHARMA",
    "NIFTYBANK",
];
const BSE_INDEX_TOOLS: &[&str] = &["SENSEX", "BANKEX", "SENSEX50"];

/// Web `_get_quote_exchange` (IV chart, straddles, vol surface, Strategy
/// Builder reference).
pub fn tool_quote_exchange(base: &str, exchange: &str) -> String {
    let up = exchange.to_ascii_uppercase();
    if NSE_INDEX_TOOLS.contains(&base) {
        "NSE_INDEX".into()
    } else if BSE_INDEX_TOOLS.contains(&base) {
        "BSE_INDEX".into()
    } else if up == "NFO" {
        "NSE".into()
    } else if up == "BFO" {
        "BSE".into()
    } else {
        up
    }
}

/// The options exchange the chain tools use for a request's exchange.
pub fn chain_options_exchange(exchange: &str) -> String {
    match exchange.to_ascii_uppercase().as_str() {
        "NSE_INDEX" | "NSE" => "NFO".into(),
        "BSE_INDEX" | "BSE" => "BFO".into(),
        other => other.to_string(),
    }
}

/// Where a history tool quotes its underlying.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub base: String,
    pub quote_symbol: String,
    pub quote_exchange: String,
    pub options_exchange: String,
}

/// Web's shared prologue of the IV chart, straddle and surface services:
/// index or stock spot, the crypto perpetual, or the near-month future on
/// exchanges with no spot.
pub fn resolve_reference(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
) -> Result<Reference, Reply> {
    let base = underlying.to_ascii_uppercase();
    let ex = exchange.to_ascii_uppercase();
    let mut quote_exchange = tool_quote_exchange(&base, &ex);
    let options_exchange = option_exchange(&quote_exchange);
    let quote_symbol = if ex == CRYPTO {
        let perp = format!("{}USDFUT", base);
        if ctx.symbols.by_symbol(&ex, &perp).is_none() {
            return Err(Reply::error(
                404,
                format!("No perpetual futures found for {} on {}", base, exchange),
            ));
        }
        perp
    } else if NO_SPOT.contains(&ex.as_str()) {
        match near_future_symbol(ctx, &base, &ex) {
            Some(s) => {
                quote_exchange = ex.clone();
                s
            }
            None => {
                return Err(Reply::error(
                    404,
                    format!("No unexpired futures found for {} on {}", base, ex),
                ))
            }
        }
    } else {
        base.clone()
    };
    Ok(Reference {
        base,
        quote_symbol,
        quote_exchange,
        options_exchange,
    })
}

/// Web `resolve_strategy_builder_reference`: the pair the option chain
/// supplied wins; crypto uses its perpetual; no-spot exchanges their near
/// future (`None` when there is none).
pub fn strategy_reference(
    ctx: &AppState,
    base: &str,
    exchange: &str,
    underlying_symbol: Option<&str>,
    underlying_exchange: Option<&str>,
) -> Option<(String, String)> {
    let base = base.trim().to_ascii_uppercase();
    let venue = exchange.trim().to_ascii_uppercase();
    if let (Some(s), Some(e)) = (underlying_symbol, underlying_exchange) {
        return Some((s.trim().to_ascii_uppercase(), e.trim().to_ascii_uppercase()));
    }
    if venue == CRYPTO {
        let perp = format!("{}USDFUT", base);
        return ctx
            .symbols
            .by_symbol(&venue, &perp)
            .map(|r| (r.symbol, r.exchange));
    }
    if NO_SPOT.contains(&venue.as_str()) {
        return near_future_symbol(ctx, &base, &venue).map(|s| (s, venue));
    }
    let qx = tool_quote_exchange(&base, &venue);
    Some((base, qx))
}

/// `DDMMMYY` -> the master's `DD-MMM-YY`.
pub fn db_expiry(expiry: &str) -> String {
    super::options_service::master_expiry(expiry)
}

/// Web `_find_futures_symbol` / `_get_nearest_futures_price` lookup: the
/// future of `underlying` expiring on `expiry`, else the nearest listed
/// one; the perpetual on crypto.
pub fn nearest_future(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Option<(String, String)> {
    let ex = exchange.to_ascii_uppercase();
    let base = underlying.to_ascii_uppercase();
    if ex == CRYPTO {
        let perp = format!("{}USDFUT", base);
        return ctx
            .symbols
            .by_symbol(&ex, &perp)
            .map(|r| (r.symbol, r.exchange));
    }
    let snap = ctx.symbols.snapshot();
    let futures: Vec<_> = snap
        .rows()
        .iter()
        .filter(|r| r.exchange == ex && r.symbol.ends_with("FUT"))
        .filter(|r| {
            r.symbol
                .strip_prefix(base.as_str())
                .and_then(|rest| rest.strip_suffix("FUT"))
                .is_some_and(|d| d.len() == 7 && parse_compact_expiry(d).is_some())
        })
        .collect();
    let wanted = db_expiry(expiry);
    if let Some(r) = futures.iter().find(|r| r.expiry == wanted) {
        return Some((r.symbol.clone(), r.exchange.clone()));
    }
    futures
        .iter()
        .min_by_key(|r| parse_oa_expiry(&r.expiry).unwrap_or(NaiveDate::MAX))
        .map(|r| (r.symbol.clone(), r.exchange.clone()))
}

// ------------------------------------------------------------ fetches

/// One quote (web `get_quotes`), with the service's status and message.
pub async fn quote(
    ctx: &AppState,
    h: &BrokerHandle,
    symbol: &str,
    exchange: &str,
) -> Result<Quote, Reply> {
    market_data_service::fetch_quote(ctx, h, symbol, exchange).await
}

fn rate_limited(r: &Reply) -> bool {
    if r.status == 429 {
        return true;
    }
    let m = r.message().to_ascii_lowercase();
    m.contains("429") || m.contains("too many") || m.contains("rate limit")
}

/// Candles from the history service (web `get_history`), with the web OI
/// profile's retry on a broker rate-limit answer (twice, 1 s then 2 s).
pub async fn history_rows(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Value>, Reply> {
    let mut delay = Duration::from_secs(1);
    for attempt in 0..3 {
        let r =
            market_data_service::history(ctx, symbol, exchange, interval, start, end, "api").await;
        if r.is_success() {
            return Ok(r
                .body
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default());
        }
        if attempt < 2 && rate_limited(&r) {
            tracing::warn!("History for {}:{} rate limited, retrying", exchange, symbol);
            tokio::time::sleep(delay).await;
            delay *= 2;
            continue;
        }
        return Err(r);
    }
    Err(Reply::error(500, "History fetch failed"))
}

/// Candle rows reduced to bars, sorted by time.
pub fn bars(rows: &[Value]) -> Vec<Bar> {
    let mut v: Vec<Bar> = rows
        .iter()
        .filter_map(|c| {
            let t = c.get("timestamp")?.as_f64()?;
            let t = if t > 1e12 {
                (t / 1000.0) as i64
            } else {
                t as i64
            };
            Some(Bar {
                time: t,
                close: c.get("close")?.as_f64()?,
                oi: c.get("oi").and_then(Value::as_f64).unwrap_or(0.0),
            })
        })
        .collect();
    v.sort_by_key(|b| b.time);
    v
}

/// History as bars through the shared gate.
pub async fn history_bars(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    interval: &str,
    (start, end): (NaiveDate, NaiveDate),
) -> Result<Vec<Bar>, Reply> {
    history_rows(ctx, symbol, exchange, interval, start, end)
        .await
        .map(|r| bars(&r))
}

/// Many histories, bounded by [`history_gate`], in input order.
pub async fn many_histories(
    ctx: &AppState,
    keys: Vec<(String, String)>,
    interval: &str,
    window: (NaiveDate, NaiveDate),
) -> Vec<Result<Vec<Bar>, Reply>> {
    fan_out(history_gate(), keys, |(s, e)| async move {
        history_bars(ctx, &s, &e, interval, window).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quote_exchanges_follow_the_web_tables() {
        assert_eq!(tool_quote_exchange("NIFTY", "NFO"), "NSE_INDEX");
        assert_eq!(tool_quote_exchange("NIFTYIT", "NFO"), "NSE_INDEX");
        assert_eq!(tool_quote_exchange("SENSEX", "BFO"), "BSE_INDEX");
        assert_eq!(tool_quote_exchange("RELIANCE", "NFO"), "NSE");
        assert_eq!(tool_quote_exchange("TCS", "bfo"), "BSE");
        assert_eq!(tool_quote_exchange("GOLD", "mcx"), "MCX");
        assert_eq!(chain_options_exchange("NSE_INDEX"), "NFO");
        assert_eq!(chain_options_exchange("bse"), "BFO");
        assert_eq!(chain_options_exchange("MCX"), "MCX");
        assert_eq!(db_expiry("30oct26"), "30-OCT-26");
    }

    #[test]
    fn bars_sort_and_normalise_milliseconds() {
        let rows = vec![
            json!({"timestamp": 1_700_000_060_000i64, "close": 2.0, "oi": 5}),
            json!({"timestamp": 1_700_000_000, "close": 1.0}),
            json!({"close": 3.0}),
        ];
        let b = bars(&rows);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].time, b[0].close, b[0].oi), (1_700_000_000, 1.0, 0.0));
        assert_eq!((b[1].time, b[1].oi), (1_700_000_060, 5.0));
    }
}
