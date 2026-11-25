//! Brokerage and statutory charges for one sandbox fill (web
//! `sandbox/charges.py`, marketcalls/openalgo#2496).
//!
//! No I/O of any kind: no database, no market data, no clock and no logging,
//! by the same rule as the risk core. Every input arrives as an argument and
//! the result leaves as a return value, so the fill, the calculator, the
//! transaction cost report and `/api/v1/charges` all reach the same figures,
//! and the arithmetic is held to a published brokerage calculator's figures
//! (`tests/fixtures/sandbox/charges_reference.json`) without a running app.
//!
//! What a fill is charged:
//! * brokerage under the trader's own plan, per segment: a flat amount per
//!   executed order and a percentage of the trade value, the lower of the two
//!   when both are set ([`brokerage_for`]);
//! * the statutory schedule the exchanges and the government publish, the
//!   same at every broker: STT (CTT on commodities) on the side the law
//!   names, exchange transaction charges (the NSE family's include the
//!   Investor Protection Fund Trust levy), SEBI turnover fees, stamp duty on
//!   the buy, and GST on brokerage, exchange and SEBI lines.
//!
//! Rounding is a contract note's: STT and stamp duty to the nearest rupee,
//! CTT and every other line to the paisa, the exchange charge and its levy
//! rounded apart and added, and a total that is the sum of its lines. A fill
//! is one note ([`compute_charges`]); the calculator prices a buy and a sell
//! as one note for the two ([`estimate_round_trip`]).
//!
//! Not modelled (as on the web): depository charges on a delivery sell; STT
//! on an option exercised at expiry (expiry settlement places no order); a
//! same-day delivery buy and sell pays delivery rates on both sides;
//! agricultural commodities and NCDEX use the MCX rates; BSE scrip groups
//! other than the standard rate. Crypto is charged nothing until the trader
//! enters a venue fee, and nothing is added to that fee.
//!
//! | File | What |
//! |---|---|
//! | `rates.rs` | the rate card: segments, markets, which segment an order is in |
//! | `note.rs` | the arithmetic: rounding, brokerage, one note, a round trip |
//! | `plan.rs` | the trader's plan: config keys, defaults, validation, wording |

mod note;
mod plan;
mod rates;

pub use note::{
    brokerage_for, compute_charges, estimate_round_trip, paisa, rupee, split_exchange_charges,
    Charges, RoundTrip, CHARGE_COLUMNS,
};
pub use plan::{
    brokerage_flat_key, brokerage_percent_key, config_keys, default_config_rows,
    default_config_values, describe_brokerage, is_enabled, parse_amount, plain, to_decimal,
    validate_brokerage, validate_config_value, Plan, CHARGES_ENABLED_KEY, MAX_BROKERAGE_FLAT,
    MAX_BROKERAGE_PERCENT,
};
pub use rates::{
    classify_segment, market_for_broker_type, segment, segments_for, Market, Segment, SEGMENTS,
    STATUTORY_SCHEDULE_AS_OF,
};

#[cfg(test)]
mod tests;
