//! The broker feed corpus (audit C09): for each broker, raw upstream
//! frames and a subscription history go through the broker's real decoder,
//! the feed manager, the bridge and the public 8765 server, and the
//! `market_data` and `order_update` frames that come out are compared with
//! the expected frames exactly: numbers and units, field presence,
//! timestamps, book clears, order constants. Where the case has the web's
//! output for the same frames (`web.json`), every difference from it must
//! be a deviation the case declares.
//!
//! * [`harness`]: the runner (see its module docs).
//! * One module per broker: how to build its feeds against the fake broker
//!   sockets, and its case list. Adding a case to a broker is a folder in
//!   `tests/fixtures/feed_corpus/<broker>/` and its name in that list;
//!   adding a broker is a module like these and a line in [`BROKERS`].
//! * [`xts`]: the XTS family's feed, which the seven members' modules
//!   build with their own configuration.
//! * [`inventory`]: the folders and the lists agree, and every case reads.
//!
//! The case format and how to add one: `tests/fixtures/feed_corpus/README.md`.

mod harness;
mod inventory;

mod aliceblue;
mod angel;
mod arrow;
mod compositedge;
mod definedge;
mod deltaexchange;
mod dhan;
mod dhan_sandbox;
mod firstock;
mod fivepaisa;
mod fivepaisaxts;
mod flattrade;
mod fyers;
mod groww;
mod hdfcsecurities;
mod hdfcsky;
mod ibulls;
mod iifl;
mod iiflcapital;
mod indmoney;
mod jainamxts;
mod kotak;
mod motilal;
mod mstock;
mod noren_family;
mod nubra;
mod paytm;
mod pocketful;
mod rmoney;
mod rupeezy;
mod sahi;
mod samco;
mod shoonya;
mod tradejini;
mod tradesmart;
mod upstox;
mod wisdom;
mod xts;
mod zebu;
mod zerodha;

/// Every broker with cases, and its cases (the folder names).
const BROKERS: &[(&str, &[&str])] = &[
    ("aliceblue", aliceblue::CASES),
    ("angel", angel::CASES),
    ("arrow", arrow::CASES),
    ("compositedge", compositedge::CASES),
    ("definedge", definedge::CASES),
    ("deltaexchange", deltaexchange::CASES),
    ("dhan", dhan::CASES),
    ("dhan_sandbox", dhan_sandbox::CASES),
    ("firstock", firstock::CASES),
    ("fivepaisa", fivepaisa::CASES),
    ("fivepaisaxts", fivepaisaxts::CASES),
    ("flattrade", flattrade::CASES),
    ("fyers", fyers::CASES),
    ("groww", groww::CASES),
    ("hdfcsecurities", hdfcsecurities::CASES),
    ("hdfcsky", hdfcsky::CASES),
    ("ibulls", ibulls::CASES),
    ("iifl", iifl::CASES),
    ("iiflcapital", iiflcapital::CASES),
    ("indmoney", indmoney::CASES),
    ("jainamxts", jainamxts::CASES),
    ("kotak", kotak::CASES),
    ("motilal", motilal::CASES),
    ("mstock", mstock::CASES),
    ("nubra", nubra::CASES),
    ("paytm", paytm::CASES),
    ("pocketful", pocketful::CASES),
    ("rmoney", rmoney::CASES),
    ("rupeezy", rupeezy::CASES),
    ("sahi", sahi::CASES),
    ("samco", samco::CASES),
    ("shoonya", shoonya::CASES),
    ("tradejini", tradejini::CASES),
    ("tradesmart", tradesmart::CASES),
    ("upstox", upstox::CASES),
    ("wisdom", wisdom::CASES),
    ("zebu", zebu::CASES),
    ("zerodha", zerodha::CASES),
];

/// Broker modules that do not build their session's broker yet: their
/// cases run without `Feeds::with_broker`, so the broker's refusals,
/// pre-subscribe step and order completion are not wired as a session
/// wires them. A ratchet the broker lanes empty: nothing joins it, a module
/// that passes its broker must leave it (the harness fails either way), and
/// the release gate (`scripts/feed_corpus/release_gate.py`) fails while it
/// is not empty.
const WITHOUT_BROKER: &[&str] = &[
    "aliceblue",
    "angel",
    "arrow",
    "compositedge",
    "deltaexchange",
    "dhan_sandbox",
    "firstock",
    "fivepaisaxts",
    "fyers",
    "hdfcsecurities",
    "hdfcsky",
    "ibulls",
    "iifl",
    "iiflcapital",
    "jainamxts",
    "kotak",
    "nubra",
    "paytm",
    "pocketful",
    "rmoney",
    "rupeezy",
    "upstox",
    "wisdom",
    "zerodha",
];
