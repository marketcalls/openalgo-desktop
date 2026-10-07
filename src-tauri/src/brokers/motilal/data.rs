//! Quotes, multiquotes, depth and the daily bar (web `api/data.py`).
//!
//! * Quotes: `getltpdata` (values in paise) and, for indices,
//!   `getindexltpdata` (rupees, a list). Dealer logins need `clientcode`,
//!   which is only sent after Motilal answers MO1062 once.
//! * Multiquotes and depth come from the binary broadcast feed, as on the
//!   web: one short-lived socket per call (login, register, collect until
//!   the snapshot is complete or the wait runs out, unregister, close on
//!   every path). At most eight such reads run at once.
//! * History: Motilal has no OHLC API; only today's daily bar is served,
//!   built from the live quote.

use super::mapping::{self, vf, vi, vs};
use super::streaming::{self, FeedState, ScripData};
use super::{motilal_error, paths, MotilalBroker, MotilalSession};
use crate::brokers::common::streaming::Message;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, TimeZone};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::{timeout, Instant};

/// web `BATCH_SIZE` of `get_multiquotes`.
pub const MULTIQUOTE_BATCH: usize = 100;

fn paise(v: f64) -> f64 {
    if v == 0.0 {
        0.0
    } else {
        v / 100.0
    }
}

/// `getltpdata` `data` -> quote (paise -> rupees; `close` is the previous
/// close; no OI on this endpoint).
pub fn quote_from_ltp_data(d: &Value, key: &QuoteKey) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: paise(vf(d, "ltp")),
        open: paise(vf(d, "open")),
        high: paise(vf(d, "high")),
        low: paise(vf(d, "low")),
        close: paise(vf(d, "close")),
        volume: vi(d, "volume"),
        bid: paise(vf(d, "bid")),
        ask: paise(vf(d, "ask")),
        ..Default::default()
    };
    fill_change(&mut q);
    q
}

/// `getindexltpdata` rows -> quote for `index_code` (rupees; first row when
/// none matches, as on the web).
pub fn index_quote_from_rows(rows: &[Value], index_code: &str, key: &QuoteKey) -> Option<Quote> {
    let row = rows
        .iter()
        .find(|r| vs(r, "scripcode").as_deref() == Some(index_code))
        .or_else(|| rows.first())?;
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: vf(row, "ltp"),
        open: vf(row, "open"),
        high: vf(row, "high"),
        low: vf(row, "low"),
        close: vf(row, "close"),
        ..Default::default()
    };
    fill_change(&mut q);
    Some(q)
}

fn fill_change(q: &mut Quote) {
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = ((q.ltp - q.close) * 100.0).round() / 100.0;
        q.change_percent = ((q.ltp - q.close) / q.close * 10000.0).round() / 100.0;
    }
}

fn instrument(
    b: &MotilalBroker,
    key: &QuoteKey,
) -> Result<crate::brokers::common::symbols::SymToken> {
    b.resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
}

/// web `_latch_dealer_mode`.
fn needs_client_code(v: &Value) -> bool {
    let code = super::error_code(v);
    let msg = vs(v, "message").unwrap_or_default().to_ascii_lowercase();
    code == "MO1062" || (msg.contains("client code") && msg.contains("provide"))
}

/// web `post_with_optional_client_code`.
pub(crate) async fn post_report(
    b: &MotilalBroker,
    s: &MotilalSession,
    path: &str,
    body: Value,
) -> Result<(reqwest::StatusCode, Value)> {
    let mut body = body;
    if b.dealer() && !s.client_code.is_empty() {
        body["clientcode"] = Value::String(s.client_code.clone());
    }
    let (status, v) = b.post_raw(s, path, Some(&body)).await?;
    if !super::is_success(&v) && !b.dealer() && needs_client_code(&v) {
        b.dealer_mode.store(true, Ordering::Relaxed);
        tracing::info!("Motilal Oswal dealer login detected; sending the client code from now on");
        if s.client_code.is_empty() {
            return Ok((status, v));
        }
        body["clientcode"] = Value::String(s.client_code.clone());
        return b.post_raw(s, path, Some(&body)).await;
    }
    Ok((status, v))
}

