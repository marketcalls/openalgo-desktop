//! History for Flow: the History node, the two nodes that read single bars
//! out of history (Prior Period OHLC, Bar Offset), and the cached fetch the
//! Indicator node shares with them (the web's `execute_history`,
//! `execute_prior_period_ohlc`, `execute_bar_offset`, `_history_error` and
//! `indicator_service.fetch_history_cached`).
//!
//! The cache collapses identical history requests within a run (RSI, SMA
//! and previous-day levels on one symbol are one broker call) and across
//! back-to-back runs, for 30 seconds: well under a one-minute candle, so a
//! new bar is never hidden. It is bounded (256 requests), keeps only a
//! successful non-empty answer, and lets concurrent misses for one request
//! share the first caller's call (single flight). The History node itself
//! never uses it, as on the web.
//!
//! Dates are IST (lead decision 1): "today" is the IST date from the run's
//! clock on any machine. The editor's `1d` is sent to `/api/v1/history` as
//! `D` (lead decision 4), the one spelling the desktop's schema refuses.

use crate::flow::contract::client::{FlowClient, History};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::flow::contract::time::ist_now;
use crate::flow::indicator_service::{
    bar_at_offset, clamp_bars, clamp_history_range, history_window, prior_period_ohlc, trim_records,
};
use async_trait::async_trait;
use chrono::{Duration as Days, NaiveDate};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

/// `FLOW_HISTORY_CACHE_TTL`: how long an answer is reused.
pub const HISTORY_CACHE_TTL: Duration = Duration::from_secs(30);
/// `FLOW_HISTORY_CACHE_MAXSIZE`: the most requests kept.
pub const HISTORY_CACHE_MAXSIZE: usize = 256;
/// `FLOW_HISTORY_FLIGHT_TIMEOUT`: how long a caller waits for another's
/// identical call before making its own.
pub const HISTORY_FLIGHT_TIMEOUT: Duration = Duration::from_secs(60);

/// The interval as `/api/v1/history` takes it: the History panel stored
/// `1d` for Daily on the web, which the desktop's schema reads as `D`.
pub fn api_interval(interval: &str) -> &str {
    if interval == "1d" {
        "D"
    } else {
        interval
    }
}

/// One cached request: the account, then the request's own fields.
type Key = [String; 7];

/// A bounded, expiring, single-flight cache of history answers.
pub struct HistoryCache {
    ttl: Duration,
    max: usize,
    wait: Duration,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<Key, (Instant, Value)>,
    /// The calls in flight; each publishes its answer once.
    flights: HashMap<Key, watch::Receiver<Option<Value>>>,
}

/// What a caller does after looking the key up.
enum Lookup {
    Hit(Value),
    Wait(watch::Receiver<Option<Value>>),
    Lead(watch::Sender<Option<Value>>),
}

/// Retires a leader's flight on every path, a cancelled run included, so a
/// waiter is never left on a call nobody makes.
struct FlightGuard<'a> {
    cache: &'a HistoryCache,
    key: &'a Key,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        self.cache.inner.lock().flights.remove(self.key);
    }
}

impl HistoryCache {
    pub fn new(ttl: Duration, max: usize, wait: Duration) -> Self {
        Self {
            ttl,
            max: max.max(1),
            wait,
            inner: Mutex::default(),
        }
    }

    /// Drop every cached answer (`clear_history_cache`).
    pub fn clear(&self) {
        self.inner.lock().entries.clear();
    }

