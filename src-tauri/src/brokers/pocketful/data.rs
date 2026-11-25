//! Quotes, depth, multiquotes and chain quotes over REST (web `api/data.py`,
//! rebuilt in marketcalls/openalgo#2467). The old path subscribed on the
//! market-data socket and waited for a packet; Pocketful's API now answers
//! these over REST, and nothing here opens a socket.
//!
//! * One route answers quote and depth:
//!   `GET /api/v1/marketdata/{exchange}/{segment}?token=` ([`market::path`],
//!   get-market-data.md). It carries the best bid and ask only: REST has no
//!   deeper book, so depth is that level padded to five with zero levels,
//!   as on the web (the five-level book is the feed's).
//! * Bid and ask: the documented answer gives them in paise beside rupee
//!   prices, so they are divided by 100 unless the undivided figure is the
//!   one near the rupee prices ([`book_price`]).
//! * Multiquotes: no route takes several tokens, so one request per symbol,
//!   in the order given. A session that ended, a broker that cannot be
//!   reached or a refused turn ends the batch (every symbol after it would
//!   fail the same way); any other refusal is that symbol's error. The
//!   batch has one deadline ([`batch_deadline`]); symbols it leaves
//!   unasked say so.
//! * Open interest: the derivatives answer is undocumented, and
//!   `current_open_interest` (greek-data.md's name) is read when present.
//!   A chain leg whose answer lacks it is reported in
//!   `ChainQuotes::oi_missing`, never counted as zero (CLAUDE.md, broker
//!   request limits); the `/quotes` body keeps the web's 0.
//! * Every request goes on the data lane at the web's 8 a second, an HTTP
//!   429 repeated twice with the web's backoff and then refused as busy
//!   ([`PocketfulBroker::send_data`]).

use super::client::{Answer, Request, DATA_INTERVAL};
use super::market::{self, number};
use super::PocketfulBroker;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// One market data answer (its `data` object).
pub(super) type Raw = Map<String, Value>;

/// What a symbol a batch's deadline left unasked answers.
pub(super) const UNANSWERED: &str =
    "Pocketful did not answer for this symbol in time. Try again in a moment.";

/// Set once Pocketful has sent a book in rupees, so that is logged once.
static RUPEE_BOOK_NOTED: AtomicBool = AtomicBool::new(false);

/// The master row of `key` on an exchange Pocketful quotes, or a refusal
/// before anything is sent (web `_route`, then the token lookup).
pub(super) fn instrument(b: &PocketfulBroker, key: &QuoteKey) -> Result<SymToken> {
    if market::route(&key.exchange).is_none() {
        return Err(not_covered(&key.exchange));
    }
    b.resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .filter(|row| !row.token.trim().is_empty())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "{} on {} is not in the Pocketful symbol list. Check the symbol, or download the master contract again.",
                key.symbol, key.exchange
            ))
        })
}

fn not_covered(exchange: &str) -> AppError {
    AppError::Validation(format!(
        "Pocketful market data does not cover the {} exchange.",
        exchange
    ))
}

/// The market data answer of one instrument (web `_market_data`): a
/// non-empty object, else the refusal naming what was asked for. An empty
/// answer is not a quote of zeros.
pub(super) async fn market_data(
    b: &PocketfulBroker,
    auth: &AuthToken,
    row: &SymToken,
) -> Result<Raw> {
    let path = market::path(&row.exchange).ok_or_else(|| not_covered(&row.exchange))?;
    let answer = b
        .send_data(auth, Request::get(path).query("token", row.token.trim()))
        .await?;
    match answer.payload() {
        Some(Value::Object(data)) if answer.ok() && !data.is_empty() => Ok(data.clone()),
        _ => Err(refusal(
            &answer,
            &format!("a quote for {} on {}", row.symbol, row.exchange),
        )),
    }
}