async fn index_quote(
    b: &MotilalBroker,
    s: &MotilalSession,
    key: &QuoteKey,
    code: &str,
) -> Result<Quote> {
    let Some(ex) = mapping::index_exchange(&key.exchange) else {
        return Err(AppError::Validation(format!(
            "Motilal Oswal gives quotes for NSE and BSE indices only, not {} on {}.",
            key.symbol, key.exchange
        )));
    };
    let known = *b.index_field.lock();
    let candidates: Vec<&'static str> = match known {
        Some(f) => vec![f],
        None => vec!["exchangename", "exchange"],
    };
    let mut last = (reqwest::StatusCode::OK, Value::Null);
    for field in candidates {
        let mut body = json!({"scripcode": code});
        body[field] = Value::String(ex.to_string());
        let (status, v) = post_report(b, s, paths::INDEX_LTP, body).await?;
        if super::is_success(&v) {
            *b.index_field.lock() = Some(field);
            let rows = match v.get("data") {
                Some(Value::Array(a)) => a.clone(),
                Some(o @ Value::Object(_)) => vec![o.clone()],
                _ => Vec::new(),
            };
            return index_quote_from_rows(&rows, code, key).ok_or_else(|| {
                AppError::Broker(format!(
                    "Motilal Oswal returned no quote for {}. Try again shortly.",
                    key.symbol
                ))
            });
        }
        let retry = super::error_code(&v) == "MO1051";
        last = (status, v);
        if !retry {
            break;
        }
    }
    Err(motilal_error(
        last.0,
        &last.1,
        "Motilal Oswal could not fetch the index quote.",
    ))
}

pub async fn get_quote(b: &MotilalBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = MotilalSession::parse(auth)?;
    let row = instrument(b, key)?;
    if mapping::is_index(&key.exchange) {
        return index_quote(b, &s, key, &row.token).await;
    }
    let scrip: i64 = row.token.trim().parse().map_err(|_| {
        AppError::Validation(format!(
            "Symbol {} has no Motilal Oswal scrip code. Download the master contract again.",
            key.symbol
        ))
    })?;
    let body = json!({"exchange": mapping::map_exchange(&key.exchange), "scripcode": scrip});
    let (status, v) = post_report(b, &s, paths::LTP, body).await?;
    if !super::is_success(&v) {
        return Err(motilal_error(
            status,
            &v,
            "Motilal Oswal could not fetch the quote.",
        ));
    }
    match v.get("data") {
        Some(d) if d.is_object() => Ok(quote_from_ltp_data(d, key)),
        _ => Err(AppError::Broker(format!(
            "Motilal Oswal returned no quote for {}. Try again shortly.",
            key.symbol
        ))),
    }
}

// ---------------------------------------------------------------------------
// One-shot broadcast socket
// ---------------------------------------------------------------------------

/// One scrip registration on the short-lived socket.
#[derive(Debug, Clone)]
pub struct Reg {
    pub exchange: String,
    pub segment: &'static str,
    pub scrip: i32,
    pub need_oi: bool,
}

