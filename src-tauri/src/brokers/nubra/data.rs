//! Quotes, depth and history (web `api/data.py`).
//!
//! * Instruments: `GET /orderbooks/{ref_id}?levels=1` (quote) and
//!   `?levels=5` (depth); prices in paise. The web tries its market socket
//!   first and falls back to these; here the REST path is primary (the live
//!   socket is the streaming feed). `/orderbooks` carries no open/high/low
//!   or OI for a quote, exactly as on the web's REST path.
//! * Indices have no order book: a one-shot market socket subscription to
//!   the `index_bucket` channel (1-minute candles) for the snapshot wait,
//!   like the web's index path; zeros when nothing arrives.
//! * History: `POST /charts/timeseries`, chunked per interval (web
//!   `chunk_limits`), whole UTC days clipped to now, 1 request a second,
//!   timestamps in nanoseconds, prices in paise; D/W/M candles normalised to
//!   midnight UTC. No OI history.

use super::mapping;
use super::proto::{self, MarketFrame};
use super::{refused, NubraBroker, DEVICE_ID};
use crate::brokers::common::history;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// web `INDEX_NAME_MAP`: feed `indexname` (upper) -> OpenAlgo symbol.
pub const INDEX_NAME_MAP: &[(&str, &str)] = &[
    ("NIFTY 50", "NIFTY"),
    ("NIFTY BANK", "BANKNIFTY"),
    ("NIFTY FINANCIAL SERVICES", "FINNIFTY"),
    ("BSE SENSEX", "SENSEX"),
    ("BSE SENSEX 50", "SENSEX50"),
];

/// web `SUBSCRIPTION_MAP`: OpenAlgo index -> subscription name.
pub const SUBSCRIPTION_MAP: &[(&str, &str)] = &[
    ("NIFTY", "Nifty 50"),
    ("BANKNIFTY", "Nifty Bank"),
    ("FINNIFTY", "Nifty Financial Services"),
    ("SENSEX", "Bse Sensex"),
    ("SENSEX50", "Bse Sensex 50"),
];

/// Feed name -> the name it is cached under (upper case, mapped).
pub fn feed_name(name: &str) -> String {
    let up = name.trim().to_ascii_uppercase();
    INDEX_NAME_MAP
        .iter()
        .find(|(k, _)| *k == up)
        .map(|(_, v)| v.to_string())
        .unwrap_or(up)
}

/// History chunk size in days per interval (web `chunk_limits`).
pub fn chunk_days(interval: &str) -> i64 {
    match interval {
        "1s" => 7,
        "1m" | "2m" => 30,
        "3m" | "5m" | "15m" => 60,
        "30m" | "1h" => 90,
        "D" => 365,
        "W" => 1000,
        "M" => 1500,
        _ => 30,
    }
}

/// `(exchange, type)` for the timeseries query (web `get_history`).
pub fn history_query_target(symbol: &str, exchange: &str) -> Option<(&'static str, &'static str)> {
    let deriv = || {
        if symbol.ends_with("CE") || symbol.ends_with("PE") {
            "OPT"
        } else {
            "FUT"
        }
    };
    Some(match exchange {
        "NSE_INDEX" => ("NSE", "INDEX"),
        "BSE_INDEX" => ("BSE", "INDEX"),
        "NFO" => ("NSE", deriv()),
        "BFO" => ("BSE", deriv()),
        "MCX" => ("MCX", deriv()),
        "NSE" => ("NSE", "STOCK"),
        "BSE" => ("BSE", "STOCK"),
        _ => return None,
    })
}

/// Market-socket exchange for an OpenAlgo exchange (web
/// `NubraExchangeMapper`).
pub fn ws_exchange(exchange: &str) -> &'static str {
    match exchange {
        "BSE" | "BFO" | "BSE_INDEX" => "BSE",
        "MCX" => "MCX",
        _ => "NSE",
    }
}

fn level(v: &Value) -> DepthLevel {
    let n = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    DepthLevel {
        price: n("p") / 100.0,
        quantity: n("q") as i64,
        orders: n("o") as i64,
    }
}

