//! Nuvama's order book, trade book, positions and holdings -> OpenAlgo
//! rows (web `mapping/order_data.py`).
//!
//! Every book answers `{"appID", "config", "msgID", "srvTm", "data": {..}}`
//! with its rows under `data.ord`, `data.trade`, `data.pos` or
//! `data.rmsHdg`. An empty book is HTTP 222 with an `error` object
//! (`ETRD0002`, `ETRD0004`). Each row's `sym` is the master contract token,
//! so rows map back to OpenAlgo symbols through it.

use super::client::Answer;
use super::mapping::{
    int, num, number, oa_exchange, price_type_from_nuvama, product_from_nuvama, side, text,
};
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{Holding, Order, PortfolioStats, Position, Trade};
use crate::error::{AppError, Result};
use serde_json::Value;

/// Which book a read asks for, with the key its rows come under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NuvamaBook {
    Orders,
    Trades,
    Positions,
    Holdings,
}

impl NuvamaBook {
    pub fn key(self) -> &'static str {
        match self {
            NuvamaBook::Orders => "ord",
            NuvamaBook::Trades => "trade",
            NuvamaBook::Positions => "pos",
            NuvamaBook::Holdings => "rmsHdg",
        }
    }

    pub fn book(self) -> Book {
        match self {
            NuvamaBook::Orders => Book::Orders,
            NuvamaBook::Trades => Book::Trades,
            NuvamaBook::Positions => Book::Positions,
            NuvamaBook::Holdings => Book::Holdings,
        }
    }
}

pub(crate) fn unread(book: Book) -> AppError {
    book_read::unreadable("nuvama", book)
}

/// The rows of a book answer: its list on a success, none for Nuvama's
/// empty-book answer, an error for anything else. A success without its
/// list (missing, `null`, not a list) is unreadable, never empty (BR-03).
pub(crate) fn rows(a: &Answer, which: NuvamaBook) -> Result<Vec<Value>> {
    if a.ok() {
        let data = a.data().ok_or_else(|| unread(which.book()))?;
        return Ok(book_read::list(data, which.key(), || unread(which.book()))?.to_vec());
    }
    if a.empty_book() {
        return Ok(Vec::new());
    }
    Err(a.refused(match which {
        NuvamaBook::Orders => "the order book",
        NuvamaBook::Trades => "the trade book",
        NuvamaBook::Positions => "the positions",
        NuvamaBook::Holdings => "the holdings",
    }))
}

/// Every row an object, else the book is unreadable.
pub(crate) fn objects(rows: &[Value], book: Book) -> Result<()> {
    if rows.iter().all(Value::is_object) {
        Ok(())
    } else {
        Err(unread(book))
    }
}

/// OpenAlgo symbol of a book row: by its token, else by its trading symbol,
/// else the broker's text (web `oa_symbol_for`).
pub fn oa_symbol_for(row: &Value, symbols: &SymbolResolver) -> String {
    let exchange = oa_exchange(&text(row.get("exc")));
    let token = text(row.get("sym"));
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(&exchange, &token) {
            return r.symbol;
        }
    }
    let trd_sym = text(row.get("trdSym"));
    if !trd_sym.is_empty() {
        if let Some(s) = symbols.oa_symbol(&trd_sym, &exchange) {
            return s;
        }
        return trd_sym;
    }
    token
}

fn first_text(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| text(row.get(*k)))
        .find(|t| !t.is_empty())
        .unwrap_or_default()
}

/// Python's truth of a JSON value: `"0"` is true, `0` and `""` are not.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The number of the first of `keys` that is set (the web's
/// `_f(row.get(a) or row.get(b))`).
fn first_number(row: &Value, keys: &[&str]) -> f64 {
    let v = keys.iter().find_map(|k| row.get(*k).filter(|v| truthy(v)));
    num(v)
}

fn to_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// One order-book row (web `map_order_data` + `transform_order_data`).
/// The fill fields are not in the web's row; they are read when Nuvama
/// sends them.
pub fn order_row(row: &Value, symbols: &SymbolResolver) -> Order {
    let quantity = first_number(row, &["ntQty", "reqQty"]) as i64;
    let filled = first_number(row, &["fldQty", "flQty", "fQty"]) as i64;
    let status = super::mapping::normalize_status(&text(row.get("sts")));
    let pending = if matches!(status.as_str(), "open" | "trigger pending") {
        (quantity - filled).max(0)
    } else {
        0
    };
    let reason = first_text(row, &["rjRsn", "rejRsn", "rRsn"]);
    Order {
        order_id: text(row.get("ordID")),
        exchange_order_id: Some(text(row.get("exOrdID"))).filter(|s| !s.is_empty()),
        symbol: oa_symbol_for(row, symbols),
        exchange: oa_exchange(&text(row.get("exc"))),
        side: side(&text(row.get("trsTyp"))),
        quantity: to_i32(quantity),
        filled_quantity: to_i32(filled),
        pending_quantity: to_i32(pending),
        price: num(row.get("prc")),
        trigger_price: num(row.get("trgPrc")),
        average_price: first_number(row, &["avgPrc", "avgFlPrc"]),
        order_type: price_type_from_nuvama(&text(row.get("ordTyp"))),
        product: product_from_nuvama(&text(row.get("prdCode"))),
        validity: Some(text(row.get("dur")))
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "DAY".into()),
        order_timestamp: first_text(row, &["ordTim", "rcvTim"]),
        exchange_timestamp: None,
        rejection_reason: (status == "rejected" && !reason.is_empty()).then_some(reason),
        status,
        order_tag: None,
    }
}

