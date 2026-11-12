//! Funds (`/Limits`) and the margin calculator (web `api/funds.py`,
//! `api/margin_api.py`, `mapping/margin_data.py`).

use super::mapping::{
    self, escape_tsym, f, mpp_margin, mpp_margin_trigger_first, noren_exchange, pricetype_code,
    product_code, MppOrder, MppQuote,
};
use super::orders::raw_book;
use super::transport::{check, emsg, session, stat_ok, Category, Session};
use super::{FundsM2m, MarginApi, MarginMpp, NorenBroker, NorenConfig, PositionPnl};
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::mpp::py_round;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// Funds from a `/Limits` answer plus (for PositionBook members) the
/// position rows.
pub fn funds_from(cfg: &NorenConfig, limits: &Value, positions: &[Value]) -> Funds {
    let cash = f(limits, "cash");
    let payin = f(limits, "payin");
    let used = f(limits, "marginused");
    let (realised, unrealised) = match cfg.funds_m2m {
        FundsM2m::Limits => (-f(limits, "rpnl"), f(limits, "unmtom")),
        FundsM2m::PositionBook => positions.iter().fold((0.0, 0.0), |(r, u), p| {
            let (_, _, pr, pu) = mapping::position_pnl(PositionPnl::RealisedPlusUnrealised, p);
            (r + pr, u + pu)
        }),
    };
    Funds {
        available_cash: cash + payin - used,
        used_margin: used,
        total_margin: cash + payin,
        opening_balance: cash,
        payin,
        payout: f(limits, "payout"),
        span: f(limits, "span"),
        exposure: f(limits, "expo"),
        collateral: (cfg.hooks.collateral)(limits),
        m2m_unrealized: unrealised,
        m2m_realized: realised,
        utilised_debits: used,
    }
}

