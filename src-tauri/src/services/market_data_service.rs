//! Market data (web `quotes_service.py`, `depth_service.py`,
//! `history_service.py`, `intervals_service.py`, `restx_api/ticker.py`,
//! `margin_service.py`). These always go to the broker, in analyzer mode too.

use super::core::{broker_handle, float, num, BrokerHandle, Reply};
use super::schemas::SUPPORTED_INTERVALS;
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::types::{Candle, DepthLevel, HistoryRequest, MarginLeg, Quote, QuoteKey};
use crate::error::AppError;
use crate::state::AppState;
use chrono::{NaiveDate, TimeZone};
use serde_json::{json, Value};

/// Web `validate_symbol_exchange`.
pub fn validate_symbol(ctx: &AppState, symbol: &str, exchange: &str) -> Result<(), String> {
    let ex = exchange.to_ascii_uppercase();
    if !super::schemas::VALID_EXCHANGES.contains(&ex.as_str()) {
        return Err(format!(
            "Invalid exchange '{}'. Must be one of: {}",
            exchange,
            super::schemas::VALID_EXCHANGES.join(", ")
        ));
    }
    if ctx.symbols.token(symbol, &ex).is_none() {
        return Err(format!(
            "Symbol '{}' not found for exchange '{}'. Please verify the symbol name and ensure master contracts are downloaded.",
            symbol, exchange
        ));
    }
    Ok(())
}

/// Web quote dict.
pub fn quote_json(q: &Quote) -> Value {
    json!({
        "ask": num(q.ask),
        "ask_qty": q.ask_qty,
        "bid": num(q.bid),
        "bid_qty": q.bid_qty,
        "high": num(q.high),
        "low": num(q.low),
        "ltp": num(q.ltp),
        "oi": q.oi,
        "open": num(q.open),
        "prev_close": num(q.close),
        "volume": q.volume,
    })
}

fn broker_message(e: &AppError) -> String {
    e.client_message()
}

/// One quote, as the web's `get_quotes` (status and message on failure).
pub async fn fetch_quote(
    ctx: &AppState,
    h: &BrokerHandle,
    symbol: &str,
    exchange: &str,
) -> Result<Quote, Reply> {
    validate_symbol(ctx, symbol, exchange).map_err(|m| Reply::error(400, m))?;
    h.broker
        .get_quote(&h.auth, &QuoteKey::new(exchange, symbol))
        .await
        .map_err(|e| Reply::error(500, broker_message(&e)))
}

/// `quotes`.
pub async fn quotes(ctx: &AppState, symbol: &str, exchange: &str) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match fetch_quote(ctx, &h, symbol, exchange).await {
        Ok(q) => Reply::ok(json!({"status": "success", "data": quote_json(&q)})),
        Err(r) => r,
    }
}

/// `multiquotes`: unknown symbols first (in request order), then the
/// broker's answers for the rest.
pub async fn multiquotes(ctx: &AppState, symbols: &[(String, String)]) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let mut results = Vec::new();
    let mut valid = Vec::new();
    let mut first_error: Option<String> = None;
    for (s, e) in symbols {
        match validate_symbol(ctx, s, e) {
            Ok(()) => valid.push(QuoteKey::new(e.clone(), s.clone())),
            Err(m) => {
                if first_error.is_none() {
                    first_error = Some(m.clone());
                }
                results.push(json!({"symbol": s, "exchange": e, "error": m}));
            }
        }
    }
    if valid.is_empty() {
        return Reply::new(
            400,
            json!({
                "status": "error",
                "message": first_error.unwrap_or_else(|| "No valid symbols provided".into()),
                "invalid_symbols": results,
            }),
        );
    }
    match h.broker.get_multiquotes(&h.auth, &valid).await {
        Ok(rows) => {
            for r in rows {
                match (r.data, r.error) {
                    (Some(q), _) => results.push(json!({
                        "symbol": r.symbol, "exchange": r.exchange, "data": quote_json(&q),
                    })),
                    (None, e) => results.push(json!({
                        "symbol": r.symbol, "exchange": r.exchange,
                        "error": e.unwrap_or_else(|| "No quote data available".into()),
                    })),
                }
            }
            Reply::ok(json!({"status": "success", "results": results}))
        }
        Err(e) => Reply::error(500, broker_message(&e)),
    }
}

fn levels(v: &[DepthLevel]) -> Vec<Value> {
    let mut out: Vec<Value> = v
        .iter()
        .take(5)
        .map(|l| json!({"price": num(l.price), "quantity": l.quantity}))
        .collect();
    while out.len() < 5 {
        out.push(json!({"price": 0, "quantity": 0}));
    }
    out
}

