//! Quotes, multiquotes and depth (web `api/data.py`).
//!
//! Pocketful has no REST quote endpoint. Like the web, a request opens the
//! market-data socket, subscribes the instruments (detailed market data for
//! quotes; for depth `full_snapquote` and, unlike the web, the detailed
//! data beside it for the trade price a snapquote lacks), takes the first
//! packet per instrument and type, unsubscribes and closes. Every step is bounded: connect
//! within `CONNECT_TIMEOUT`, collect for at most the web's wait (10 s quote,
//! 15 s depth, 3-15 s for a batch), and the socket is closed on every exit
//! path (dropped if the close itself stalls). Multiquotes go in batches of
//! 50 like the web.

use super::mapping::exchange_code;
use super::streaming::{
    decode_frame, feed_url, heartbeat_frame, pocketful_mode, sub_frame, Detailed, Packet,
    Snapquote, MODE_DETAILED, MODE_SNAPQUOTE,
};
use super::PocketfulBroker;
use crate::brokers::common::streaming::{round2, FeedMode, Message};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

/// web `BATCH_SIZE` for multiquotes.
pub const MULTIQUOTE_BATCH: usize = 50;
/// web `_QUOTE_WAIT_SECONDS` / `_DEPTH_WAIT_SECONDS`.
pub const QUOTE_WAIT: Duration = Duration::from_secs(10);
pub const DEPTH_WAIT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// web `max_wait_time = min(max(n * 0.5, 3), 15)` seconds for a batch.
pub fn batch_wait(n: usize) -> Duration {
    Duration::from_secs_f64((n as f64 * 0.5).clamp(3.0, 15.0))
}

fn lookup(b: &PocketfulBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// (exchange code, token) for a master row: indices go to their cash
/// exchange (web `data.py`: NSE_INDEX -> 1, BSE_INDEX -> 6).
pub fn instrument(row: &SymToken) -> Option<(u8, u32)> {
    let token = row.token.trim().parse::<u32>().ok()?;
    Some((exchange_code(&row.exchange), token))
}

/// Subscribe `instruments` at `pocketful_mode` on a fresh socket and
/// collect the first packet per token until all arrived or `wait` ran out.
/// Packets are keyed by token alone, like the web's `_is_packet_for`.
pub(crate) async fn snapshot(
    b: &PocketfulBroker,
    auth: &AuthToken,
    instruments: &[(u8, u32)],
    mode: u8,
    wait: Duration,
) -> Result<HashMap<u32, Packet>> {
    let got = packets(b, auth, instruments, &[mode], wait).await?;
    Ok(got.into_iter().map(|((tok, _), p)| (tok, p)).collect())
}

/// The Pocketful type a packet is of, for the snapshot's own types.
fn packet_mode(p: &Packet) -> Option<u8> {
    match p {
        Packet::Detailed(_) => Some(MODE_DETAILED),
        Packet::Snapquote(_) => Some(MODE_SNAPQUOTE),
        _ => None,
    }
}

/// `snapshot` for several Pocketful types at once: the first packet per
/// token and type, keyed by both.
async fn packets(
    b: &PocketfulBroker,
    auth: &AuthToken,
    instruments: &[(u8, u32)],
    modes: &[u8],
    wait: Duration,
) -> Result<HashMap<(u32, u8), Packet>> {
    let client_id = b.client_id(auth).await?;
    let token = PocketfulBroker::token(auth)?;
    let url = feed_url(&b.urls.ws, &client_id, token);
    let mut got: HashMap<(u32, u8), Packet> = HashMap::new();
    if instruments.is_empty() {
        return Ok(got);
    }
    let connect = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url));
    let mut ws = match connect.await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp)))
            if matches!(resp.status().as_u16(), 401 | 403) =>
        {
            return Err(super::session_expired());
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Pocketful quote socket did not open: {}",
                super::streaming::ws_error_kind(&e)
            );
            return Err(feed_unavailable());
        }
        Err(_) => {
            tracing::warn!("Pocketful quote socket timed out while opening");
            return Err(feed_unavailable());
        }
    };
    let wanted: std::collections::HashSet<u32> = instruments.iter().map(|(_, t)| *t).collect();
    let deadline = Instant::now() + wait;
    let outcome: Result<()> = async {
        for &mode in modes {
            for (code, tok) in instruments {
                send(&mut ws, sub_frame("subscribe", *code, *tok, mode)).await?;
            }
        }
        if modes.contains(&MODE_SNAPQUOTE) {
            // web `_get_market_depth_websocket` sends a heartbeat first.
            send(&mut ws, heartbeat_frame()).await?;
        }
        while got.len() < wanted.len() * modes.len() {
            let next = tokio::time::timeout_at(deadline, ws.next()).await;
            let msg = match next {
                Err(_) => break, // wait is over
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    tracing::warn!(
                        "Pocketful quote socket failed: {}",
                        super::streaming::ws_error_kind(&e)
                    );
                    break;
                }
                Ok(Some(Ok(m))) => m,
            };
            if let Message::Ping(p) = &msg {
                let _ = send(&mut ws, Message::Pong(p.clone())).await;
                continue;
            }
            if let Some(p) = decode_frame(&msg) {
                let mode = packet_mode(&p).filter(|m| modes.contains(m));
                if let (Some(mode), Some((_, tok))) = (mode, p.token()) {
                    if wanted.contains(&tok) {
                        got.entry((tok, mode)).or_insert(p);
                    }
                }
            }
        }
        // Release the instruments before closing (web `finally` unsubscribe).
        for &mode in modes {
            for (code, tok) in instruments {
                send(&mut ws, sub_frame("unsubscribe", *code, *tok, mode)).await?;
            }
        }
        Ok(())
    }
    .await;
    // Close on every path; a stalled close is abandoned (the stream drops).
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
    if let Err(e) = outcome {
        tracing::debug!("Pocketful quote socket ended early: {}", e.code());
    }
    Ok(got)
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Ws, m: Message) -> Result<()> {
    match tokio::time::timeout(SEND_TIMEOUT, ws.send(m)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(AppError::WebSocket(Box::new(e))),
        Err(_) => Err(feed_unavailable()),
    }
}

