//! OpenAlgo <-> SmartAPI vocabulary (web `mapping/transform_data.py`,
//! `mapping/order_data.py`).

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::master_contract::format_strike;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::lower_status;
use crate::brokers::types::*;
use serde::Deserialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// OpenAlgo -> Angel (transform_data.py)
// ---------------------------------------------------------------------------

/// web `map_variety`: SL / SL-M are `STOPLOSS`, everything else `NORMAL`.
pub fn map_variety(pricetype: &str) -> &'static str {
    match pricetype {
        "SL" | "SL-M" => "STOPLOSS",
        _ => "NORMAL",
    }
}

/// web `map_order_type` (default MARKET).
pub fn map_order_type(pricetype: &str) -> &'static str {
    match pricetype {
        "LIMIT" => "LIMIT",
        "SL" => "STOPLOSS_LIMIT",
        "SL-M" => "STOPLOSS_MARKET",
        _ => "MARKET",
    }
}

/// web `map_product_type` (default INTRADAY).
pub fn map_product_type(product: &str) -> &'static str {
    match product {
        "CNC" => "DELIVERY",
        "NRML" => "CARRYFORWARD",
        _ => "INTRADAY",
    }
}

/// web `reverse_map_product_type` (None for unknown).
pub fn reverse_map_product_type(producttype: &str) -> Option<&'static str> {
    match producttype {
        "DELIVERY" => Some("CNC"),
        "CARRYFORWARD" => Some("NRML"),
        "INTRADAY" => Some("MIS"),
        _ => None,
    }
}

/// Number as SmartAPI wants it in a JSON string (`"0"`, `"1500.5"`).
pub fn num(v: f64) -> String {
    format_strike(v)
}

/// A fresh order tag, `oa` + 16 hex digits (web `place_order_api`, #2176).
/// Angel shows `ordertag` in the order book, so it identifies this order when
/// the placement answer is lost or unreadable.
pub fn new_ordertag() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("oa{}", &hex[..16])
}

/// `placeOrder` body (web `transform_data` + the payload in
/// `place_order_api`). `triggerprice` and `stoploss` both carry the trigger,
/// always as a string, never null; `ordertag` is the per-order tag.
pub fn place_order_body(o: &ResolvedOrder, ordertag: &str) -> Value {
    let pt = o.pricetype.as_str();
    json!({
        "variety": map_variety(pt),
        "tradingsymbol": o.brsymbol(),
        "symboltoken": o.token(),
        "transactiontype": o.action.as_str().to_ascii_uppercase(),
        "exchange": o.exchange.as_str(),
        "ordertype": map_order_type(pt),
        "producttype": map_product_type(o.product.as_str()),
        "duration": "DAY",
        "price": num(o.price),
        "triggerprice": num(o.trigger_price),
        "squareoff": "0",
        "stoploss": num(o.trigger_price),
        "quantity": o.quantity.to_string(),
        "ordertag": ordertag,
    })
}

/// `modifyOrder` body (web `transform_modify_order_data`).
pub fn modify_order_body(m: &ResolvedModify) -> Value {
    let pt = m.pricetype.as_str();
    json!({
        "variety": map_variety(pt),
        "orderid": m.order_id,
        "ordertype": map_order_type(pt),
        "producttype": map_product_type(m.product.as_str()),
        "duration": "DAY",
        "price": num(m.price),
        "quantity": m.quantity.to_string(),
        "tradingsymbol": m.brsymbol(),
        "symboltoken": m.token(),
        "exchange": m.exchange.as_str(),
        "disclosedquantity": m.disclosed_quantity.to_string(),
        "stoploss": num(m.trigger_price),
    })
}

/// `cancelOrder` body: always `NORMAL` variety, as on the web.
pub fn cancel_order_body(order_id: &str) -> Value {
    json!({"variety": "NORMAL", "orderid": order_id})
}

// ---------------------------------------------------------------------------
// Angel -> OpenAlgo (order_data.py)
// ---------------------------------------------------------------------------

