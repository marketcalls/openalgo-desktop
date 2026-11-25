//! Market Price Protection for an XTS member whose RMS may refuse a native
//! MARKET or SL-M order from an API session (iBulls: web
//! `broker/ibulls/mapping/transform_data.py`, marketcalls/openalgo#2466,
//! 6a8e3febd). A member opts in with `XtsHooks::price_protection`; the
//! others send MARKET and STOPMARKET as they are.
//!
//! * MARKET goes out as a LIMIT at the LTP moved by the MPP slab in the
//!   direction it fills (`mpp::beyond`): above for a BUY, below for a SELL,
//!   at least one tick past the LTP and on a tick, and never below one
//!   tick. Without a tick size (the LTP is then not read) or an LTP (a
//!   quote that fails or says 0) there is no price to send, and it goes out
//!   as MARKET for the broker to accept or refuse.
//! * SL-M goes out as a STOPLIMIT whose limit is its own trigger moved the
//!   same way, never the LTP. Without a trigger, without a tick size, or
//!   for a SELL trigger too low to leave a positive limit, it is refused
//!   before anything is sent.
//!
//! The LTP is one touchline read (1502, no open-interest call) with the
//! session's market-data token, whoever places the order; it runs before
//! the order is sent (`outcome::before_send`), so its failure never reads
//! as the order's answer. The web read a fixed user's token here until
//! #2466; the desktop never ported that and sent MARKET unpriced.

use super::{data, mapping, XtsBroker};
use crate::brokers::common::mapping::{Action, Exchange, PriceType};
use crate::brokers::common::mpp::{self, StopLimit};
use crate::brokers::common::outcome;
use crate::brokers::types::{AuthToken, QuoteKey, ResolvedModify, ResolvedOrder};
use crate::error::{AppError, Result};

/// The `orderType` and limit price an order goes out with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Priced {
    pub order_type: &'static str,
    pub limit: f64,
}

impl Priced {
    /// The order as the trader set it: its own type and price.
    pub fn as_set(pricetype: PriceType, price: f64) -> Self {
        Self {
            order_type: mapping::order_type(pricetype),
            limit: price,
        }
    }

    const MARKET: Priced = Priced {
        order_type: "MARKET",
        limit: 0.0,
    };
}

/// What the pricing reads of an order to place, to modify, or to square
/// off.
pub struct Pricing<'a> {
    pub symbol: &'a str,
    pub exchange: Exchange,
    pub action: Action,
    pub pricetype: PriceType,
    pub price: f64,
    pub trigger: f64,
    /// The master contract's tick size; 0 when it has none.
    pub tick: f64,
}

impl<'a> Pricing<'a> {
    pub fn of_order(o: &'a ResolvedOrder) -> Self {
        Self {
            symbol: &o.symbol,
            exchange: o.exchange,
            action: o.action,
            pricetype: o.pricetype,
            price: o.price,
            trigger: o.trigger_price,
            tick: o.instrument.tick_size,
        }
    }

    pub fn of_modify(m: &'a ResolvedModify) -> Self {
        Self {
            symbol: &m.symbol,
            exchange: m.exchange,
            action: m.action,
            pricetype: m.pricetype,
            price: m.price,
            trigger: m.trigger_price,
            tick: m.instrument.tick_size,
        }
    }
}

fn usable_tick(tick: f64) -> Option<f64> {
    (tick.is_finite() && tick > 0.0).then_some(tick)
}

/// SL-M as a STOPLIMIT beyond its own trigger, or refused before sending
/// (web `protected_order_type`, its SL-M branch).
pub fn stop_limit(
    symbol: &str,
    exchange: Exchange,
    action: Action,
    trigger: f64,
    tick: f64,
) -> Result<Priced> {
    if !trigger.is_finite() || trigger <= 0.0 {
        return Err(AppError::Validation(
            "An SL-M order needs a trigger price above zero.".into(),
        ));
    }
    match mpp::stop_limit_off_trigger(symbol, action, trigger, tick)? {
        StopLimit::Priced(limit) => {
            tracing::info!(
                "Price protection: SL-M sent as a stop-limit for {} on {}, trigger {}, limit {}",
                symbol,
                exchange,
                trigger,
                limit
            );
            Ok(Priced {
                order_type: "STOPLIMIT",
                limit,
            })
        }
        StopLimit::NoTick => Err(AppError::Validation(format!(
            "OpenAlgo cannot price the SL-M order: the tick size of {} on {} is missing from the master contract. Download the master contract again, or place an SL order with your own limit price.",
            symbol, exchange
        ))),
        StopLimit::TooLow => Err(AppError::Validation(format!(
            "The trigger price {} is too low for an SL-M sell on {}: there is no limit price below it. Place an SL order with your own limit price instead.",
            trigger, symbol
        ))),
    }
}

