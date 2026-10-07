//! Market data (web `api/data.py`).
//!
//! Tradejini has no REST quote or depth endpoint. Like the web, each quote,
//! multiquote or depth request opens its own NxtradStream socket,
//! subscribes, waits a bounded time for the data, and closes the socket on
//! every path (success, timeout, error).
//!
//! * quote: 3 s settle, `L1` subscribe, then up to 40 one-second steps; a
//!   quote holding every field (`ltp, open, high, low, close, vol, bidPrice,
//!   askPrice` and `OI` on derivatives) is returned at once, an incomplete
//!   one at the end of the first step it is seen in.
//! * multiquote: batches of 100 on one socket, 2 s settle per batch, wait
//!   `clamp(n * 0.05 s, 2 s, 10 s)` or until every quote is complete.
//! * depth: `L5` subscribe, the first book within 20 s.
//! * history: `GET /api/mkt-data/chart/interval-data?id=<brsymbol>&interval=
//!   &from=<epoch s>&to=`, from 09:15:00 IST of the start date to 23:59:59
//!   IST of the end date, one request (no chunking), bars as objects
//!   `{time,open,high,low,close,volume}` or arrays `[t,o,h,l,c,v]`.

use super::streaming::{self, decode_message, Packet, L1, L5};
use super::{mapping, Body, TradejiniBroker, TIMEFRAME_MAP};
use crate::brokers::common::history::{sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::common::streaming::Message;
use crate::brokers::hdfcsky::streaming::ws_error_kind;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Multiquote batch size (web `BATCH_SIZE`).
pub const MULTI_BATCH: usize = 100;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// An instrument resolved for the socket.
#[derive(Debug, Clone)]
struct Target {
    key: QuoteKey,
    ws_key: String,
}

fn resolve(b: &TradejiniBroker, key: &QuoteKey) -> Result<Target> {
    let row = b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })?;
    Ok(Target {
        key: key.clone(),
        ws_key: streaming::ws_key(&row.token, &row.brexchange, &row.exchange),
    })
}

/// Socket session state: merged L1 per key and the latest L5 per key.
#[derive(Default)]
struct Session {
    l1: HashMap<String, L1>,
    l5: HashMap<String, L5>,
}

impl Session {
    fn absorb(&mut self, msg: &Message) {
        let Message::Binary(b) = msg else {
            return;
        };
        for p in decode_message(b) {
            match p {
                Packet::L1(d) => {
                    let k = d.key();
                    self.l1
                        .entry(k)
                        .or_insert_with(|| L1 {
                            token: d.token,
                            exch: d.exch.clone(),
                            ..Default::default()
                        })
                        .merge(&d);
                }
                Packet::L5(book) => {
                    self.l5.insert(book.key(), book);
                }
                _ => {}
            }
        }
    }
}

async fn open(b: &TradejiniBroker, auth: &AuthToken) -> Result<Ws> {
    let (key, token) = TradejiniBroker::pair(auth)?;
    // The URL carries the access token: it is never logged, and a connect
    // error is logged by kind only (its text can echo the request).
    let url = crate::security::secret::Secret::new(format!(
        "{}?token={}:{}&version={}",
        b.stream_url,
        key,
        token,
        streaming::STREAM_VERSION
    ));
    let connect = tokio_tungstenite::connect_async(url.expose());
    match tokio::time::timeout(b.timings.connect, connect).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r)))
            if matches!(r.status().as_u16(), 401 | 403) =>
        {
            Err(super::session_expired())
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Tradejini quote socket could not connect: {}",
                ws_error_kind(&e)
            );
            Err(feed_unavailable())
        }
        Err(_) => {
            tracing::warn!("Tradejini quote socket timed out while connecting");
            Err(feed_unavailable())
        }
    }
}

fn feed_unavailable() -> AppError {
    AppError::Broker(
        "Tradejini's live price service did not respond. Try again in a few seconds.".into(),
    )
}

