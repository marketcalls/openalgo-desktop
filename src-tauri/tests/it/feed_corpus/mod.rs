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
mod dhan;
mod firstock;
mod flattrade;
mod ibulls;
mod iifl;
mod samco;
mod shoonya;
mod tradesmart;
mod wisdom;
mod xts;
mod zebu;
mod zerodha;

/// Every broker with cases, and its cases (the folder names).
const BROKERS: &[(&str, &[&str])] = &[
    ("aliceblue", aliceblue::CASES),
    ("angel", angel::CASES),
    ("dhan", dhan::CASES),
    ("firstock", firstock::CASES),
    ("flattrade", flattrade::CASES),
    ("ibulls", ibulls::CASES),
    ("iifl", iifl::CASES),
    ("samco", samco::CASES),
    ("shoonya", shoonya::CASES),
    ("tradesmart", tradesmart::CASES),
    ("wisdom", wisdom::CASES),
    ("zebu", zebu::CASES),
    ("zerodha", zerodha::CASES),
];
