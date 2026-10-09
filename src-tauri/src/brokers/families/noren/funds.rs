//! Funds (`/Limits`) and the margin calculator (web `api/funds.py`,
//! `api/margin_api.py`, `mapping/margin_data.py`).

use super::mapping::{
    self, escape_tsym, f, mpp_margin, mpp_margin_trigger_first, noren_exchange, pricetype_code,
    product_code, MppOrder, MppQuote,
};
use super::orders::raw_book;
use super::transport::{check, session, Category, Session};
use super::{FundsM2m, MarginApi, MarginMpp, NorenBroker, NorenConfig, PositionPnl};
use crate::brokers::common::mapping::{Action, PriceType};
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

/// One margin leg; `None` when the symbol does not resolve.
fn leg(b: &NorenBroker, l: &MarginLeg, prctyp: &str, prc: &str) -> Option<Value> {
    let row = b.resolver().by_symbol(&l.key.exchange, &l.key.symbol)?;
    Some(json!({
        "exch": l.key.exchange,
        "tsym": escape_tsym(row.br_symbol()),
        "qty": l.quantity.to_string(),
        "prc": prc,
        "trgprc": mapping::num(l.trigger_price),
        "prd": product_code(l.product),
        "trantype": if l.action == Action::Buy { "B" } else { "S" },
        "prctyp": prctyp,
    }))
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

async fn margin_quote(b: &NorenBroker, s: &Session, l: &MarginLeg) -> Option<MppQuote> {
    let row = b.resolver().by_symbol(&l.key.exchange, &l.key.symbol)?;
    let q = super::data::quote_response(b, s, noren_exchange(&l.key.exchange), &row.token)
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

pub async fn calculate_margin(
    b: &NorenBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let s = session(b.cfg, auth)?;
    match b.cfg.margin {
        MarginApi::Unsupported => Err(AppError::Unsupported("margin")),
        MarginApi::Basket => {
            let mut built = Vec::new();
            for l in legs {
                let Some(row) = b.resolver().by_symbol(&l.key.exchange, &l.key.symbol) else {
                    tracing::warn!(
                        "Margin leg skipped, symbol not found: {} ({})",
                        l.key.symbol,
                        l.key.exchange
                    );
                    continue;
                };
                let quote = if matches!(l.pricetype, PriceType::Market | PriceType::SlM) {
                    margin_quote(b, &s, l).await
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
                    MarginMpp::LtpOrSupplied => mpp_margin(&order, quote),
                    MarginMpp::TriggerFirst => {
                        mpp_margin_trigger_first(&order, quote, row.tick_size)
                            .map_err(AppError::Validation)?
                    }
                };
                match leg(b, l, prctyp, &prc) {
                    Some(v) => built.push(v),
                    None => tracing::warn!(
                        "Margin leg skipped, symbol not found: {} ({})",
                        l.key.symbol,
                        l.key.exchange
                    ),
                }
            }
            let body = basket_body(&s.uid, built).ok_or_else(no_legs)?;
            let v = b
                .post_raw("/GetBasketMargin", body, &s, Category::Data)
                .await?;
            let v = check(b.cfg.name, "/GetBasketMargin", v)?;
            Ok(MarginResult {
                total_margin_required: (b.cfg.hooks.margin_total)(&v),
                span_margin: 0.0,
                exposure_margin: 0.0,
            })
        }
        MarginApi::PerLeg => {
            let mut total = 0.0;
            let mut any = false;
            for l in legs {
                let Some(mut body) = leg(b, l, pricetype_code(l.pricetype), &mapping::num(l.price))
                else {
                    continue;
                };
                any = true;
                body["uid"] = json!(s.uid);
                body["actid"] = json!(s.uid);
                body["rorgqty"] = json!("0");
                body["rorgprc"] = json!("0");
                let v = b
                    .post_raw("/GetOrderMargin", body, &s, Category::Data)
                    .await?;
                let v = check(b.cfg.name, "/GetOrderMargin", v)?;
                total += (b.cfg.hooks.margin_total)(&v);
            }
            if !any {
                return Err(no_legs());
            }
            Ok(MarginResult {
                total_margin_required: total,
                span_margin: 0.0,
                exposure_margin: 0.0,
            })
        }
    }
}
