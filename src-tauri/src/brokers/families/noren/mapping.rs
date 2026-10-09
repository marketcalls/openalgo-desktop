//! Noren <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `streaming/*_order_adapter.py`).
//!
//! Noren sends every number as a string and omits fields that do not
//! apply, so rows are read as `serde_json::Value` with lenient accessors.

use super::{MppScope, NorenConfig, PositionPnl};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::master_contract::format_strike;
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price, py_round};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Lenient accessors
// ---------------------------------------------------------------------------

pub fn num_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn num_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => {
            let t = s.trim();
            t.parse::<i64>()
                .unwrap_or_else(|_| t.parse::<f64>().unwrap_or(0.0) as i64)
        }
        _ => 0,
    }
}

pub fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub fn f(v: &Value, k: &str) -> f64 {
    num_f64(v.get(k))
}

pub fn i(v: &Value, k: &str) -> i64 {
    num_i64(v.get(k))
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// A number as Noren expects it in `jData` (`"0"`, `"950.5"`).
pub fn num(v: f64) -> String {
    format_strike(v)
}

// ---------------------------------------------------------------------------
// Enum maps
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> Noren exchange (indices quote on their cash
/// segment).
pub fn noren_exchange(oa: &str) -> &str {
    match oa {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        other => other,
    }
}

pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Cnc => "C",
        Product::Nrml => "M",
        Product::Mis => "I",
    }
}

/// `C/M/I` -> OpenAlgo product (close-all uses this).
pub fn reverse_product(code: &str) -> Option<&'static str> {
    match code {
        "C" => Some("CNC"),
        "M" => Some("NRML"),
        "I" => Some("MIS"),
        _ => None,
    }
}

/// Book product as the web maps it: `C` only on NSE/BSE, `M` only on
/// derivatives, `I` anywhere; anything else passes through.
pub fn book_product(exch: &str, prd: &str) -> String {
    match (exch, prd) {
        ("NSE" | "BSE", "C") => "CNC".into(),
        (_, "I") => "MIS".into(),
        ("NFO" | "MCX" | "BFO" | "CDS", "M") => "NRML".into(),
        (_, other) => other.into(),
    }
}

pub fn pricetype_code(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MKT",
        PriceType::Limit => "LMT",
        PriceType::Sl => "SL-LMT",
        PriceType::SlM => "SL-MKT",
    }
}

/// Noren `prctyp` -> OpenAlgo price type (unknown values pass through).
pub fn book_pricetype(code: &str) -> String {
    match code.trim().to_ascii_uppercase().as_str() {
        "MKT" | "MARKET" => "MARKET".into(),
        "LMT" | "LIMIT" => "LIMIT".into(),
        "SL-MKT" | "SLMKT" | "SL-MARKET" | "SL-M" => "SL-M".into(),
        "SL-LMT" | "SLLMT" | "SL-LIMIT" | "SL" => "SL".into(),
        _ => code.to_string(),
    }
}

pub fn side(trantype: &str) -> String {
    match trantype {
        "B" => "BUY".into(),
        "S" => "SELL".into(),
        other => other.into(),
    }
}