/// One trade-book row (web `transform_tradebook_data`).
pub fn trade_row(row: &Value, symbols: &SymbolResolver) -> Trade {
    let quantity = first_number(row, &["flQty", "fldQty"]) as i64;
    let price = num(row.get("flPrc"));
    let stamp = [text(row.get("flDt")), text(row.get("flTim"))]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Trade {
        order_id: text(row.get("ordID")),
        trade_id: first_text(row, &["flID", "trdID"]),
        symbol: oa_symbol_for(row, symbols),
        exchange: oa_exchange(&text(row.get("exc"))),
        product: product_from_nuvama(&text(row.get("prdCode"))),
        side: side(&text(row.get("trsTyp"))),
        quantity: to_i32(quantity),
        average_price: price,
        trade_value: crate::brokers::common::streaming::round2(quantity as f64 * price),
        timestamp: if stamp.is_empty() {
            text(row.get("ordTim"))
        } else {
            stamp
        },
        order_tag: None,
    }
}

/// A position row's net quantity, `None` when it cannot be read.
pub fn net_quantity(row: &Value) -> Option<i64> {
    book_read::quantity(row, "ntQty")
}

/// One position row (web `transform_positions_data`); its `ntQty` was
/// checked readable.
pub fn position_row(row: &Value, symbols: &SymbolResolver) -> Position {
    let net = net_quantity(row).unwrap_or(0);
    let realized = num(row.get("rlzPL"));
    let unrealized = num(row.get("urlzPL"));
    let pnl = number(row.get("ntPL")).unwrap_or(realized + unrealized);
    // The average of the side the open quantity is on.
    let average = match net {
        n if n > 0 => num(row.get("avgByPrc")),
        n if n < 0 => num(row.get("avgSlPrc")),
        _ => 0.0,
    };
    let round2 = crate::brokers::common::streaming::round2;
    Position {
        symbol: oa_symbol_for(row, symbols),
        exchange: oa_exchange(&text(row.get("exc"))),
        product: product_from_nuvama(&text(row.get("prdCode"))),
        quantity: to_i32(net),
        overnight_quantity: 0,
        average_price: round2(average),
        ltp: round2(num(row.get("ltp"))),
        pnl: round2(pnl),
        realized_pnl: realized,
        unrealized_pnl: unrealized,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
        average_price_basis: None,
    }
}

/// One holding row (web `map_portfolio_data`, `transform_holdings_data`).
/// RMS holdings carry no buy average, so the average and P&L stay 0
/// rather than being made up. `None` for a row with no quantity (the web
/// drops those).
pub fn holding_row(row: &Value, symbols: &SymbolResolver) -> Option<Holding> {
    let quantity = int(row.get("totalQty"));
    if quantity == 0 {
        return None;
    }
    let ltp = num(row.get("ltp"));
    let isin = text(row.get("isin"));
    Some(Holding {
        symbol: oa_symbol_for(row, symbols),
        exchange: oa_exchange(&text(row.get("exc"))),
        product: "CNC".into(),
        isin: Some(isin).filter(|i| !i.is_empty()),
        quantity: to_i32(quantity),
        t1_quantity: 0,
        average_price: 0.0,
        ltp,
        close_price: 0.0,
        pnl: 0.0,
        pnl_percentage: 0.0,
        current_value: number(row.get("totalVal")).unwrap_or(ltp * quantity as f64),
    })
}

/// The web's portfolio totals: the holding value from `totalVal` (else
/// LTP times quantity), and no invested value or P&L (web
/// `calculate_portfolio_statistics`).
pub fn portfolio_totals(rows: &[Value]) -> PortfolioStats {
    let value: f64 = rows
        .iter()
        .map(|h| {
            number(h.get("totalVal")).unwrap_or(num(h.get("ltp")) * int(h.get("totalQty")) as f64)
        })
        .sum();
    PortfolioStats {
        totalholdingvalue: crate::brokers::common::streaming::round2(value),
        totalinvvalue: 0.0,
        totalprofitandloss: 0.0,
        totalpnlpercentage: 0.0,
    }
}