fn num(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `/orderbooks` answer -> quote (web `_get_quotes_via_rest`).
pub fn quote_from_orderbook(key: &QuoteKey, resp: &Value) -> Option<Quote> {
    let ob = resp.get("orderBook").filter(|o| o.is_object())?;
    let bid = ob.get("bid").and_then(|a| a.get(0)).map(level);
    let ask = ob.get("ask").and_then(|a| a.get(0)).map(level);
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: num(ob, "ltp") / 100.0,
        close: num(ob, "prev_close") / 100.0,
        volume: num(ob, "volume") as i64,
        bid: bid.map(|l| l.price).unwrap_or(0.0),
        bid_qty: bid.map(|l| l.quantity).unwrap_or(0),
        ask: ask.map(|l| l.price).unwrap_or(0.0),
        ask_qty: ask.map(|l| l.quantity).unwrap_or(0),
        ..Default::default()
    };
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = ((q.ltp - q.close) * 100.0).round() / 100.0;
        q.change_percent = ((q.ltp - q.close) / q.close * 10000.0).round() / 100.0;
    }
    Some(q)
}

/// `/orderbooks?levels=5` answer -> depth (web `_get_depth_via_rest`).
pub fn depth_from_orderbook(key: &QuoteKey, resp: &Value) -> Option<MarketDepth> {
    let ob = resp.get("orderBook").filter(|o| o.is_object())?;
    let side = |k: &str| -> (Vec<DepthLevel>, i64) {
        let all: Vec<DepthLevel> = ob
            .get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(level).collect())
            .unwrap_or_default();
        let total = all.iter().map(|l| l.quantity).sum();
        let mut five: Vec<DepthLevel> = all.into_iter().take(5).collect();
        five.resize(5, DepthLevel::default());
        (five, total)
    };
    let (bids, tb) = side("bid");
    let (asks, ts) = side("ask");
    Some(MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids,
        asks,
        ltp: num(ob, "ltp") / 100.0,
        ltq: num(ob, "ltq") as i64,
        open: num(ob, "open") / 100.0,
        high: num(ob, "high") / 100.0,
        low: num(ob, "low") / 100.0,
        prev_close: num(ob, "prev_close") / 100.0,
        volume: num(ob, "volume") as i64,
        oi: num(ob, "oi") as i64,
        total_buy_qty: tb,
        total_sell_qty: ts,
    })
}

fn zero_quote(key: &QuoteKey) -> Quote {
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ..Default::default()
    }
}

fn zero_depth(key: &QuoteKey) -> MarketDepth {
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: vec![DepthLevel::default(); 5],
        asks: vec![DepthLevel::default(); 5],
        ..Default::default()
    }
}

fn instrument_ref(b: &NubraBroker, key: &QuoteKey) -> Result<i64> {
    let token = b
        .resolver()
        .token(&key.symbol, &key.exchange)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })?;
    mapping::ref_id(&token).ok_or_else(|| {
        AppError::Validation(format!(
            "{} on {} has no Nubra instrument id. Download the master contract again.",
            key.symbol, key.exchange
        ))
    })
}

async fn orderbook(b: &NubraBroker, auth: &AuthToken, rid: i64, levels: u8) -> Result<Value> {
    let path = format!("/orderbooks/{}?levels={}", rid, levels);
    let (status, v) = b.call(Method::GET, &path, auth, None).await?;
    if status == 401 || status == 403 {
        return Err(super::session_expired());
    }
    if status >= 400 {
        tracing::warn!(status, "Nubra order book request refused");
        return Err(refused(&v, status));
    }
    Ok(v)
}

pub async fn get_quote(b: &NubraBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    if key.exchange.ends_with("_INDEX") {
        let name = b
            .resolver()
            .br_symbol(&key.symbol, &key.exchange)
            .unwrap_or_else(|| key.symbol.clone());
        let snap = index_snapshot(b, auth, &key.symbol, &name, ws_exchange(&key.exchange)).await;
        return Ok(match snap {
            Some(mut q) => {
                q.symbol = key.symbol.clone();
                q.exchange = key.exchange.clone();
                q
            }
            None => zero_quote(key),
        });
    }
    let rid = instrument_ref(b, key)?;
    let v = orderbook(b, auth, rid, 1).await?;
    Ok(quote_from_orderbook(key, &v).unwrap_or_else(|| {
        tracing::info!("Nubra returned an empty order book for a quote");
        zero_quote(key)
    }))
}

