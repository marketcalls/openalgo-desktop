//! Quotes, multiquotes and depth (web `api/data.py`).
//!
//! Every read is `GET /data/v1/price/live?mode=FULL&pref=<prefs>` where a
//! pref is `EXCHANGE:security_id:TYPE` (`NFO`/`NSE_INDEX` -> `NSE`,
//! `BFO`/`BSE_INDEX` -> `BSE`; type `INDEX|EQUITY|FUTURE|OPTION`), several
//! comma-joined. FULL mode carries OI and the five-level book. Paytm Money
//! has no history API.

use super::mapping::{paytm_exchange, scrip_type};
use super::PaytmBroker;
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// web `get_multiquotes` `BATCH_SIZE` and `RATE_LIMIT_DELAY`.
pub const QUOTE_BATCH: usize = 100;
pub const BATCH_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmOhlc {
    #[serde(deserialize_with = "f64_lenient")]
    pub open: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub high: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub low: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmLevel {
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub orders: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmDepth {
    pub buy: Vec<PaytmLevel>,
    pub sell: Vec<PaytmLevel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaytmLive {
    #[serde(deserialize_with = "string_lenient")]
    pub security_id: String,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub last_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub volume_traded: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub volume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub oi: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub change_absolute: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub change_percent: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub last_trade_time: String,
    pub ohlc: PaytmOhlc,
    pub depth: PaytmDepth,
}

impl PaytmLive {
    fn volume(&self) -> i64 {
        if self.volume_traded != 0 {
            self.volume_traded
        } else {
            self.volume
        }
    }
}

/// `EXCHANGE:security_id:TYPE` for one master row.
pub fn pref(row: &SymToken) -> String {
    let exchange = row
        .exchange
        .parse::<Exchange>()
        .map(paytm_exchange)
        .unwrap_or("NSE");
    format!("{}:{}:{}", exchange, row.token, scrip_type(row, false))
}

/// The `/data/v1/price/live` path for these prefs (URL-encoded, like
/// `urllib.parse.quote` with `:` and `,` escaped).
pub fn live_path(prefs: &[String]) -> String {
    format!(
        "/data/v1/price/live?mode=FULL&pref={}",
        urlencoding::encode(&prefs.join(","))
    )
}

/// web quote dict from one live entry (bid/ask are not sent by Paytm).
pub fn to_quote(key: &QuoteKey, q: &PaytmLive) -> Quote {
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.last_price,
        open: q.ohlc.open,
        high: q.ohlc.high,
        low: q.ohlc.low,
        close: q.ohlc.close,
        volume: q.volume(),
        bid: 0.0,
        ask: 0.0,
        bid_qty: 0,
        ask_qty: 0,
        oi: q.oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: q.last_trade_time.clone(),
    };
    if quote.close > 0.0 {
        quote.change = round2(quote.ltp - quote.close);
        quote.change_percent = round2((quote.ltp - quote.close) / quote.close * 100.0);
    }
    quote
}

/// web `get_market_depth`: five levels a side padded with zeros; totals are
/// the sums of the levels sent.
pub fn to_depth(key: &QuoteKey, q: &PaytmLive) -> MarketDepth {
    let pad = |side: &[PaytmLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                side.get(i)
                    .map(|l| DepthLevel {
                        price: l.price,
                        quantity: l.quantity,
                        orders: l.orders,
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&q.depth.buy),
        asks: pad(&q.depth.sell),
        ltp: q.last_price,
        ltq: q.last_quantity,
        open: q.ohlc.open,
        high: q.ohlc.high,
        low: q.ohlc.low,
        prev_close: q.ohlc.close,
        volume: q.volume(),
        oi: q.oi,
        total_buy_qty: q.depth.buy.iter().map(|l| l.quantity).sum(),
        total_sell_qty: q.depth.sell.iter().map(|l| l.quantity).sum(),
    }
}

fn lookup(b: &PaytmBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

async fn fetch(
    b: &PaytmBroker,
    auth: &AuthToken,
    prefs: &[String],
) -> Result<HashMap<String, PaytmLive>> {
    let env = b.call(Method::GET, &live_path(prefs), auth, None).await?;
    Ok(env
        .rows::<PaytmLive>()
        .into_iter()
        .map(|q| (q.security_id.clone(), q))
        .collect())
}

async fn one(b: &PaytmBroker, auth: &AuthToken, key: &QuoteKey) -> Result<PaytmLive> {
    let row = lookup(b, key)?;
    let mut data = fetch(b, auth, &[pref(&row)]).await?;
    data.remove(&row.token).ok_or_else(|| {
        AppError::Broker(format!(
            "Paytm Money returned no market data for {} {}.",
            key.exchange, key.symbol
        ))
    })
}

pub async fn get_quote(b: &PaytmBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    Ok(to_quote(key, &one(b, auth, key).await?))
}

pub async fn get_market_depth(
    b: &PaytmBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    Ok(to_depth(key, &one(b, auth, key).await?))
}

pub async fn get_multiquotes(
    b: &PaytmBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let resolved: Vec<(QuoteKey, Option<SymToken>)> = keys
        .iter()
        .map(|k| (k.clone(), b.resolver().by_symbol(&k.exchange, &k.symbol)))
        .collect();
    let wanted: Vec<&SymToken> = resolved.iter().filter_map(|(_, r)| r.as_ref()).collect();
    let mut quotes: HashMap<String, PaytmLive> = HashMap::new();
    // Per-batch failures become per-symbol errors, as on the web.
    let mut failed: HashMap<String, String> = HashMap::new();
    for (i, batch) in wanted.chunks(QUOTE_BATCH).enumerate() {
        if i > 0 {
            tokio::time::sleep(BATCH_DELAY).await;
        }
        let prefs: Vec<String> = batch.iter().map(|r| pref(r)).collect();
        match fetch(b, auth, &prefs).await {
            Ok(q) => quotes.extend(q),
            Err(e) => {
                let msg = e.client_message();
                for r in batch {
                    failed.insert(r.token.clone(), msg.clone());
                }
            }
        }
    }
    Ok(resolved
        .into_iter()
        .map(|(k, row)| {
            let (data, error) = match row {
                None => (None, Some("Could not resolve token".to_string())),
                Some(r) => match quotes.get(&r.token) {
                    Some(q) => (Some(to_quote(&k, q)), None),
                    None => (
                        None,
                        Some(
                            failed
                                .get(&r.token)
                                .cloned()
                                .unwrap_or_else(|| "No data received".to_string()),
                        ),
                    ),
                },
            };
            QuoteResult {
                symbol: k.symbol,
                exchange: k.exchange,
                data,
                error,
            }
        })
        .collect())
}

/// Paytm Money provides no historical candles (web `get_history`).
/// Paytm Money publishes no candle API. The web returns an empty frame
/// rather than raising, so `/history` answers success with no rows.
pub fn get_history() -> Result<Vec<Candle>> {
    Ok(Vec::new())
}