/// `depth`.
pub async fn depth(ctx: &AppState, symbol: &str, exchange: &str) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if let Err(m) = validate_symbol(ctx, symbol, exchange) {
        return Reply::error(400, m);
    }
    match h
        .broker
        .get_market_depth(&h.auth, &QuoteKey::new(exchange, symbol))
        .await
    {
        Ok(d) => Reply::ok(json!({"status": "success", "data": {
            "asks": levels(&d.asks),
            "bids": levels(&d.bids),
            "high": num(d.high),
            "low": num(d.low),
            "ltp": num(d.ltp),
            "ltq": d.ltq,
            "oi": d.oi,
            "open": num(d.open),
            "prev_close": num(d.prev_close),
            "totalbuyqty": d.total_buy_qty,
            "totalsellqty": d.total_sell_qty,
            "volume": d.volume,
        }})),
        Err(e) => Reply::error(500, broker_message(&e)),
    }
}

pub fn candle_json(c: &Candle) -> Value {
    json!({
        "close": float(c.close),
        "high": float(c.high),
        "low": float(c.low),
        "oi": c.oi,
        "open": float(c.open),
        "timestamp": c.timestamp,
        "volume": c.volume,
    })
}

async fn broker_history(
    h: &BrokerHandle,
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Candle>, Reply> {
    // Like the web, an interval the broker does not offer is refused by
    // the broker itself.
    if end < start {
        return Ok(Vec::new());
    }
    let req = HistoryRequest {
        key: QuoteKey::new(exchange, symbol),
        interval: interval.to_string(),
        start,
        end,
    };
    h.broker
        .get_history(&h.auth, &req)
        .await
        .map_err(|e| Reply::error(500, broker_message(&e)))
}

/// `history`.
pub async fn history(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
    source: &str,
) -> Reply {
    if source == "db" {
        return ctx
            .historify
            .history_from_db(symbol, exchange, interval, start, end)
            .await;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if let Err(m) = validate_symbol(ctx, symbol, exchange) {
        return Reply::error(400, m);
    }
    match broker_history(&h, symbol, exchange, interval, start, end).await {
        Ok(c) => Reply::ok(json!({"status": "success",
            "data": c.iter().map(candle_json).collect::<Vec<_>>()})),
        Err(r) => r,
    }
}

fn leading_int(s: &str) -> u32 {
    s.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Web `intervals_service`: the broker's timeframes bucketed by unit.
pub fn bucket_intervals(map: &[(&str, &str)]) -> Value {
    let offered: Vec<&str> = map
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| SUPPORTED_INTERVALS.contains(k))
        .collect();
    let by_suffix = |suf: char| {
        let mut v: Vec<&str> = offered
            .iter()
            .copied()
            .filter(|k| k.ends_with(suf))
            .collect();
        v.sort_by_key(|k| leading_int(k));
        v
    };
    let exact = |x: &str| -> Vec<&str> { offered.iter().copied().filter(|k| *k == x).collect() };
    json!({
        "seconds": by_suffix('s'),
        "minutes": by_suffix('m'),
        "hours": by_suffix('h'),
        "days": exact("D"),
        "weeks": exact("W"),
        "months": exact("M"),
    })
}

/// `intervals`.
pub fn intervals(ctx: &AppState) -> Reply {
    match broker_handle(ctx) {
        Ok(h) => Reply::ok(json!({"status": "success",
            "data": bucket_intervals(h.broker.timeframe_map())})),
        Err(r) => r,
    }
}

// ------------------------------------------------------------------ ticker

/// The `/ticker/<EXCH:SYMBOL>` path part. A path without the exchange
/// prefix is read as an NSE symbol.
pub fn split_ticker(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split(':').collect();
    match parts.as_slice() {
        [e, s] => (e.to_string(), s.to_string()),
        [s] if !s.is_empty() => ("NSE".to_string(), s.to_string()),
        _ => ("NSE".to_string(), path.replace(':', "")),
    }
}

/// Web `validate_and_adjust_date_range`.
pub fn clamp_range(interval: &str, start: NaiveDate, end: NaiveDate) -> NaiveDate {
    let max_days = if matches!(interval.to_ascii_uppercase().as_str(), "D" | "W" | "M") {
        3650
    } else {
        30
    };
    let earliest = end - chrono::Duration::days(max_days);
    if start < earliest {
        earliest
    } else {
        start
    }
}

/// Python `str(float)`.
fn py_float(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}

/// Ticker text lines: `EXCH:SYM,YYYY-MM-DD[,HH:MM:SS],o,h,l,c,v`, IST.
pub fn ticker_text(exchange: &str, symbol: &str, interval: &str, candles: &[Candle]) -> String {
    let daily = interval.eq_ignore_ascii_case("D");
    candles
        .iter()
        .map(|c| {
            let t = chrono_tz::Asia::Kolkata
                .timestamp_opt(c.timestamp, 0)
                .single()
                .map(|d| d.naive_local())
                .unwrap_or_default();
            let date = t.format("%Y-%m-%d");
            let ohlcv = format!(
                "{},{},{},{},{}",
                py_float(c.open),
                py_float(c.high),
                py_float(c.low),
                py_float(c.close),
                c.volume
            );
            if daily {
                format!("{}:{},{},{}", exchange, symbol, date, ohlcv)
            } else {
                format!(
                    "{}:{},{},{},{}",
                    exchange,
                    symbol,
                    date,
                    t.format("%H:%M:%S"),
                    ohlcv
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The ticker data (no symbol validation, as on the web).
pub async fn ticker_candles(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Candle>, Reply> {
    let h = broker_handle(ctx)?;
    let start = clamp_range(interval, start, end);
    broker_history(&h, symbol, exchange, interval, start, end).await
}

// ------------------------------------------------------------------ margin

/// Web `validate_margin_data` (1-based position numbers).
pub fn validate_margin(positions: &[Value]) -> Result<Vec<MarginLeg>, String> {
    let mut legs = Vec::with_capacity(positions.len());
    for (n, p) in positions.iter().enumerate() {
        let i = n + 1;
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let ex = s("exchange");
        if !super::schemas::VALID_EXCHANGES.contains(&ex.as_str()) {
            return Err(format!(
                "Position {}: Invalid exchange. Must be one of: {}",
                i,
                super::schemas::VALID_EXCHANGES.join(", ")
            ));
        }
        let action = s("action").to_ascii_uppercase();
        let Ok(action) = action.parse::<Action>() else {
            return Err(format!(
                "Position {}: Invalid action. Must be one of: BUY, SELL",
                i
            ));
        };
        let Ok(pricetype) = s("pricetype").parse::<PriceType>() else {
            return Err(format!(
                "Position {}: Invalid price type. Must be one of: MARKET, LIMIT, SL, SL-M",
                i
            ));
        };
        let Ok(product) = s("product").parse::<Product>() else {
            return Err(format!(
                "Position {}: Invalid product type. Must be one of: CNC, NRML, MIS",
                i
            ));
        };
        let Ok(qty) = s("quantity").trim().parse::<i64>() else {
            return Err(format!("Position {}: Invalid quantity format", i));
        };
        if qty <= 0 {
            return Err(format!(
                "Position {}: Quantity must be a positive number",
                i
            ));
        }
        let price_s = s("price");
        let Ok(price) = (if price_s.is_empty() {
            "0"
        } else {
            price_s.trim()
        })
        .parse::<f64>() else {
            return Err(format!("Position {}: Invalid price format", i));
        };
        if price < 0.0 {
            return Err(format!("Position {}: Price cannot be negative", i));
        }
        let trigger = s("trigger_price").trim().parse::<f64>().unwrap_or(0.0);
        legs.push(MarginLeg {
            key: QuoteKey::new(ex, s("symbol")),
            action,
            quantity: qty,
            product,
            pricetype,
            price,
            trigger_price: trigger,
        });
    }
    Ok(legs)
}

/// `margin` (always the live broker, as on the web).
pub async fn margin(ctx: &AppState, positions: &[Value]) -> Reply {
    let legs = match validate_margin(positions) {
        Ok(l) => l,
        Err(m) => return Reply::error(400, m),
    };
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    // BR-01: a leg missing from the master refuses the basket by name
    // instead of being dropped, so a "success" always covers every leg (a
    // deliberate difference from the web, whose adapters skip such legs and
    // total the rest).
    if let Some((i, l)) = legs.iter().enumerate().find(|(_, l)| {
        ctx.symbols
            .by_symbol(&l.key.exchange, &l.key.symbol)
            .is_none()
    }) {
        return Reply::error(
            400,
            format!(
                "Position {}: Symbol {} not found on {}. Check the symbol and exchange, or download the master contract again.",
                i + 1,
                l.key.symbol,
                l.key.exchange
            ),
        );
    }
    match h.broker.calculate_margin(&h.auth, &legs).await {
        Ok(m) => Reply::ok(json!({"status": "success", "data": {
            "total_margin_required": num(m.total_margin_required),
            "span_margin": num(m.span_margin),
            "exposure_margin": num(m.exposure_margin),
        }})),
        Err(AppError::Unsupported(_)) => Reply::error(
            501,
            format!("Margin calculation not implemented for broker: {}", h.id),
        ),
        Err(AppError::Broker(m)) | Err(AppError::Validation(m)) => Reply::error(400, m),
        Err(e) => {
            tracing::error!("Margin calculation failed: {}", e);
            Reply::error(500, "Failed to calculate margin due to internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_bucket_like_the_web() {
        let map = [
            ("1m", "minute"),
            ("3m", "3minute"),
            ("5m", "5minute"),
            ("10m", "10minute"),
            ("15m", "15minute"),
            ("30m", "30minute"),
            ("60m", "60minute"),
            ("1h", "60minute"),
            ("D", "day"),
        ];
        assert_eq!(
            bucket_intervals(&map),
            json!({"seconds": [], "minutes": ["1m", "3m", "5m", "10m", "15m", "30m"],
                "hours": ["1h"], "days": ["D"], "weeks": [], "months": []})
        );
    }

    #[test]
    fn ticker_paths_and_text() {
        assert_eq!(
            split_ticker("NSE:RELIANCE"),
            ("NSE".into(), "RELIANCE".into())
        );
        assert_eq!(split_ticker("SBIN"), ("NSE".into(), "SBIN".into()));
        let c = Candle {
            timestamp: 1790826300,
            open: 1180.1,
            high: 1182.5,
            low: 1177.0,
            close: 1180.2,
            volume: 653284,
            oi: 0,
        };
        assert_eq!(
            ticker_text("NSE", "RELIANCE", "5m", &[c]),
            "NSE:RELIANCE,2026-10-01,09:15:00,1180.1,1182.5,1177.0,1180.2,653284"
        );
        assert_eq!(
            ticker_text("NSE", "RELIANCE", "D", &[c]),
            "NSE:RELIANCE,2026-10-01,1180.1,1182.5,1177.0,1180.2,653284"
        );
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            clamp_range("5m", d("2026-01-01"), d("2026-10-01")),
            d("2026-09-01")
        );
        assert_eq!(
            clamp_range("D", d("2026-01-01"), d("2026-10-01")),
            d("2026-01-01")
        );
    }

    #[test]
    fn margin_validation_messages() {
        let p = |q: &str| {
            json!([{"symbol": "SBIN", "exchange": "NSE", "action": "buy",
            "quantity": q, "product": "MIS", "pricetype": "MARKET", "price": "0"}])
        };
        assert!(validate_margin(p("10").as_array().unwrap()).is_ok());
        assert_eq!(
            validate_margin(p("1.5").as_array().unwrap()).unwrap_err(),
            "Position 1: Invalid quantity format"
        );
        assert_eq!(
            validate_margin(p("0").as_array().unwrap()).unwrap_err(),
            "Position 1: Quantity must be a positive number"
        );
    }

    /// BR-01: a basket with a leg missing from the master is refused by
    /// name before any broker call, never totalled without it; a basket of
    /// known legs reaches the broker whole.
    #[tokio::test]
    async fn margin_refuses_an_unknown_leg_by_name() {
        use crate::brokers::common::symbols::tests::row;
        use crate::brokers::mock::{MockBroker, MockCall};
        use crate::brokers::{Broker, BrokerRegistry};
        use std::sync::Arc;
        let mock = Arc::new(MockBroker::new("zerodha"));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn Broker>]),
            chrono::Utc::now(),
        );
        let ctx = &t.ctx;
        ctx.load_symbol_cache(vec![
            row("SBIN", "SBIN", "NSE", "779521"),
            row("INFY", "INFY", "NSE", "408065"),
        ]);
        ctx.set_broker_session(Some(crate::state::BrokerSession {
            broker_id: "zerodha".into(),
            auth_token: crate::security::Secret::new("t"),
            feed_token: None,
            user_id: "AB1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        }));
        let leg = |s: &str| {
            json!({"symbol": s, "exchange": "NSE", "action": "BUY", "quantity": "1",
                "product": "MIS", "pricetype": "MARKET", "price": "0"})
        };
        let r = margin(ctx, &[leg("SBIN"), leg("NOSUCH")]).await;
        assert_eq!(r.status, 400);
        assert_eq!(
            r.message(),
            "Position 2: Symbol NOSUCH not found on NSE. Check the symbol and exchange, or download the master contract again."
        );
        assert!(!mock
            .calls
            .lock()
            .iter()
            .any(|c| matches!(c, MockCall::Margin(_))));
        let r = margin(ctx, &[leg("SBIN"), leg("INFY")]).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(mock.calls.lock().contains(&MockCall::Margin(2)));
    }
}
