//! OAuth-redirect broker batch (Arrow, Paytm Money, Pocketful, HDFC Sky,
//! HDFC Securities) end to end against local fake brokers on ephemeral
//! ports. One module per broker.

#[path = "brokers_oauth_batch/arrow.rs"]
mod arrow;
