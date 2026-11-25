//! 5paisa XTS after web 19f993c92 and 5749d4126 (marketcalls/openalgo#2490,
//! #2504), against a local fake XTS (`fake`): history a year at a time and
//! XTS answers read as the XTS v2 docs describe them. Ported from web
//! test/test_fivepaisaxts_xts_docs.py; each test names the web test it
//! ports. The other XTS members keep the template (`brokers_xts.rs`).

mod answers;
mod fake;
mod history;
