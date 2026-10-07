//! OAuth-redirect broker batch (Arrow, Paytm Money, Pocketful, HDFC Sky,
//! HDFC Securities) end to end against local fake brokers on ephemeral
//! ports. One module per broker.

#[path = "brokers_oauth_batch/arrow.rs"]
mod arrow;
#[path = "brokers_oauth_batch/hdfcsecurities.rs"]
mod hdfcsecurities;
#[path = "brokers_oauth_batch/hdfcsky.rs"]
mod hdfcsky;
#[path = "brokers_oauth_batch/paytm.rs"]
mod paytm;
#[path = "brokers_oauth_batch/pocketful.rs"]
mod pocketful;
