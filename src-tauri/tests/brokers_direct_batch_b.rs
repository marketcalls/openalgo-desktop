//! Direct-login batch B brokers (Tradejini, 5paisa, Nubra, INDmoney,
//! IIFL Capital) end to end against local fakes on ephemeral ports: sign-in,
//! order bodies, books normalised to OpenAlgo symbols, funds, margin,
//! quotes, depth, history chunking, master contracts and feeds.
//!
//! One module per broker; each owns its fake server and its fixtures under
//! `tests/fixtures/brokers/<broker>/`.

#[path = "brokers_direct_batch_b/fivepaisa.rs"]
mod fivepaisa;
#[path = "brokers_direct_batch_b/iiflcapital.rs"]
mod iiflcapital;
#[path = "brokers_direct_batch_b/indmoney.rs"]
mod indmoney;
#[path = "brokers_direct_batch_b/nubra.rs"]
mod nubra;
#[path = "brokers_direct_batch_b/tradejini.rs"]
mod tradejini;
