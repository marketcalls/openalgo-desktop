//! Quotes, depth and history (web `api/data.py`).

use super::mapping::{self, text};
use super::{is_success, url_quote, SamcoBroker, MULTIQUOTE_BATCH};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::Duration as Days;
use reqwest::Method;
use serde_json::{Map, Value};

fn is_index(exchange: &str) -> bool {
    matches!(exchange, "NSE_INDEX" | "BSE_INDEX")
}

/// Fallback only; the master stores every index's Samco name as brsymbol.
const INDEX_NAME_FALLBACK: &[(&str, &str)] = &[
    ("NIFTY", "NIFTY 50"),
    ("BANKNIFTY", "NIFTY BANK"),
    ("FINNIFTY", "NIFTY FIN SERVICE"),
    ("MIDCPNIFTY", "NIFTY MID SELECT"),
    ("NIFTYNXT50", "NIFTY NEXT 50"),
    ("INDIAVIX", "INDIA VIX"),
    ("SENSEX", "SENSEX"),
    ("BANKEX", "BANKEX"),
];

/// web `_get_index_name`.
pub fn index_name(b: &SamcoBroker, key: &QuoteKey) -> String {
    if let Some(br) = b.resolver().br_symbol(&key.symbol, &key.exchange) {
        return br;
    }
    let upper = key.symbol.to_ascii_uppercase();
    INDEX_NAME_FALLBACK
        .iter()
        .find(|(s, _)| *s == upper)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| key.symbol.clone())
}

fn not_found(key: &QuoteKey) -> AppError {
    AppError::Validation(format!(
        "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
        key.symbol, key.exchange
    ))
}

/// web `_api_exchange`: the stored brexchange (MCX derivatives are `MFO`).
fn api_exchange(b: &SamcoBroker, key: &QuoteKey) -> String {
    b.resolver()
        .brexchange(&key.symbol, &key.exchange)
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| key.exchange.clone())
}

fn failure(v: &Value, what: &str) -> AppError {
    let m = text(v.get("statusMessage"));
    if m.is_empty() {
        AppError::Broker(format!("Samco returned no {}.", what))
    } else {
        AppError::Broker(format!("Samco: {}", m))
    }
}

async fn index_details(b: &SamcoBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
    let name = index_name(b, key);
    let v = b
        .data_call(
            Method::GET,
            &format!("/quote/indexQuote?indexName={}", url_quote(&name)),
            auth,
            None,
        )
        .await?;
    if !is_success(&v) {
        return Err(failure(&v, "index quote"));
    }
    let d = v
        .get("indexDetails")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .ok_or_else(|| AppError::Broker(format!("Samco returned no data for {}.", key.symbol)))?;
    // The feed subscribes indices by listingId: remember it.
    let lid = text(d.get("listingId"));
    if !lid.is_empty() {
        b.listing_ids
            .lock()
            .insert(&key.exchange, &key.symbol, &lid);
    }
    Ok(d)
}

/// web `get_index_listing_id`.
pub async fn index_listing_id(b: &SamcoBroker, auth: &AuthToken, key: &QuoteKey) -> Result<String> {
    if let Some(id) = b.listing_ids.lock().get(&key.exchange, &key.symbol) {
        return Ok(id);
    }
    let d = index_details(b, auth, key).await?;
    let lid = text(d.get("listingId"));
    if lid.is_empty() {
        return Err(AppError::Broker(format!(
            "Samco did not return a streaming id for {}.",
            key.symbol
        )));
    }
    Ok(lid)
}

pub async fn get_quote(b: &SamcoBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    if is_index(&key.exchange) {
        let d = index_details(b, auth, key).await?;
        return Ok(mapping::quote_from_index(key, &d));
    }
    let row = b
        .resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .ok_or_else(|| not_found(key))?;
    let api_ex = api_exchange(b, key);
    let mut path = format!(
        "/quote/getQuote?symbolName={}",
        url_quote(mapping::br(&row))
    );
    if !api_ex.is_empty() && api_ex != "NSE" {
        path.push_str(&format!("&exchange={}", api_ex));
    }
    let v = b.data_call(Method::GET, &path, auth, None).await?;
    if !is_success(&v) {
        return Err(failure(&v, "quote"));
    }
    let q = v
        .get("quoteDetails")
        .filter(|q| mapping::truthy(Some(q)))
        .ok_or_else(|| AppError::Broker(format!("Samco returned no quote for {}.", key.symbol)))?;
    Ok(mapping::quote_from_details(key, q))
}

