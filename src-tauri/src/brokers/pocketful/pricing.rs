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
use crate::brokers::common::mpp::{
    beyond, instrument_type_from_symbol, stop_limit_off_trigger, StopLimit,
};
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

/// The instrument's tick size, `None` when missing or not a positive number.
fn tick_size(row: &SymToken) -> Option<f64> {
    (row.tick_size.is_finite() && row.tick_size > 0.0).then_some(row.tick_size)
}

/// The SL a SL-M goes out as (web `protected_order_type`, SL-M branch).
pub(crate) fn stop_market(ask: &Ask<'_>) -> Result<Priced> {
    let trigger = ask.trigger_price;
    if !trigger.is_finite() || trigger <= 0.0 {
        return Err(outcome::refusal(
            "An SL-M order needs a trigger price above zero.",
        ));
    }
    let limit = match stop_limit_off_trigger(
        ask.symbol,
        ask.action,
        trigger,
        ask.instrument.tick_size,
    )? {
        StopLimit::Priced(limit) => limit,
        StopLimit::NoTick => {
            return Err(outcome::refusal(format!(
                "OpenAlgo cannot price the SL-M order: the tick size of {} on {} is missing from the master contract. Download the master contract again, or place an SL order with your own limit price.",
                ask.symbol, ask.exchange
            )))
        }
        StopLimit::TooLow => {
            return Err(outcome::refusal(format!(
                "The trigger price {} is too low for an SL-M sell on {}: there is no limit price below it. Place an SL order with your own limit price instead.",
                trigger, ask.symbol
            )))
        }
    };
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
    let instrument_type = instrument_type_from_symbol(ask.symbol);
    let limit = beyond(ltp, ask.action, instrument_type, tick)?.max(tick);
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
