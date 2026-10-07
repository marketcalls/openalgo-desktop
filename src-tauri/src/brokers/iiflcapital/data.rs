//! Market data (web `api/data.py`). Every call is a `POST`:
//!
//! * quotes: `/marketdata/marketquotes` with a list of
//!   `{"exchange": <segment>, "instrumentId": <token>}`; OI comes from the
//!   single-instrument `/marketdata/openinterest` (derivatives only, best
//!   effort: a failure is OI 0, never a failed quote).
//! * depth: `/marketdata/marketdepth` with one instrument.
//! * history: `/marketdata/historicaldata` with `DD-Mon-YYYY` dates; one
//!   request for the whole range (the web does not chunk).

use super::mapping::{self, first, int, num, text};
use super::IiflCapitalBroker;
use crate::brokers::common::history::sort_dedupe;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use futures_util::stream::{self, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Exchanges that carry open interest (web `iiflcapital_mapping`).
pub fn supports_oi(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO" | "CDS" | "BCD" | "MCX")
}

/// Concurrent OI calls in a multiquote fan-out (the pacer is the real limit).
const OI_CONCURRENCY: usize = 8;

/// Strings that hold JSON are decoded (web `_try_json`).
fn try_json(v: &Value) -> Value {
    if let Value::String(s) = v {
        let t = s.trim();
        if t.starts_with('{') || t.starts_with('[') {
            if let Ok(x) = serde_json::from_str::<Value>(t) {
                return x;
            }
        }
    }
    v.clone()
}

const MARKET_KEYS: &[&str] = &[
    "ltp",
    "lastTradedPrice",
    "lastPrice",
    "open",
    "high",
    "low",
    "close",
    "tradedVolume",
    "volume",
    "bestBidPrice",
    "bestAskPrice",
    "besAskPrice",
    "marketDepth",
    "depth",
    "instrumentId",
];

fn looks_like_market_row(v: &Value) -> bool {
    v.is_object()
        && (MARKET_KEYS.iter().any(|k| v.get(*k).is_some())
            || first(v, &["touchline", "Touchline"]).is_some_and(Value::is_object))
}

/// Rows from any container IIFL uses (web `data._extract_rows`).
pub fn market_rows(payload: &Value) -> Vec<Value> {
    let payload = try_json(payload);
    if let Value::Array(list) = payload {
        return list;
    }
    if !payload.is_object() {
        return Vec::new();
    }
    for key in [
        "result",
        "data",
        "quotes",
        "candles",
        "historicalData",
        "marketDepth",
    ] {
        let Some(raw) = payload.get(key) else {
            continue;
        };
        let value = try_json(raw);
        match &value {
            Value::Array(list) => return list.clone(),
            Value::Object(_) => {
                for sub in [
                    "data",
                    "rows",
                    "quotes",
                    "candles",
                    "historicalData",
                    "marketDepth",
                    "listQuotes",
                ] {
                    if let Some(sv) = value.get(sub) {
                        let sv = try_json(sv);
                        match &sv {
                            Value::Array(list) => return list.clone(),
                            Value::Object(_) if looks_like_market_row(&sv) => return vec![sv],
                            Value::String(s) if s.contains('|') => return vec![sv],
                            _ => {}
                        }
                    }
                }
                if looks_like_market_row(&value) {
                    return vec![value];
                }
            }
            _ => {}
        }
    }
    if looks_like_market_row(&payload) {
        return vec![payload];
    }
    Vec::new()
}

/// web `data._is_success`.
pub fn is_success(status: StatusCode, payload: &Value) -> bool {
    let verdict = |v: Option<&Value>| -> Option<bool> {
        let s = v?.as_str()?.to_ascii_lowercase();
        match s.as_str() {
            "error" | "failed" | "failure" | "false" | "ko" => Some(false),
            "ok" | "success" | "true" | "200" => Some(true),
            _ => None,
        }
    };
    if payload.is_object() {
        if let Some(v) = verdict(payload.get("status")) {
            return v;
        }
        let nested = match payload.get("result") {
            Some(r @ Value::Object(_)) => verdict(r.get("status")),
            Some(Value::Array(list)) => list.first().and_then(|r| verdict(r.get("status"))),
            _ => None,
        };
        if let Some(v) = nested {
            return v;
        }
    }
    status == StatusCode::OK
}

fn row_error(row: &Value) -> Option<String> {
    let s = text(first(row, &["status", "Status"])).to_ascii_lowercase();
    if matches!(s.as_str(), "error" | "failed" | "failure" | "false" | "ko") {
        Some(
            text(first(row, &["message", "error", "description", "emsg"]))
                .chars()
                .take(200)
                .collect::<String>(),
        )
        .map(|m| {
            if m.is_empty() {
                "Request failed".into()
            } else {
                m
            }
        })
    } else {
        None
    }
}

/// One market-data POST: pacing and throttle retries come from `call`; a
/// non-success answer becomes a trader-facing error.
async fn post(b: &IiflCapitalBroker, auth: &AuthToken, path: &str, body: &Value) -> Result<Value> {
    let (status, data) = b.call(Method::POST, path, auth, Some(body), true).await?;
    if is_success(status, &data) {
        return Ok(data);
    }
    let why = mapping::message_of(&data).unwrap_or_else(|| "no data".into());
    tracing::warn!(
        status = status.as_u16(),
        "IIFL Capital {} failed: {}",
        path,
        why
    );
    Err(AppError::Broker(format!(
        "IIFL Capital could not return market data: {}",
        why
    )))
}

/// `{"exchange": <segment>, "instrumentId": <token>}` for a key (web
/// `_instrument`): the stored brexchange, or the normalised segment when it
/// is empty or `INDICES`.
pub fn instrument(b: &IiflCapitalBroker, key: &QuoteKey) -> Result<Value> {
    let row = b
        .resolver()
        .by_symbol(&key.exchange, &key.symbol)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })?;
    let mut seg = row.brexchange.trim().to_ascii_uppercase();
    if seg.is_empty() || seg == "INDICES" {
        seg = mapping::data_segment(&key.exchange);
    }
    Ok(json!({"exchange": seg, "instrumentId": row.token}))
}

