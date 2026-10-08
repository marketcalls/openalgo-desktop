//! Everything the runner needs from the rest of the app, behind one trait so
//! the lifecycle is tested against a fake and runs in the app over the real
//! services.
//!
//! **Where an order goes is decided once, when the run starts.** The run's
//! mode is the analyzer setting at that moment. A sandbox run's orders go to
//! the sandbox engine whatever the toggle says later; a live run's orders go
//! to the broker with `force_live`, so switching analyzer mode on while a live
//! run holds a real position cannot divert its exits into the sandbox (which
//! would report success and leave the real position with nothing managing
//! it). Otherwise a live order takes the `/api/v1/placeorder` path exactly as
//! an API client's does, Semi-Auto order mode included.

use crate::events::{Event, Mode};
use crate::services::core::{meta, safe_request, Reply};
use crate::services::order_service::{place_order, sandbox_order, Route};
use crate::state::AppState;
use crate::strategy::dispatch::{
    AppGateway, Book, DispatchResult, OrderGateway, OrderStatusResult, RunMode,
};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::sync::Weak;

/// One bar as the runner page reads it: `time` in epoch milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub oi: f64,
}

impl Bar {
    pub fn json(&self) -> Value {
        json!({
            "time": self.time,
            "open": self.open,
            "high": self.high,
            "low": self.low,
            "close": self.close,
            "volume": self.volume,
            "oi": self.oi,
        })
    }
}

#[async_trait]
pub trait RunnerServices: Send + Sync {
    /// Whether analyzer (sandbox) mode is on right now.
    fn analyzer_on(&self) -> bool;
    /// Whether an order can be sent to this destination now.
    fn authorised(&self, mode: RunMode) -> Result<(), String>;
    /// History for one instrument, oldest first.
    async fn history(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Bar>, String>;
    /// Place one `/api/v1/placeorder` body on the run's side. `internal` is
    /// an order the app places for the signed-in trader (the exits of a Stop
    /// they pressed), which Semi-Auto order mode does not hold for approval.
    async fn place(&self, mode: RunMode, request: &Value, internal: bool) -> DispatchResult;
    async fn cancel(&self, mode: RunMode, orderid: &str) -> DispatchResult;
    async fn order_status(&self, mode: RunMode, orderid: &str) -> OrderStatusResult;
    /// One account book from the run's side, in the service's own envelope.
    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value>;
    /// The master contract's lot size, when it is a positive whole number.
    fn lot_size(&self, symbol: &str, exchange: &str) -> Option<i64>;
    /// Whether the exchange trades on this date (the market calendar).
    fn is_trading_day(&self, exchange: &str, date: NaiveDate) -> bool;
    /// The instrument record the engine reads (`/openscript/instrument`).
    fn instrument(&self, symbol: &str, exchange: &str, date: NaiveDate) -> Value;
    /// Whether the OpenAlgo user is signed in.
    fn signed_in(&self) -> bool;
}

/// The production services, over the app context.
pub struct AppServices {
    ctx: Weak<AppState>,
    gateway: AppGateway,
}

impl AppServices {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self {
            gateway: AppGateway::new(ctx.clone()),
            ctx,
        }
    }
}

const SHUTTING_DOWN: &str = "OpenAlgo is shutting down";

