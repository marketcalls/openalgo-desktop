//! Market data (web `quotes_service.py`, `depth_service.py`,
//! `history_service.py`, `intervals_service.py`, `restx_api/ticker.py`,
//! `margin_service.py`). These always go to the broker, in analyzer mode too.

use super::core::{broker_handle, float, num, BrokerHandle, Reply};
use super::schemas::SUPPORTED_INTERVALS;
use crate::brokers::common::history;
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
        .map_err(|e| Reply::broker_failure(&e, 500))
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
        Err(e) => Reply::broker_failure(&e, 500),
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
        Err(e) => Reply::broker_failure(&e, 500),
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

/// The broker's candles for `req`, each session opening at the pre-open
/// discovered price where the broker's bars include the pre-open
/// (`pre_open`, web flattrade openalgo#2482), on the session grid where
/// the broker's are not (`session_grid`), without the bars a broker
/// appends after the session close (web dhan `_drop_bars_after_close`,
/// openalgo#2308): session opens and closes come from the market
/// calendar, which only a service reads. Every history read
/// (`/api/v1/history`, the ticker, the charts, Historify) comes through
/// here.
pub async fn broker_candles(
    ctx: &AppState,
    h: &BrokerHandle,
    req: &HistoryRequest,
) -> crate::error::Result<Vec<Candle>> {
    let mut candles = h.broker.get_history(&h.auth, req).await?;
    if let Some(bar_minutes) = h.broker.history_pre_open_minutes(req) {
        candles = super::pre_open::open_sessions(ctx, req, bar_minutes, candles).await;
    }
    if h.broker.history_on_session_grid(req) {
        candles = super::session_grid::align(ctx, h, req, candles).await?;
    }
    if !h.broker.history_trims_after_close(req) {
        return Ok(candles);
    }
    let exchange = history::session_exchange(&req.key.exchange);
    Ok(history::drop_bars_after_close(candles, |day| {
        session_close_ms(ctx, day, exchange)
    }))
}

/// The latest close of `exchange`'s sessions on `day`, epoch ms; `None`
/// when the calendar has no session for it that day (or cannot be read,
/// which leaves the bars as the broker sent them).
fn session_close_ms(ctx: &AppState, day: NaiveDate, exchange: &str) -> Option<i64> {
    match super::market_calendar_service::timings_for(ctx, day) {
        Ok(windows) => windows
            .iter()
            .filter(|w| w.exchange == exchange)
            .map(|w| w.end_time)
            .max(),
        Err(e) => {
            tracing::warn!(
                "Market calendar for {} could not be read; history bars after the close are kept: {}",
                day,
                e.code()
            );
            None
        }
    }
}

