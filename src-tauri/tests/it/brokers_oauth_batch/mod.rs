//! OAuth-redirect broker batch (Arrow, Paytm Money, Pocketful, HDFC Sky,
//! HDFC Securities) end to end against local fake brokers on ephemeral
//! ports. One module per broker.

mod arrow;
mod hdfcsecurities;
mod hdfcsky;
mod paytm;
mod pocketful;