/// web `map_order_data` product rule: cash DELIVERY -> CNC, INTRADAY -> MIS,
/// derivatives CARRYFORWARD -> NRML, anything else as sent.
pub fn oa_product(exchange: &str, producttype: &str) -> String {
    match (exchange, producttype) {
        ("NSE" | "BSE", "DELIVERY") => "CNC".into(),
        (_, "INTRADAY") => "MIS".into(),
        ("NFO" | "MCX" | "BFO" | "CDS", "CARRYFORWARD") => "NRML".into(),
        _ => producttype.to_string(),
    }
}

/// web `transform_order_data`: STOPLOSS_LIMIT -> SL, STOPLOSS_MARKET -> SL-M.
pub fn oa_pricetype(ordertype: &str) -> String {
    match ordertype {
        "STOPLOSS_LIMIT" => "SL".into(),
        "STOPLOSS_MARKET" => "SL-M".into(),
        other => other.to_string(),
    }
}

/// OpenAlgo symbol of a book row: by token first (web `get_symbol`), then by
/// broker symbol (`get_oa_symbol`), else the raw tradingsymbol.
pub fn oa_symbol(symbols: &SymbolResolver, token: &str, brsymbol: &str, exchange: &str) -> String {
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(exchange, token) {
            return r.symbol;
        }
    }
    symbols.oa_symbol_or_raw(brsymbol, exchange)
}

