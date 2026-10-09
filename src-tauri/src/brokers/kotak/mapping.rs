//! Kotak <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`).
//!
//! Every jData value is a string; zero prices are `"0"` (Kotak rejects
//! `"0.0"`); `pc` is the raw OpenAlgo product; `ig` is required on place
//! and unique per order (`openalgo-<uuid4>`). SL-M is never sent: it becomes SL with a protective limit one
//! Kotak MPP band past the trigger, snapped to the tick away from it.

use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::mpp::{instrument_type_from_symbol, py_round};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// Prefix of the order tag `ig`, echoed back as `GuiOrdId`.
///
/// Kotak rejects a blank `ig`, so it is always set. It is also a client order
/// id: a value already used on the account is rejected with "Client OrderID
/// already exists", so a fixed tag lets only the first order of the day
/// through (web #2177). Every Place Order therefore gets a fresh
/// `<prefix>-<uuid4>`, the shape Kotak's own apps send. Modify takes no `ig`.
pub const ORDER_TAG_PREFIX: &str = "openalgo";
const MAX_TAG_PREFIX: usize = 15;

/// A fresh, unique order tag (web `_order_tag`). A caller-supplied prefix is
/// trimmed and capped at 15 characters so the tag stays within the
/// 52-character ids Kotak itself issues; a blank one falls back to `openalgo`.
pub fn order_tag(prefix: Option<&str>) -> String {
    let p = prefix
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(ORDER_TAG_PREFIX);
    let p: String = p.chars().take(MAX_TAG_PREFIX).collect();
    format!("{p}-{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// Static maps
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> Kotak segment (web `reverse_map_exchange`; no index
/// segments for orders).
pub fn reverse_map_exchange(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" => "nse_cm",
        "BSE" => "bse_cm",
        "CDS" => "cde_fo",
        "NFO" => "nse_fo",
        "BFO" => "bse_fo",
        "BCD" => "bcs_fo",
        "MCX" => "mcx_fo",
        _ => return None,
    })
}

/// Kotak segment -> OpenAlgo exchange (web `map_exchange`).
pub fn map_exchange(segment: &str) -> Option<&'static str> {
    Some(match segment {
        "nse_cm" => "NSE",
        "bse_cm" => "BSE",
        "cde_fo" => "CDS",
        "nse_fo" => "NFO",
        "bse_fo" => "BFO",
        "bcs_fo" => "BCD",
        "mcx_fo" => "MCX",
        _ => return None,
    })
}

/// OpenAlgo exchange as a Kotak row reports it (unknown segments as is).
pub fn row_exchange(segment: &str) -> String {
    map_exchange(segment)
        .map(str::to_string)
        .unwrap_or_else(|| segment.to_string())
}

/// OpenAlgo price type -> Kotak `pt` (web `map_order_type`).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "L",
        PriceType::Sl => "SL",
        PriceType::SlM => "SL-M",
    }
}

/// Kotak `prcTp` -> OpenAlgo price type.
pub fn reverse_order_type(t: &str) -> String {
    match t.trim().to_ascii_uppercase().as_str() {
        "MKT" | "MARKET" => "MARKET".into(),
        "L" | "LMT" | "LIMIT" => "LIMIT".into(),
        "SL" => "SL".into(),
        "SL-M" => "SL-M".into(),
        other => other.to_string(),
    }
}