pub async fn get_market_depth(
    b: &NubraBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    if key.exchange.ends_with("_INDEX") {
        return Ok(zero_depth(key));
    }
    let rid = instrument_ref(b, key)?;
    let v = orderbook(b, auth, rid, 5).await?;
    Ok(depth_from_orderbook(key, &v).unwrap_or_else(|| zero_depth(key)))
}

/// Candle state from the `index_bucket` channel.
#[derive(Debug, Default, Clone, Copy)]
struct Bucket {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
}

/// One-shot index snapshot over the market socket (web
/// `_get_quotes_via_websocket` index branch: 1-minute `index_bucket`, full
/// wait). `None` when the socket is unreachable or nothing arrived.
async fn index_snapshot(
    b: &NubraBroker,
    auth: &AuthToken,
    oa_symbol: &str,
    name: &str,
    ws_ex: &str,
) -> Option<Quote> {
    let mut req = b.market_ws_url.as_str().into_client_request().ok()?;
    let h = req.headers_mut();
    h.insert(
        "Authorization",
        format!("Bearer {}", auth.raw()).parse().ok()?,
    );
    h.insert("x-device-id", DEVICE_ID.parse().ok()?);
    let connect = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio_tungstenite::connect_async(req),
    )
    .await;
    let mut ws = match connect {
        Ok(Ok((ws, _))) => ws,
        _ => {
            tracing::debug!("Nubra market socket unavailable for an index quote");
            return None;
        }
    };
    let payload = super::streaming::batch_payload(&[], &[name.to_string()]);
    let sub = format!(
        "batch_subscribe {} index_bucket {} 1m {}",
        auth.raw(),
        payload,
        ws_ex
    );
    if ws.send(Message::Text(sub)).await.is_err() {
        return None;
    }
    let wanted = [feed_name(name), feed_name(oa_symbol)];
    let mut last: Option<Bucket> = None;
    let deadline = tokio::time::Instant::now() + b.snapshot_wait;
    loop {
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Message::Text(t) = &msg {
            if t.trim() == "Invalid Token" {
                break;
            }
        }
        let Message::Binary(raw) = msg else { continue };
        if let Some(MarketFrame::Bucket(m)) = proto::decode_market(&raw) {
            for x in m.indexes.iter().chain(m.instruments.iter()) {
                if wanted.contains(&feed_name(&x.indexname)) {
                    let mut bk = last.unwrap_or_default();
                    if x.open != 0 {
                        bk.open = x.open as f64 / 100.0;
                    }
                    if x.high != 0 {
                        bk.high = x.high as f64 / 100.0;
                    }
                    if x.low != 0 {
                        bk.low = x.low as f64 / 100.0;
                    }
                    if x.close != 0 {
                        bk.close = x.close as f64 / 100.0;
                    }
                    bk.volume = if x.cumulative_volume != 0 {
                        x.cumulative_volume
                    } else {
                        x.bucket_volume
                    };
                    last = Some(bk);
                }
            }
        }
    }
    let unsub = format!(
        "batch_unsubscribe {} index_bucket {} 1m {}",
        auth.raw(),
        payload,
        ws_ex
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let _ = ws.send(Message::Text(unsub)).await;
        let _ = ws.close(None).await;
    })
    .await;
    let bk = last.filter(|b| b.close > 0.0)?;
    Some(Quote {
        ltp: bk.close,
        open: bk.open,
        high: bk.high,
        low: bk.low,
        volume: bk.volume,
        ..Default::default()
    })
}

/// Timeseries request body for one chunk.
pub fn history_body(
    api_exchange: &str,
    kind: &str,
    brsymbol: &str,
    start_iso: &str,
    end_iso: &str,
    interval: &str,
) -> Value {
    let mut fields = vec!["open", "high", "low", "close"];
    if kind != "INDEX" {
        // INDEX rejects the whole query for tick_volume.
        fields.push("tick_volume");
    }
    json!({"query": [{
        "exchange": api_exchange,
        "type": kind,
        "values": [brsymbol],
        "fields": fields,
        "startDate": start_iso,
        "endDate": end_iso,
        "interval": interval,
        "intraDay": false,
        "realTime": false
    }]})
}

