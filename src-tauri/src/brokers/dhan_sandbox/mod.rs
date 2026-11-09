//! Dhan sandbox (web `broker/dhan_sandbox/**`): a thin delta over the Dhan
//! adapter, which runs it as `DhanBroker` with `Variant::Sandbox`.
//!
//! Differences from live Dhan, all from the web plugin:
//! * REST base `https://sandbox.dhan.co`, same `/v2/...` paths.
//! * The access token is the API secret (pasted), not a consent login; a
//!   pasted JWT or a consent `tokenId` is accepted too.
//! * `client-id` is sent on every call, reads and funds included.
//! * No client-side pacing; HTTP 429 is retried three times instead.
//! * No `/v2/marketfeed/*`: quotes, multiquotes and depth come from today's
//!   1-minute candles (`POST /v2/charts/intraday`); depth has no book.
//! * Intraday history is fetched in 5-day chunks.
//! * Tick size from the scrip master is kept unscaled.
//! * SL-M is sent as a bare STOP_LOSS_MARKET (no protective conversion).
//! * No Forever Orders (GTT). No market feed from Dhan: the feed is the
//!   web's simulated one, ported exactly (maintainer, 2026-11-09), so its
//!   prices are simulated prices, not market prices (`feed`).
//!
//! Not carried over: the web's `_apply_sandbox_mock_realism`, which adds
//! deterministic noise and a fake OI to sandbox quotes so its option Greeks
//! do not divide by zero. Quotes here are what the sandbox candles say.

pub mod feed;

use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::dhan::{DhanBroker, DhanSession};
use crate::brokers::types::{AuthToken, Quote, QuoteKey};
use crate::brokers::Broker;
use crate::error::Result;
use serde_json::{json, Value};

pub const BASE_URL: &str = "https://sandbox.dhan.co";

/// Intraday history chunk length on the sandbox (web `data.py`).
pub const INTRADAY_CHUNK_DAYS: i64 = 5;

/// A `dhan_sandbox` adapter sharing `symbols`.
pub fn broker(symbols: SymbolResolver) -> DhanBroker {
    DhanBroker::sandbox(symbols)
}

fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A quote from one day of 1-minute candles (web `_get_quotes_via_chart`):
/// LTP is the last close, open the first open, high the highest high, low
/// the lowest non-zero low, volume the sum.
pub fn quote_from_chart(key: &QuoteKey, v: &Value) -> Quote {
    let arr = |k: &str| {
        v.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (opens, highs, lows, closes, volumes) = (
        arr("open"),
        arr("high"),
        arr("low"),
        arr("close"),
        arr("volume"),
    );
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ..Default::default()
    };
    if closes.is_empty() {
        return q;
    }
    q.ltp = num(closes.last());
    q.open = num(opens.first());
    q.high = highs.iter().map(|h| num(Some(h))).fold(0.0_f64, f64::max);
    q.low = lows
        .iter()
        .map(|l| num(Some(l)))
        .filter(|l| *l > 0.0)
        .fold(f64::INFINITY, f64::min);
    if !q.low.is_finite() {
        q.low = 0.0;
    }
    q.volume = volumes.iter().map(|x| num(Some(x)) as i64).sum();
    q
}

/// Today's chart-derived quote for one instrument.
pub async fn chart_quote(b: &DhanBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = DhanSession::parse(auth)?;
    let row = b
        .symbols()
        .and_then(|r| r.by_symbol(&key.exchange, &key.symbol));
    let seg = crate::brokers::dhan::mapping::data_segment(&key.exchange);
    let (Some(row), Some(seg)) = (row, seg) else {
        // web: an unresolvable symbol is an all-zero quote.
        return Ok(Quote {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            ..Default::default()
        });
    };
    let instrument = crate::brokers::dhan::history_instrument_type(&key.exchange, &key.symbol)?;
    let today = crate::brokers::dhan::ist_today()
        .format("%Y-%m-%d")
        .to_string();
    let body = json!({
        "securityId": row.token.trim(),
        "exchangeSegment": seg,
        "instrument": instrument,
        "interval": "1",
        "fromDate": today,
        "toDate": today,
    });
    let v = crate::brokers::dhan::data_call(b, &s, "/v2/charts/intraday", &body).await?;
    Ok(quote_from_chart(key, &v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_quote_takes_day_ohlc_from_candles() {
        let v = json!({
            "open": [100.0, 101.0, 102.5],
            "high": [101.5, 103.0, 102.9],
            "low": [0, 100.5, 101.0],
            "close": [101.0, 102.5, 102.0],
            "volume": [1000, 2500.0, "500"],
            "timestamp": [1, 2, 3]
        });
        let q = quote_from_chart(&QuoteKey::new("NSE", "SBIN"), &v);
        assert_eq!((q.ltp, q.open, q.high, q.low), (102.0, 100.0, 103.0, 100.5));
        assert_eq!(q.volume, 4000);
        let empty = quote_from_chart(&QuoteKey::new("NSE", "SBIN"), &json!({}));
        assert_eq!(empty.ltp, 0.0);
        assert_eq!(empty.symbol, "SBIN");
    }

    #[test]
    fn sandbox_adapter_identity() {
        let b = broker(SymbolResolver::new());
        assert_eq!(b.id(), "dhan_sandbox");
        assert!(!b.capabilities().gtt);
        assert!(b.capabilities().streaming);
        assert!(!b.capabilities().order_feed);
        let feed = b
            .create_feed(&AuthToken::new("sandbox-secret"))
            .expect("the simulated feed");
        assert_eq!(feed.broker(), "dhan_sandbox");
        assert_eq!(feed.supported_depth_levels(), &[5]);
        assert_eq!(
            b.login_kind(),
            crate::brokers::types::LoginKind::AccessToken
        );
    }
}