fn level0(levels: Option<&Value>) -> Value {
    levels
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .cloned()
        .unwrap_or(Value::Null)
}

/// web `_parse_quote_row`.
pub fn parse_quote_row(row: &Value, key: &QuoteKey) -> Quote {
    let touch = first(row, &["touchline", "Touchline", "quote", "Quote"])
        .cloned()
        .unwrap_or(Value::Null);
    let depth = first(row, &["depth", "marketDepth", "Depth"])
        .cloned()
        .unwrap_or(Value::Null);
    let bid1 = level0(first(&depth, &["buy", "bids", "Buy"]));
    let ask1 = level0(first(&depth, &["sell", "asks", "Sell"]));
    let bid_info = first(&touch, &["BidInfo", "bidInfo"])
        .cloned()
        .unwrap_or(Value::Null);
    let ask_info = first(&touch, &["AskInfo", "askInfo"])
        .cloned()
        .unwrap_or(Value::Null);
    let pick = |keys: &[&str], fallback: &[(&Value, &[&str])]| -> f64 {
        if let Some(v) = first(row, keys) {
            return num(Some(v));
        }
        for (src, ks) in fallback {
            if let Some(v) = first(src, ks) {
                return num(Some(v));
            }
        }
        0.0
    };
    let ltp = pick(
        &["ltp", "LTP", "lastTradedPrice", "lastPrice", "last_price"],
        &[(&touch, &["LastTradedPrice", "lastTradedPrice"])],
    );
    let close = pick(
        &[
            "close",
            "closePrice",
            "previousClose",
            "previousClosePrice",
            "prevClose",
            "prev_close",
            "Close",
        ],
        &[(&touch, &["Close", "close"])],
    );
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: pick(
            &["open", "openPrice", "dayOpen", "Open"],
            &[(&touch, &["Open", "open"])],
        ),
        high: pick(
            &["high", "highPrice", "dayHigh", "High"],
            &[(&touch, &["High", "high"])],
        ),
        low: pick(
            &["low", "lowPrice", "dayLow", "Low"],
            &[(&touch, &["Low", "low"])],
        ),
        close,
        volume: pick(
            &[
                "volume",
                "tradedVolume",
                "totalTradedVolume",
                "totalTradedQuantity",
            ],
            &[(&touch, &["TotalTradedQuantity", "totalTradedQuantity"])],
        ) as i64,
        bid: pick(
            &["bid", "bidPrice", "bestBid", "bestBidPrice"],
            &[
                (&bid_info, &["Price", "price"]),
                (&bid1, &["price", "Price"]),
            ],
        ),
        ask: pick(
            &["ask", "askPrice", "bestAsk", "bestAskPrice", "besAskPrice"],
            &[
                (&ask_info, &["Price", "price"]),
                (&ask1, &["price", "Price"]),
            ],
        ),
        bid_qty: int(first(row, &["bestBidQuantity", "bidQuantity"])),
        ask_qty: int(first(row, &["bestAskQuantity", "askQuantity"])),
        oi: int(first(row, &["oi", "openInterest", "OpenInterest", "OI"])),
        ..Default::default()
    };
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = mapping::round2(q.ltp - q.close);
        q.change_percent = mapping::round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