/// `B`/`S` -> `BUY`/`SELL`.
pub fn map_action(t: &str) -> String {
    match t.trim() {
        "B" => "BUY".into(),
        "S" => "SELL".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// Kotak `ordSt` (lower-case free text) -> OpenAlgo status. The REST book
/// only collapses `trigger pending`; the order feed's fuller map is applied
/// to the in-flight OMS states so every consumer reads one vocabulary.
pub fn map_status(s: &str) -> String {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "complete" => "complete".into(),
        "rejected" => "rejected".into(),
        "cancelled" | "canceled" | "cancelled after market order" => "cancelled".into(),
        "open"
        | "trigger pending"
        | "put order req received"
        | "validation pending"
        | "open pending"
        | "modified"
        | "modify validation pending"
        | "modify pending"
        | "cancel pending"
        | "after market order req received"
        | "modify after market order req received" => "open".into(),
        "" => "open".into(),
        _ => l,
    }
}

// ---------------------------------------------------------------------------
// Numbers as Kotak wants them
// ---------------------------------------------------------------------------

/// Python `str(float)`: `100.0`, `100.5`, `0.05`.
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// web `_fmt_price`: `"0"` for zero, else the stringified value.
pub fn fmt_price(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else {
        py_float(v)
    }
}

// ---------------------------------------------------------------------------
// SL-M protective limit (Kotak MPP grid)
// ---------------------------------------------------------------------------

/// Kotak's protection offset in rupees (web `_mpp_offset`): EQ/FUT
/// 2% / 1% / 0.5% under 100 / 500 / above; options 10% / 5% / 3% / 2% / 1%
/// under 5 / 10 / 100 / 500 / above, and an absolute 0.10 under 1.
pub fn mpp_offset(price: f64, instrument_type: &str) -> f64 {
    let slabs: &[(f64, f64)] = if matches!(instrument_type, "CE" | "PE") {
        if price < 1.0 {
            return 0.10;
        }
        &[
            (5.0, 10.0),
            (10.0, 5.0),
            (100.0, 3.0),
            (500.0, 2.0),
            (f64::INFINITY, 1.0),
        ]
    } else {
        &[(100.0, 2.0), (500.0, 1.0), (f64::INFINITY, 0.5)]
    };
    for (max, pct) in slabs {
        if price < *max {
            return price * pct / 100.0;
        }
    }
    0.0
}

fn tick_decimals(tick: f64) -> i32 {
    let s = format!("{}", tick);
    s.split_once('.').map(|(_, f)| f.len() as i32).unwrap_or(0)
}

fn snap_to_tick(value: f64, tick: f64, floor: bool) -> f64 {
    let ratio = py_round(value / tick, 6);
    let k = if floor { ratio.floor() } else { ratio.ceil() };
    py_round(k * tick, tick_decimals(tick))
}

/// web `_slm_protected_price` with Kotak's grid.
pub fn slm_protected_price(symbol: &str, action: Action, trigger: f64, tick: f64) -> Result<f64> {
    if !tick.is_finite() || tick <= 0.0 {
        return Err(AppError::Validation(format!(
            "Cannot place the SL-M order for {}: the master contract has no tick size for it. Download the master contract again, then retry.",
            symbol
        )));
    }
    let offset = mpp_offset(trigger, instrument_type_from_symbol(symbol));
    match action {
        Action::Sell => {
            let raw = (trigger - offset).min(trigger - tick);
            let limit = snap_to_tick(raw, tick, true);
            if limit <= 0.0 {
                return Err(AppError::Validation(format!(
                    "The SL-M trigger {} for {} is too low to place a protected stop. Use an SL order with your own limit price.",
                    trigger, symbol
                )));
            }
            Ok(limit)
        }
        Action::Buy => {
            let raw = (trigger + offset).max(trigger + tick);
            Ok(snap_to_tick(raw, tick, false))
        }
    }
}

fn apply_slm(
    m: &mut serde_json::Map<String, Value>,
    pricetype: PriceType,
    symbol: &str,
    action: Action,
    trigger: f64,
    tick: f64,
) -> Result<()> {
    if pricetype != PriceType::SlM {
        return Ok(());
    }
    if trigger <= 0.0 {
        return Err(AppError::Validation(
            "Trigger price is required and must be positive for SL-M orders".into(),
        ));
    }
    let limit = slm_protected_price(symbol, action, trigger, tick)?;
    m.insert("pt".into(), json!("SL"));
    m.insert("pr".into(), json!(fmt_price(limit)));
    Ok(())
}

fn tt(a: Action) -> &'static str {
    match a {
        Action::Buy => "B",
        Action::Sell => "S",
    }
}