/// REST order-book status (web `normalize_order_status`): `TRIGGER_PENDING`
/// folds into `open` so the order book offers Modify/Cancel.
pub fn normalize_status(raw: &str) -> String {
    let s = raw.trim().to_ascii_uppercase().replace('_', " ");
    match s.as_str() {
        "COMPLETE" => "complete".into(),
        "OPEN"
        | "PENDING"
        | "TRIGGER PENDING"
        | "NEW"
        | "REPLACED"
        | "OPEN PENDING"
        | "MODIFY PENDING"
        | "CANCEL PENDING"
        | "AFTER MARKET ORDER REQ RECEIVED" => "open".into(),
        "REJECTED" | "REJECT" => "rejected".into(),
        "CANCELED" | "CANCELLED" => "cancelled".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// Push (order-update) status: keeps `trigger pending` distinct, falls
/// back to `reporttype` for terminal states (web `*_order_adapter.py`).
pub fn push_status(status: &str, reporttype: &str) -> String {
    let t = |s: &str| s.trim().to_ascii_lowercase().replace('_', " ");
    let st = t(status);
    let mapped = match st.as_str() {
        "complete" | "executed" => Some("complete"),
        "open"
        | "new"
        | "replaced"
        | "pending"
        | "open pending"
        | "modify pending"
        | "cancel pending"
        | "after market order req received" => Some("open"),
        "trigger pending" => Some("trigger pending"),
        "rejected" | "reject" => Some("rejected"),
        "canceled" | "cancelled" => Some("cancelled"),
        _ => None,
    };
    if let Some(m) = mapped {
        return m.into();
    }
    match t(reporttype).as_str() {
        "rejected" => "rejected".into(),
        "canceled" | "cancelled" => "cancelled".into(),
        _ if !st.is_empty() => st,
        _ => "open".into(),
    }
}

/// `&` in a trading symbol becomes `%26` on order paths.
pub fn escape_tsym(s: &str) -> String {
    s.replace('&', "%26")
}

// ---------------------------------------------------------------------------
// Market Price Protection (web `transform_data` + `utils/mpp_slab.py`)
// ---------------------------------------------------------------------------

/// What a quote contributes to MPP.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MppQuote {
    pub ltp: f64,
    /// `ti` from GetQuotes.
    pub tick: Option<f64>,
}

/// The order fields MPP reads.
#[derive(Debug, Clone, Copy)]
pub struct MppOrder<'a> {
    /// OpenAlgo symbol (its suffix picks the option slab).
    pub symbol: &'a str,
    pub action: Action,
    pub pricetype: PriceType,
    pub price: f64,
    pub trigger: f64,
}

/// `(prctyp, prc)` for a place order. `quote` is `None` when it could not
/// be fetched; `master_tick` is the SymToken tick size.
pub fn mpp_place(
    scope: MppScope,
    o: &MppOrder<'_>,
    quote: Option<MppQuote>,
    master_tick: f64,
) -> (&'static str, String) {
    let MppOrder {
        symbol,
        action,
        pricetype,
        price,
        trigger,
    } = *o;
    let base = pricetype_code(pricetype);
    let is_market = pricetype == PriceType::Market;
    let is_slm = pricetype == PriceType::SlM;
    if !is_market && !is_slm {
        return (base, num(price));
    }
    let it = instrument_type_from_symbol(symbol);
    let target = if is_market { "LMT" } else { "SL-LMT" };
    let ltp = quote.map(|q| q.ltp).unwrap_or(0.0);
    let tick = quote.and_then(|q| q.tick).filter(|t| *t > 0.0);
    match scope {
        MppScope::MarketOnly => {
            if is_market && ltp > 0.0 {
                ("LMT", num(protected_price(ltp, action, it, tick)))
            } else {
                (base, num(price))
            }
        }
        MppScope::MarketAndStop => {
            if ltp > 0.0 {
                return (target, num(protected_price(ltp, action, it, tick)));
            }
            if is_market {
                return (base, num(price));
            }
            // SL-M without a quote still goes as SL-LMT, priced off the
            // trigger; the MPP buffer only when the master has a tick size.
            if trigger > 0.0 {
                if master_tick > 0.0 {
                    let p = protected_price(trigger, action, it, Some(master_tick));
                    return ("SL-LMT", num(p));
                }
                return ("SL-LMT", num(trigger));
            }
            ("SL-LMT", num(price))
        }
        MppScope::AlwaysConvert => {
            let basis = if is_market {
                ltp
            } else if trigger > 0.0 {
                trigger
            } else {
                ltp
            };
            if basis > 0.0 {
                (target, num(protected_price(basis, action, it, tick)))
            } else if is_market {
                (target, num(price))
            } else {
                (target, num(trigger))
            }
        }
    }
}

/// Margin legs: MARKET/SL-M always convert (GetBasketMargin rejects
/// MKT/SL-MKT); without a quote MARKET uses the supplied price and SL-M the
/// trigger.
pub fn mpp_margin(o: &MppOrder<'_>, quote: Option<MppQuote>) -> (&'static str, String) {
    let MppOrder {
        symbol,
        action,
        pricetype,
        price,
        trigger,
    } = *o;
    match pricetype {
        PriceType::Market | PriceType::SlM => {
            let target = if pricetype == PriceType::Market {
                "LMT"
            } else {
                "SL-LMT"
            };
            let fallback = if pricetype == PriceType::Market {
                price
            } else {
                trigger
            };
            match quote.filter(|q| q.ltp > 0.0) {
                Some(q) => (
                    target,
                    num(protected_price(
                        q.ltp,
                        action,
                        instrument_type_from_symbol(symbol),
                        q.tick.filter(|t| *t > 0.0),
                    )),
                ),
                None => (target, num(fallback)),
            }
        }
        other => (pricetype_code(other), num(price)),
    }
}

