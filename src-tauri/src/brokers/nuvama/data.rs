//! Quotes, depth and history (web `api/data.py`).
//!
//! * Quote and depth: `GET {content}quote/scrip/{token}/` with the master
//!   token (`2885_NSE`, `-29` for an index). One answer carries the quote
//!   (`data.qt`) and five levels each side (`data.mkd`). There is no batch
//!   route, so multiquotes are single quotes on the data lane.
//! * History: `POST {content}charts/v2/main/{interval}/{exc}/{aTyp}/{token}`,
//!   intervals M1, M3, M5, M15, M30, H1 and D1, paged backwards with
//!   `ltt`. Candle times are IST wall-clock text (`2024-01-05 09:15:00`),
//!   stamped with the bar's close; intraday bars are moved to their open.
//!   The history service then puts them on the exchange session's grid
//!   (`Broker::history_on_session_grid`).

use super::client::{Call, Lane};
use super::mapping::{chart_asset_type, chart_exchange, int, num, number, text};
use super::{NuvamaBroker, TIMEFRAME_MAP};
use crate::brokers::common::history::{intraday_minutes, sort_dedupe};
use crate::brokers::common::python::truthy;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Datelike, Days, NaiveDate, NaiveDateTime};
use reqwest::Method;
use serde_json::{json, Value};
use std::time::Duration;

const IST_OFFSET_SECONDS: i64 = 19_800;
/// web: `range(200)` pages at most.
const MAX_PAGES: usize = 200;
/// web: `timeout=30.0` on the chart route.
const CHART_TIMEOUT: Duration = Duration::from_secs(30);

/// The quote snapshot of one instrument: `(qt, mkd)`.
async fn snapshot(b: &NuvamaBroker, auth: &AuthToken, key: &QuoteKey) -> Result<(Value, Value)> {
    let row = b.instrument(key)?;
    let s = b.session(auth)?;
    let url = format!(
        "{}quote/scrip/{}/",
        b.urls.content,
        urlencoding::encode(&row.token)
    );
    let a = b.call(&s, Call::get(url, Lane::Data)).await?;
    if !a.ok() {
        let msg = a.message("Nuvama sent no quote");
        tracing::warn!(
            status = a.status,
            "Nuvama sent no quote for {}:{}",
            key.exchange,
            key.symbol
        );
        return Err(AppError::Broker(format!(
            "No quote for {}:{}: {}",
            key.exchange, key.symbol, msg
        )));
    }
    let data = a.data().cloned().unwrap_or(Value::Null);
    let part = |k: &str| {
        data.get(k)
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}))
    };
    Ok((part("qt"), part("mkd")))
}

fn side(mkd: &Value, key: &str) -> Vec<Value> {
    mkd.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A quote field, else the best level's (web `qt.get(a) or best.get(b)`):
/// the level stands in only for a field that is missing, empty or a
/// numeric 0, so a text `"0"` is Nuvama's own value, as on the web.
fn or_best(qt: &Value, key: &str, best: Option<&Value>, level_key: &str) -> f64 {
    let field = qt.get(key).filter(|v| truthy(Some(v)));
    num(field.or_else(|| best.and_then(|l| l.get(level_key))))
}

/// One snapshot -> OpenAlgo quote (web `_quote_from`).
pub(crate) fn quote_from(key: &QuoteKey, qt: &Value, mkd: &Value) -> Quote {
    let (bids, asks) = (side(mkd, "bid"), side(mkd, "ask"));
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: num(qt.get("ltp")),
        open: num(qt.get("o")),
        high: num(qt.get("h")),
        low: num(qt.get("l")),
        close: num(qt.get("c")),
        volume: int(qt.get("vol")),
        bid: or_best(qt, "bidPr", bids.first(), "price"),
        ask: or_best(qt, "askPr", asks.first(), "price"),
        bid_qty: or_best(qt, "bdSz", bids.first(), "qty") as i64,
        ask_qty: or_best(qt, "akSz", asks.first(), "qty") as i64,
        oi: int(qt.get("opInt")),
        ..Default::default()
    }
}

