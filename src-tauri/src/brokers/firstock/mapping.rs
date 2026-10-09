//! Firstock <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`). Noren enum maps are shared with the family.

use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price, py_round};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::mapping::{
    book_pricetype, book_product, escape_tsym, f, i, num, oa_symbol, pricetype_code, product_code,
    side, text,
};
use crate::brokers::types::*;
use serde_json::{json, Value};

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Firstock order status (web map): `TRIGGER_PENDING` stays distinct as
/// `trigger_pending`, unlike the Noren siblings.
pub fn map_status(raw: &str) -> String {
    let s = raw.trim().to_ascii_uppercase();
    match s.as_str() {
        "COMPLETE" => "complete".into(),
        "OPEN" | "PENDING" => "open".into(),
        "REJECTED" => "rejected".into(),
        "CANCELED" | "CANCELLED" => "cancelled".into(),
        "TRIGGER PENDING" | "TRIGGER_PENDING" => "trigger_pending".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// `(priceType, price, mkt_protection)` with client-side MPP: MARKET and
/// SL-M convert when an LTP is known; otherwise the order goes as sent and
/// Firstock's server-side protection applies. Tick size comes from the
/// master (`/getQuote` has none).
pub fn mpp(
    symbol: &str,
    action: Option<Action>,
    pricetype: PriceType,
    price: f64,
    ltp: Option<f64>,
    tick: f64,
    on_modify: bool,
) -> (&'static str, String, &'static str) {
    let base = pricetype_code(pricetype);
    if !matches!(pricetype, PriceType::Market | PriceType::SlM) {
        return (base, num(price), "0");
    }
    let target = if pricetype == PriceType::Market {
        "LMT"
    } else {
        "SL-LMT"
    };
    match (action, ltp.filter(|l| *l > 0.0)) {
        (Some(a), Some(l)) => {
            let t = (tick > 0.0).then_some(tick);
            let p = protected_price(l, a, instrument_type_from_symbol(symbol), t);
            (target, num(p), "0")
        }
        // Modify without a usable quote defers to server-side protection.
        _ if on_modify => (base, num(price), "1"),
        _ => (base, num(price), "0"),
    }
}

/// `/placeOrder` body (without `jKey`/`userId`).
pub fn place_body(o: &ResolvedOrder, ltp: Option<f64>) -> Value {
    let (pt, prc, mp) = mpp(
        &o.symbol,
        Some(o.action),
        o.pricetype,
        o.price,
        ltp,
        o.instrument.tick_size,
        false,
    );
    json!({
        "exchange": o.exchange.as_str(),
        "tradingSymbol": escape_tsym(o.brsymbol()),
        "quantity": o.quantity.to_string(),
        "price": prc,
        "triggerPrice": num(o.trigger_price),
        "product": product_code(o.product),
        "transactionType": if o.action == Action::Buy { "B" } else { "S" },
        "priceType": pt,
        "retention": "DAY",
        "mkt_protection": mp,
        "remarks": "Place Order",
    })
}

/// `/modifyOrder` body (without `jKey`/`userId`).
pub fn modify_body(m: &ResolvedModify, ltp: Option<f64>) -> Value {
    let (pt, prc, mp) = mpp(
        &m.symbol,
        Some(m.action),
        m.pricetype,
        m.price,
        ltp,
        m.instrument.tick_size,
        true,
    );
    json!({
        "exchange": m.exchange.as_str(),
        "orderNumber": m.order_id,
        "priceType": pt,
        "price": prc,
        "quantity": m.quantity.to_string(),
        "tradingSymbol": escape_tsym(m.brsymbol()),
        "triggerPrice": num(m.trigger_price),
        "retention": "DAY",
        "mkt_protection": mp,
        "product": product_code(m.product),
    })
}

pub fn map_order(o: &Value, symbols: &SymbolResolver) -> Order {
    let exch = text(o, "exchange");
    let qty = i(o, "quantity");
    let filled = i(o, "fillShares").max(i(o, "filledQuantity"));
    let status = map_status(&text(o, "status"));
    let trig = f(o, "triggerPrice");
    Order {
        order_tag: None,
        order_id: text(o, "orderNumber"),
        exchange_order_id: None,
        symbol: oa_symbol(symbols, &exch, &text(o, "token"), &text(o, "tradingSymbol")),
        exchange: exch.clone(),
        side: side(&text(o, "transactionType")),
        quantity: clamp_i32(qty),
        filled_quantity: clamp_i32(filled),
        pending_quantity: clamp_i32((qty - filled).max(0)),
        price: f(o, "price"),
        trigger_price: trig,
        average_price: f(o, "averagePrice"),
        order_type: book_pricetype(&text(o, "priceType").replace('_', "-")),
        product: book_product(&exch, &text(o, "product")),
        rejection_reason: if status == "rejected" {
            Some(text(o, "rejectReason")).filter(|s| !s.is_empty())
        } else {
            None
        },
        status,
        validity: "DAY".into(),
        order_timestamp: text(o, "orderTime"),
        exchange_timestamp: None,
    }
}

pub fn map_trade(t: &Value, symbols: &SymbolResolver) -> Trade {
    let exch = text(t, "exchange");
    let qty = i(t, "fillQuantity");
    let price = f(t, "fillPrice");
    let order_id = text(t, "orderNumber");
    let fill = text(t, "fillId");
    Trade {
        order_tag: None,
        trade_id: if fill.is_empty() {
            order_id.clone()
        } else {
            fill
        },
        order_id,
        symbol: oa_symbol(symbols, &exch, &text(t, "token"), &text(t, "tradingSymbol")),
        exchange: exch.clone(),
        product: book_product(&exch, &text(t, "product")),
        side: side(&text(t, "transactionType")),
        quantity: clamp_i32(qty),
        average_price: price,
        trade_value: py_round(qty as f64 * price, 2),
        timestamp: text(t, "fillTime"),
    }
}

pub fn map_position(p: &Value, symbols: &SymbolResolver) -> Position {
    let exch = text(p, "exchange");
    let realized = f(p, "RealizedPNL");
    Position {
        symbol: oa_symbol(symbols, &exch, &text(p, "token"), &text(p, "tradingSymbol")),
        exchange: exch.clone(),
        product: book_product(&exch, &text(p, "product")),
        quantity: clamp_i32(i(p, "netQuantity")),
        overnight_quantity: 0,
        average_price: f(p, "netAveragePrice"),
        ltp: 0.0,
        // The web reports realised P&L only (no LTP in this answer).
        pnl: realized,
        realized_pnl: realized,
        unrealized_pnl: f(p, "unrealizedMTOM"),
        buy_quantity: clamp_i32(i(p, "dayBuyQuantity")),
        buy_value: f(p, "dayBuyAmount"),
        sell_quantity: clamp_i32(i(p, "daySellQuantity")),
        sell_value: f(p, "daySellAmount"),
    }
}

/// Holdings: the V1 answer names the scrip (NSE leg of
/// `exchangeTradingSymbol`, or top-level fields) but carries no quantity
/// or price on most accounts; a quantity is read when present.
pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    let mut out = Vec::new();
    for h in rows {
        let leg = h
            .get("exchangeTradingSymbol")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find(|l| text(l, "exchange") == "NSE").cloned())
            .unwrap_or_else(|| h.clone());
        let tsym = text(&leg, "tradingSymbol");
        if tsym.is_empty() {
            continue;
        }
        let exch = {
            let e = text(&leg, "exchange");
            if e.is_empty() {
                "NSE".to_string()
            } else {
                e
            }
        };
        let qty = ["holdQuantity", "quantity", "holdqty"]
            .iter()
            .map(|k| i(h, k))
            .find(|q| *q != 0)
            .unwrap_or(0);
        let avg = f(h, "averagePrice");
        let ltp = f(&leg, "ltp");
        let pnl = if ltp > 0.0 && avg > 0.0 {
            (ltp - avg) * qty as f64
        } else {
            0.0
        };
        out.push(Holding {
            symbol: {
                let s = oa_symbol(symbols, &exch, &text(&leg, "token"), &tsym);
                if s == tsym {
                    tsym.replace("-EQ", "")
                } else {
                    s
                }
            },
            exchange: exch,
            product: "CNC".into(),
            isin: None,
            quantity: clamp_i32(qty),
            t1_quantity: 0,
            average_price: avg,
            ltp,
            close_price: 0.0,
            pnl: py_round(pnl, 2),
            pnl_percentage: if avg > 0.0 && ltp > 0.0 {
                py_round((ltp - avg) / avg * 100.0, 2)
            } else {
                0.0
            },
            current_value: ltp * qty as f64,
        });
    }
    out
}

/// `/limit` -> funds (M2M is not reported).
pub fn funds_from(d: &Value) -> Funds {
    let cash = f(d, "cash");
    let payin = f(d, "payin");
    let used = f(d, "marginused");
    Funds {
        available_cash: cash + payin - used,
        used_margin: used,
        total_margin: cash + payin,
        opening_balance: cash,
        payin,
        collateral: f(d, "brkcollamt"),
        utilised_debits: used,
        ..Default::default()
    }
}

/// `/basketMargin` body: first leg flat, the rest in `BasketList_Params`.
pub fn basket_body(legs: Vec<Value>) -> Option<Value> {
    let mut it = legs.into_iter();
    let mut first = it.next()?;
    first["BasketList_Params"] = Value::Array(it.collect());
    Some(first)
}