fn parse_bars(body: &Value) -> Vec<Bar> {
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    body.get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let ts = r.get("timestamp").and_then(Value::as_i64)?;
                    Some(Bar {
                        // History candles are epoch seconds; the engine reads ms.
                        time: ts * 1000,
                        open: n(r, "open"),
                        high: n(r, "high"),
                        low: n(r, "low"),
                        close: n(r, "close"),
                        volume: n(r, "volume"),
                        oi: n(r, "oi"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[async_trait]
impl RunnerServices for AppServices {
    fn analyzer_on(&self) -> bool {
        self.ctx
            .upgrade()
            .is_some_and(|c| crate::services::core::is_analyze(&c))
    }

    fn authorised(&self, mode: RunMode) -> Result<(), String> {
        self.gateway.authorised(mode)
    }

    async fn history(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Bar>, String> {
        let ctx = self.ctx.upgrade().ok_or(SHUTTING_DOWN)?;
        let reply: Reply = crate::services::market_data_service::history(
            &ctx, symbol, exchange, interval, start, end, "api",
        )
        .await;
        if reply.is_success() {
            Ok(parse_bars(&reply.body))
        } else {
            Err(reply.message())
        }
    }

    async fn place(&self, mode: RunMode, request: &Value, internal: bool) -> DispatchResult {
        let Some(ctx) = self.ctx.upgrade() else {
            return DispatchResult::refused(SHUTTING_DOWN);
        };
        let reply = match mode {
            RunMode::Sandbox => {
                // The sandbox engine directly, never the global toggle.
                let reply = match ctx.sandbox.place_order(sandbox_order(request)).await {
                    Ok(p) => Reply::from_ser(&p),
                    Err(e) => Reply::sandbox(&e),
                };
                let s = |k: &str| crate::services::core::s(request, k);
                ctx.bus.publish(Event::OrderPlaced {
                    meta: meta(
                        Mode::Analyze,
                        "placeorder",
                        safe_request(request),
                        &reply.body,
                    ),
                    strategy: s("strategy"),
                    symbol: s("symbol"),
                    exchange: s("exchange"),
                    action: s("action").to_ascii_uppercase(),
                    quantity: crate::services::core::i(request, "quantity"),
                    pricetype: s("pricetype"),
                    product: s("product"),
                    orderid: reply
                        .body
                        .get("orderid")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
                reply
            }
            RunMode::Live => {
                if let Err(e) = self.gateway.authorised(mode) {
                    return DispatchResult::refused(e);
                }
                // force_live: the run decided its side when it started.
                place_order(
                    &ctx,
                    request,
                    Route {
                        force_live: true,
                        internal,
                    },
                )
                .await
            }
        };
        let orderid = reply.body.get("orderid").and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        if reply.is_success() {
            DispatchResult {
                ok: true,
                broker_order_id: orderid,
                response: reply.body,
                error: None,
            }
        } else {
            let m = reply.message();
            DispatchResult {
                ok: false,
                broker_order_id: orderid,
                response: reply.body,
                error: Some(if m.is_empty() {
                    "Order rejected".into()
                } else {
                    m
                }),
            }
        }
    }

    async fn cancel(&self, mode: RunMode, orderid: &str) -> DispatchResult {
        self.gateway.cancel(mode, orderid).await
    }

    async fn order_status(&self, mode: RunMode, orderid: &str) -> OrderStatusResult {
        self.gateway.order_status(mode, orderid).await
    }

    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value> {
        self.gateway.book(mode, book).await
    }

    fn lot_size(&self, symbol: &str, exchange: &str) -> Option<i64> {
        let ctx = self.ctx.upgrade()?;
        ctx.symbols
            .by_symbol(exchange, symbol)
            .map(|r| i64::from(r.lot_size))
            .filter(|l| *l > 0)
    }

    fn is_trading_day(&self, exchange: &str, date: NaiveDate) -> bool {
        let Some(ctx) = self.ctx.upgrade() else {
            return false;
        };
        let cal_ex = crate::trading::instrument::calendar_exchange(exchange);
        match crate::services::market_calendar_service::timings_for(&ctx, date) {
            Ok(list) => list.iter().any(|w| w.exchange == cal_ex),
            Err(e) => {
                // A start on a closed day costs a strategy that finds no
                // market; refusing on an open day costs the session.
                tracing::error!("Could not read the trading calendar: {}", e);
                true
            }
        }
    }

    fn instrument(&self, symbol: &str, exchange: &str, date: NaiveDate) -> Value {
        match self.ctx.upgrade() {
            Some(ctx) => ctx.trading.facts.get(&ctx, symbol, exchange, date),
            None => Value::Null,
        }
    }

    fn signed_in(&self) -> bool {
        self.ctx
            .upgrade()
            .is_some_and(|c| c.signed_in_user().is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_candles_become_millisecond_bars() {
        let bars = parse_bars(&json!({"status": "success", "data": [
            {"timestamp": 1_700_000_000, "open": 1.0, "high": 2.0, "low": 0.5, "close": 1.5, "volume": 10, "oi": 0},
            {"open": 1.0}
        ]}));
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].time, 1_700_000_000_000);
        assert_eq!(bars[0].json()["close"], 1.5);
    }
}