async fn broker_history(
    ctx: &AppState,
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
    broker_candles(ctx, h, &req)
        .await
        .map_err(|e| Reply::broker_failure(&e, 500))
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
    match broker_history(ctx, &h, symbol, exchange, interval, start, end).await {
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
    broker_history(ctx, &h, symbol, exchange, interval, start, end).await
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

/// Brokers whose web plugin has no `api/margin_api` module. The web's
/// `import_broker_module` fails for them before any symbol or broker call
/// and answers 404 "not supported"; the 501 "not implemented" is for a
/// broker whose module exists but raises `NotImplementedError`
/// (`services/margin_service.py`, web ee123273b).
const NO_WEB_MARGIN_MODULE: &[&str] = &["hdfcsecurities", "nuvama", "rupeezy"];

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
    if NO_WEB_MARGIN_MODULE.contains(&h.id.as_str()) {
        return Reply::error(
            404,
            format!("Margin calculation not supported for broker: {}", h.id),
        );
    }
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
        Err(e @ AppError::Busy(_)) => Reply::broker_failure(&e, 500),
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

    #[tokio::test]
    async fn history_drops_the_bar_a_broker_appends_after_the_close() {
        use crate::brokers::common::symbols::tests::row;
        use crate::brokers::mock::MockBroker;
        use crate::brokers::types::Candle;
        use crate::brokers::{Broker, BrokerRegistry};
        use std::sync::Arc;
        // Web openalgo#2308: after the close Dhan appends one flat bar,
        // stamped at the request time, to index series (SENSEX 15m: 15:15,
        // then 16:00). The bar at 16:00 goes on every history read; the
        // session's bars stay, and a broker that does not append one keeps
        // every bar. Tuesday 2026-10-27, a BSE trading day closing 15:30.
        let mock = Arc::new(MockBroker::new("dhan"));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn Broker>]),
            chrono::Utc::now(),
        );
        let ctx = &t.ctx;
        let mut sensex = row("SENSEX", "SENSEX", "BSE_INDEX", "51");
        sensex.brexchange = "IDX_I".into();
        ctx.load_symbol_cache(vec![sensex]);
        ctx.set_broker_session(Some(crate::state::BrokerSession {
            broker_id: "dhan".into(),
            auth_token: crate::security::Secret::new("t"),
            feed_token: None,
            user_id: "AB1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        }));
        let day = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let ist = |h: u32, m: u32| day.and_hms_opt(h, m, 0).unwrap().and_utc().timestamp() - 19_800;
        let bar = |ts: i64| Candle {
            timestamp: ts,
            open: 82_000.0,
            high: 82_000.0,
            low: 82_000.0,
            close: 82_000.0,
            ..Default::default()
        };
        *mock.history.lock() = Some(Ok(vec![bar(ist(9, 15)), bar(ist(15, 15)), bar(ist(16, 0))]));
        let stamps = |r: &Reply| -> Vec<i64> {
            r.body["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["timestamp"].as_i64().unwrap())
                .collect()
        };
        *mock.trims_after_close.lock() = true;
        let r = history(ctx, "SENSEX", "BSE_INDEX", "15m", day, day, "api").await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(stamps(&r), vec![ist(9, 15), ist(15, 15)]);
        let ticker = ticker_candles(ctx, "SENSEX", "BSE_INDEX", "15m", day, day)
            .await
            .unwrap();
        assert_eq!(ticker.len(), 2);

        *mock.trims_after_close.lock() = false;
        let r = history(ctx, "SENSEX", "BSE_INDEX", "15m", day, day, "api").await;
        assert_eq!(stamps(&r), vec![ist(9, 15), ist(15, 15), ist(16, 0)]);
    }

    /// A context with the mock broker logged in as `id` and SBIN and INFY
    /// in the master, for the margin tests and the long daily history.
    fn margin_as(
        id: &'static str,
    ) -> (
        crate::state::testing::TestCtx,
        std::sync::Arc<crate::brokers::mock::MockBroker>,
    ) {
        use crate::brokers::common::symbols::tests::row;
        use crate::brokers::mock::MockBroker;
        use crate::brokers::{Broker, BrokerRegistry};
        use std::sync::Arc;
        let mock = Arc::new(MockBroker::new(id));
        let t = crate::state::testing::build(
            BrokerRegistry::with(vec![mock.clone() as Arc<dyn Broker>]),
            chrono::Utc::now(),
        );
        t.ctx.load_symbol_cache(vec![
            row("SBIN", "SBIN", "NSE", "779521"),
            row("INFY", "INFY", "NSE", "408065"),
        ]);
        t.ctx.set_broker_session(Some(crate::state::BrokerSession {
            broker_id: id.into(),
            auth_token: crate::security::Secret::new("t"),
            feed_token: None,
            user_id: "AB1".into(),
            user_name: None,
            authenticated_at: t.ctx.now(),
        }));
        (t, mock)
    }

    fn margin_leg(symbol: &str) -> Value {
        json!({"symbol": symbol, "exchange": "NSE", "action": "BUY", "quantity": "1",
            "product": "MIS", "pricetype": "MARKET", "price": "0"})
    }

    fn margin_asked(mock: &crate::brokers::mock::MockBroker) -> bool {
        use crate::brokers::mock::MockCall;
        mock.calls
            .lock()
            .iter()
            .any(|c| matches!(c, MockCall::Margin(_)))
    }

    /// BR-01: a basket with a leg missing from the master is refused by
    /// name before any broker call, never totalled without it; a basket of
    /// known legs reaches the broker whole.
    #[tokio::test]
    async fn margin_refuses_an_unknown_leg_by_name() {
        use crate::brokers::mock::MockCall;
        let (t, mock) = margin_as("zerodha");
        let ctx = &t.ctx;
        let r = margin(ctx, &[margin_leg("SBIN"), margin_leg("NOSUCH")]).await;
        assert_eq!(r.status, 400);
        assert_eq!(
            r.message(),
            "Position 2: Symbol NOSUCH not found on NSE. Check the symbol and exchange, or download the master contract again."
        );
        assert!(!margin_asked(&mock));
        let r = margin(ctx, &[margin_leg("SBIN"), margin_leg("INFY")]).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(mock.calls.lock().contains(&MockCall::Margin(2)));
    }

    /// A broker whose web plugin has no margin module (Nuvama, Rupeezy,
    /// HDFC Securities) answers the web's 404 "not supported" before any
    /// symbol or broker call, as the web's failed import does
    /// (`services/margin_service.py`, web ee123273b), even for a basket
    /// of known legs; any other broker still has its legs checked against
    /// the master (BR-01) and reaches the broker.
    #[tokio::test]
    async fn margin_without_a_web_module_is_not_supported() {
        for id in ["nuvama", "rupeezy", "hdfcsecurities"] {
            let (t, mock) = margin_as(id);
            for leg in ["SBIN", "NOSUCH"] {
                let r = margin(&t.ctx, &[margin_leg(leg)]).await;
                assert_eq!(r.status, 404, "{id} {leg}: {}", r.body);
                assert_eq!(
                    r.body,
                    json!({"status": "error",
                        "message": format!("Margin calculation not supported for broker: {id}")})
                );
            }
            assert!(!margin_asked(&mock), "{id}");
        }
        let (t, mock) = margin_as("zerodha");
        let r = margin(&t.ctx, &[margin_leg("NOSUCH")]).await;
        assert_eq!(r.status, 400, "{}", r.body);
        let r = margin(&t.ctx, &[margin_leg("SBIN")]).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(margin_asked(&mock));
    }

    /// The `/trading` chart builds W and M from daily bars and asks for up
    /// to 2000 days of D a call (web openalgo#2384, 6c9d35b61; the change is
    /// the chart's alone). The window reaches the broker whole, in one call,
    /// and every daily bar comes back as the broker sent it, including one
    /// on a day the calendar shows closed (the Sunday Muhurat session of
    /// 2019-10-27, before the holiday list starts), which the chart places
    /// on its own date.
    #[tokio::test]
    async fn history_answers_a_long_daily_window_whole() {
        use crate::brokers::mock::MockCall;
        use crate::brokers::types::Candle;
        let (t, mock) = margin_as("zerodha");
        let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let ist_midnight =
            |day: NaiveDate| day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - 19_800;
        let start = date(2016, 11, 1);
        let end = start + chrono::Duration::days(2000);
        let days = [
            start,
            date(2019, 10, 25),
            date(2019, 10, 27),
            date(2019, 10, 29),
            end,
        ];
        let stamps: Vec<i64> = days.iter().map(|d| ist_midnight(*d)).collect();
        *mock.history.lock() = Some(Ok(stamps
            .iter()
            .map(|&timestamp| Candle {
                timestamp,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: 10,
                ..Default::default()
            })
            .collect()));
        let r = history(&t.ctx, "SBIN", "NSE", "D", start, end, "api").await;
        assert_eq!(r.status, 200, "{}", r.body);
        let answered: Vec<i64> = r.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["timestamp"].as_i64().unwrap())
            .collect();
        assert_eq!(answered, stamps);
        let asked: Vec<(String, NaiveDate, NaiveDate)> = mock
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                MockCall::History(req) => Some((req.interval, req.start, req.end)),
                _ => None,
            })
            .collect();
        assert_eq!(asked, vec![("D".to_string(), start, end)]);
    }
}
