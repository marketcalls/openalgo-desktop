//! Batch A direct-login and redirect brokers (AliceBlue, Definedge, mStock,
//! Motilal Oswal, Samco) end to end against a local fake broker on an
//! ephemeral loopback port: every login variant, order bodies, books,
//! funds, margin, quotes, depth, history, master contracts and order feeds.
//!
//! One module per broker; fixtures live under
//! `tests/fixtures/brokers/<broker>/`.

mod support;

mod aliceblue;
mod definedge;
mod motilal;
mod mstock;
mod samco;