/// Place-order jData (web `transform_data`).
pub fn place_order_jdata(o: &ResolvedOrder) -> Result<Value> {
    let es = reverse_map_exchange(o.exchange.as_str()).ok_or_else(|| {
        AppError::Validation(format!("Kotak does not accept orders on {}.", o.exchange))
    })?;
    let mut m = serde_json::Map::new();
    m.insert("am".into(), json!("NO"));
    m.insert("dq".into(), json!(o.disclosed_quantity.to_string()));
    m.insert("es".into(), json!(es));
    m.insert("mp".into(), json!("0"));
    m.insert("pc".into(), json!(o.product.as_str()));
    m.insert("pf".into(), json!("N"));
    m.insert("pr".into(), json!(fmt_price(o.price)));
    m.insert("pt".into(), json!(order_type(o.pricetype)));
    m.insert("qt".into(), json!(o.quantity.to_string()));
    m.insert("rt".into(), json!("DAY"));
    m.insert("tp".into(), json!(fmt_price(o.trigger_price)));
    m.insert("ts".into(), json!(o.brsymbol()));
    m.insert("tt".into(), json!(tt(o.action)));
    m.insert("ig".into(), json!(order_tag(None)));
    apply_slm(
        &mut m,
        o.pricetype,
        &o.symbol,
        o.action,
        o.trigger_price,
        o.instrument.tick_size,
    )?;
    Ok(Value::Object(m))
}

/// Modify-order jData (web `transform_modify_order_data`): no `ig`, `am`,
/// `rt` or `pf`.
pub fn modify_order_jdata(o: &ResolvedModify) -> Result<Value> {
    let es = reverse_map_exchange(o.exchange.as_str()).ok_or_else(|| {
        AppError::Validation(format!("Kotak does not accept orders on {}.", o.exchange))
    })?;
    let mut m = serde_json::Map::new();
    m.insert("tk".into(), json!(o.token()));
    m.insert("dq".into(), json!(o.disclosed_quantity.to_string()));
    m.insert("es".into(), json!(es));
    m.insert("mp".into(), json!("0"));
    m.insert("dd".into(), json!("NA"));
    m.insert("vd".into(), json!("DAY"));
    m.insert("pc".into(), json!(o.product.as_str()));
    m.insert("pr".into(), json!(fmt_price(o.price)));
    m.insert("pt".into(), json!(order_type(o.pricetype)));
    m.insert("qt".into(), json!(o.quantity.to_string()));
    m.insert("tp".into(), json!(fmt_price(o.trigger_price)));
    m.insert("ts".into(), json!(o.brsymbol()));
    m.insert("no".into(), json!(o.order_id));
    m.insert("tt".into(), json!(tt(o.action)));
    apply_slm(
        &mut m,
        o.pricetype,
        &o.symbol,
        o.action,
        o.trigger_price,
        o.instrument.tick_size,
    )?;
    Ok(Value::Object(m))
}

// ---------------------------------------------------------------------------
// Book normalisers
// ---------------------------------------------------------------------------