/// The error for an answer that carried no usable data, naming what was
/// asked for (web `_refusal`): "log in again" for a refused session, else
/// Pocketful's own words when it gave any.
pub(super) fn refusal(answer: &Answer, what: &str) -> AppError {
    let message = answer.message();
    tracing::warn!(
        status = answer.status.as_u16(),
        "Pocketful refused {}: {}",
        what,
        message
    );
    if answer.session_refused() {
        return super::session_expired();
    }
    let said = message.trim().trim_end_matches('.');
    if said.is_empty() {
        AppError::Broker(format!(
            "Pocketful did not return {}. Try again in a moment.",
            what
        ))
    } else {
        AppError::Broker(format!("Pocketful did not return {}: {}.", what, said))
    }
}

/// An error every later request would share (web
/// `PocketfulUnavailableError` and `BrokerBusyError`): the session ended,
/// Pocketful could not be reached, or the request's turn was refused.
pub(super) fn ends_the_batch(e: &AppError) -> bool {
    matches!(e, AppError::Auth(_) | AppError::Http(_) | AppError::Busy(_))
}

/// A best bid or ask in rupees (web `_book_price`). The documented answer
/// gives them in paise (token 14366 at 12.90 with a bid of 1285), so the
/// figure is divided by 100, unless the undivided figure is the nearer of
/// the two to `reference` (the LTP or another rupee price of the same
/// answer). The readings differ a hundredfold, so only a book more than
/// tenfold away from its last trade could be read the wrong way.
pub(super) fn book_price(raw: Option<&Value>, reference: f64) -> f64 {
    let value = raw.map(number).unwrap_or(0.0);
    if value <= 0.0 {
        return 0.0;
    }
    let in_paise = value / 100.0;
    if reference > 0.0 && (value / reference).ln().abs() < (in_paise / reference).ln().abs() {
        if !RUPEE_BOOK_NOTED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "Pocketful sent bid and ask in rupees, not in paise as its documentation shows"
            );
        }
        return value;
    }
    in_paise
}

fn field(raw: &Raw, key: &str) -> f64 {
    raw.get(key).map(number).unwrap_or(0.0)
}

fn whole(raw: &Raw, key: &str) -> i64 {
    field(raw, key) as i64
}

/// OpenAlgo's quote from one market data answer (web `_quote_from`; field
/// names from get-market-data.md). `close` is the previous close.
pub(super) fn quote_from(key: &QuoteKey, raw: &Raw) -> Quote {
    let ltp = field(raw, "last_trade_price");
    let close = field(raw, "close_price");
    let open = field(raw, "open_price");
    // The first rupee price the answer has, for reading the book's unit.
    let reference = [ltp, close, open, field(raw, "average_trade_price")]
        .into_iter()
        .find(|p| *p != 0.0)
        .unwrap_or(0.0);
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open,
        high: field(raw, "high_price"),
        low: field(raw, "low_price"),
        close,
        volume: whole(raw, "trade_volume"),
        bid: book_price(raw.get("bid_price"), reference),
        ask: book_price(raw.get("ask_price"), reference),
        bid_qty: whole(raw, "bid_qty"),
        ask_qty: whole(raw, "ask_qty"),
        oi: whole(raw, "current_open_interest"),
        ..Default::default()
    }
}

/// Whether the answer carried open interest (a number, or text that reads
/// as one); without it a leg's `oi` is not a reading.
fn carries_oi(raw: &Raw) -> bool {
    match raw.get("current_open_interest") {
        Some(Value::Number(_)) => true,
        Some(Value::String(s)) => s.trim().parse::<f64>().is_ok_and(f64::is_finite),
        _ => false,
    }
}

