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
//! * [`inventory`]: the folders and the lists agree, and every case reads.
//!
//! The case format and how to add one: `tests/fixtures/feed_corpus/README.md`.

mod harness;
mod inventory;

mod angel;
mod definedge;
mod deltaexchange;
mod dhan;
mod dhan_sandbox;
mod fivepaisa;
mod fyers;
mod motilal;
mod mstock;
mod nubra;
mod samco;
mod tradejini;
mod upstox;
mod zerodha;

/// Every broker with cases, and its cases (the folder names).
const BROKERS: &[(&str, &[&str])] = &[
    ("angel", angel::CASES),
    ("definedge", definedge::CASES),
    ("deltaexchange", deltaexchange::CASES),
    ("dhan", dhan::CASES),
    ("dhan_sandbox", dhan_sandbox::CASES),
    ("fivepaisa", fivepaisa::CASES),
    ("fyers", fyers::CASES),
    ("motilal", motilal::CASES),
    ("mstock", mstock::CASES),
    ("nubra", nubra::CASES),
    ("samco", samco::CASES),
    ("tradejini", tradejini::CASES),
    ("upstox", upstox::CASES),
    ("zerodha", zerodha::CASES),
];