fn feed_unavailable() -> AppError {
    AppError::Broker(
        "Motilal Oswal live market data is not reachable right now. Try again in a moment.".into(),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Read frames into `state` until `deadline` or `done`.
async fn pump(
    ws: &mut Ws,
    state: &mut FeedState,
    deadline: Instant,
    done: &(dyn Fn(&FeedState) -> bool + Sync),
) -> Result<()> {
    loop {
        if done(state) {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        match timeout(deadline - now, ws.next()).await {
            Err(_) => return Ok(()),
            Ok(None) => return Err(feed_unavailable()),
            Ok(Some(Err(e))) => {
                tracing::warn!("Motilal Oswal market data socket failed: {}", e);
                return Err(feed_unavailable());
            }
            Ok(Some(Ok(Message::Binary(b)))) => {
                state.apply(&b);
            }
            Ok(Some(Ok(Message::Close(_)))) => return Err(feed_unavailable()),
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn session(
    ws: &mut Ws,
    client_code: &str,
    regs: &[Reg],
    indices: &[&'static str],
    connect: Duration,
    wait: Duration,
    done: &(dyn Fn(&FeedState) -> bool + Sync),
) -> Result<FeedState> {
    let mut state = FeedState::new(!indices.is_empty());
    ws.send(Message::Binary(streaming::login_packet(client_code)))
        .await
        .map_err(|_| feed_unavailable())?;
    // The first binary reply authenticates (web on_message).
    pump(
        ws,
        &mut state,
        Instant::now() + connect,
        &|s: &FeedState| s.authenticated(),
    )
    .await?;
    if !state.authenticated() {
        tracing::warn!("Motilal Oswal market data socket did not answer the login");
        return Err(feed_unavailable());
    }
    for r in regs {
        state.register(&r.exchange, r.scrip);
        ws.send(Message::Binary(streaming::register_packet(
            &r.exchange,
            r.segment,
            r.scrip,
            true,
        )))
        .await
        .map_err(|_| feed_unavailable())?;
    }
    for ex in indices {
        ws.send(Message::Text(streaming::index_frame(client_code, ex, true)))
            .await
            .map_err(|_| feed_unavailable())?;
    }
    let pumped = pump(ws, &mut state, Instant::now() + wait, done).await;
    // Unregister whatever was registered, even after a read failure.
    for r in regs {
        let _ = ws
            .send(Message::Binary(streaming::register_packet(
                &r.exchange,
                r.segment,
                r.scrip,
                false,
            )))
            .await;
    }
    for ex in indices {
        let _ = ws
            .send(Message::Text(streaming::index_frame(
                client_code,
                ex,
                false,
            )))
            .await;
    }
    pumped.map(|_| state)
}

/// Open the broadcast socket, collect, and close it on every path.
pub(crate) async fn feed_snapshot(
    b: &MotilalBroker,
    client_code: &str,
    regs: &[Reg],
    indices: &[&'static str],
    wait: Duration,
    done: &(dyn Fn(&FeedState) -> bool + Sync),
) -> Result<FeedState> {
    let _permit = timeout(b.timings.connect, b.feed_gate.clone().acquire_owned())
        .await
        .map_err(|_| {
            AppError::Broker(
                "Too many live quote and depth requests are already waiting on Motilal Oswal. Try again in a few seconds."
                    .into(),
            )
        })?
        .map_err(|_| feed_unavailable())?;
    let (mut ws, _) = match timeout(
        b.timings.connect,
        tokio_tungstenite::connect_async(b.feed_ws_url.as_str()),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!("Motilal Oswal market data socket could not open: {}", e);
            return Err(feed_unavailable());
        }
        Err(_) => {
            tracing::warn!("Motilal Oswal market data socket timed out while opening");
            return Err(feed_unavailable());
        }
    };
    let result = session(
        &mut ws,
        client_code,
        regs,
        indices,
        b.timings.connect,
        wait,
        done,
    )
    .await;
    let _ = timeout(Duration::from_secs(2), ws.close(None)).await;
    result
}

enum Target {
    Scrip(usize),
    Index(&'static str, String),
    Failed(String),
}

fn quote_from_scrip(d: &ScripData, key: &QuoteKey) -> Quote {
    let bid = ScripData::best(&d.bids);
    let ask = ScripData::best(&d.asks);
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: d.ltp,
        open: d.open,
        high: d.high,
        low: d.low,
        close: d.prev_close,
        volume: d.volume,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.quantity,
        ask_qty: ask.quantity,
        oi: d.oi,
        ..Default::default()
    };
    fill_change(&mut q);
    q
}

async fn multiquote_batch(
    b: &MotilalBroker,
    s: &MotilalSession,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut regs: Vec<Reg> = Vec::new();
    let mut indices: Vec<&'static str> = Vec::new();
    let targets: Vec<Target> = keys
        .iter()
        .map(|k| {
            let Some(row) = b.resolver().by_symbol(&k.exchange, &k.symbol) else {
                return Target::Failed("Could not resolve token".into());
            };
            if mapping::is_index(&k.exchange) {
                return match mapping::index_exchange(&k.exchange) {
                    Some(ex) => {
                        if !indices.contains(&ex) {
                            indices.push(ex);
                        }
                        Target::Index(ex, row.token.clone())
                    }
                    None => {
                        Target::Failed("IndexRegister supports NSE and BSE indices only".into())
                    }
                };
            }
            let Ok(scrip) = row.token.trim().parse::<i32>() else {
                return Target::Failed("Could not resolve token".into());
            };
            let exchange = mapping::map_exchange(&k.exchange).to_string();
            let segment = mapping::segment(&k.exchange);
            let idx = match regs
                .iter()
                .position(|r| r.exchange == exchange && r.scrip == scrip)
            {
                Some(i) => i,
                None => {
                    regs.push(Reg {
                        exchange,
                        segment,
                        scrip,
                        need_oi: segment == "DERIVATIVES",
                    });
                    regs.len() - 1
                }
            };
            Target::Scrip(idx)
        })
        .collect();

    let pending = regs.len()
        + targets
            .iter()
            .filter(|t| matches!(t, Target::Index(..)))
            .count();
    let state = if pending == 0 {
        FeedState::default()
    } else {
        let t = b.timings;
        let wait = (t.multi_per_symbol * pending as u32).clamp(t.multi_min, t.multi_max);
        let has_index = !indices.is_empty();
        let regs_ref = &regs;
        let done = move |st: &FeedState| {
            !has_index
                && regs_ref.iter().all(|r| {
                    st.get(&r.exchange, r.scrip)
                        .is_some_and(|d| d.complete(r.need_oi, false))
                })
        };
        feed_snapshot(b, &s.client_code, &regs, &indices, wait, &done).await?
    };

    Ok(keys
        .iter()
        .zip(targets)
        .map(|(k, t)| {
            let data = match t {
                Target::Failed(e) => Err(e),
                Target::Scrip(i) => {
                    let r = &regs[i];
                    state
                        .get(&r.exchange, r.scrip)
                        .map(|d| quote_from_scrip(d, k))
                        .ok_or_else(|| "No data received".to_string())
                }
                Target::Index(ex, code) => {
                    let d = code
                        .trim()
                        .parse::<i32>()
                        .ok()
                        .and_then(|c| state.get(ex, c));
                    match d {
                        Some(d) if d.index_rate.unwrap_or(d.ltp) != 0.0 || d.has_ohlc => {
                            let mut q = quote_from_scrip(d, k);
                            q.ltp = d.index_rate.unwrap_or(d.ltp);
                            q.bid = 0.0;
                            q.ask = 0.0;
                            q.bid_qty = 0;
                            q.ask_qty = 0;
                            q.volume = 0;
                            q.oi = 0;
                            fill_change(&mut q);
                            Ok(q)
                        }
                        _ => Err("No data received".to_string()),
                    }
                }
            };
            match data {
                Ok(q) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: Some(q),
                    error: None,
                },
                Err(e) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some(e),
                },
            }
        })
        .collect())
}

pub async fn get_multiquotes(
    b: &MotilalBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let s = MotilalSession::parse(auth)?;
    let mut out = Vec::with_capacity(keys.len());
    let batches: Vec<&[QuoteKey]> = keys.chunks(MULTIQUOTE_BATCH).collect();
    let n = batches.len();
    for (i, batch) in batches.into_iter().enumerate() {
        out.extend(multiquote_batch(b, &s, batch).await?);
        if n > 1 && i + 1 < n {
            tokio::time::sleep(b.timings.batch_pause).await;
        }
    }
    Ok(out)
}

fn empty_levels() -> Vec<DepthLevel> {
    vec![DepthLevel::default(); 5]
}

pub async fn get_market_depth(
    b: &MotilalBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let s = MotilalSession::parse(auth)?;
    let row = instrument(b, key)?;
    let scrip: i32 = row.token.trim().parse().map_err(|_| {
        AppError::Validation(format!(
            "Symbol {} has no Motilal Oswal scrip code. Download the master contract again.",
            key.symbol
        ))
    })?;
    if mapping::is_index(&key.exchange) {
        let Some(ex) = mapping::index_exchange(&key.exchange) else {
            return Err(AppError::Validation(format!(
                "Motilal Oswal streams NSE and BSE indices only, not {} on {}.",
                key.symbol, key.exchange
            )));
        };
        let never = |_: &FeedState| false;
        let state =
            feed_snapshot(b, &s.client_code, &[], &[ex], b.timings.index_wait, &never).await?;
        let d = state.get(ex, scrip).cloned().unwrap_or_default();
        let ltp = d.index_rate.unwrap_or(d.ltp);
        if ltp == 0.0 {
            return Err(AppError::Broker(format!(
                "Motilal Oswal sent no index data for {}. Try again shortly.",
                key.symbol
            )));
        }
        return Ok(MarketDepth {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            bids: empty_levels(),
            asks: empty_levels(),
            ltp,
            open: d.open,
            high: d.high,
            low: d.low,
            prev_close: d.prev_close,
            ..Default::default()
        });
    }
    let reg = Reg {
        exchange: mapping::map_exchange(&key.exchange).to_string(),
        segment: mapping::segment(&key.exchange),
        scrip,
        need_oi: mapping::segment(&key.exchange) == "DERIVATIVES",
    };
    let regs = vec![reg.clone()];
    let done = |st: &FeedState| {
        st.get(&reg.exchange, reg.scrip)
            .is_some_and(|d| d.complete(reg.need_oi, true))
    };
    let state = feed_snapshot(b, &s.client_code, &regs, &[], b.timings.depth_wait, &done).await?;
    let Some(d) = state.get(&reg.exchange, reg.scrip) else {
        return Err(AppError::Broker(format!(
            "Motilal Oswal sent no market depth for {}. Try again shortly.",
            key.symbol
        )));
    };
    Ok(MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: ScripData::levels(&d.bids),
        asks: ScripData::levels(&d.asks),
        ltp: d.ltp,
        ltq: d.ltq,
        open: d.open,
        high: d.high,
        low: d.low,
        prev_close: d.prev_close,
        volume: d.volume,
        oi: d.oi,
        // Motilal publishes no exchange-wide totals (web get_depth).
        total_buy_qty: 0,
        total_sell_qty: 0,
    })
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Today's date in IST.
pub fn today_ist() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive()
}

/// web `_today_bar`: today's daily candle from a live quote, stamped at
/// midnight UTC of the IST date; `None` when nothing has traded.
pub fn history_from_quote(q: &Quote, today: NaiveDate) -> Option<Candle> {
    let close = q.ltp;
    let open = q.open;
    if close <= 0.0 && open <= 0.0 {
        return None;
    }
    let high = q.high.max(open).max(close);
    let positives: Vec<f64> = [q.low, open, close]
        .into_iter()
        .filter(|x| *x > 0.0)
        .collect();
    let low = positives.iter().copied().fold(f64::INFINITY, f64::min);
    let low = if low.is_finite() { low } else { 0.0 };
    let ts = chrono::Utc
        .from_utc_datetime(&today.and_hms_opt(0, 0, 0)?)
        .timestamp();
    Some(Candle {
        timestamp: ts,
        open: if open != 0.0 { open } else { close },
        high,
        low: if low != 0.0 { low } else { close },
        close,
        volume: q.volume,
        oi: q.oi,
    })
}

pub async fn get_history(
    b: &MotilalBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = req.interval.trim().to_ascii_uppercase();
    if !matches!(interval.as_str(), "D" | "1D" | "DAY" | "DAILY") {
        return Err(AppError::Validation(format!(
            "Interval '{}' is not available with Motilal Oswal: it publishes no historical or intraday candles. Only the daily interval (D) is available, and only for today.",
            req.interval
        )));
    }
    let today = today_ist();
    if !(req.start <= today && today <= req.end) {
        tracing::info!("Motilal Oswal has no candles before today; returning none");
        return Ok(Vec::new());
    }
    let q = get_quote(b, auth, &req.key).await?;
    Ok(history_from_quote(&q, today).into_iter().collect())
}