/// OI of one derivative instrument; 0 on any failure (web
/// `_fetch_openinterest`).
async fn open_interest(b: &IiflCapitalBroker, auth: &AuthToken, inst: &Value) -> i64 {
    match post(b, auth, "/marketdata/openinterest", inst).await {
        Ok(v) => {
            let r = v.get("result").unwrap_or(&v);
            let r = match r {
                Value::Array(list) => list.first().cloned().unwrap_or(Value::Null),
                other => other.clone(),
            };
            int(first(&r, &["openInterest", "oi"]))
        }
        Err(e) => {
            tracing::debug!("IIFL Capital OI fetch failed: {}", e.code());
            0
        }
    }
}

pub async fn get_quote(b: &IiflCapitalBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let inst = instrument(b, key)?;
    let data = post(
        b,
        auth,
        "/marketdata/marketquotes",
        &Value::Array(vec![inst.clone()]),
    )
    .await?;
    let rows = market_rows(&data);
    let row = rows.first().map(try_json).ok_or_else(|| {
        AppError::Broker(format!(
            "IIFL Capital returned no quote for {} on {}.",
            key.symbol, key.exchange
        ))
    })?;
    if let Some(e) = row_error(&row) {
        return Err(AppError::Broker(e));
    }
    if !looks_like_market_row(&row) {
        return Err(AppError::Broker(format!(
            "IIFL Capital returned no quote for {} on {}.",
            key.symbol, key.exchange
        )));
    }
    let mut q = parse_quote_row(&row, key);
    if supports_oi(&key.exchange.to_ascii_uppercase()) {
        q.oi = open_interest(b, auth, &inst).await;
    }
    Ok(q)
}

fn identity(inst: &Value) -> String {
    format!(
        "{}:{}",
        text(inst.get("exchange")).to_ascii_uppercase(),
        text(inst.get("instrumentId"))
    )
}

pub async fn get_multiquotes(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let mut slots: Vec<std::result::Result<Value, String>> = Vec::with_capacity(keys.len());
    let mut insts = Vec::new();
    for k in keys {
        match instrument(b, k) {
            Ok(i) => {
                insts.push(i.clone());
                slots.push(Ok(i));
            }
            Err(e) => slots.push(Err(e.client_message())),
        }
    }
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut positional: Vec<Value> = Vec::new();
    if !insts.is_empty() {
        let data = post(
            b,
            auth,
            "/marketdata/marketquotes",
            &Value::Array(insts.clone()),
        )
        .await?;
        for row in market_rows(&data) {
            let row = try_json(&row);
            if !row.is_object() {
                continue;
            }
            let ex = text(first(&row, &["exchange", "exchangeSegment"])).to_ascii_uppercase();
            let tok = text(first(
                &row,
                &["instrumentId", "token", "exchangeInstrumentID"],
            ));
            if !ex.is_empty() && !tok.is_empty() {
                by_id.insert(format!("{}:{}", ex, tok), row.clone());
            }
            positional.push(row);
        }
    }
    // OI for the derivative legs, fanned out (no batch OI endpoint).
    let oi_insts: Vec<Value> = keys
        .iter()
        .zip(&slots)
        .filter(|(k, s)| s.is_ok() && supports_oi(&k.exchange.to_ascii_uppercase()))
        .filter_map(|(_, s)| s.as_ref().ok().cloned())
        .collect();
    let oi: HashMap<String, i64> = stream::iter(oi_insts)
        .map(|inst| async move {
            let v = open_interest(b, auth, &inst).await;
            (identity(&inst), v)
        })
        .buffer_unordered(OI_CONCURRENCY)
        .collect()
        .await;
    let use_identity = !by_id.is_empty();
    let mut valid_idx = 0usize;
    let mut out = Vec::with_capacity(keys.len());
    for (k, slot) in keys.iter().zip(slots) {
        let entry = |data: Option<Quote>, error: Option<String>| QuoteResult {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            data,
            error,
        };
        let inst = match slot {
            Ok(i) => i,
            Err(e) => {
                out.push(entry(None, Some(e)));
                continue;
            }
        };
        let id = identity(&inst);
        let row = if use_identity {
            by_id.get(&id).cloned().or_else(|| {
                by_id
                    .get(&format!(
                        "{}:{}",
                        k.exchange.to_ascii_uppercase(),
                        text(inst.get("instrumentId"))
                    ))
                    .cloned()
            })
        } else {
            positional.get(valid_idx).cloned()
        };
        valid_idx += 1;
        match row {
            None => out.push(entry(None, Some("No quote data available".into()))),
            Some(r) => match row_error(&r) {
                Some(e) => out.push(entry(None, Some(e))),
                None => {
                    let mut q = parse_quote_row(&r, k);
                    if let Some(v) = oi.get(&id) {
                        q.oi = *v;
                    }
                    out.push(entry(Some(q), None));
                }
            },
        }
    }
    Ok(out)
}