fn feed_unavailable() -> AppError {
    AppError::Broker(
        "Pocketful's live price feed is not answering right now. Try again in a few seconds."
            .into(),
    )
}

fn no_data(what: &str, key: &QuoteKey) -> AppError {
    AppError::Broker(format!(
        "Pocketful sent no {} for {} {} in time. The market may be closed or the instrument inactive; try again.",
        what, key.exchange, key.symbol
    ))
}

/// web `_get_quotes_compact` fields from a detailed packet.
pub fn to_quote(key: &QuoteKey, d: &Detailed) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: d.ltp,
        open: d.open,
        high: d.high,
        low: d.low,
        close: d.close,
        volume: d.volume,
        bid: d.bid,
        ask: d.ask,
        bid_qty: d.bid_qty,
        ask_qty: d.ask_qty,
        oi: d.oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    };
    if q.close > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

/// web `_get_market_depth_websocket` from a snapquote packet: five levels
/// a side, in the web's shape. A snapquote has no trade price: `ltp`, the
/// last quantity and OI come from the detailed packet streamed beside it,
/// 0 without one, never the snapquote's average trade price, which the
/// web passes off as `ltp` (lead decision 2026-11-09, review round).
pub fn to_depth(key: &QuoteKey, s: &Snapquote, trade: Option<&Detailed>) -> MarketDepth {
    let pad = |side: &[DepthLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| side.get(i).copied().unwrap_or_default())
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&s.bids),
        asks: pad(&s.asks),
        ltp: trade.map_or(0.0, |d| d.ltp),
        ltq: trade.map_or(0, |d| d.ltq),
        open: s.open,
        high: s.high,
        low: s.low,
        prev_close: s.close,
        volume: s.volume,
        oi: trade.map_or(0, |d| d.oi),
        total_buy_qty: s.total_buy_qty,
        total_sell_qty: s.total_sell_qty,
    }
}

