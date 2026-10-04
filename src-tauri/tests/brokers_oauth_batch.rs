//! OAuth-redirect broker adapters end to end against local fake brokers
//! (ephemeral ports): sign-in, orders, books, funds, quotes and master
//! contracts. One module per broker.

#[path = "brokers_oauth_batch/paytm.rs"]
mod paytm;