fn depth_levels(levels: Option<&Value>) -> Vec<DepthLevel> {
    let mut out: Vec<DepthLevel> = levels
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(20)
                .filter_map(|l| match l {
                    Value::Object(_) => Some(DepthLevel {
                        price: num(first(l, &["price", "Price"])),
                        quantity: int(first(l, &["quantity", "qty", "Quantity"])),
                        orders: int(first(l, &["orders", "numOrders", "Orders"])),
                    }),
                    Value::Array(a) if a.len() >= 2 => Some(DepthLevel {
                        price: num(a.first()),
                        quantity: int(a.get(1)),
                        orders: int(a.get(2)),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    out.truncate(5);
    while out.len() < 5 {
        out.push(DepthLevel::default());
    }
    out
}

/// web `get_depth` without the OI call.
pub fn parse_depth_row(row: &Value, key: &QuoteKey) -> MarketDepth {
    let depth = first(row, &["depth", "marketDepth", "Depth"])
        .cloned()
        .unwrap_or(Value::Null);
    let bids = depth_levels(first(&depth, &["buy", "bids", "Buy"]));
    let asks = depth_levels(first(&depth, &["sell", "asks", "Sell"]));
    let tb = match first(
        row,
        &[
            "totalBidQuantity",
            "totalBuyQuantity",
            "totBuyQuan",
            "totalbuyqty",
        ],
    ) {
        Some(v) => int(Some(v)),
        None => bids.iter().map(|l| l.quantity).sum(),
    };
    let ts = match first(
        row,
        &[
            "totalAskQuantity",
            "totalSellQuantity",
            "totSellQuan",
            "totalsellqty",
        ],
    ) {
        Some(v) => int(Some(v)),
        None => asks.iter().map(|l| l.quantity).sum(),
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids,
        asks,
        ltp: num(first(row, &["ltp", "lastTradedPrice"])),
        ltq: int(first(row, &["ltq", "lastTradedQuantity", "lastTradeQty"])),
        open: num(first(row, &["open", "openPrice"])),
        high: num(first(row, &["high", "highPrice"])),
        low: num(first(row, &["low", "lowPrice"])),
        prev_close: num(first(row, &["close", "previousClose"])),
        volume: int(first(row, &["volume", "tradedVolume"])),
        oi: int(first(row, &["oi", "openInterest"])),
        total_buy_qty: tb,
        total_sell_qty: ts,
    }
}

pub async fn get_market_depth(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let inst = instrument(b, key)?;
    let data = post(b, auth, "/marketdata/marketdepth", &inst).await?;
    let row = market_rows(&data)
        .first()
        .map(try_json)
        .filter(Value::is_object)
        .ok_or_else(|| {
            AppError::Broker(format!(
                "IIFL Capital returned no market depth for {} on {}.",
                key.symbol, key.exchange
            ))
        })?;
    let mut d = parse_depth_row(&row, key);
    if d.oi == 0 && supports_oi(&key.exchange.to_ascii_uppercase()) {
        d.oi = open_interest(b, auth, &inst).await;
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Timestamp text or number -> epoch seconds (ms divided down).
fn epoch(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::String(s)) if !s.trim().chars().all(|c| c.is_ascii_digit()) => {
            parse_datetime(s.trim()).unwrap_or(0)
        }
        other => {
            let t = int(other);
            if t > 1_000_000_000_000 {
                t / 1000
            } else {
                t
            }
        }
    }
}

/// `pd.to_datetime` for the shapes IIFL sends: ISO with or without offset
/// (naive times are taken as UTC, as pandas does), or a bare date.
fn parse_datetime(s: &str) -> Option<i64> {
    if let Some(t) = crate::brokers::common::history::parse_iso_epoch(s) {
        return Some(t);
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
    ] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(d.and_utc().timestamp());
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp())
}

fn candle_from_seq(a: &[Value]) -> Option<Candle> {
    if a.len() < 6 {
        return None;
    }
    Some(Candle {
        timestamp: epoch(a.first()),
        open: num(a.get(1)),
        high: num(a.get(2)),
        low: num(a.get(3)),
        close: num(a.get(4)),
        volume: int(a.get(5)),
        oi: int(a.get(6)),
    })
}

/// Candles from any of the row shapes IIFL returns (web
/// `_parse_history_rows`): `[ts,o,h,l,c,v,oi?]` arrays, objects holding a
/// `candles` list, pipe-delimited strings, or keyed objects.
pub fn parse_history_rows(rows: &[Value]) -> Vec<Candle> {
    let mut out = Vec::new();
    for raw in rows {
        let row = try_json(raw);
        if let Some(nested) = row
            .as_object()
            .and_then(|_| first(&row, &["candles", "Candles"]))
        {
            if let Value::Array(list) = try_json(nested) {
                out.extend(
                    list.iter()
                        .filter_map(|c| c.as_array().and_then(|a| candle_from_seq(a))),
                );
                continue;
            }
        }
        if let Some(c) = row.as_array().and_then(|a| candle_from_seq(a)) {
            out.push(c);
            continue;
        }
        if let Value::String(s) = &row {
            if s.contains('|') {
                for part in s.split(',') {
                    let p: Vec<Value> = part
                        .split('|')
                        .map(|x| Value::String(x.trim().to_string()))
                        .collect();
                    if p.len() >= 6 {
                        out.push(Candle {
                            timestamp: int(p.first()),
                            open: num(p.get(1)),
                            high: num(p.get(2)),
                            low: num(p.get(3)),
                            close: num(p.get(4)),
                            volume: int(p.get(5)),
                            oi: int(p.get(6)),
                        });
                    }
                }
            }
            continue;
        }
        if !row.is_object() {
            continue;
        }
        out.push(Candle {
            timestamp: epoch(first(
                &row,
                &[
                    "timestamp",
                    "time",
                    "dateTime",
                    "datetime",
                    "epoch",
                    "candleTime",
                ],
            )),
            open: num(first(&row, &["open", "o"])),
            high: num(first(&row, &["high", "h"])),
            low: num(first(&row, &["low", "l"])),
            close: num(first(&row, &["close", "c"])),
            volume: int(first(&row, &["volume", "v"])),
            oi: int(first(&row, &["oi", "openInterest"])),
        });
    }
    sort_dedupe(out)
}

/// `2024-01-05` -> `05-Jan-2024` (web `_format_iifl_date`).
pub fn iifl_date(d: chrono::NaiveDate) -> String {
    d.format("%d-%b-%Y").to_string()
}

pub async fn get_history(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let interval = super::TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == req.interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Interval '{}' is not supported by IIFL Capital.",
                req.interval
            ))
        })?;
    let inst = instrument(b, &req.key)?;
    let body = json!({
        "exchange": inst["exchange"],
        "instrumentId": inst["instrumentId"],
        "interval": interval,
        "fromDate": iifl_date(req.start),
        "toDate": iifl_date(req.end),
    });
    let data = post(b, auth, "/marketdata/historicaldata", &body).await?;
    Ok(parse_history_rows(&market_rows(&data)))
}