    /// Cached answers held now.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.inner.lock().entries.len()
    }

    /// `fetch_history_cached`: `req` through the cache for the account
    /// `tag`.
    pub async fn fetch(&self, client: &dyn FlowClient, tag: &str, req: &History) -> Value {
        if self.ttl.is_zero() {
            return client.get_history(req).await;
        }
        let key: Key = [
            tag.to_string(),
            req.symbol.clone(),
            req.exchange.clone(),
            req.interval.clone(),
            req.start_date.clone().unwrap_or_default(),
            req.end_date.clone().unwrap_or_default(),
            req.source.clone(),
        ];
        match self.lookup(&key) {
            Lookup::Hit(v) => {
                tracing::debug!(
                    "History cache hit: {} {} ({})",
                    req.symbol,
                    req.interval,
                    req.source
                );
                v
            }
            Lookup::Wait(mut rx) => {
                let shared: Option<Value> =
                    match tokio::time::timeout(self.wait, rx.wait_for(Option::is_some)).await {
                        Ok(Ok(answer)) => answer.clone(),
                        _ => None,
                    };
                if let Some(v) = shared {
                    return v;
                }
                tracing::warn!(
                    "History single-flight wait timed out for {} {}; fetching directly",
                    req.symbol,
                    req.interval
                );
                self.fetch_and_keep(client, &key, req).await
            }
            Lookup::Lead(tx) => {
                let _retire = FlightGuard {
                    cache: self,
                    key: &key,
                };
                let v = self.fetch_and_keep(client, &key, req).await;
                let _ = tx.send(Some(v.clone()));
                v
            }
        }
    }

    fn lookup(&self, key: &Key) -> Lookup {
        let mut inner = self.inner.lock();
        if let Some((at, v)) = inner.entries.get(key) {
            if at.elapsed() < self.ttl {
                return Lookup::Hit(v.clone());
            }
            inner.entries.remove(key);
        }
        if let Some(rx) = inner.flights.get(key) {
            return Lookup::Wait(rx.clone());
        }
        let (tx, rx) = watch::channel(None);
        inner.flights.insert(key.clone(), rx);
        Lookup::Lead(tx)
    }

    /// One call; a successful answer with rows is kept (an error or an
    /// empty range stays retryable).
    async fn fetch_and_keep(&self, client: &dyn FlowClient, key: &Key, req: &History) -> Value {
        let v = client.get_history(req).await;
        if v.get("data").is_some_and(pyfmt::truthy) {
            let mut inner = self.inner.lock();
            let ttl = self.ttl;
            inner.entries.retain(|_, (at, _)| at.elapsed() < ttl);
            while inner.entries.len() >= self.max {
                let oldest = inner
                    .entries
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => inner.entries.remove(&k),
                    None => break,
                };
            }
            inner
                .entries
                .insert(key.clone(), (Instant::now(), v.clone()));
        }
        v
    }
}

/// The app's history cache: one per process, as the web's module-level
/// cache. Tests get one per test thread, so parallel tests never read each
/// other's answers (the web's recorder clears it per scenario).
#[cfg(not(test))]
pub fn shared_cache() -> Arc<HistoryCache> {
    static CACHE: std::sync::LazyLock<Arc<HistoryCache>> = std::sync::LazyLock::new(|| {
        Arc::new(HistoryCache::new(
            HISTORY_CACHE_TTL,
            HISTORY_CACHE_MAXSIZE,
            HISTORY_FLIGHT_TIMEOUT,
        ))
    });
    CACHE.clone()
}

/// The app's history cache: one per process, as the web's module-level
/// cache. Tests get one per test thread, so parallel tests never read each
/// other's answers (the web's recorder clears it per scenario).
#[cfg(test)]
pub fn shared_cache() -> Arc<HistoryCache> {
    thread_local! {
        static CACHE: Arc<HistoryCache> = Arc::new(HistoryCache::new(
            HISTORY_CACHE_TTL,
            HISTORY_CACHE_MAXSIZE,
            HISTORY_FLIGHT_TIMEOUT,
        ));
    }
    CACHE.with(Arc::clone)
}

/// Whose history a cached answer is (`_account_tag`): the connected broker
/// account, never a secret, or `anon` without one. Two accounts on brokers
/// whose bars differ never share an answer.
fn account_tag(ctx: &NodeCtx) -> String {
    match ctx.app().and_then(|a| a.get_broker_session()) {
        Some(s) => {
            let digest = Sha256::digest(format!("{}:{}", s.broker_id, s.user_id).as_bytes());
            hex::encode(digest).chars().take(12).collect()
        }
        None => "anon".into(),
    }
}

