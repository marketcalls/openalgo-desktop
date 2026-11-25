//! The order type and price an order goes out with (web
//! `mapping/transform_data.py` `protected_order_type`).
//!
//! The annexure allows only LIMIT and SL from the API ("MARKET and SL-M
//! orders are restricted / not allowed"), so OpenAlgo prices both itself
//! with the shared Market Price Protection slabs (`common::mpp`):
//!
//! * MARKET -> LIMIT at the last price moved by the slab, in the direction
//!   the order fills (above for a BUY, below for a SELL), at least one tick
//!   past the last price, on a tick, and never below one tick.
//! * SL-M -> SL whose limit is the trigger moved by the slab, at least one
//!   tick past it and on a tick. It is priced off its own trigger, never the
//!   last price, so the limit is always on the side the order fills from.
//!
//! An order that cannot be priced (no tick size in the master, no last
//! price, an SL-M sell too low to leave a positive limit) is refused before
//! anything is sent: Pocketful would refuse the native MARKET or SLM anyway.

use super::{market, PocketfulBroker};
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::mpp::{instrument_type_from_symbol, mpp_percentage, py_round};
use crate::brokers::common::outcome;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::{AuthToken, ResolvedModify, ResolvedOrder};
use crate::error::{AppError, Result};

/// What pricing needs of a place or a modify.
pub(crate) struct Ask<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub action: Action,
    pub pricetype: PriceType,
    pub price: f64,
    pub trigger_price: f64,
    pub instrument: &'a SymToken,
}

impl<'a> Ask<'a> {
    pub fn order(o: &'a ResolvedOrder) -> Self {
        Self {
            symbol: &o.symbol,
            exchange: o.exchange.as_str(),
            action: o.action,
            pricetype: o.pricetype,
            price: o.price,
            trigger_price: o.trigger_price,
            instrument: &o.instrument,
        }
    }

    pub fn modify(m: &'a ResolvedModify) -> Self {
        Self {
            symbol: &m.symbol,
            exchange: m.exchange.as_str(),
            action: m.action,
            pricetype: m.pricetype,
            price: m.price,
            trigger_price: m.trigger_price,
            instrument: &m.instrument,
        }
    }
}

/// What goes out: Pocketful's `order_type`, the limit price and the
/// trigger (0 unless the order type is SL).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Priced {
    pub order_type: &'static str,
    pub price: f64,
    pub trigger_price: f64,
}

/// Decimal places a tick size implies (0.05 -> 2, 0.0025 -> 4, 1 -> 0).
fn tick_decimals(tick: f64) -> i32 {
    let text = format!("{}", tick);
    text.split_once('.')
        .map_or(0, |(_, frac)| frac.len() as i32)
}

/// `value` on a multiple of `tick`, rounded down (`up` false) or up. The
/// ratio is tamed to 6 places first, as the web does, so float noise
/// (804.5025 / 0.05) does not move it a tick.
fn snap(value: f64, tick: f64, up: bool) -> f64 {
    let ratio = py_round(value / tick, 6);
    let k = if up { ratio.ceil() } else { ratio.floor() };
    py_round(k * tick, tick_decimals(tick))
}

/// A limit `pct` past `reference` in the fill direction, at least one tick
/// past it and on a tick (web `_beyond`).
pub(crate) fn beyond(reference: f64, action: Action, pct: f64, tick: f64) -> f64 {
    match action {
        Action::Buy => snap((reference * (1.0 + pct)).max(reference + tick), tick, true),
        Action::Sell => snap((reference * (1.0 - pct)).min(reference - tick), tick, false),
    }
}

/// The instrument's tick size, `None` when missing or not a positive number.
fn tick_size(row: &SymToken) -> Option<f64> {
    (row.tick_size.is_finite() && row.tick_size > 0.0).then_some(row.tick_size)
}

fn pct(price: f64, symbol: &str) -> Result<f64> {
    Ok(mpp_percentage(price, instrument_type_from_symbol(symbol))? / 100.0)
}