/// Read frames into `s` until `deadline` or until `done(s)` holds.
/// Returns `Err` when the socket closed or failed.
async fn pump(
    ws: &mut Ws,
    s: &mut Session,
    deadline: Instant,
    done: &(dyn Fn(&Session) -> bool + Sync),
) -> Result<bool> {
    loop {
        if done(s) {
            return Ok(true);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        match tokio::time::timeout(deadline - now, ws.next()).await {
            Err(_) => return Ok(done(s)),
            Ok(Some(Ok(m @ Message::Binary(_)))) => s.absorb(&m),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                return Err(feed_unavailable())
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Run `body` on a fresh socket and close it whatever happens.
async fn with_socket<T, F>(b: &TradejiniBroker, auth: &AuthToken, body: F) -> Result<T>
where
    F: for<'a> FnOnce(
        &'a mut Ws,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>,
    >,
{
    let mut ws = open(b, auth).await?;
    let out = body(&mut ws).await;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
    out
}

/// Settle pause before subscribing; frames that arrive meanwhile (the auth
/// acknowledgement) are drained.
async fn settle(ws: &mut Ws, d: Duration) -> Result<()> {
    let mut s = Session::default();
    pump(ws, &mut s, Instant::now() + d, &|_| false)
        .await
        .map(|_| ())
}

fn to_quote(key: &QuoteKey, q: &L1) -> Quote {
    let ltp = q.ltp.unwrap_or(0.0);
    let close = q.close.unwrap_or(0.0);
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: q.open.unwrap_or(0.0),
        high: q.high.unwrap_or(0.0),
        low: q.low.unwrap_or(0.0),
        close,
        volume: q.vol.unwrap_or(0),
        bid: q.bid_price.unwrap_or(0.0),
        ask: q.ask_price.unwrap_or(0.0),
        bid_qty: q.bid_qty.unwrap_or(0),
        ask_qty: q.ask_qty.unwrap_or(0),
        oi: q.oi.unwrap_or(0),
        change: q.chng.unwrap_or(0.0),
        change_percent: q.chng_per.unwrap_or(0.0),
        timestamp: String::new(),
    };
    if q.chng.is_none() && close > 0.0 {
        out.change = ((ltp - close) * 100.0).round() / 100.0;
        out.change_percent = ((ltp - close) / close * 10_000.0).round() / 100.0;
    }
    out
}

pub async fn get_quote(b: &TradejiniBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let t = resolve(b, key)?;
    let timings = b.timings;
    let q = with_socket(b, auth, |ws| {
        Box::pin(async move {
            settle(ws, timings.quote_settle).await?;
            ws.send(streaming::sub_frame("L1", std::slice::from_ref(&t.ws_key)))
                .await
                .map_err(|_| feed_unavailable())?;
            let mut s = Session::default();
            let exchange = t.key.exchange.clone();
            let wk = t.ws_key.clone();
            let complete = move |s: &Session| {
                s.l1.get(&wk)
                    .map(|q| q.is_complete(&exchange))
                    .unwrap_or(false)
            };
            for _ in 0..timings.quote_steps.max(1) {
                pump(ws, &mut s, Instant::now() + timings.quote_step, &complete).await?;
                if let Some(q) = s.l1.get(&t.ws_key) {
                    return Ok(Some(q.clone()));
                }
            }
            Ok(None)
        })
    })
    .await?;
    match q {
        Some(q) => Ok(to_quote(key, &q)),
        None => {
            tracing::warn!(
                "Tradejini sent no quote for {} {}",
                key.exchange,
                key.symbol
            );
            Err(AppError::Broker(format!(
                "Tradejini sent no price for {} on {}. The market may be closed or the instrument inactive.",
                key.symbol, key.exchange
            )))
        }
    }
}

pub async fn get_multiquotes(
    b: &TradejiniBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    // Resolve first: unknown symbols are reported per entry, not sent.
    let mut out: Vec<Option<QuoteResult>> = vec![None; keys.len()];
    let mut targets: Vec<(usize, Target)> = Vec::new();
    for (idx, k) in keys.iter().enumerate() {
        match resolve(b, k) {
            Ok(t) => targets.push((idx, t)),
            Err(_) => {
                out[idx] = Some(QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("Could not resolve token".into()),
                })
            }
        }
    }
    if !targets.is_empty() {
        let timings = b.timings;
        let found: HashMap<usize, L1> = with_socket(b, auth, |ws| {
            Box::pin(async move {
                let mut found = HashMap::new();
                for batch in targets.chunks(MULTI_BATCH) {
                    settle(ws, timings.multi_settle).await?;
                    let mut ws_keys: Vec<String> = Vec::new();
                    for (_, t) in batch {
                        if !ws_keys.contains(&t.ws_key) {
                            ws_keys.push(t.ws_key.clone());
                        }
                    }
                    ws.send(streaming::sub_frame("L1", &ws_keys))
                        .await
                        .map_err(|_| feed_unavailable())?;
                    let wait = (timings.multi_per_symbol * ws_keys.len() as u32)
                        .clamp(timings.multi_min, timings.multi_max);
                    let want: Vec<(String, String)> = batch
                        .iter()
                        .map(|(_, t)| (t.ws_key.clone(), t.key.exchange.clone()))
                        .collect();
                    let all = move |s: &Session| {
                        want.iter()
                            .all(|(k, e)| s.l1.get(k).map(|q| q.is_complete(e)).unwrap_or(false))
                    };
                    let mut s = Session::default();
                    pump(ws, &mut s, Instant::now() + wait, &all).await?;
                    for (idx, t) in batch {
                        if let Some(q) = s.l1.get(&t.ws_key) {
                            found.insert(*idx, q.clone());
                        }
                    }
                }
                Ok(found)
            })
        })
        .await?;
        for (idx, k) in keys.iter().enumerate() {
            if out[idx].is_some() {
                continue;
            }
            out[idx] = Some(match found.get(&idx) {
                Some(q) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: Some(to_quote(k, q)),
                    error: None,
                },
                None => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("No data received".into()),
                },
            });
        }
    }
    Ok(out.into_iter().flatten().collect())
}