pub async fn get_quote(b: &NuvamaBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let (qt, mkd) = snapshot(b, auth, key).await?;
    Ok(quote_from(key, &qt, &mkd))
}

/// Five levels at most, as Nuvama sent them (web `levels`, which pads to
/// five with zeros; the desktop never pads a book).
fn levels(rows: &[Value]) -> Vec<DepthLevel> {
    rows.iter()
        .take(5)
        .map(|r| DepthLevel {
            price: num(r.get("price")),
            quantity: int(r.get("qty")),
            orders: 0,
        })
        .collect()
}

/// One snapshot -> OpenAlgo depth (web `get_depth`).
pub(crate) fn depth_from(key: &QuoteKey, qt: &Value, mkd: &Value) -> MarketDepth {
    let (bids, asks) = (side(mkd, "bid"), side(mkd, "ask"));
    let total = |field: &str, rows: &[Value]| match int(mkd.get(field)) {
        0 => rows.iter().map(|r| int(r.get("qty"))).sum(),
        v => v,
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: levels(&bids),
        asks: levels(&asks),
        ltp: num(qt.get("ltp")),
        ltq: int(qt.get("ltTQty")),
        open: num(qt.get("o")),
        high: num(qt.get("h")),
        low: num(qt.get("l")),
        prev_close: num(qt.get("c")),
        volume: int(qt.get("vol")),
        oi: int(qt.get("opInt")),
        total_buy_qty: total("buyQty", &bids),
        total_sell_qty: total("sellQty", &asks),
    }
}

pub async fn get_market_depth(
    b: &NuvamaBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let (qt, mkd) = snapshot(b, auth, key).await?;
    Ok(depth_from(key, &qt, &mkd))
}

/// The chart route of an instrument (web `_chart_route`).
fn chart_url(b: &NuvamaBroker, key: &QuoteKey, resolution: &str) -> Result<String> {
    let row = b.instrument(key)?;
    let index_underlying = matches!(key.exchange.as_str(), "NFO" | "BFO")
        && ["NSE_INDEX", "BSE_INDEX"]
            .iter()
            .any(|ex| b.resolver().token(&row.name, ex).is_some());
    let asset = chart_asset_type(&key.exchange, &row.instrument_type, index_underlying);
    Ok(format!(
        "{}charts/v2/main/{}/{}/{}/{}",
        b.urls.content,
        resolution,
        chart_exchange(&key.exchange),
        asset,
        urlencoding::encode(&row.token)
    ))
}

/// One bar as Nuvama sent it: its IST wall-clock stamp and values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawBar {
    pub stamp: NaiveDateTime,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
}

/// The bars of a chart answer's `data.pltPnts` (parallel `ltt`, `open`,
/// `high`, `low`, `close`, `vol` lists). A bar whose time or price does
/// not read is left out (the web's `errors="coerce"` makes it NaN).
pub(crate) fn raw_bars(points: &Value) -> Vec<RawBar> {
    let list = |k: &str| {
        points
            .get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (ltt, open, high, low, close, vol) = (
        list("ltt"),
        list("open"),
        list("high"),
        list("low"),
        list("close"),
        list("vol"),
    );
    ltt.iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let stamp = NaiveDateTime::parse_from_str(&text(Some(t)), "%Y-%m-%d %H:%M:%S").ok()?;
            Some(RawBar {
                stamp,
                open: number(open.get(i))?,
                high: number(high.get(i))?,
                low: number(low.get(i))?,
                close: number(close.get(i))?,
                volume: number(vol.get(i)).map(|v| v as i64).unwrap_or(0),
            })
        })
        .collect()
}