/// The SL a SL-M goes out as (web `protected_order_type`, SL-M branch).
pub(crate) fn stop_market(ask: &Ask<'_>) -> Result<Priced> {
    let trigger = ask.trigger_price;
    if !(trigger.is_finite() && trigger > 0.0) {
        return Err(outcome::refusal(
            "An SL-M order needs a trigger price above zero.",
        ));
    }
    let Some(tick) = tick_size(ask.instrument) else {
        return Err(outcome::refusal(format!(
            "OpenAlgo cannot price the SL-M order: the tick size of {} on {} is missing from the master contract. Download the master contract again, or place an SL order with your own limit price.",
            ask.symbol, ask.exchange
        )));
    };
    let limit = beyond(trigger, ask.action, pct(trigger, ask.symbol)?, tick);
    if limit <= 0.0 {
        return Err(outcome::refusal(format!(
            "The trigger price {} is too low for an SL-M sell on {}: there is no limit price below it. Place an SL order with your own limit price instead.",
            trigger, ask.symbol
        )));
    }
    tracing::info!(
        "MPP: SL-M -> SL for {}:{}, trigger {}, limit {}",
        ask.exchange,
        ask.symbol,
        trigger,
        limit
    );
    Ok(Priced {
        order_type: "SL",
        price: limit,
        trigger_price: trigger,
    })
}

/// The LIMIT a MARKET order goes out as, from `ltp` (web
/// `protected_order_type`, MARKET branch).
pub(crate) fn market_limit(ask: &Ask<'_>, ltp: Option<f64>) -> Result<Priced> {
    let Some(tick) = tick_size(ask.instrument) else {
        return Err(outcome::refusal(format!(
            "OpenAlgo cannot price the market order: the tick size of {} on {} is missing from the master contract, and Pocketful takes only priced orders from the API. Download the master contract again, or place a LIMIT order.",
            ask.symbol, ask.exchange
        )));
    };
    let Some(ltp) = ltp.filter(|p| p.is_finite() && *p > 0.0) else {
        return Err(outcome::refusal(format!(
            "OpenAlgo could not get a live price for {} on {}, so the market order was not sent: Pocketful takes only priced orders from the API. Try again in a moment, or place a LIMIT order.",
            ask.symbol, ask.exchange
        )));
    };
    // A SELL off the lowest-priced contracts can floor to zero: the
    // smallest price there is, one tick, is still at or below the LTP.
    let limit = beyond(ltp, ask.action, pct(ltp, ask.symbol)?, tick).max(tick);
    tracing::info!(
        "MPP: MARKET -> LIMIT for {}:{}, LTP {}, limit {}",
        ask.exchange,
        ask.symbol,
        ltp,
        limit
    );
    Ok(Priced {
        order_type: "LIMIT",
        price: limit,
        trigger_price: 0.0,
    })
}

/// What `ask` goes out as. A MARKET order reads the last price first; that
/// read comes before the order is sent and never counts against its answer
/// (LOG-08).
pub(crate) async fn price(b: &PocketfulBroker, auth: &AuthToken, ask: &Ask<'_>) -> Result<Priced> {
    match ask.pricetype {
        PriceType::Limit => Ok(Priced {
            order_type: "LIMIT",
            price: ask.price,
            trigger_price: 0.0,
        }),
        PriceType::Sl => Ok(Priced {
            order_type: "SL",
            price: ask.price,
            trigger_price: ask.trigger_price,
        }),
        PriceType::SlM => stop_market(ask),
        PriceType::Market => {
            if tick_size(ask.instrument).is_none() {
                // Refused without asking for a price it could not use.
                return market_limit(ask, None);
            }
            let ltp = outcome::before_send(async {
                Ok::<_, AppError>(market::last_price(b, auth, ask.instrument).await)
            })
            .await?;
            market_limit(ask, ltp)
        }
    }
}