fn clamp(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn non_empty(s: String) -> Option<String> {
    (!s.trim().is_empty()).then_some(s)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub orderid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchangeorderid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transactiontype: String,
    #[serde(deserialize_with = "string_lenient")]
    pub variety: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub filledshares: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub unfilledshares: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub triggerprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub averageprice: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub ordertype: String,
    #[serde(deserialize_with = "string_lenient")]
    pub producttype: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub orderstatus: String,
    #[serde(deserialize_with = "string_lenient")]
    pub duration: String,
    #[serde(deserialize_with = "string_lenient")]
    pub updatetime: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchtime: String,
    #[serde(deserialize_with = "string_lenient")]
    pub text: String,
    #[serde(deserialize_with = "string_lenient")]
    pub ordertag: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelTrade {
    #[serde(deserialize_with = "string_lenient")]
    pub orderid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub fillid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub producttype: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transactiontype: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub fillsize: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub fillprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub tradevalue: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub filltime: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelPosition {
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub producttype: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub netqty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cfbuyqty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub cfsellqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub avgnetprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub realised: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub unrealised: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub buyqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub buyamount: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub sellqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub sellamount: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelHolding {
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub isin: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub t1quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub averageprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub profitandloss: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub pnlpercentage: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelTotalHolding {
    #[serde(deserialize_with = "f64_lenient")]
    pub totalholdingvalue: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub totalinvvalue: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub totalprofitandloss: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub totalpnlpercentage: f64,
}

/// `getAllHolding` data; `holdings` is null for an empty demat.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelPortfolio {
    pub holdings: Option<Vec<AngelHolding>>,
    pub totalholding: Option<AngelTotalHolding>,
}

impl AngelPortfolio {
    /// web `calculate_portfolio_statistics`: Angel's own totals (zeros when
    /// `totalholding` is null).
    pub fn stats(&self) -> PortfolioStats {
        let t = self.totalholding.clone().unwrap_or_default();
        PortfolioStats {
            totalholdingvalue: t.totalholdingvalue,
            totalinvvalue: t.totalinvvalue,
            totalprofitandloss: t.totalprofitandloss,
            totalpnlpercentage: t.totalpnlpercentage,
        }
    }
}

/// Order book rows in OpenAlgo vocabulary. Statuses are Angel's own
/// lowercase strings (`open`, `trigger pending`, `complete`, ...).
pub fn map_orders(rows: Vec<AngelOrder>, symbols: &SymbolResolver) -> Vec<Order> {
    rows.into_iter()
        .map(|o| {
            let status = if o.status.is_empty() {
                &o.orderstatus
            } else {
                &o.status
            };
            Order {
                order_tag: None,
                symbol: oa_symbol(symbols, &o.symboltoken, &o.tradingsymbol, &o.exchange),
                order_type: oa_pricetype(&o.ordertype),
                product: oa_product(&o.exchange, &o.producttype),
                status: lower_status(status),
                exchange_order_id: non_empty(o.exchangeorderid),
                rejection_reason: non_empty(o.text),
                exchange_timestamp: non_empty(o.exchtime),
                order_id: o.orderid,
                exchange: o.exchange,
                side: o.transactiontype.to_ascii_uppercase(),
                quantity: clamp(o.quantity),
                filled_quantity: clamp(o.filledshares),
                pending_quantity: clamp(o.unfilledshares),
                price: o.price,
                trigger_price: o.triggerprice,
                average_price: o.averageprice,
                validity: o.duration,
                order_timestamp: o.updatetime,
            }
        })
        .collect()
}

/// Trade book rows (web `map_trade_data` + `transform_tradebook_data`):
/// symbol through `get_oa_symbol(tradingsymbol, exchange)`, `fillprice` as
/// the average, `filltime` as the timestamp.
pub fn map_trades(rows: Vec<AngelTrade>, symbols: &SymbolResolver) -> Vec<Trade> {
    rows.into_iter()
        .map(|t| Trade {
            order_tag: None,
            symbol: symbols.oa_symbol_or_raw(&t.tradingsymbol, &t.exchange),
            product: oa_product(&t.exchange, &t.producttype),
            // The trade book carries `fillsize`; `quantity` is a fallback.
            quantity: clamp(if t.fillsize != 0 {
                t.fillsize
            } else {
                t.quantity
            }),
            order_id: t.orderid,
            trade_id: t.fillid,
            exchange: t.exchange,
            side: t.transactiontype.to_ascii_uppercase(),
            average_price: t.fillprice,
            trade_value: t.tradevalue,
            timestamp: t.filltime,
        })
        .collect()
}

/// Net positions (web `map_position_data` + `transform_positions_data`).
pub fn map_positions(rows: Vec<AngelPosition>, symbols: &SymbolResolver) -> Vec<Position> {
    rows.into_iter()
        .map(|p| Position {
            symbol: oa_symbol(symbols, &p.symboltoken, &p.tradingsymbol, &p.exchange),
            product: oa_product(&p.exchange, &p.producttype),
            exchange: p.exchange,
            quantity: clamp(p.netqty),
            overnight_quantity: clamp(p.cfbuyqty - p.cfsellqty),
            average_price: p.avgnetprice,
            ltp: p.ltp,
            pnl: p.pnl,
            realized_pnl: p.realised,
            unrealized_pnl: p.unrealised,
            buy_quantity: clamp(p.buyqty),
            buy_value: p.buyamount,
            sell_quantity: clamp(p.sellqty),
            sell_value: p.sellamount,
        })
        .collect()
}

/// Holdings (web `map_portfolio_data` + `transform_holdings_data`): symbol
/// through `get_oa_symbol`, product always CNC.
pub fn map_holdings(p: AngelPortfolio, symbols: &SymbolResolver) -> Vec<Holding> {
    p.holdings
        .unwrap_or_default()
        .into_iter()
        .map(|h| {
            if !h.product.is_empty() && h.product != "DELIVERY" {
                tracing::info!(
                    "Angel One holding with product {}, mapped to CNC",
                    h.product
                );
            }
            Holding {
                symbol: symbols.oa_symbol_or_raw(&h.tradingsymbol, &h.exchange),
                exchange: h.exchange,
                product: "CNC".into(),
                isin: non_empty(h.isin),
                quantity: clamp(h.quantity),
                t1_quantity: clamp(h.t1quantity),
                average_price: h.averageprice,
                ltp: h.ltp,
                close_price: h.close,
                pnl: h.profitandloss,
                pnl_percentage: h.pnlpercentage,
                current_value: h.quantity as f64 * h.ltp,
            }
        })
        .collect()
}