/// Margin legs for `MarginMpp::TriggerFirst` (web flattrade
/// `margin_data._apply_mpp`, #2161). GetBasketMargin refuses MKT/SL-MKT
/// and a zero price, so MARKET and SL-M go out as LMT and SL-LMT with a
/// positive price:
/// - MARKET is protected off the LTP; SL-M off its trigger, so the limit
///   stays on the right side of `trgprc`, and off the LTP only without a
///   trigger;
/// - the protection rounds to the quote's tick, else the master's
///   (`master_tick`); with neither the base price goes unprotected;
/// - when the quote could not be fetched (`quote` is `None`) or nothing
///   above gives a positive price, MARKET falls back to the supplied price
///   and SL-M to the trigger;
/// - `Err` with the trader-facing refusal when none of these is positive:
///   dropping the leg would report the margin of a different basket.
pub fn mpp_margin_trigger_first(
    o: &MppOrder<'_>,
    quote: Option<MppQuote>,
    master_tick: f64,
) -> std::result::Result<(&'static str, String), String> {
    let MppOrder {
        symbol,
        action,
        pricetype,
        price,
        trigger,
    } = *o;
    let target = match pricetype {
        PriceType::Market => "LMT",
        PriceType::SlM => "SL-LMT",
        other => return Ok((pricetype_code(other), num(price))),
    };
    let positive = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
    let trigger = positive(trigger);
    let fallback = if pricetype == PriceType::Market {
        positive(price)
    } else {
        trigger
    };
    // A failed quote skips the protection, as the web's exception path does.
    if let Some(q) = quote {
        let reference = if trigger > 0.0 && pricetype == PriceType::SlM {
            trigger
        } else {
            positive(q.ltp)
        };
        let tick = q
            .tick
            .filter(|t| *t > 0.0)
            .or_else(|| (master_tick > 0.0).then_some(master_tick));
        if reference > 0.0 {
            let Some(tick) = tick else {
                return Ok((target, num(reference)));
            };
            let protected = protected_price(
                reference,
                action,
                instrument_type_from_symbol(symbol),
                Some(tick),
            );
            if positive(protected) > 0.0 {
                return Ok((target, num(protected)));
            }
        }
    }
    if fallback > 0.0 {
        return Ok((target, num(fallback)));
    }
    Err(format!(
        "Could not get a live price for {}. Enter a price for this leg, or try again in a moment.",
        symbol
    ))
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

/// PlaceOrder `jData` (web `transform_data`); `prctyp`/`prc` come from MPP.
pub fn place_jdata(
    cfg: &NorenConfig,
    uid: &str,
    o: &ResolvedOrder,
    prctyp: &str,
    prc: &str,
) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_string(), Value::String(v));
    };
    put("uid", uid.into());
    put("actid", uid.into());
    put("exch", o.exchange.as_str().into());
    put("tsym", escape_tsym(o.brsymbol()));
    put("qty", o.quantity.to_string());
    put("prc", prc.into());
    put("trgprc", num(o.trigger_price));
    put("dscqty", o.disclosed_quantity.to_string());
    put("prd", product_code(o.product).into());
    put(
        "trantype",
        if o.action == Action::Buy { "B" } else { "S" }.into(),
    );
    put("prctyp", prctyp.into());
    if cfg.send_mkt_protection {
        put("mkt_protection", "0".into());
    }
    put("ret", "DAY".into());
    put("ordersource", "API".into());
    if let Some(r) = cfg.place_remarks {
        put("remarks", r.into());
    }
    Value::Object(m)
}

/// ModifyOrder `jData` (web `transform_modify_order_data`). `trgprc` only
/// for SL / SL-M: a zero trigger on a LIMIT is refused ("Trigger price
/// invalid").
pub fn modify_jdata(cfg: &NorenConfig, uid: &str, m: &ResolvedModify) -> Value {
    let prc = if cfg.modify_market_price_zero && m.pricetype == PriceType::Market {
        "0".to_string()
    } else {
        num(m.price)
    };
    let mut v = json!({
        "uid": uid,
        "exch": m.exchange.as_str(),
        "norenordno": m.order_id,
        "prctyp": pricetype_code(m.pricetype),
        "prc": prc,
        "qty": m.quantity.to_string(),
        "tsym": escape_tsym(m.brsymbol()),
        "ret": "DAY",
        "dscqty": m.disclosed_quantity.to_string(),
    });
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        v["trgprc"] = Value::String(num(m.trigger_price));
    }
    v
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// OpenAlgo symbol for a book row: by token (web `get_symbol(token,
/// exch)`), then by trading symbol, else the broker symbol.
pub fn oa_symbol(symbols: &SymbolResolver, exch: &str, token: &str, tsym: &str) -> String {
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(exch, token) {
            return r.symbol;
        }
    }
    symbols.oa_symbol_or_raw(tsym, exch)
}