/// One page of candles; none for Nuvama's "no data" answer.
async fn page(
    b: &NuvamaBroker,
    auth: &AuthToken,
    url: &str,
    till: Option<NaiveDate>,
) -> Result<Vec<RawBar>> {
    let s = b.session(auth)?;
    let body = json!({
        "chTyp": "Interval",
        "conti": false,
        "ltt": till.map(|d| d.format("%Y-%m-%d").to_string()),
    });
    let mut call = Call::send(Method::POST, url.to_string(), &body, Lane::Data);
    call.timeout = CHART_TIMEOUT;
    let a = b.call(&s, call).await?;
    if !a.ok() {
        if a.status == super::client::NUVAMA_REFUSAL_STATUS || a.error_code().is_some() {
            tracing::debug!("Nuvama chart answered no data: {}", a.message(""));
            return Ok(Vec::new());
        }
        return Err(a.refused("a chart"));
    }
    let points = a
        .data()
        .and_then(|d| d.get("pltPnts"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(raw_bars(&points))
}

/// The epoch-seconds stamp of a bar (web `_series`): a daily bar at its
/// date's 00:00 UTC, as the other plugins report it; an intraday bar at its
/// open, its close stamp less a second floored onto the bar grid, in IST.
pub(crate) fn bar_open(stamp: NaiveDateTime, minutes: Option<i64>) -> i64 {
    let naive = stamp.and_utc().timestamp();
    match minutes {
        None => stamp
            .date()
            .and_hms_opt(0, 0, 0)
            .map_or(naive, |d| d.and_utc().timestamp()),
        Some(m) => {
            let step = m * 60;
            (naive - 1).div_euclid(step) * step - IST_OFFSET_SECONDS
        }
    }
}

/// Where the next page ends, or `None` when paging is done (web: pages
/// are whole years for daily candles, about 1500 bars up to the day before
/// `ltt` intraday; a page asks through its earliest day again).
pub(crate) fn next_till(
    daily: bool,
    earliest: NaiveDate,
    till: Option<NaiveDate>,
) -> Option<NaiveDate> {
    let next = if daily {
        NaiveDate::from_ymd_opt(earliest.year(), 1, 1)?
    } else {
        earliest.checked_add_days(Days::new(1))?
    };
    match till {
        Some(t) if next >= t => None,
        _ => Some(next),
    }
}

/// The first page's `ltt`: none for a range that reaches today (or this
/// year, daily).
pub(crate) fn first_till(daily: bool, end: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    if daily {
        (end.year() < today.year())
            .then(|| NaiveDate::from_ymd_opt(end.year() + 1, 1, 1))
            .flatten()
    } else {
        (end < today)
            .then(|| end.checked_add_days(Days::new(1)))
            .flatten()
    }
}

pub async fn get_history(
    b: &NuvamaBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let Some((_, resolution)) = TIMEFRAME_MAP.iter().find(|(k, _)| *k == req.interval) else {
        let offered: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
        return Err(AppError::Validation(format!(
            "Interval {} is not available from Nuvama. Use one of: {}",
            req.interval,
            offered.join(", ")
        )));
    };
    let minutes = intraday_minutes(&req.interval);
    let daily = minutes.is_none();
    let url = chart_url(b, &req.key, resolution)?;
    let today = crate::brokers::common::ratelimit::today_ist();
    let mut till = first_till(daily, req.end, today);
    let mut bars = Vec::new();
    for _ in 0..MAX_PAGES {
        let got = page(b, auth, &url, till).await?;
        let Some(earliest) = got.iter().map(|r| r.stamp.date()).min() else {
            break;
        };
        bars.extend(got);
        if earliest <= req.start {
            break;
        }
        match next_till(daily, earliest, till) {
            Some(t) => till = Some(t),
            None => break,
        }
    }
    let candles = bars
        .into_iter()
        .filter(|r| (req.start..=req.end).contains(&r.stamp.date()))
        .map(|r| Candle {
            timestamp: bar_open(r.stamp, minutes),
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            volume: r.volume,
            oi: 0,
        })
        .collect();
    Ok(sort_dedupe(candles))
}
