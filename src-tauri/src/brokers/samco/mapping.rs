//! Samco <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `mapping/margin_data.py`, `api/funds.py`,
//! `api/data.py` parsers). Pure functions over broker JSON.

use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::mpp;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Value helpers (web `safe_float` / `safe_int` / `_clean_text`)
// ---------------------------------------------------------------------------

/// String form of a JSON value (`null` -> empty), trimmed.
pub fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// web `safe_float`: numbers, numeric strings with thousands commas.
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().replace(',', "").parse::<f64>().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

/// web `safe_int`: `int(float(x))`.
pub fn int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| num(v) as i64),
        _ => num(v) as i64,
    }
}

const PLACEHOLDERS: &[&str] = &["", "NA", "N/A", "--", "-", "None", "null"];

/// web `_clean_text`: Samco's `NA` / `--` placeholders become empty.
pub fn clean_text(v: Option<&Value>) -> String {
    let t = text(v);
    if PLACEHOLDERS.contains(&t.as_str()) {
        String::new()
    } else {
        t
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// Python `str(float(x))`: `100.0`, `100.5`.
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// Python `f"{x:g}"` for the MPP slab percentages (0.5, 1, 2, 3, 5).
pub fn py_g(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

fn round2(v: f64) -> f64 {
    mpp::py_round(v, 2)
}

/// Samco files MCX derivatives under `MFO`; OpenAlgo calls them `MCX`.
pub fn oa_exchange(samco_exchange: &str) -> &str {
    if samco_exchange == "MFO" {
        "MCX"
    } else {
        samco_exchange
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// web `map_order_type` (MARKET / SL-M are placeholders resolved by MPP).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "L",
        PriceType::Sl => "SL",
        PriceType::SlM => "SL-M",
    }
}

/// web `map_product_type` (identity, MIS default).
pub fn product(p: Product) -> &'static str {
    p.as_str()
}

/// web `reverse_map_product_type`.
pub fn reverse_product(code: &str) -> &'static str {
    match code {
        "CNC" => "CNC",
        "NRML" => "NRML",
        _ => "MIS",
    }
}

/// web `map_order_status`: lowercase, with Samco's extras folded in.
pub fn map_status(status: &str) -> String {
    let s = status.trim().to_ascii_lowercase();
    match s.as_str() {
        "open" | "pending" | "ordered" | "trigger pending" | "after market order req received" => {
            "open".to_string()
        }
        "complete" | "completed" | "executed" | "filled" => "complete".to_string(),
        "cancelled" | "canceled" => "cancelled".to_string(),
        "rejected" => "rejected".to_string(),
        _ => s,
    }
}

/// Book order type back to OpenAlgo: `L` with `marketProtection` was a
/// protected MARKET order.
pub fn reverse_order_type(order_type: &str, market_protection: Option<&Value>) -> String {
    match order_type {
        "L" if truthy(market_protection) => "MARKET".into(),
        "L" => "LIMIT".into(),
        "MKT" => "MARKET".into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Orders (MPP)
// ---------------------------------------------------------------------------

/// Order type, price and slab Samco is sent after Market Price Protection
/// (web `resolve_order_type`): MARKET -> `L` at the protected LTP, SL-M ->
/// `SL` with a limit protected from the trigger.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub order_type: &'static str,
    pub price: String,
    pub mpp_percentage: Option<f64>,
}

pub fn resolve_order_type(
    pricetype: PriceType,
    action: Action,
    symbol: &str,
    price: f64,
    trigger_price: f64,
    tick_size: Option<f64>,
    ltp: Option<f64>,
) -> std::result::Result<Resolved, String> {
    let it = mpp::instrument_type_from_symbol(symbol);
    let tick = tick_size.filter(|t| *t > 0.0);
    match pricetype {
        PriceType::Market => {
            let ltp = ltp.unwrap_or(0.0);
            if ltp <= 0.0 {
                return Err(format!(
                    "MARKET order failed: no live price for {}, so a protected price could not be set",
                    symbol
                ));
            }
            Ok(Resolved {
                order_type: "L",
                price: py_float(mpp::protected_price(ltp, action, it, tick)),
                mpp_percentage: Some(mpp::mpp_percentage(ltp, it)),
            })
        }
        PriceType::SlM => {
            if trigger_price <= 0.0 {
                return Err(format!(
                    "SL-M order failed: trigger price is required for {}",
                    symbol
                ));
            }
            Ok(Resolved {
                order_type: "SL",
                price: py_float(mpp::protected_price(trigger_price, action, it, tick)),
                mpp_percentage: Some(mpp::mpp_percentage(trigger_price, it)),
            })
        }
        other => Ok(Resolved {
            order_type: order_type(other),
            price: py_float(price),
            mpp_percentage: None,
        }),
    }
}

fn add_price_fields(body: &mut Map<String, Value>, r: &Resolved, pt: PriceType, trigger: f64) {
    if matches!(r.order_type, "L" | "SL") {
        body.insert("price".into(), json!(r.price));
    }
    if r.order_type == "SL" || matches!(pt, PriceType::Sl | PriceType::SlM) {
        body.insert("triggerPrice".into(), json!(py_float(trigger)));
    }
    if matches!(pt, PriceType::Market | PriceType::SlM) {
        if let Some(p) = r.mpp_percentage {
            body.insert("marketProtection".into(), json!(py_g(p)));
        }
    }
}

/// `POST /order/placeOrder` body (web `transform_data` + `place_order_api`).
pub fn place_body(o: &ResolvedOrder, r: &Resolved) -> Value {
    let mut body = Map::new();
    body.insert("symbolName".into(), json!(o.brsymbol()));
    body.insert("exchange".into(), json!(o.exchange.as_str()));
    body.insert("transactionType".into(), json!(o.action.as_str()));
    body.insert("orderType".into(), json!(r.order_type));
    body.insert("quantity".into(), json!(o.quantity.to_string()));
    body.insert(
        "disclosedQuantity".into(),
        json!(o.disclosed_quantity.to_string()),
    );
    body.insert("orderValidity".into(), json!("DAY"));
    body.insert("productType".into(), json!(product(o.product)));
    body.insert("afterMarketOrderFlag".into(), json!("NO"));
    add_price_fields(&mut body, r, o.pricetype, o.trigger_price);
    Value::Object(body)
}

/// `PUT /order/modifyOrder/{id}` body (web `transform_modify_order_data`).
pub fn modify_body(m: &ResolvedModify, r: &Resolved) -> Value {
    let mut body = Map::new();
    body.insert("orderType".into(), json!(r.order_type));
    body.insert("quantity".into(), json!(m.quantity.to_string()));
    body.insert("orderValidity".into(), json!("DAY"));
    if m.disclosed_quantity > 0 {
        body.insert(
            "disclosedQuantity".into(),
            json!(m.disclosed_quantity.to_string()),
        );
    }
    add_price_fields(&mut body, r, m.pricetype, m.trigger_price);
    Value::Object(body)
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// OpenAlgo symbol and exchange for a book row (web `get_oa_symbol`; the
/// raw trading symbol is kept when the master does not know it).
pub fn oa_symbol(
    symbols: &SymbolResolver,
    trading_symbol: &str,
    exchange: &str,
) -> (String, String) {
    let ex = oa_exchange(exchange).to_string();
    let sym = symbols
        .by_brsymbol(&ex, trading_symbol)
        .map(|r| r.symbol)
        .unwrap_or_else(|| trading_symbol.to_string());
    (sym, ex)
}

fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// web `map_order_data` + `transform_order_data`.
pub fn order_book(v: &Value, symbols: &SymbolResolver) -> Vec<Order> {
    rows(v, "orderBookDetails")
        .iter()
        .filter(|o| o.is_object())
        .map(|o| {
            let (symbol, exchange) = oa_symbol(
                symbols,
                &text(o.get("tradingSymbol")),
                &text(o.get("exchange")),
            );
            let pending = match o.get("unfilledQuantity") {
                Some(x) if !x.is_null() => int(Some(x)),
                _ => int(o.get("pendingQuantity")),
            };
            let avg = {
                let a = num(o.get("averagePrice"));
                if a != 0.0 {
                    a
                } else {
                    num(o.get("fillPrice"))
                }
            };
            let reason = clean_text(o.get("rejectionReason"));
            Order {
                order_id: text(o.get("orderNumber")),
                exchange_order_id: None,
                symbol,
                exchange,
                side: text(o.get("transactionType")),
                quantity: int(o.get("totalQuanity")) as i32,
                filled_quantity: int(o.get("filledQuantity")) as i32,
                pending_quantity: pending as i32,
                price: num(o.get("orderPrice")),
                trigger_price: num(o.get("triggerPrice")),
                average_price: avg,
                order_type: reverse_order_type(
                    &text(o.get("orderType")),
                    o.get("marketProtection"),
                ),
                product: text(o.get("productCode")),
                status: map_status(&text(o.get("orderStatus"))),
                validity: "DAY".into(),
                order_timestamp: text(o.get("orderTime")),
                exchange_timestamp: None,
                rejection_reason: (!reason.is_empty()).then_some(reason),
            }
        })
        .collect()
}

/// web `map_trade_data` + `transform_tradebook_data`.
pub fn trade_book(v: &Value, symbols: &SymbolResolver) -> Vec<Trade> {
    rows(v, "tradeBookDetails")
        .iter()
        .filter(|t| t.is_object())
        .map(|t| {
            let (symbol, exchange) = oa_symbol(
                symbols,
                &text(t.get("tradingSymbol")),
                &text(t.get("exchange")),
            );
            Trade {
                order_id: text(t.get("orderNumber")),
                trade_id: text(t.get("tradeNumber")),
                symbol,
                exchange,
                product: text(t.get("productCode")),
                side: text(t.get("transactionType")),
                quantity: int(t.get("filledQuantity")) as i32,
                average_price: num(t.get("tradePrice")),
                trade_value: num(t.get("orderValue")),
                timestamp: text(t.get("tradeTime")),
            }
        })
        .collect()
}

/// Signed net quantity: Samco reports it positive with the direction in
/// `transactionType`.
pub fn signed_net(p: &Value) -> i64 {
    let q = int(p.get("netQuantity"));
    if text(p.get("transactionType")) == "SELL" && q > 0 {
        -q
    } else {
        q
    }
}

/// web `map_position_data` + `transform_positions_data`.
pub fn positions(v: &Value, symbols: &SymbolResolver) -> Vec<Position> {
    rows(v, "positionDetails")
        .iter()
        .filter(|p| p.is_object())
        .map(|p| position(p, symbols))
        .collect()
}

pub fn position(p: &Value, symbols: &SymbolResolver) -> Position {
    let (symbol, exchange) = oa_symbol(
        symbols,
        &text(p.get("tradingSymbol")),
        &text(p.get("exchange")),
    );
    let avg = if text(p.get("transactionType")) == "SELL" {
        num(p.get("averageSellPrice"))
    } else {
        num(p.get("averageBuyPrice"))
    };
    let realized = num(p.get("realizedGainAndLoss"));
    let unrealized = num(p.get("unrealizedGainAndLoss"));
    Position {
        symbol,
        exchange,
        product: text(p.get("productCode")),
        quantity: signed_net(p) as i32,
        overnight_quantity: 0,
        average_price: round2(avg),
        ltp: round2(num(p.get("lastTradedPrice"))),
        pnl: round2(realized + unrealized),
        realized_pnl: realized,
        unrealized_pnl: unrealized,
        buy_quantity: int(p.get("totalBuyQuantity")) as i32,
        buy_value: num(p.get("totalBuyValue")),
        sell_quantity: int(p.get("totalSellQuantity")) as i32,
        sell_value: num(p.get("totalSellValue")),
    }
}

/// web `transform_holdings_data`: pnl% = pnl / (holdingsValue - pnl).
pub fn holding_pnl_percent(pnl: f64, holdings_value: f64) -> f64 {
    if holdings_value > 0.0 && holdings_value - pnl != 0.0 {
        round2(pnl / (holdings_value - pnl) * 100.0)
    } else {
        0.0
    }
}

/// web `map_portfolio_data` + `transform_holdings_data`.
pub fn holdings(v: &Value, symbols: &SymbolResolver) -> Vec<Holding> {
    if !super::is_success(v) {
        return Vec::new();
    }
    rows(v, "holdingDetails")
        .iter()
        .filter(|h| h.is_object())
        .map(|h| {
            let raw_ex = {
                let e = text(h.get("exchange"));
                if e.is_empty() {
                    "NSE".to_string()
                } else {
                    e
                }
            };
            let (symbol, exchange) = oa_symbol(symbols, &text(h.get("tradingSymbol")), &raw_ex);
            let quantity = int(h.get("holdingsQuantity"));
            let pnl = num(h.get("totalGainAndLoss"));
            let value = num(h.get("holdingsValue"));
            let isin = text(h.get("isin"));
            Holding {
                symbol,
                exchange,
                product: "CNC".into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: quantity as i32,
                t1_quantity: 0,
                average_price: num(h.get("averagePrice")),
                ltp: num(h.get("lastTradedPrice")),
                close_price: num(h.get("previousClose")),
                pnl: round2(pnl),
                pnl_percentage: holding_pnl_percent(pnl, value),
                current_value: value,
            }
        })
        .collect()
}

/// web `calculate_portfolio_statistics` from `holdingSummary`.
pub fn portfolio_stats(v: &Value) -> PortfolioStats {
    let Some(s) = v.get("holdingSummary").filter(|s| s.is_object()) else {
        return PortfolioStats::default();
    };
    let value = num(s.get("portfolioValue"));
    let pnl = num(s.get("totalGainAndLossAmount"));
    let inv = value - pnl;
    PortfolioStats {
        totalholdingvalue: round2(value),
        totalinvvalue: round2(inv),
        totalprofitandloss: round2(pnl),
        totalpnlpercentage: if inv != 0.0 {
            round2(pnl / inv * 100.0)
        } else {
            0.0
        },
    }
}

/// web `get_margin_data`: the equity segment carries the fund pool.
pub fn funds(v: &Value) -> Funds {
    let e = v.get("equityLimit").cloned().unwrap_or(Value::Null);
    let available = round2(num(e.get("netAvailableMargin")));
    let used = round2(num(e.get("marginUsed")));
    Funds {
        available_cash: available,
        used_margin: used,
        collateral: round2(num(e.get("collateralMarginAgainstShares"))),
        utilised_debits: used,
        m2m_realized: 0.0,
        m2m_unrealized: 0.0,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Margin
// ---------------------------------------------------------------------------

/// spanMargin works for derivatives only (web `valid_exchanges`).
pub const MARGIN_EXCHANGES: &[&str] = &["NFO", "MCX", "CDS", "BFO", "MFO"];

/// web `transform_margin_position`; `None` skips the leg.
pub fn margin_leg(leg: &MarginLeg, brsymbol: Option<&str>) -> Option<Value> {
    if leg.quantity == 0 || !MARGIN_EXCHANGES.contains(&leg.key.exchange.as_str()) {
        return None;
    }
    let br = brsymbol.filter(|s| !s.is_empty())?;
    Some(json!({
        "exchange": leg.key.exchange,
        "tradingSymbol": br,
        "qty": leg.quantity.to_string(),
        "productType": leg.product.as_str(),
        "orderType": "L",
        "transactionType": leg.action.as_str(),
        "price": if leg.price > 0.0 { py_float(leg.price) } else { "0".to_string() },
    }))
}

/// web `parse_margin_response`.
pub fn parse_margin(v: &Value) -> std::result::Result<MarginResult, String> {
    if !super::is_success(v) {
        let m = text(v.get("statusMessage"));
        return Err(if m.is_empty() {
            "Failed to calculate margin".into()
        } else {
            m
        });
    }
    let or = |a: f64, b: f64| if a != 0.0 { a } else { b };
    match v.get("spanDetails").filter(|s| truthy(Some(s))) {
        Some(s) => Ok(MarginResult {
            total_margin_required: or(num(s.get("totalMargin")), num(s.get("totalRequirement"))),
            span_margin: or(num(s.get("marginRequired")), num(s.get("spanRequirement"))),
            exposure_margin: num(s.get("exposureMargin")),
        }),
        None => Ok(MarginResult {
            total_margin_required: or(num(v.get("totalMargin")), num(v.get("marginRequired"))),
            span_margin: num(v.get("marginRequired")),
            exposure_margin: num(v.get("exposureMargin")),
        }),
    }
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

/// `/quote/getQuote` `quoteDetails`.
pub fn quote_from_details(key: &QuoteKey, q: &Value) -> Quote {
    let first = |k: &str| {
        q.get(k)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map(|x| num(x.get("price")))
            .unwrap_or(0.0)
    };
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: num(q.get("lastTradedPrice")),
        open: num(q.get("openValue")),
        high: num(q.get("highValue")),
        low: num(q.get("lowValue")),
        close: num(q.get("previousClose")),
        volume: int(q.get("totalTradedVolume")),
        bid: first("bestBids"),
        ask: first("bestAsks"),
        oi: int(q.get("openInterest")),
        ..Default::default()
    };
    derive_change(&mut out);
    out
}

/// `/quote/indexQuote` `indexDetails[0]` (no bid/ask, no OI).
pub fn quote_from_index(key: &QuoteKey, q: &Value) -> Quote {
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: num(q.get("spotPrice")),
        open: num(q.get("openValue")),
        high: num(q.get("highValue")),
        low: num(q.get("lowValue")),
        close: num(q.get("closeValue")),
        volume: int(q.get("totalTradedVolume")),
        ..Default::default()
    };
    derive_change(&mut out);
    out
}

/// One `/quote/multiQuote` entry.
pub fn quote_from_multi(key: &QuoteKey, q: &Value) -> Quote {
    let mut out = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bid: num(q.get("bidPrice")),
        ask: num(q.get("askPrice")),
        bid_qty: int(q.get("bidSize")),
        ask_qty: int(q.get("askSize")),
        open: num(q.get("open")),
        high: num(q.get("high")),
        low: num(q.get("low")),
        ltp: num(q.get("lastTradePrice")),
        close: num(q.get("previousClose")),
        volume: int(q.get("totalTradeVolume")),
        oi: int(q.get("openInterest")),
        ..Default::default()
    };
    derive_change(&mut out);
    out
}

fn derive_change(q: &mut Quote) {
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
}

/// Join multiQuote entries back to requests (web `_process_multiquotes_batch`):
/// by token (`<scripCode>_<seg>`, always echoed as `symbol`), then by
/// `exchange:tradingSymbol` / `exchange:symbolName`.
pub fn match_multiquote<'a>(
    entries: &'a [Value],
    token: &str,
    api_exchange: &str,
    oa_exchange: &str,
    brsymbol: &str,
) -> Option<&'a Value> {
    if !token.is_empty() {
        if let Some(q) = entries.iter().find(|q| text(q.get("symbol")) == token) {
            return Some(q);
        }
    }
    for ex in [api_exchange, oa_exchange] {
        if let Some(q) = entries
            .iter()
            .find(|q| text(q.get("exchange")) == ex && text(q.get("tradingSymbol")) == brsymbol)
        {
            return Some(q);
        }
        if let Some(q) = entries
            .iter()
            .find(|q| text(q.get("exchange")) == ex && text(q.get("symbolName")) == brsymbol)
        {
            return Some(q);
        }
    }
    None
}

/// `/marketDepth` `MarketDepthDetails.marketDepth` -> five levels a side,
/// plus total buy / sell quantities.
pub fn depth_levels(d: &Value) -> (Vec<DepthLevel>, Vec<DepthLevel>, i64, i64) {
    let side = |key: &str, pk: &str, qk: &str| {
        let src = d.get(key).and_then(Value::as_array);
        (0..5)
            .map(|i| {
                src.and_then(|a| a.get(i))
                    .map(|l| DepthLevel {
                        price: num(l.get(pk)),
                        quantity: int(l.get(qk)),
                        orders: 0,
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
    };
    (
        side("bestFiveBid", "bidPrice", "bidSize"),
        side("bestFiveAsk", "askPrice", "askSize"),
        int(d.get("tBuyQty")),
        int(d.get("tSellQty")),
    )
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn candle_fields(c: &Value, timestamp: i64) -> Candle {
    Candle {
        timestamp,
        open: num(c.get("open")),
        high: num(c.get("high")),
        low: num(c.get("low")),
        close: num(c.get("close")),
        volume: num(c.get("volume")) as i64,
        oi: int(c.get("oi")),
    }
}

/// Daily candles: `date` normalised to midnight, epoch of that UTC midnight
/// (web `_get_historical_data`).
pub fn daily_candles(rows: &[Value]) -> Vec<Candle> {
    let out = rows
        .iter()
        .filter_map(|c| {
            let d = text(c.get("date").or_else(|| c.get("timestamp")));
            let day = chrono::NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok()?;
            let ts = day.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
            Some(candle_fields(c, ts))
        })
        .collect();
    crate::brokers::common::history::sort_dedupe(out)
}

/// Intraday candles: `dateTime` (`2019-11-11 10:01:00`) is IST.
pub fn intraday_candles(rows: &[Value]) -> Vec<Candle> {
    let out = rows
        .iter()
        .filter_map(|c| {
            let d = text(c.get("dateTime").or_else(|| c.get("timestamp")));
            let ts = parse_ist(&d)?;
            Some(candle_fields(c, ts))
        })
        .collect();
    crate::brokers::common::history::sort_dedupe(out)
}

/// `YYYY-MM-DD HH:MM[:SS[.f]]` in IST -> epoch seconds.
pub fn parse_ist(s: &str) -> Option<i64> {
    use chrono::TimeZone;
    let s = s.trim().replace('T', " ");
    let naive = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|f| chrono::NaiveDateTime::parse_from_str(&s, f).ok())?;
    chrono_tz::Asia::Kolkata
        .from_local_datetime(&naive)
        .single()
        .map(|d| d.timestamp())
}

/// The trading symbol for an instrument row (`SymToken::br_symbol`).
pub fn br(row: &SymToken) -> &str {
    row.br_symbol()
}
