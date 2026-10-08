//! Direct-login batch B brokers (Tradejini, 5paisa, Nubra, INDmoney,
//! IIFL Capital) end to end against local fakes on ephemeral ports: sign-in,
//! order bodies, books normalised to OpenAlgo symbols, funds, margin,
//! quotes, depth, history chunking, master contracts and feeds.
//!
//! One module per broker; each owns its fake server and its fixtures under
//! `tests/fixtures/brokers/<broker>/`.

mod fivepaisa;
mod iiflcapital;
mod indmoney;
mod nubra;
mod secrets;
mod tradejini;