/// Merge one `charts` answer into `candles` (keyed by nanosecond `ts`).
/// Returns `Err(reason)` for a rejection.
pub fn merge_chart(
    resp: &Value,
    brsymbol: &str,
    candles: &mut BTreeMap<i64, Candle>,
) -> std::result::Result<(), String> {
    if let Some(e) = resp.get("error").filter(|e| !e.is_null()) {
        return Err(match e {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    if resp.get("message").and_then(Value::as_str) != Some("charts") {
        return Ok(());
    }
    let Some(values) = resp
        .get("result")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("values"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(data) = values.iter().find_map(|v| v.get(brsymbol)) else {
        return Ok(());
    };
    let series = |k: &str| -> Vec<(i64, f64)> {
        data.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|p| {
                        (
                            p.get("ts").and_then(Value::as_i64).unwrap_or(0),
                            p.get("v").and_then(Value::as_f64).unwrap_or(0.0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for (field, apply) in [("open", 0usize), ("high", 1), ("low", 2), ("close", 3)] {
        for (ts, v) in series(field) {
            let c = candles.entry(ts).or_insert(Candle {
                timestamp: ts,
                ..Default::default()
            });
            let p = v / 100.0;
            match apply {
                0 => c.open = p,
                1 => c.high = p,
                2 => c.low = p,
                _ => c.close = p,
            }
        }
    }
    let mut vol = series("tick_volume");
    if vol.is_empty() {
        vol = series("cumulative_volume");
    }
    for (ts, v) in vol {
        if let Some(c) = candles.get_mut(&ts) {
            c.volume = v as i64;
        }
    }
    Ok(())
}

/// Nanosecond candles -> epoch seconds (D/W/M at midnight UTC), sorted.
pub fn finish_candles(candles: BTreeMap<i64, Candle>, interval: &str) -> Vec<Candle> {
    let daily = matches!(interval, "D" | "W" | "M");
    let out: Vec<Candle> = candles
        .into_values()
        .map(|mut c| {
            let mut secs = c.timestamp.div_euclid(1_000_000_000);
            if daily {
                secs -= secs.rem_euclid(86_400);
            }
            c.timestamp = secs;
            c.oi = 0;
            c
        })
        .collect();
    history::sort_dedupe(out)
}

pub async fn get_history(
    b: &NubraBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = super::TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let supported: Vec<&str> = super::TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Timeframe '{}' is not supported by Nubra. Supported timeframes are: {}",
                req.interval,
                supported.join(", ")
            ))
        })?;
    let (api_exchange, kind) = history_query_target(&req.key.symbol, &req.key.exchange)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Exchange '{}' is not supported by Nubra. Supported exchanges: NSE, BSE, NFO, BFO, MCX, NSE_INDEX, BSE_INDEX",
                req.key.exchange
            ))
        })?;
    let brsymbol = b
        .resolver()
        .br_symbol(&req.key.symbol, &req.key.exchange)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                req.key.symbol, req.key.exchange
            ))
        })?;
    let now = chrono::Utc::now().naive_utc();
    let mut candles: BTreeMap<i64, Candle> = BTreeMap::new();
    let mut last_failure: Option<String> = None;
    for (from, to) in history::chunks(req.start, req.end, chunk_days(&req.interval)) {
        let start = from.and_hms_opt(0, 0, 0).unwrap_or_default();
        let mut end = to.and_hms_opt(23, 59, 59).unwrap_or_default();
        if end > now {
            end = now;
        }
        if end <= start {
            break;
        }
        let body = history_body(
            api_exchange,
            kind,
            &brsymbol,
            &start.format("%Y-%m-%dT%H:%M:%S.000Z").to_string(),
            &end.format("%Y-%m-%dT%H:%M:%S.000Z").to_string(),
            interval,
        );
        b.history_pacer.acquire().await;
        match b
            .call(Method::POST, "/charts/timeseries", auth, Some(&body))
            .await
        {
            Ok((status, v)) => {
                if status == 403 {
                    return Err(super::session_expired());
                }
                if let Err(why) = merge_chart(&v, &brsymbol, &mut candles) {
                    tracing::warn!("Nubra refused a history chunk: {}", why);
                    last_failure = Some(why);
                }
            }
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("Nubra history chunk failed: {}", e.code());
                last_failure = Some(e.client_message());
            }
        }
    }
    if candles.is_empty() {
        if let Some(why) = last_failure {
            return Err(AppError::Broker(format!(
                "Nubra rejected the historical data request for {} ({}, {}): {}",
                req.key.symbol, req.key.exchange, req.interval, why
            )));
        }
        return Ok(Vec::new());
    }
    Ok(finish_candles(candles, &req.interval))
}