struct Wanted {
    idx: usize,
    key: QuoteKey,
    br: String,
    api_ex: String,
    token: String,
}

pub async fn get_multiquotes(
    b: &SamcoBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut out: Vec<Option<QuoteResult>> = vec![None; keys.len()];
    let err = |k: &QuoteKey, e: String| QuoteResult {
        symbol: k.symbol.clone(),
        exchange: k.exchange.clone(),
        data: None,
        error: Some(e),
    };
    let mut regular = Vec::new();
    let mut indices = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        if is_index(&k.exchange) {
            indices.push(i);
            continue;
        }
        match b.resolver().by_symbol(&k.exchange, &k.symbol) {
            Some(row) => regular.push(Wanted {
                idx: i,
                key: k.clone(),
                br: mapping::br(&row).to_string(),
                api_ex: api_exchange(b, k),
                token: row.token.clone(),
            }),
            None => out[i] = Some(err(k, "Could not resolve broker symbol".into())),
        }
    }
    let batches: Vec<&[Wanted]> = regular.chunks(MULTIQUOTE_BATCH).collect();
    for (n, batch) in batches.iter().enumerate() {
        if n > 0 {
            tokio::time::sleep(b.batch_delay).await;
        }
        let mut payload = Map::new();
        for w in batch.iter() {
            let list = payload
                .entry(w.api_ex.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(a) = list {
                a.push(Value::String(w.br.clone()));
            }
        }
        let v = b
            .data_call(
                Method::POST,
                "/quote/multiQuote",
                auth,
                Some(&Value::Object(payload)),
            )
            .await?;
        if !is_success(&v) {
            return Err(failure(&v, "quotes"));
        }
        let entries = v
            .get("multiQuotes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for w in batch.iter() {
            out[w.idx] = Some(
                match mapping::match_multiquote(
                    &entries,
                    &w.token,
                    &w.api_ex,
                    &w.key.exchange,
                    &w.br,
                ) {
                    Some(q) => QuoteResult {
                        symbol: w.key.symbol.clone(),
                        exchange: w.key.exchange.clone(),
                        data: Some(mapping::quote_from_multi(&w.key, q)),
                        error: None,
                    },
                    None => err(&w.key, "No quote data available".into()),
                },
            );
        }
    }
    for i in indices {
        let k = &keys[i];
        out[i] = Some(match index_details(b, auth, k).await {
            Ok(d) => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: Some(mapping::quote_from_index(k, &d)),
                error: None,
            },
            Err(e) => err(k, e.client_message()),
        });
    }
    Ok(out
        .into_iter()
        .zip(keys)
        .map(|(r, k)| r.unwrap_or_else(|| err(k, "No quote data available".into())))
        .collect())
}

pub async fn get_market_depth(
    b: &SamcoBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let zero = || vec![DepthLevel::default(); 5];
    if is_index(&key.exchange) {
        let q = get_quote(b, auth, key).await?;
        return Ok(MarketDepth {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            bids: zero(),
            asks: zero(),
            ltp: q.ltp,
            open: q.open,
            high: q.high,
            low: q.low,
            prev_close: q.close,
            volume: q.volume,
            ..Default::default()
        });
    }
    let row = b
        .resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .ok_or_else(|| not_found(key))?;
    let mut body = Map::new();
    body.insert("symbolName".into(), Value::String(mapping::br(&row).into()));
    let api_ex = api_exchange(b, key);
    if !api_ex.is_empty() && api_ex != "NSE" {
        body.insert("exchange".into(), Value::String(api_ex));
    }
    let v = b
        .data_call(
            Method::POST,
            "/marketDepth",
            auth,
            Some(&Value::Object(body)),
        )
        .await?;
    if !is_success(&v) {
        return Err(failure(&v, "market depth"));
    }
    let d = v
        .get("MarketDepthDetails")
        .and_then(|m| m.get("marketDepth"))
        .filter(|d| mapping::truthy(Some(d)))
        .ok_or_else(|| AppError::Broker(format!("Samco returned no depth for {}.", key.symbol)))?;
    let (bids, asks, tbq, tsq) = mapping::depth_levels(d);
    // marketDepth carries no OHLC; the web fills it from a quote and keeps
    // zeros when that fails.
    let q = get_quote(b, auth, key).await.unwrap_or_default();
    Ok(MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids,
        asks,
        ltp: q.ltp,
        ltq: 0,
        open: q.open,
        high: q.high,
        low: q.low,
        prev_close: q.close,
        volume: q.volume,
        oi: q.oi,
        total_buy_qty: tbq,
        total_sell_qty: tsq,
    })
}