/// MARKET as a LIMIT off `ltp` (web `protected_order_type`, its MARKET
/// branch); MARKET as it is without a tick size or an LTP.
pub fn market_limit(symbol: &str, action: Action, ltp: f64, tick: f64) -> Result<Priced> {
    let Some(tick) = usable_tick(tick) else {
        return Ok(Priced::MARKET);
    };
    if ltp <= 0.0 {
        return Ok(Priced::MARKET);
    }
    let limit = mpp::beyond(ltp, action, mpp::instrument_type_from_symbol(symbol), tick)?;
    // A SELL off the lowest-priced contracts can floor to zero: the
    // smallest price there is, one tick, is still at or below the LTP.
    Ok(Priced {
        order_type: "LIMIT",
        limit: limit.max(tick),
    })
}

/// The LTP to price a MARKET order off, or 0 when it cannot be read (the
/// order then goes out as MARKET, as on the web).
async fn ltp(b: &XtsBroker, auth: &AuthToken, symbol: &str, exchange: Exchange) -> f64 {
    let key = QuoteKey::new(exchange.as_str(), symbol);
    match outcome::before_send(data::get_ltp(b, auth, &key)).await {
        Ok(ltp) => ltp,
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Price protection: no LTP for {} on {} ({}); MARKET sent unpriced",
                symbol,
                exchange,
                e.code()
            );
            0.0
        }
    }
}

/// The type and price `p` goes out with: protected for MARKET and SL-M on
/// a member that asks for it, as set otherwise.
pub async fn price(b: &XtsBroker, auth: &AuthToken, p: &Pricing<'_>) -> Result<Priced> {
    if !b.cfg.hooks.price_protection {
        return Ok(Priced::as_set(p.pricetype, p.price));
    }
    match p.pricetype {
        PriceType::SlM => stop_limit(p.symbol, p.exchange, p.action, p.trigger, p.tick),
        PriceType::Market => {
            if usable_tick(p.tick).is_none() {
                tracing::warn!(
                    broker = b.cfg.id,
                    "Price protection: no tick size for {} on {}; MARKET sent unpriced",
                    p.symbol,
                    p.exchange
                );
                return Ok(Priced::MARKET);
            }
            let ltp = ltp(b, auth, p.symbol, p.exchange).await;
            market_limit(p.symbol, p.action, ltp, p.tick)
        }
        _ => Ok(Priced::as_set(p.pricetype, p.price)),
    }
}

/// A square-off is a MARKET order, priced like one placed through the API
/// (web `close_all_positions`). Pricing never stops a square-off: a
/// position whose instrument is not in the master, or whose price cannot
/// be worked out, goes out as MARKET.
pub async fn square_off(
    b: &XtsBroker,
    auth: &AuthToken,
    segment: &str,
    token: &str,
    action: Action,
) -> Priced {
    if !b.cfg.hooks.price_protection {
        return Priced::MARKET;
    }
    let exchange = mapping::oa_exchange(segment);
    let row = b.resolver().by_token(&exchange, token);
    let (Some(row), Ok(ex)) = (row, exchange.parse::<Exchange>()) else {
        return Priced::MARKET;
    };
    let p = Pricing {
        symbol: &row.symbol,
        exchange: ex,
        action,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger: 0.0,
        tick: row.tick_size,
    };
    match price(b, auth, &p).await {
        Ok(priced) => priced,
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Price protection: square-off of {} on {} not priced ({}); sent as MARKET",
                row.symbol,
                ex,
                e.code()
            );
            Priced::MARKET
        }
    }
}