pub fn map_order(cfg: &NorenConfig, o: &Value, symbols: &SymbolResolver) -> Order {
    let exch = text(o, "exch");
    let prctyp = book_pricetype(&text(o, "prctyp"));
    let qty = i(o, "qty");
    let filled = i(o, "fillshares");
    let mut price = f(o, "prc");
    if cfg.orderbook_price_fallback {
        let avg = f(o, "avgprc");
        if !text(o, "instname").is_empty() && avg > 0.0 {
            price = avg;
        } else if (prctyp == "MARKET" || prctyp == "SL-M") && price == 0.0 {
            let r = f(o, "rprc");
            if r > 0.0 {
                price = r;
            }
        }
    }
    let status = normalize_status(&text(o, "status"));
    Order {
        order_id: text(o, "norenordno"),
        exchange_order_id: non_empty(text(o, "exchordid")),
        symbol: oa_symbol(symbols, &exch, &text(o, "token"), &text(o, "tsym")),
        exchange: exch.clone(),
        side: side(&text(o, "trantype")),
        quantity: clamp_i32(qty),
        filled_quantity: clamp_i32(filled),
        pending_quantity: clamp_i32((qty - filled).max(0)),
        price,
        trigger_price: f(o, "trgprc"),
        average_price: f(o, "avgprc"),
        order_type: prctyp,
        product: book_product(&exch, &text(o, "prd")),
        rejection_reason: if status == "rejected" {
            non_empty(text(o, "rejreason"))
        } else {
            None
        },
        status,
        validity: {
            let r = text(o, "ret");
            if r.is_empty() {
                "DAY".into()
            } else {
                r
            }
        },
        order_timestamp: text(o, "norentm"),
        exchange_timestamp: non_empty(text(o, "exch_tm")),
    }
}

pub fn map_trade(cfg: &NorenConfig, t: &Value, symbols: &SymbolResolver) -> Trade {
    let exch = text(t, "exch");
    let qty = i(t, "qty").max(i(t, "flqty"));
    let mut avg = f(t, "avgprc");
    if avg == 0.0 {
        avg = f(t, "flprc");
    }
    let mut value = avg * qty as f64;
    if !cfg.tradebook_time_only {
        avg = py_round(avg, 2);
        value = py_round(avg * qty as f64, 2);
    }
    let ts = text(t, "norentm");
    let timestamp = if cfg.tradebook_time_only {
        ts.split(' ').next().unwrap_or("").to_string()
    } else {
        ts
    };
    let order_id = text(t, "norenordno");
    let flid = text(t, "flid");
    Trade {
        trade_id: if flid.is_empty() {
            order_id.clone()
        } else {
            flid
        },
        order_id,
        symbol: symbols.oa_symbol_or_raw(&text(t, "tsym"), &exch),
        exchange: exch.clone(),
        product: book_product(&exch, &text(t, "prd")),
        side: side(&text(t, "trantype")),
        quantity: clamp_i32(qty),
        average_price: avg,
        trade_value: value,
        timestamp,
    }
}

/// Realised and unrealised P&L of one PositionBook row.
pub fn position_pnl(rule: PositionPnl, p: &Value) -> (f64, f64, f64, f64) {
    let netqty = f(p, "netqty");
    let mut avg = f(p, "netavgprc");
    let lp = f(p, "lp");
    let rpnl = f(p, "rpnl");
    let urmtom = f(p, "urmtom");
    match rule {
        PositionPnl::NetAverage => {
            if avg == 0.0 && netqty == 0.0 {
                avg = f(p, "daybuyavgprc");
                if avg == 0.0 {
                    avg = f(p, "totbuyavgprc");
                }
            }
            let pnl = if netqty != 0.0 && lp > 0.0 {
                if urmtom != 0.0 {
                    urmtom + rpnl
                } else if netqty > 0.0 {
                    (lp - avg) * netqty + rpnl
                } else {
                    (avg - lp) * netqty.abs() + rpnl
                }
            } else {
                rpnl
            };
            (avg, pnl, rpnl, pnl - rpnl)
        }
        PositionPnl::RealisedPlusUnrealised => {
            let mut u = urmtom;
            if u == 0.0 && netqty != 0.0 {
                let factor = match p.get("prcftr") {
                    Some(v) if !v.is_null() => num_f64(Some(v)),
                    _ => 1.0,
                };
                u = (lp - avg) * netqty * factor;
            }
            (avg, py_round(rpnl + u, 2), rpnl, u)
        }
    }
}