fn candles<'a>(v: &'a Value, keys: &[&str]) -> &'a [Value] {
    keys.iter()
        .find_map(|k| {
            v.get(*k)
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
        })
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub async fn get_history(
    b: &SamcoBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = super::TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Interval {} is not supported by Samco. Use one of 1m, 5m, 10m, 15m, 30m, 1h, D.",
                req.interval
            ))
        })?;
    let key = &req.key;
    let index = is_index(&key.exchange);
    let br = if index {
        index_name(b, key)
    } else {
        b.resolver()
            .br_symbol(&key.symbol, &key.exchange)
            .ok_or_else(|| not_found(key))?
    };
    // Candle endpoints take the OpenAlgo exchange (no MFO), omitted for NSE.
    let exch = if !index && key.exchange != "NSE" {
        format!("&exchange={}", key.exchange)
    } else {
        String::new()
    };
    if resolution == "DAY" {
        let today = b.today();
        let mut end = req.end;
        if end == today && req.start < today {
            // web: daily through yesterday when the range ends today.
            end = today - Days::days(1);
        }
        let (path, keys): (String, &[&str]) = if index {
            (
                format!(
                    "/history/indexCandleData?indexName={}&fromDate={}&toDate={}",
                    url_quote(&br),
                    req.start.format("%Y-%m-%d"),
                    end.format("%Y-%m-%d")
                ),
                &["indexCandleData", "historicalCandleData"],
            )
        } else {
            (
                format!(
                    "/history/candleData?symbolName={}&fromDate={}&toDate={}{}",
                    url_quote(&br),
                    req.start.format("%Y-%m-%d"),
                    end.format("%Y-%m-%d"),
                    exch
                ),
                &["historicalCandleData"],
            )
        };
        let v = b.data_call(Method::GET, &path, auth, None).await?;
        if !is_success(&v) {
            tracing::warn!("Samco daily history was refused");
            return Ok(Vec::new());
        }
        return Ok(mapping::daily_candles(candles(&v, keys)));
    }
    let from = url_quote(&format!("{} 00:00:00", req.start.format("%Y-%m-%d")));
    let to = url_quote(&format!("{} 23:59:59", req.end.format("%Y-%m-%d")));
    let interval = if req.interval == "1m" {
        String::new()
    } else {
        format!("&interval={}", resolution)
    };
    let (path, keys): (String, &[&str]) = if index {
        (
            format!(
                "/intraday/indexCandleData?indexName={}&fromDate={}&toDate={}{}",
                url_quote(&br),
                from,
                to,
                interval
            ),
            &["indexIntraDayCandleData", "intradayCandleData"],
        )
    } else {
        (
            format!(
                "/intraday/candleData?symbolName={}&fromDate={}&toDate={}{}{}",
                url_quote(&br),
                from,
                to,
                interval,
                exch
            ),
            &["intradayCandleData"],
        )
    };
    let v = b.data_call(Method::GET, &path, auth, None).await?;
    if !is_success(&v) {
        tracing::warn!("Samco intraday history was refused");
        return Ok(Vec::new());
    }
    Ok(mapping::intraday_candles(candles(&v, keys)))
}