/// web `_format_depth`: five levels each side, totals summed from them.
pub fn to_depth(key: &QuoteKey, book: &L5) -> MarketDepth {
    let bids = streaming::pad5(&book.bids);
    let asks = streaming::pad5(&book.asks);
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: bids.iter().map(|l| l.quantity).sum(),
        total_sell_qty: asks.iter().map(|l| l.quantity).sum(),
        bids,
        asks,
        ..Default::default()
    }
}

pub async fn get_market_depth(
    b: &TradejiniBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let t = resolve(b, key)?;
    let timings = b.timings;
    let book = with_socket(b, auth, |ws| {
        Box::pin(async move {
            ws.send(streaming::sub_frame("L5", std::slice::from_ref(&t.ws_key)))
                .await
                .map_err(|_| feed_unavailable())?;
            let mut s = Session::default();
            let wk = t.ws_key.clone();
            let arrived = move |s: &Session| s.l5.contains_key(&wk);
            pump(ws, &mut s, Instant::now() + timings.depth_wait, &arrived).await?;
            Ok(s.l5.remove(&t.ws_key))
        })
    })
    .await?;
    match book {
        Some(book) => Ok(to_depth(key, &book)),
        None => {
            tracing::warn!(
                "Tradejini sent no depth for {} {}",
                key.exchange,
                key.symbol
            );
            Err(AppError::Broker(format!(
                "Tradejini sent no market depth for {} on {}. The market may be closed.",
                key.symbol, key.exchange
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// `from` / `to` epoch seconds: 09:15:00 IST of `start`, 23:59:59 IST of
/// `end` (web `parse_timestamp`).
pub fn history_window(req: &HistoryRequest) -> (i64, i64) {
    let at = |d: chrono::NaiveDate, h, m, s| {
        d.and_hms_opt(h, m, s)
            .map(|t| t.and_utc().timestamp() - IST_OFFSET_SECS)
            .unwrap_or(0)
    };
    (at(req.start, 9, 15, 0), at(req.end, 23, 59, 59))
}

/// Bars from a `d.bars` array. Times in epoch seconds; epoch milliseconds
/// (any bar above 1e12) are scaled down, like the web.
pub fn parse_bars(v: &Value) -> Vec<Candle> {
    let bars = v
        .get("d")
        .and_then(|d| d.get("bars"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<Candle> = Vec::with_capacity(bars.len());
    for bar in &bars {
        let c = match bar {
            Value::Object(_) => Candle {
                timestamp: mapping::i(bar, "time"),
                open: mapping::f(bar, "open"),
                high: mapping::f(bar, "high"),
                low: mapping::f(bar, "low"),
                close: mapping::f(bar, "close"),
                volume: mapping::i(bar, "volume"),
                oi: 0,
            },
            Value::Array(a) if a.len() >= 5 => Candle {
                timestamp: mapping::num(a.first()) as i64,
                open: mapping::num(a.get(1)),
                high: mapping::num(a.get(2)),
                low: mapping::num(a.get(3)),
                close: mapping::num(a.get(4)),
                volume: mapping::num(a.get(5)) as i64,
                oi: 0,
            },
            _ => continue,
        };
        out.push(c);
    }
    if out.iter().any(|c| c.timestamp > 1_000_000_000_000) {
        for c in &mut out {
            c.timestamp /= 1000;
        }
    }
    sort_dedupe(out)
}

pub async fn get_history(
    b: &TradejiniBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Interval {} is not available for Tradejini. Use 1m, 5m or 30m.",
                req.interval
            ))
        })?;
    let row = b
        .resolver()
        .by_symbol(&req.key.exchange, &req.key.symbol)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                req.key.symbol, req.key.exchange
            ))
        })?;
    let (from, to) = history_window(req);
    let v = b
        .call(
            Method::GET,
            "/api/mkt-data/chart/interval-data",
            &[
                ("id", row.br_symbol().to_string()),
                ("interval", interval.to_string()),
                ("from", from.to_string()),
                ("to", to.to_string()),
            ],
            auth,
            Body::None,
        )
        .await?;
    Ok(parse_bars(&v))
}