fn resolve_instrument(b: &PocketfulBroker, key: &QuoteKey) -> Result<(u8, u32)> {
    let row = lookup(b, key)?;
    instrument(&row).ok_or_else(|| {
        AppError::Broker(format!(
            "The master contract has no Pocketful token for {}. Download the master contract again.",
            key.symbol
        ))
    })
}

pub async fn get_quote(b: &PocketfulBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let inst = resolve_instrument(b, key)?;
    let mode = pocketful_mode(FeedMode::Quote);
    let mut got = snapshot(b, auth, &[inst], mode, QUOTE_WAIT).await?;
    match got.remove(&inst.1) {
        Some(Packet::Detailed(d)) => Ok(to_quote(key, &d)),
        _ => Err(no_data("live quote", key)),
    }
}

pub async fn get_market_depth(
    b: &PocketfulBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let inst = resolve_instrument(b, key)?;
    let book = pocketful_mode(FeedMode::Depth);
    let mut got = packets(b, auth, &[inst], &[MODE_DETAILED, book], DEPTH_WAIT).await?;
    let trade = match got.remove(&(inst.1, MODE_DETAILED)) {
        Some(Packet::Detailed(d)) => Some(d),
        _ => None,
    };
    match got.remove(&(inst.1, book)) {
        Some(Packet::Snapquote(s)) => Ok(to_depth(key, &s, trade.as_ref())),
        _ => Err(no_data("market depth", key)),
    }
}

pub async fn get_multiquotes(
    b: &PocketfulBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(MULTIQUOTE_BATCH) {
        let resolved: Vec<(QuoteKey, Option<(u8, u32)>)> = batch
            .iter()
            .map(|k| {
                let inst = b
                    .resolver()
                    .by_symbol(&k.exchange, &k.symbol)
                    .and_then(|r| instrument(&r));
                (k.clone(), inst)
            })
            .collect();
        let mut instruments: Vec<(u8, u32)> = resolved.iter().filter_map(|(_, i)| *i).collect();
        instruments.sort_unstable();
        instruments.dedup();
        let got = if instruments.is_empty() {
            HashMap::new()
        } else {
            match snapshot(
                b,
                auth,
                &instruments,
                pocketful_mode(FeedMode::Quote),
                batch_wait(instruments.len()),
            )
            .await
            {
                Ok(g) => g,
                Err(e @ AppError::Auth(_)) => return Err(e),
                Err(e) => {
                    // web: a connection failure marks every symbol.
                    let msg = e.client_message();
                    out.extend(resolved.into_iter().map(|(k, _)| QuoteResult {
                        symbol: k.symbol,
                        exchange: k.exchange,
                        data: None,
                        error: Some(msg.clone()),
                    }));
                    continue;
                }
            }
        };
        // web order: unresolved symbols first, then the resolved ones.
        let (missing, found): (Vec<_>, Vec<_>) =
            resolved.into_iter().partition(|(_, i)| i.is_none());
        out.extend(missing.into_iter().map(|(k, _)| QuoteResult {
            symbol: k.symbol,
            exchange: k.exchange,
            data: None,
            error: Some("Could not resolve token".into()),
        }));
        for (k, inst) in found {
            let packet = inst.and_then(|(_, t)| got.get(&t));
            out.push(match packet {
                Some(Packet::Detailed(d)) => QuoteResult {
                    data: Some(to_quote(&k, d)),
                    symbol: k.symbol,
                    exchange: k.exchange,
                    error: None,
                },
                _ => QuoteResult {
                    symbol: k.symbol,
                    exchange: k.exchange,
                    data: None,
                    error: Some("No data received".into()),
                },
            });
        }
    }
    Ok(out)
}

/// Last price for price protection, `None` when the feed has none.
pub(crate) async fn ltp(b: &PocketfulBroker, auth: &AuthToken, row: &SymToken) -> Option<f64> {
    let inst = instrument(row)?;
    let got = snapshot(b, auth, &[inst], MODE_DETAILED, QUOTE_WAIT)
        .await
        .map_err(|e| tracing::warn!("Pocketful price lookup failed: {}", e.code()))
        .ok()?;
    match got.get(&inst.1) {
        Some(Packet::Detailed(d)) if d.ltp > 0.0 => Some(d.ltp),
        _ => None,
    }
}