pub fn map_position(cfg: &NorenConfig, p: &Value, symbols: &SymbolResolver) -> Position {
    let exch = text(p, "exch");
    let (avg, pnl, realized, unrealized) = position_pnl(cfg.position_pnl, p);
    Position {
        symbol: symbols.oa_symbol_or_raw(&text(p, "tsym"), &exch),
        exchange: exch.clone(),
        product: book_product(&exch, &text(p, "prd")),
        quantity: clamp_i32(i(p, "netqty")),
        overnight_quantity: clamp_i32(i(p, "cfbuyqty") - i(p, "cfsellqty")),
        average_price: avg,
        ltp: f(p, "lp"),
        pnl,
        realized_pnl: realized,
        unrealized_pnl: unrealized,
        buy_quantity: clamp_i32(i(p, "daybuyqty")),
        buy_value: f(p, "daybuyamt"),
        sell_quantity: clamp_i32(i(p, "daysellqty")),
        sell_value: f(p, "daysellamt"),
    }
}

/// Holdings: rows with `stat == Ok`, NSE legs only (web
/// `transform_holdings_data`). The Holdings answer has no live price, so
/// the upload price stands in for it (the web values holdings at it).
pub fn map_holdings(cfg: &NorenConfig, rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    let mut out = Vec::new();
    for h in rows {
        if h.get("stat")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "Ok")
        {
            continue;
        }
        let qty = (cfg.hooks.holding_qty)(h);
        let upld = f(h, "upldprc");
        let legs = h
            .get("exch_tsym")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for leg in legs.iter().filter(|l| text(l, "exch") == "NSE") {
            let tsym = text(leg, "tsym");
            out.push(Holding {
                symbol: symbols.oa_symbol_or_raw(&tsym, "NSE"),
                exchange: "NSE".into(),
                product: "CNC".into(),
                isin: non_empty(text(leg, "isin")),
                quantity: clamp_i32(qty),
                t1_quantity: clamp_i32(i(h, "btstqty")),
                average_price: upld,
                ltp: upld,
                close_price: 0.0,
                pnl: 0.0,
                pnl_percentage: 0.0,
                current_value: upld * qty as f64,
            });
        }
    }
    out
}

/// One `om` frame -> order update.
pub fn order_update(o: &Value, symbols: &SymbolResolver) -> OrderUpdate {
    let exch = text(o, "exch");
    let qty = i(o, "qty");
    let filled = i(o, "fillshares");
    let status = push_status(&text(o, "status"), &text(o, "reporttype"));
    let mut orderid = text(o, "norenordno");
    if orderid.is_empty() {
        orderid = text(o, "norenoordno");
    }
    let prd = {
        let p = text(o, "prd");
        if p.is_empty() {
            text(o, "pcode")
        } else {
            p
        }
    };
    let prctyp = text(o, "prctyp");
    OrderUpdate {
        orderid,
        symbol: symbols.oa_symbol_or_raw(&text(o, "tsym"), &exch),
        exchange: exch,
        action: side(&text(o, "trantype")),
        quantity: qty,
        price: f(o, "prc"),
        trigger_price: f(o, "trgprc"),
        pricetype: match prctyp.as_str() {
            "LMT" | "MKT" | "SL-LMT" | "SL-MKT" => book_pricetype(&prctyp),
            _ => prctyp,
        },
        product: reverse_product(&prd).map(str::to_string).unwrap_or(prd),
        rejection_reason: if status == "rejected" {
            text(o, "rejreason")
        } else {
            String::new()
        },
        order_status: status,
        filled_quantity: filled,
        pending_quantity: (qty - filled).max(0),
        average_price: f(o, "avgprc"),
    }
}
