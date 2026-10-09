//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::{AngelBroker, Category, TIMEFRAME_MAP};
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::history::{chunks, parse_iso_epoch, sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::common::streaming::round2;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const QUOTE_PATH: &str = "/rest/secure/angelbroking/market/v1/quote/";
pub const CANDLE_PATH: &str = "/rest/secure/angelbroking/historical/v1/getCandleData";
pub const OI_PATH: &str = "/rest/secure/angelbroking/historical/v1/getOIData";

/// Angel hard-caps `market/v1/quote` at 50 tokens per request.
pub const QUOTE_BATCH: usize = 50;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelDepthLevel {
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub orders: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelDepth {
    pub buy: Vec<AngelDepthLevel>,
    pub sell: Vec<AngelDepthLevel>,
}

/// One `fetched[]` row of a FULL-mode quote.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[allow(non_snake_case)]
pub struct AngelQuote {
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingSymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symbolToken: String,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub open: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub high: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub low: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub lastTradeQty: i64,
    #[serde(deserialize_with = "string_lenient")]
    pub exchFeedTime: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub tradeVolume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub opnInterest: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub totBuyQuan: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub totSellQuan: i64,
    pub depth: Option<AngelDepth>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelQuoteData {
    pub fetched: Option<Vec<AngelQuote>>,
    pub unfetched: Option<Vec<Value>>,
}

/// `NSE_INDEX` -> `NSE` etc. for market-data calls (web `get_quotes`).
pub fn api_exchange(oa: &str) -> &str {
    match oa {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => other,
    }
}

/// web quote fields from one FULL-mode row; bid/ask are the first depth
/// levels; `close` is the previous close.
pub fn to_quote(key: &QuoteKey, q: &AngelQuote) -> Quote {
    let d = q.depth.clone().unwrap_or_default();
    let bid = d.buy.first().cloned().unwrap_or_default();
    let ask = d.sell.first().cloned().unwrap_or_default();
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.ltp,
        open: q.open,
        high: q.high,
        low: q.low,
        close: q.close,
        volume: q.tradeVolume,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.quantity,
        ask_qty: ask.quantity,
        oi: q.opnInterest,
        change: 0.0,
        change_percent: 0.0,
        timestamp: q.exchFeedTime.clone(),
    };
    if quote.close > 0.0 {
        quote.change = round2(quote.ltp - quote.close);
        quote.change_percent = round2((quote.ltp - quote.close) / quote.close * 100.0);
    }
    quote
}

/// web `get_depth`: exactly five levels per side, zero-padded, plus the
/// quote's totals and OHLC.
pub fn to_depth(key: &QuoteKey, q: &AngelQuote) -> MarketDepth {
    let d = q.depth.clone().unwrap_or_default();
    let pad = |side: &[AngelDepthLevel]| -> Vec<DepthLevel> {
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
        bids: pad(&d.buy),
        asks: pad(&d.sell),
        ltp: q.ltp,
        ltq: q.lastTradeQty,
        open: q.open,
        high: q.high,
        low: q.low,
        prev_close: q.close,
        volume: q.tradeVolume,
        oi: q.opnInterest,
        total_buy_qty: q.totBuyQuan,
        total_sell_qty: q.totSellQuan,
    }
}

/// `{"mode":"FULL","exchangeTokens":{exchange:[token,..]}}` for resolved
/// `(api_exchange, token)` pairs.
pub fn quote_body(pairs: &[(String, String)]) -> Value {
    let mut tokens: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (ex, t) in pairs {
        tokens.entry(ex.as_str()).or_default().push(t.as_str());
    }
    json!({"mode": "FULL", "exchangeTokens": tokens})
}

/// One quote call; rows keyed `exchange:symbolToken` as the web does.
async fn fetch(
    b: &AngelBroker,
    auth: &AuthToken,
    pairs: &[(String, String)],
) -> Result<HashMap<String, AngelQuote>> {
    let body = quote_body(pairs);
    let data: AngelQuoteData = b
        .call(Method::POST, QUOTE_PATH, auth, Some(&body), Category::Quote)
        .await?
        .unwrap_or_default();
    if let Some(u) = data.unfetched.as_ref().filter(|u| !u.is_empty()) {
        tracing::warn!("Angel One could not fetch {} instrument(s)", u.len());
    }
    Ok(data
        .fetched
        .unwrap_or_default()
        .into_iter()
        .map(|q| (format!("{}:{}", q.exchange, q.symbolToken), q))
        .collect())
}

async fn one(b: &AngelBroker, auth: &AuthToken, key: &QuoteKey, what: &str) -> Result<AngelQuote> {
    let row = b.lookup(key)?;
    let ex = api_exchange(&key.exchange).to_string();
    let k = format!("{}:{}", ex, row.token);
    let mut map = fetch(b, auth, &[(ex, row.token.clone())]).await?;
    map.remove(&k).ok_or_else(|| {
        AppError::Broker(format!(
            "Angel One returned no {} for {} {}.",
            what, key.exchange, key.symbol
        ))
    })
}

pub async fn get_quote(b: &AngelBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let q = one(b, auth, key, "quote").await?;
    Ok(to_quote(key, &q))
}

pub async fn get_market_depth(
    b: &AngelBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let q = one(b, auth, key, "market depth").await?;
    Ok(to_depth(key, &q))
}

/// web `get_multiquotes`: batches of 50 tokens; unresolved symbols and rows
/// Angel did not return are per-symbol errors. Results are in request order.
pub async fn get_multiquotes(
    b: &AngelBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let resolved: Vec<(QuoteKey, Option<(String, String)>)> = keys
        .iter()
        .map(|k| {
            let pair = b
                .resolver()
                .token(&k.symbol, &k.exchange)
                .filter(|t| !t.is_empty())
                .map(|t| (api_exchange(&k.exchange).to_string(), t));
            (k.clone(), pair)
        })
        .collect();
    let mut wanted: Vec<(String, String)> =
        resolved.iter().filter_map(|(_, p)| p.clone()).collect();
    wanted.sort();
    wanted.dedup();
    let mut quotes: HashMap<String, AngelQuote> = HashMap::new();
    for batch in wanted.chunks(QUOTE_BATCH) {
        quotes.extend(fetch(b, auth, batch).await?);
    }
    Ok(resolved
        .into_iter()
        .map(|(k, pair)| {
            let (data, error) = match pair {
                None => (None, Some("Could not resolve token".to_string())),
                Some((ex, t)) => match quotes.get(&format!("{}:{}", ex, t)) {
                    Some(q) => (Some(to_quote(&k, q)), None),
                    None => (None, Some("No quote data available".to_string())),
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

/// Angel interval for an OpenAlgo interval key.
pub fn angel_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Angel One. Use one of: {}.",
                interval,
                list.join(", ")
            ))
        })
}

/// Days per `getCandleData` / `getOIData` request (web `interval_limits`).
pub fn chunk_days(interval: &str) -> i64 {
    match interval {
        "1m" => 30,
        "3m" => 60,
        "5m" | "10m" => 100,
        "15m" | "30m" => 200,
        "1h" => 400,
        _ => 2000,
    }
}

/// `fromdate` / `todate` for one chunk: `YYYY-MM-DD 00:00` to `23:59`, or to
/// the current minute when the chunk ends today (IST), as the web does.
pub fn chunk_window(from: NaiveDate, to: NaiveDate, now_ist: NaiveDateTime) -> (String, String) {
    let start = format!("{} 00:00", from.format("%Y-%m-%d"));
    let end = if to == now_ist.date() {
        now_ist.format("%Y-%m-%d %H:%M").to_string()
    } else {
        format!("{} 23:59", to.format("%Y-%m-%d"))
    };
    (start, end)
}

fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Epoch seconds of an Angel timestamp. Angel sends ISO with its `+05:30`
/// offset; one without an offset is IST wall-clock time, never the host's
/// zone (web `_angel_timestamps_to_epoch`, #2176).
pub fn angel_epoch(s: &str) -> Option<i64> {
    parse_iso_epoch(s).or_else(|| {
        let s = s.trim();
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M"))
            .ok()
            .map(|t| t.and_utc().timestamp() - IST_OFFSET_SECS)
    })
}

/// `getCandleData` rows `[ts, o, h, l, c, v]` -> candles. `ts` is ISO with
/// offset; daily candles are shifted +5:30 to 00:00 UTC of their date, the
/// stamp every other broker uses (the web's Angel no longer shifts them; see
/// `tests/fixtures/web/INDEX.md`).
pub fn parse_candles(rows: &[Vec<Value>], daily: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let ts = angel_epoch(r.first()?.as_str()?)?;
            Some(Candle {
                timestamp: if daily { ts + IST_OFFSET_SECS } else { ts },
                open: num(r.get(1)),
                high: num(r.get(2)),
                low: num(r.get(3)),
                close: num(r.get(4)),
                volume: num(r.get(5)) as i64,
                oi: 0,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelOi {
    #[serde(deserialize_with = "string_lenient")]
    pub time: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub oi: i64,
}

/// `getOIData` rows -> `(epoch, oi)`, daily shifted like the candles.
pub fn parse_oi(rows: &[AngelOi], daily: bool) -> HashMap<i64, i64> {
    rows.iter()
        .filter_map(|r| {
            let ts = angel_epoch(&r.time)?;
            Some((if daily { ts + IST_OFFSET_SECS } else { ts }, r.oi))
        })
        .collect()
}

/// Left-join OI onto candles by timestamp (missing -> 0).
pub fn merge_oi(candles: &mut [Candle], oi: &HashMap<i64, i64>) {
    for c in candles {
        c.oi = oi.get(&c.timestamp).copied().unwrap_or(0);
    }
}

fn now_ist() -> NaiveDateTime {
    chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .naive_local()
}

/// web `get_history`: candles in chunks, then OI for derivatives.
///
/// Chunks are whole days (`00:00`..`23:59`), so no session is skipped at a
/// chunk boundary (the web's datetime loop ends each chunk at `00:00` of its
/// last day and so drops that day's intraday candles).
pub async fn get_history(
    b: &AngelBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = angel_interval(&req.interval)?;
    let row = b.lookup(&req.key)?;
    let exchange = api_exchange(&req.key.exchange).to_string();
    let daily = req.interval == "D";
    let now = now_ist();
    let windows: Vec<(String, String)> = chunks(req.start, req.end, chunk_days(&req.interval))
        .into_iter()
        .map(|(f, t)| chunk_window(f, t, now))
        .collect();
    let mut candles = Vec::new();
    for (from, to) in &windows {
        let body = json!({
            "exchange": exchange,
            "symboltoken": row.token,
            "interval": interval,
            "fromdate": from,
            "todate": to,
        });
        let rows: Vec<Vec<Value>> = b
            .call(
                Method::POST,
                CANDLE_PATH,
                auth,
                Some(&body),
                Category::History,
            )
            .await?
            .unwrap_or_default();
        candles.extend(parse_candles(&rows, daily));
    }
    let mut candles = sort_dedupe(candles);
    if matches!(exchange.as_str(), "NFO" | "BFO" | "CDS" | "MCX") && !candles.is_empty() {
        let mut oi = HashMap::new();
        for (from, to) in &windows {
            let body = json!({
                "exchange": exchange,
                "symboltoken": row.token,
                "interval": interval,
                "fromdate": from,
                "todate": to,
            });
            // OI is best effort, as on the web: a failed chunk leaves zeros.
            match b
                .call::<Vec<AngelOi>>(Method::POST, OI_PATH, auth, Some(&body), Category::History)
                .await
            {
                Ok(rows) => oi.extend(parse_oi(&rows.unwrap_or_default(), daily)),
                Err(e) => tracing::warn!("Angel One OI chunk failed: {}", e.code()),
            }
        }
        merge_oi(&mut candles, &oi);
    }
    Ok(candles)
}
