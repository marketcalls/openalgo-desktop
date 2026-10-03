//! Batch A direct-login and redirect brokers (AliceBlue, Definedge, mStock,
//! Motilal Oswal, Samco) end to end against a local fake broker on an
//! ephemeral loopback port: every login variant, order bodies, books,
//! funds, margin, quotes, depth, history and master contracts. One module
//! per broker under `brokers_direct_batch_a/`.

#[path = "brokers_direct_batch_a/support.rs"]
mod support;

#[path = "brokers_direct_batch_a/aliceblue.rs"]
mod aliceblue;
#[path = "brokers_direct_batch_a/definedge.rs"]
mod definedge;
#[path = "brokers_direct_batch_a/motilal.rs"]
mod motilal;
#[path = "brokers_direct_batch_a/mstock.rs"]
mod mstock;
#[path = "brokers_direct_batch_a/samco.rs"]
mod samco;