pub(crate) fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Kotak numeric field (strings, nulls, empties read as 0; web `_number`).
pub(crate) fn n(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(x)) => x.as_f64().unwrap_or(0.0),
        Some(Value::String(x)) => x.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn clamp_i32(v: f64) -> i32 {
    v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// web `_openalgo_symbol`: token first (`tok` is empty on trade rows), then
/// the broker trading symbol, else the raw broker symbol.
pub fn openalgo_symbol(symbols: &SymbolResolver, row: &Value, exchange: &str) -> String {
    let tok = s(row, "tok");
    if !tok.is_empty() {
        if let Some(r) = symbols.by_token(exchange, &tok) {
            return r.symbol;
        }
    }
    let br = {
        let t = s(row, "trdSym");
        if t.is_empty() {
            s(row, "sym")
        } else {
            t
        }
    };
    if !br.is_empty() {
        if let Some(sym) = symbols.oa_symbol(&br, exchange) {
            return sym;
        }
    }
    br
}

/// The `data` rows of a `stat`/`data` envelope; `Not_Ok` and `null` read as
/// no rows (web `map_order_data`).
pub fn data_rows(v: &Value) -> Vec<Value> {
    if s(v, "stat") == "Not_Ok" {
        return Vec::new();
    }
    v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `map_order_data` + `transform_order_data`.
pub fn map_orders(rows: &[Value], symbols: &SymbolResolver) -> Vec<Order> {
    rows.iter()
        .map(|o| {
            let exchange = row_exchange(&s(o, "exSeg"));
            let pricetype = reverse_order_type(&s(o, "prcTp"));
            let raw_status = s(o, "ordSt");
            let status = map_status(&raw_status);
            // The limit price for a working LIMIT/SL, else the average.
            let price = if matches!(pricetype.as_str(), "LIMIT" | "SL") && status != "complete" {
                n(o, "prc")
            } else {
                n(o, "avgPrc")
            };
            let qty = n(o, "qty");
            let filled = n(o, "fldQty");
            Order {
                order_tag: Some(s(o, "GuiOrdId")),
                order_id: s(o, "nOrdNo"),
                exchange_order_id: Some(s(o, "exOrdId")).filter(|x| !x.is_empty()),
                symbol: openalgo_symbol(symbols, o, &exchange),
                side: map_action(&s(o, "trnsTp")),
                quantity: clamp_i32(qty),
                filled_quantity: clamp_i32(filled),
                pending_quantity: clamp_i32({
                    let u = n(o, "unFldSz");
                    if u > 0.0 {
                        u
                    } else {
                        (qty - filled).max(0.0)
                    }
                }),
                price,
                trigger_price: n(o, "trgPrc"),
                average_price: n(o, "avgPrc"),
                order_type: pricetype,
                product: s(o, "prod"),
                rejection_reason: if status == "rejected" {
                    Some(s(o, "rejRsn")).filter(|x| !x.is_empty())
                } else {
                    None
                },
                status,
                validity: {
                    let v = s(o, "vldt");
                    if v.is_empty() {
                        "DAY".into()
                    } else {
                        v
                    }
                },
                order_timestamp: s(o, "ordEntTm"),
                exchange_timestamp: None,
                exchange,
            }
        })
        .collect()
}

/// `map_trade_data` + `transform_tradebook_data`.
pub fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .map(|t| {
            let exchange = row_exchange(&s(t, "exSeg"));
            let qty = n(t, "fldQty");
            let avg = n(t, "avgPrc");
            Trade {
                order_tag: Some(s(t, "GuiOrdId")),
                order_id: s(t, "nOrdNo"),
                trade_id: s(t, "flId"),
                symbol: openalgo_symbol(symbols, t, &exchange),
                product: s(t, "prod"),
                side: map_action(&s(t, "trnsTp")),
                quantity: clamp_i32(qty),
                average_price: avg,
                trade_value: qty * avg,
                timestamp: s(t, "exTm"),
                exchange,
            }
        })
        .collect()
}

/// `multiplier * (genNum/genDen) * (prcNum/prcDen)`, each term defaulting
/// to 1 (web `_price_factor`).
pub fn price_factor(p: &Value) -> f64 {
    let term = |k: &str| {
        let v = n(p, k);
        if v == 0.0 {
            1.0
        } else {
            v
        }
    };
    term("multiplier") * (term("genNum") / term("genDen")) * (term("prcNum") / term("prcDen"))
}

/// What the carried-forward leg cost (web `_carry_forward_amounts`): at
/// `upldPrc` when Kotak sends it, else Kotak's carry-forward valuation
/// (flagged).
pub fn carry_forward_amounts(p: &Value, factor: f64) -> (f64, f64, bool) {
    let cfb = n(p, "cfBuyQty");
    let cfs = n(p, "cfSellQty");
    if cfb == 0.0 && cfs == 0.0 {
        return (0.0, 0.0, false);
    }
    let up = n(p, "upldPrc");
    if up > 0.0 {
        return (cfb * up * factor, cfs * up * factor, false);
    }
    (n(p, "cfBuyAmt"), n(p, "cfSellAmt"), true)
}

/// Net quantity of a position row: day plus carried legs.
pub fn net_quantity(p: &Value) -> i64 {
    ((n(p, "flBuyQty") - n(p, "flSellQty")) + (n(p, "cfBuyQty") - n(p, "cfSellQty"))) as i64
}

fn zero_neg(v: f64) -> f64 {
    if v == 0.0 {
        0.0
    } else {
        v
    }
}

/// `transform_positions_data`: average and P&L per Kotak's documented
/// formula, LTP from `ltp` (the multiquote backfill, 0 when unknown).
pub fn map_position(p: &Value, symbols: &SymbolResolver, ltp: f64) -> Position {
    let exchange = row_exchange(&s(p, "exSeg"));
    let quantity = net_quantity(p);
    let factor = price_factor(p);
    let (cf_buy, cf_sell, _carried) = carry_forward_amounts(p, factor);
    let total_buy = cf_buy + n(p, "buyAmt");
    let total_sell = cf_sell + n(p, "sellAmt");
    let buy_qty = n(p, "flBuyQty") + n(p, "cfBuyQty");
    let sell_qty = n(p, "flSellQty") + n(p, "cfSellQty");
    let average_price = if quantity > 0 && buy_qty > 0.0 {
        py_round(total_buy / (buy_qty * factor), 2)
    } else if quantity < 0 && sell_qty > 0.0 {
        py_round(total_sell / (sell_qty * factor), 2)
    } else if quantity != 0 {
        0.0
    } else {
        n(p, "avgnetprice")
    };
    let realized = total_sell - total_buy;
    let pnl = if quantity == 0 {
        zero_neg(py_round(realized, 2))
    } else if ltp != 0.0 {
        zero_neg(py_round(realized + quantity as f64 * ltp * factor, 2))
    } else {
        0.0
    };
    Position {
        symbol: openalgo_symbol(symbols, p, &exchange),
        product: s(p, "prod"),
        quantity: clamp_i32(quantity as f64),
        overnight_quantity: clamp_i32(n(p, "cfBuyQty") - n(p, "cfSellQty")),
        average_price,
        ltp: py_round(ltp, 2),
        pnl,
        realized_pnl: if quantity == 0 {
            zero_neg(py_round(realized, 2))
        } else {
            0.0
        },
        unrealized_pnl: if quantity == 0 { 0.0 } else { pnl },
        buy_quantity: clamp_i32(buy_qty),
        buy_value: total_buy,
        sell_quantity: clamp_i32(sell_qty),
        sell_value: total_sell,
        exchange,
    }
}

/// `map_portfolio_data` + `transform_holdings_data`. The symbol is the
/// master-contract symbol for `instrumentToken` (the web keeps Kotak's
/// `displaySymbol` here, which does not resolve; books must carry
/// OpenAlgo symbols), falling back to `displaySymbol`.
pub fn map_holding(h: &Value, symbols: &SymbolResolver) -> Holding {
    let exchange = row_exchange(&s(h, "exchangeSegment"));
    let tok = s(h, "instrumentToken");
    let symbol = symbols
        .by_token(&exchange, &tok)
        .map(|r| r.symbol)
        .unwrap_or_else(|| s(h, "displaySymbol"));
    let qty = n(h, "quantity");
    let mkt = n(h, "mktValue");
    let cost = n(h, "holdingCost");
    let product = match s(h, "instrumentType").as_str() {
        "Equity" | "" => "CNC".to_string(),
        other => other.to_string(),
    };
    Holding {
        symbol,
        exchange,
        product,
        isin: Some(s(h, "isin")).filter(|x| !x.is_empty()),
        quantity: clamp_i32(qty),
        t1_quantity: 0,
        average_price: py_round(n(h, "averagePrice"), 2),
        ltp: if qty != 0.0 {
            py_round(mkt / qty, 2)
        } else {
            n(h, "closingPrice")
        },
        close_price: n(h, "closingPrice"),
        pnl: py_round(mkt - cost, 2),
        pnl_percentage: if cost != 0.0 {
            py_round((mkt - cost) / cost * 100.0, 2)
        } else {
            0.0
        },
        current_value: mkt,
    }
}

/// Open-position match (web `get_open_position`): `trdSym`, `exSeg` and
/// `prod` must all match.
pub fn position_matches(p: &Value, brsymbol: &str, segment: &str, product: &str) -> bool {
    s(p, "trdSym") == brsymbol && s(p, "exSeg") == segment && s(p, "prod") == product
}
