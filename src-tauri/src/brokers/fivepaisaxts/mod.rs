//! 5paisa XTS (web `broker/fivepaisaxts`, the family template). `baseurl.py:4` has a trailing
//! slash that the web turns into `//apimarketdata`; the config drops it.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "fivepaisaxts",
    name: "5paisa (XTS)",
    base_url: "https://xtsmum.5paisa.com",
    interactive_path: "/interactive",
    md_rest_path: "/apimarketdata",
    socket_path: "/apimarketdata/socket.io",
    socket_login_path: "/apibinarymarketdata/auth/login",
    socket_login_source: true,
    subscription_path: "/apimarketdata",
    broadcast_mode: "FULL",
    login: XtsLogin::Direct,
    supported_exchanges: &[
        Exchange::Nse,
        Exchange::Bse,
        Exchange::Nfo,
        Exchange::Bfo,
        Exchange::NseIndex,
        Exchange::BseIndex,
    ],
    master_segments: &["NSECM", "NSEFO", "BSECM", "BSEFO"],
    stream_mode_codes: [1512, 1501, 1502],
    binary_decoder: None,
    hooks: XtsHooks {
        funds_balance_header: None,
        margin_details: false,
        multiquote_oi: false,
        split_duplicate_batch: false,
        data_stall_watchdog: true,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