/// One cached history fetch for a node: the range and the source it asked
/// for, the interval mapped for `/api/v1`.
pub(crate) async fn fetch_cached(
    ctx: &NodeCtx,
    symbol: &str,
    exchange: &str,
    interval: &str,
    (start, end): (String, String),
    source: &str,
) -> Value {
    let req = History {
        symbol: symbol.into(),
        exchange: exchange.into(),
        interval: api_interval(interval).into(),
        start_date: Some(start),
        end_date: Some(end),
        source: source.into(),
    };
    shared_cache()
        .fetch(ctx.client.as_ref(), &account_tag(ctx), &req)
        .await
}

/// `_history_error`: the failure of a history fetch in words a trader can
/// act on (the broker may not offer the interval), or `None`.
pub(crate) fn history_error(
    response: &Value,
    symbol: &str,
    interval: &str,
    source: &str,
) -> Option<String> {
    let failed = response.get("status").and_then(Value::as_str) == Some("error")
        || response.get("error").is_some_and(pyfmt::truthy);
    if !failed {
        return None;
    }
    let detail = ["message", "error"]
        .iter()
        .filter_map(|k| response.get(*k))
        .find(|v| pyfmt::truthy(v))
        .map(pyfmt::str)
        .unwrap_or_else(|| "unknown error".into());
    let hint = if source == "db" {
        String::new()
    } else {
        format!(
            " If your broker does not support the '{}' interval, use the Intervals node to list supported ones, or set Source to Historify DB to resample it locally.",
            interval
        )
    };
    Some(format!(
        "History fetch failed for {} ({}): {}.{}",
        symbol, interval, detail, hint
    ))
}

/// The answer's rows, at most the latest 200 (`trim_records`).
pub(crate) fn records_of(response: &Value) -> Vec<Value> {
    match response.get("data") {
        Some(Value::Array(rows)) => trim_records(rows).to_vec(),
        _ => Vec::new(),
    }
}

/// Today in IST, the end of every window a node asks for.
pub(crate) fn today_ist(ctx: &NodeCtx) -> NaiveDate {
    ist_now(ctx.clock.as_ref()).date()
}

/// A history failure: logged at error level and returned.
fn failed(ctx: &NodeCtx, message: String) -> NodeOutcome {
    ctx.error(message.clone());
    NodeOutcome::error(message)
}

/// History: candles for a symbol over an explicit range or the last N days,
/// never more than 200 bars.
pub struct HistoryNode;

#[async_trait]
impl FlowNode for HistoryNode {
    fn node_type(&self) -> &'static str {
        "history"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let interval = ctx.get_str(data, "interval", "5m");
        let mut start = ctx.get_str(data, "startDate", "");
        let mut end = ctx.get_str(data, "endDate", "");
        if start.is_empty() || end.is_empty() {
            let days = ctx.get_int(data, "days", 0);
            if days > 0 {
                let now = ist_now(ctx.clock.as_ref());
                let first = Days::try_days(days)
                    .and_then(|d| now.checked_sub_signed(d))
                    .map(|t| t.date())
                    .or_else(|| NaiveDate::from_ymd_opt(1, 1, 1))
                    .unwrap_or(NaiveDate::MIN);
                start = first.format("%Y-%m-%d").to_string();
                end = now.format("%Y-%m-%d").to_string();
                ctx.info(format!(
                    "History range from days={}: {} to {}",
                    days, start, end
                ));
            }
        }
        if !start.is_empty() && !end.is_empty() {
            (start, end) = clamp_history_range(&start, &end, &interval);
        }
        ctx.info(format!("Getting history for: {} ({})", symbol, interval));
        let mut result = ctx
            .client
            .get_history(&History {
                symbol,
                exchange,
                interval: api_interval(&interval).into(),
                start_date: Some(start),
                end_date: Some(end),
                source: "api".into(),
            })
            .await;
        if let Some(Value::Array(rows)) = result.get("data") {
            if !rows.is_empty() {
                let kept = trim_records(rows).to_vec();
                if let Some(m) = result.as_object_mut() {
                    m.insert("data".into(), Value::Array(kept));
                }
            }
        }
        ctx.info("History data received");
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

/// Prior Period OHLC: the last complete hour, day, week or month (PDH, PDL,
/// PDC for `previous_day`).
pub struct PriorPeriodOhlcNode;

#[async_trait]
impl FlowNode for PriorPeriodOhlcNode {
    fn node_type(&self) -> &'static str {
        "priorPeriodOhlc"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        // Interpolated once: the web interpolated the symbol twice, so a
        // payload value holding `{{...}}` was resolved again (N-dataCommon-14).
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let period = ctx.get_str(data, "period", "previous_day");
        let source = ctx.get_str(data, "source", "api");
        if symbol.is_empty() {
            return NodeOutcome::error("Symbol is required");
        }
        let interval = if period == "previous_hour" { "1h" } else { "D" };
        let lookback_days = match period.as_str() {
            "previous_hour" => 5,
            "previous_month" => 60,
            "previous_week" => 21,
            _ => 10,
        };
        let window = history_window(100, Some(lookback_days), interval, today_ist(ctx));
        ctx.info(format!(
            "Fetching {} OHLC for: {} ({})",
            period, symbol, exchange
        ));
        let history = fetch_cached(ctx, &symbol, &exchange, interval, window, &source).await;
        if let Some(message) = history_error(&history, &symbol, interval, &source) {
            return failed(ctx, message);
        }
        let records = records_of(&history);
        let row = match prior_period_ohlc(&records, &period, ist_now(ctx.clock.as_ref())) {
            Ok(row) => row,
            Err(e) => {
                ctx.error(format!("Prior period OHLC error: {}", e));
                return NodeOutcome::error(e.message);
            }
        };
        let mut output = Map::new();
        output.insert("status".into(), json!("success"));
        output.insert("symbol".into(), json!(symbol));
        output.insert("exchange".into(), json!(exchange));
        output.extend(row.to_map());
        output.insert("pdh".into(), row.high.clone());
        output.insert("pdl".into(), row.low.clone());
        output.insert("pdc".into(), row.close.clone());
        ctx.info(format!(
            "{} OHLC for {}: H={} L={} C={} (bucket={})",
            period,
            symbol,
            pyfmt::str(&row.high),
            pyfmt::str(&row.low),
            pyfmt::str(&row.close),
            row.date
        ));
        let output = Value::Object(output);
        ctx.store_output(data, output.clone());
        NodeOutcome::result(output)
    }
}

