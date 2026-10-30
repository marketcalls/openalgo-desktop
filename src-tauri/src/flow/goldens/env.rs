//! A golden's `env` as the node services of [`crate::flow::contract::env`]:
//! the master contract rows, the strategy book, the feed, the HTTP stub and
//! the holiday table the web's nodes read besides the client. Each records
//! what the node asked so the replay can compare it with the web's.

use crate::brokers::common::symbols::SymToken;
use crate::flow::contract::env::{
    FeedError, HttpCall, HttpReply, MarketFeed, NodeEnv, OutboundHttp, StrategyBook, SymbolMaster,
    SymbolQuery, TradingCalendar,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// The fakes for one golden, and what they recorded.
pub struct GoldenEnv {
    pub feed: Arc<Feed>,
    pub http: Arc<Http>,
}

impl GoldenEnv {
    pub fn new(env: &Value) -> (Self, NodeEnv) {
        let feed = Arc::new(Feed {
            spec: env["feed"].clone(),
            ops: Mutex::default(),
        });
        let http = Arc::new(Http {
            spec: env["http"].clone(),
            requests: Mutex::default(),
        });
        let node_env = NodeEnv {
            symbols: Arc::new(Symbols(env["master_contract"].clone())),
            strategy_book: Arc::new(Book(env["strategy_book"].clone())),
            feed: feed.clone(),
            http: http.clone(),
            calendar: Arc::new(Calendar(env["calendar_holidays"].clone())),
        };
        (Self { feed, http }, node_env)
    }
}

struct Symbols(Value);

impl SymbolMaster for Symbols {
    fn rows(&self, q: &SymbolQuery) -> Result<Vec<SymToken>, String> {
        if self.0 == "unreadable" {
            return Err("database is locked".into());
        }
        let text = |r: &Value, k: &str| r[k].as_str().unwrap_or("").to_string();
        let mut out: Vec<SymToken> = self
            .0
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| SymToken {
                symbol: text(r, "symbol"),
                brsymbol: r["brsymbol"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| text(r, "symbol")),
                name: text(r, "name"),
                exchange: text(r, "exchange"),
                brexchange: r["brexchange"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| text(r, "exchange")),
                token: text(r, "token"),
                expiry: text(r, "expiry"),
                strike: r["strike"].as_f64().unwrap_or(0.0),
                lot_size: r["lotsize"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(0),
                instrument_type: text(r, "instrumenttype"),
                tick_size: r["tick_size"].as_f64().unwrap_or(0.05),
            })
            .filter(|s| {
                q.symbol.as_ref().is_none_or(|x| &s.symbol == x)
                    && q.symbol_prefix
                        .as_ref()
                        .is_none_or(|x| s.symbol.starts_with(x.as_str()))
                    && q.name.as_ref().is_none_or(|x| &s.name == x)
                    && q.exchange.as_ref().is_none_or(|x| &s.exchange == x)
                    && q.instrumenttype
                        .as_ref()
                        .is_none_or(|x| &s.instrument_type == x)
            })
            .collect();
        if let Some(n) = q.limit {
            out.truncate(n);
        }
        Ok(out)
    }
}

struct Book(Value);

impl StrategyBook for Book {
    fn legs(&self, strategy: &str) -> Result<Vec<Value>, String> {
        if let Some(msg) = self.0.get("unavailable").and_then(Value::as_str) {
            return Err(msg.to_string());
        }
        Ok(self
            .0
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| strategy.is_empty() || l["strategy"].as_str() == Some(strategy))
            .cloned()
            .collect())
    }
}

/// The feed: the session and subscription outcome the golden scripts, the
/// frames it delivers, and every subscribe and unsubscribe asked of it.
pub struct Feed {
    spec: Value,
    pub ops: Mutex<Vec<Value>>,
}

#[async_trait]
impl MarketFeed for Feed {
    async fn first_frame(
        &self,
        symbol: &str,
        exchange: &str,
        mode: &str,
        timeout: Duration,
        accept: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
    ) -> Result<Option<Value>, FeedError> {
        if self.spec.get("username").is_some_and(Value::is_null) {
            return Err(FeedError::InvalidKey);
        }
        if let Some(e) = self.spec["connect_error"].as_str() {
            return Err(FeedError::Connect(e.to_string()));
        }
        self.ops.lock().push(json!({
            "op": "subscribe", "symbol": symbol, "exchange": exchange, "mode": mode,
            "timeout": timeout.as_secs_f64(),
        }));
        if let Some(e) = self.spec["subscribe_error"].as_str() {
            return Err(FeedError::Subscribe(e.to_string()));
        }
        for tick in self.spec["ticks"].as_array().into_iter().flatten() {
            if accept(tick) {
                return Ok(Some(tick.clone()));
            }
        }
        Ok(None)
    }

    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> Result<(), String> {
        self.ops.lock().push(
            json!({"op": "unsubscribe", "symbol": symbol, "exchange": exchange, "mode": mode}),
        );
        match self.spec.get("unsubscribe_error") {
            Some(e) => Err(e.as_str().unwrap_or("").to_string()),
            None => Ok(()),
        }
    }
}

/// The HTTP stub: the addresses the guard sees, the one reply, and every
/// request sent (with whether it went to a vetted address).
pub struct Http {
    spec: Value,
    pub requests: Mutex<Vec<Value>>,
}

impl Http {
    fn addresses(&self, host: &str) -> Option<Vec<IpAddr>> {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(vec![ip]);
        }
        let list = self.spec["resolve"].get(host)?.as_array()?.clone();
        Some(
            list.iter()
                .filter_map(|a| a.as_str()?.parse().ok())
                .collect(),
        )
    }
}

#[async_trait]
impl OutboundHttp for Http {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.addresses(host)
            .ok_or_else(|| "no address is known for this name".to_string())
    }

    async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
        let url = url::Url::parse(&call.url).map_err(|e| e.to_string())?;
        let vetted = url
            .host_str()
            .and_then(|h| self.addresses(h))
            .is_some_and(|a| a.contains(&call.connect_to.ip()));
        let keep: Vec<String> = self.spec["record_headers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|h| h.as_str().map(str::to_lowercase))
            .chain(std::iter::once("content-type".to_string()))
            .collect();
        let headers: serde_json::Map<String, Value> = call
            .headers
            .iter()
            .filter(|(k, _)| keep.contains(&k.to_lowercase()))
            .map(|(k, v)| (k.to_lowercase(), json!(v)))
            .collect();
        let body = call
            .body
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        self.requests.lock().push(json!({
            "method": call.method, "path": url.path(), "query": url.query().unwrap_or(""),
            "headers": headers, "body": body, "vetted": vetted,
        }));
        let resp = &self.spec["response"];
        let delay = resp["delay"].as_f64().unwrap_or(0.0);
        if delay > call.timeout.as_secs_f64() {
            return Err("timed out".into());
        }
        let content_type = resp["content_type"].as_str().unwrap_or("application/json");
        Ok(HttpReply {
            status: resp["status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200),
            headers: vec![("content-type".into(), content_type.into())],
            body: resp["body"].as_str().unwrap_or("").as_bytes().to_vec(),
        })
    }
}

struct Calendar(Value);

impl TradingCalendar for Calendar {
    fn holidays(&self, year: i32) -> Result<Vec<Value>, String> {
        match self.0.get(year.to_string()) {
            Some(Value::String(s)) => Err(s.clone()),
            Some(Value::Array(a)) => Ok(a.clone()),
            _ => Ok(Vec::new()),
        }
    }
}