pub async fn get_funds(b: &NorenBroker, auth: &AuthToken) -> Result<Funds> {
    let s = session(b.cfg, auth)?;
    let v = b
        .post_raw(
            "/Limits",
            json!({"uid": s.uid, "actid": s.uid}),
            &s,
            Category::Data,
        )
        .await?;
    let limits = check(b.cfg.name, "/Limits", v)?;
    // BR-03: an Ok without the cash figure is not a zero balance.
    if limits.get("cash").is_none_or(Value::is_null) {
        tracing::warn!(broker = b.cfg.id, "Limits answered without the cash figure");
        return Err(crate::brokers::common::book_read::unreadable(
            b.cfg.id,
            crate::brokers::common::book_read::Book::Funds,
        ));
    }
    let positions = if b.cfg.funds_m2m == FundsM2m::PositionBook {
        match raw_book(b, &s, "/PositionBook").await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    broker = b.cfg.id,
                    "Position P&L for funds failed: {}",
                    e.code()
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    Ok(funds_from(b.cfg, &limits, &positions))
}

/// The master row of every leg. A leg that does not resolve fails the
/// basket, naming it: pricing the rest would report the margin of a
/// different basket (web #2156; the margin service refuses such a basket
/// first, BR-01, so this guards a master replaced in between).
fn resolved_legs(b: &NorenBroker, legs: &[MarginLeg]) -> Result<Vec<SymToken>> {
    let mut rows = Vec::with_capacity(legs.len());
    let mut missing = Vec::new();
    for l in legs {
        match b.resolver().by_symbol(&l.key.exchange, &l.key.symbol) {
            Some(row) => rows.push(row),
            None => missing.push(format!("{} ({})", l.key.symbol, l.key.exchange)),
        }
    }
    if !missing.is_empty() {
        tracing::warn!(
            broker = b.cfg.id,
            "Margin legs not found in the master: {}",
            missing.join(", ")
        );
        return Err(AppError::Validation(format!(
            "Symbol not found: {}. Check the symbol name and expiry date.",
            missing.join(", ")
        )));
    }
    if rows.is_empty() {
        return Err(no_legs());
    }
    Ok(rows)
}

/// One margin leg's body.
fn leg(l: &MarginLeg, row: &SymToken, prctyp: &str, prc: &str) -> Value {
    json!({
        "exch": l.key.exchange,
        "tsym": escape_tsym(row.br_symbol()),
        "qty": l.quantity.to_string(),
        "prc": prc,
        "trgprc": mapping::num(l.trigger_price),
        "prd": product_code(l.product),
        "trantype": if l.action == Action::Buy { "B" } else { "S" },
        "prctyp": prctyp,
    })
}

/// GetBasketMargin body: first leg flat, the rest in `basketlists`.
pub fn basket_body(uid: &str, legs: Vec<Value>) -> Option<Value> {
    let mut it = legs.into_iter();
    let first = it.next()?;
    let mut body = first;
    body["uid"] = json!(uid);
    body["actid"] = json!(uid);
    body["basketlists"] = Value::Array(it.collect());
    Some(body)
}

async fn margin_quote(b: &NorenBroker, s: &Session, row: &SymToken) -> Option<MppQuote> {
    let q = super::data::quote_response(b, s, noren_exchange(&row.exchange), &row.token)
        .await
        .ok()?;
    let tick = f(&q, "ti");
    Some(MppQuote {
        ltp: f(&q, "lp"),
        tick: (tick > 0.0).then_some(tick),
    })
}

fn no_legs() -> AppError {
    AppError::Validation(
        "No valid positions to calculate margin. Check if symbols are valid.".into(),
    )
}

/// `marginusedtrade` of a GetBasketMargin answer: the margin after the
/// trade, hedge benefit applied. Never `marginused` (the pre-hedge figure)
/// in its place, which quoted a calendar spread at naked-leg margin.
fn post_trade_margin(basket: &Value) -> Result<f64> {
    match basket.get("marginusedtrade") {
        Some(Value::String(t)) if t.is_empty() => None,
        Some(Value::Null) | None => None,
        Some(v) => Some(mapping::num_f64(Some(v))),
    }
    .ok_or_else(|| {
        tracing::warn!("GetBasketMargin returned no marginusedtrade");
        AppError::Broker("Broker did not return the post-trade margin".into())
    })
}

/// The basket's own margin (web flattrade `parse_margin_response` with
/// `existing_margin_used`): both GetBasketMargin figures are account
/// totals, so the margin already held by open positions and orders
/// (`/Limits` `marginused`) is taken off, per the Noren calculation note
/// (post trade margin = marginusedtrade(GetBasketMargin) - marginused(Limits)).
/// It is what Flattrade's basket screen shows with "Include existing
/// margin" unticked (web #2156).
pub(crate) fn own_basket_margin(basket: &Value, in_use: f64) -> Result<f64> {
    Ok(py_round(post_trade_margin(basket)? - in_use, 2))
}

/// `marginused` of an Ok `/Limits` answer, read as 0 when left out (as
/// funds read it); `None` when the call failed (web
/// `parse_limits_margin_used`).
pub(crate) fn limits_margin_used(limits: &Value) -> Option<f64> {
    stat_ok(limits).then(|| f(limits, "marginused"))
}

/// The margin open positions and orders already hold. A failed read fails
/// the calculation: the basket figure alone is not the basket's margin.
async fn margin_in_use(b: &NorenBroker, s: &Session) -> Result<f64> {
    let v = b
        .post_raw(
            "/Limits",
            json!({"uid": s.uid, "actid": s.uid}),
            s,
            Category::Data,
        )
        .await?;
    limits_margin_used(&v).ok_or_else(|| {
        let reason = emsg(&v);
        tracing::warn!(
            broker = b.cfg.id,
            "Limits failed during basket margin: {}",
            reason
        );
        AppError::Broker(format!(
            "Could not read margin already in use: {}",
            if reason.trim().is_empty() {
                "Limits request failed"
            } else {
                reason.trim()
            }
        ))
    })
}

/// `/GetBasketMargin` for the whole basket, MARKET and SL-M legs priced
/// the member's way (`MarginMpp`).
async fn basket_margin(b: &NorenBroker, s: &Session, legs: &[MarginLeg]) -> Result<f64> {
    let rows = resolved_legs(b, legs)?;
    let mut built = Vec::with_capacity(rows.len());
    for (l, row) in legs.iter().zip(&rows) {
        let quote = if matches!(l.pricetype, PriceType::Market | PriceType::SlM) {
            margin_quote(b, s, row).await
        } else {
            None
        };
        let order = MppOrder {
            symbol: &l.key.symbol,
            action: l.action,
            pricetype: l.pricetype,
            price: l.price,
            trigger: l.trigger_price,
        };
        let (prctyp, prc) = match b.cfg.margin_mpp {
            MarginMpp::LtpOrSupplied => mpp_margin(&order, quote)?,
            MarginMpp::TriggerFirst => mpp_margin_trigger_first(&order, quote, row.tick_size)
                .map_err(AppError::Validation)?,
        };
        built.push(leg(l, row, prctyp, &prc));
    }
    let body = basket_body(&s.uid, built).ok_or_else(no_legs)?;
    let v = b
        .post_raw("/GetBasketMargin", body, s, Category::Data)
        .await?;
    let v = check(b.cfg.name, "/GetBasketMargin", v)?;
    if b.cfg.margin == MarginApi::BasketLessInUse {
        let in_use = margin_in_use(b, s).await?;
        return own_basket_margin(&v, in_use);
    }
    Ok((b.cfg.hooks.margin_total)(&v))
}

/// `/GetOrderMargin` per leg, summed (no hedge benefit between legs).
async fn per_leg_margin(b: &NorenBroker, s: &Session, legs: &[MarginLeg]) -> Result<f64> {
    let rows = resolved_legs(b, legs)?;
    let mut total = 0.0;
    for (l, row) in legs.iter().zip(&rows) {
        let mut body = leg(l, row, pricetype_code(l.pricetype), &mapping::num(l.price));
        body["uid"] = json!(s.uid);
        body["actid"] = json!(s.uid);
        body["rorgqty"] = json!("0");
        body["rorgprc"] = json!("0");
        let v = b
            .post_raw("/GetOrderMargin", body, s, Category::Data)
            .await?;
        let v = check(b.cfg.name, "/GetOrderMargin", v)?;
        total += (b.cfg.hooks.margin_total)(&v);
    }
    Ok(total)
}

pub async fn calculate_margin(
    b: &NorenBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let s = session(b.cfg, auth)?;
    let total = match b.cfg.margin {
        MarginApi::Unsupported => return Err(AppError::Unsupported("margin")),
        MarginApi::Basket | MarginApi::BasketLessInUse => basket_margin(b, &s, legs).await?,
        MarginApi::PerLeg => per_leg_margin(b, &s, legs).await?,
    };
    Ok(MarginResult {
        total_margin_required: total,
        span_margin: 0.0,
        exposure_margin: 0.0,
    })
}