/// Bar Offset: the OHLCV of the Nth closed bar back (0 is the most recent
/// closed bar, never the one still forming).
pub struct BarOffsetNode;

#[async_trait]
impl FlowNode for BarOffsetNode {
    fn node_type(&self) -> &'static str {
        "barOffset"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let interval = ctx.get_str(data, "interval", "D");
        let source = ctx.get_str(data, "source", "api");
        let requested = ctx.get_int(data, "offsetBars", 0).saturating_add(1);
        let offset_bars = clamp_bars(requested, "offset bars") - 1;
        if symbol.is_empty() {
            return NodeOutcome::error("Symbol is required");
        }
        let window = history_window(offset_bars + 5, None, &interval, today_ist(ctx));
        ctx.info(format!(
            "Fetching bar offset {} for {} ({})",
            offset_bars, symbol, interval
        ));
        let history = fetch_cached(ctx, &symbol, &exchange, &interval, window, &source).await;
        if let Some(message) = history_error(&history, &symbol, &interval, &source) {
            return failed(ctx, message);
        }
        let records = records_of(&history);
        let row = match bar_at_offset(
            &records,
            offset_bars,
            &interval,
            ist_now(ctx.clock.as_ref()),
        ) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return NodeOutcome::error(format!(
                    "Not enough history to reach {} bars back for {}",
                    offset_bars, symbol
                ))
            }
            // A history row whose time cannot be read stopped the whole
            // run on the web; here it fails only this node (IND-043).
            Err(e) => {
                ctx.error(format!("Bar offset error: {}", e));
                return NodeOutcome::error(e.message);
            }
        };
        let field = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
        let output = json!({
            "status": "success",
            "symbol": symbol,
            "exchange": exchange,
            "offsetBars": offset_bars,
            "timestamp": field("timestamp"),
            "open": field("open"),
            "high": field("high"),
            "low": field("low"),
            "close": field("close"),
            "volume": field("volume"),
        });
        ctx.info(format!(
            "Bar offset {} for {}: {}",
            offset_bars,
            symbol,
            pyfmt::str(&output)
        ));
        ctx.store_output(data, output.clone());
        NodeOutcome::result(output)
    }
}