/// OpenAlgo's five-level depth (web `_depth_from`): the best bid and ask,
/// then zero levels; the totals, OHLC, LTP and LTQ are the broker's.
fn depth_from(key: &QuoteKey, raw: &Raw) -> MarketDepth {
    let q = quote_from(key, raw);
    let side = |price: f64, quantity: i64| -> Vec<DepthLevel> {
        let mut levels = vec![DepthLevel {
            price,
            quantity,
            orders: 0,
        }];
        levels.resize(5, DepthLevel::default());
        levels
    };
    MarketDepth {
        symbol: q.symbol.clone(),
        exchange: q.exchange.clone(),
        bids: side(q.bid, q.bid_qty),
        asks: side(q.ask, q.ask_qty),
        ltp: q.ltp,
        ltq: whole(raw, "last_trade_qty"),
        open: q.open,
        high: q.high,
        low: q.low,
        prev_close: q.close,
        volume: q.volume,
        oi: q.oi,
        total_buy_qty: whole(raw, "total_buy_qty"),
        total_sell_qty: whole(raw, "total_sell_qty"),
    }
}

async fn raw_quote(b: &PocketfulBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Raw> {
    let row = instrument(b, key)?;
    market_data(b, auth, &row).await
}

pub async fn get_quote(b: &PocketfulBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    Ok(quote_from(key, &raw_quote(b, auth, key).await?))
}

pub async fn get_market_depth(
    b: &PocketfulBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    Ok(depth_from(key, &raw_quote(b, auth, key).await?))
}

pub async fn get_multiquotes(
    b: &PocketfulBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    Ok(quote_each(b, auth, keys, batch_deadline(keys.len()))
        .await?
        .0)
}

/// Quotes for a chain's legs, saying which came back without open
/// interest (and which came back without a quote at all).
pub async fn get_chain_quotes(
    b: &PocketfulBroker,
    auth: &AuthToken,
    req: &ChainQuoteRequest,
) -> Result<ChainQuotes> {
    let deadline = batch_deadline(req.keys.len());
    let (quotes, oi_missing) = quote_each(b, auth, &req.keys, deadline).await?;
    Ok(ChainQuotes { quotes, oi_missing })
}

/// The time a batch of `n` quotes may take: twice what pacing alone takes,
/// plus 30 s. Room for an ordinary network, never hours behind a broker
/// that stopped answering (CLAUDE.md: one request, one deadline).
pub(super) fn batch_deadline(n: usize) -> Duration {
    let paced =
        DATA_INTERVAL.saturating_mul(u32::try_from(n.saturating_mul(2)).unwrap_or(u32::MAX));
    Duration::from_secs(30).saturating_add(paced)
}

fn failed(key: &QuoteKey, message: String) -> QuoteResult {
    QuoteResult {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        data: None,
        error: Some(message),
    }
}

/// One quote per key, in request order (web `get_multiquotes`), and the
/// symbols whose answer carried no open interest; whatever `within` leaves
/// unasked answers [`UNANSWERED`] (tests shorten it).
pub(super) async fn quote_each(
    b: &PocketfulBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
    within: Duration,
) -> Result<(Vec<QuoteResult>, HashSet<String>)> {
    let deadline = Instant::now() + within;
    let mut out = Vec::with_capacity(keys.len());
    let mut oi_missing = HashSet::new();
    for (i, key) in keys.iter().enumerate() {
        let Ok(answer) = tokio::time::timeout_at(deadline, raw_quote(b, auth, key)).await else {
            tracing::warn!(
                "Pocketful quotes ran past their deadline; {} of {} symbols left unasked",
                keys.len() - i,
                keys.len()
            );
            for rest in &keys[i..] {
                oi_missing.insert(rest.symbol.clone());
                out.push(failed(rest, UNANSWERED.into()));
            }
            break;
        };
        match answer {
            Ok(raw) => {
                if !carries_oi(&raw) {
                    oi_missing.insert(key.symbol.clone());
                }
                out.push(QuoteResult {
                    symbol: key.symbol.clone(),
                    exchange: key.exchange.clone(),
                    data: Some(quote_from(key, &raw)),
                    error: None,
                });
            }
            Err(e) if ends_the_batch(&e) => return Err(e),
            Err(e) => {
                oi_missing.insert(key.symbol.clone());
                out.push(failed(key, e.client_message()));
            }
        }
    }
    Ok((out, oi_missing))
}
