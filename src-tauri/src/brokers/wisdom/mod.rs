//! Wisdom Capital (web `broker/wisdom`): XTS in code although the skill
//! table lists it under Noren (audit D-xts 0).

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "wisdom",
    name: "Wisdom Capital (XTS)",
    base_url: "https://trade.wisdomcapital.in",
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
        Exchange::Cds,
        Exchange::Mcx,
        Exchange::NseIndex,
        Exchange::BseIndex,
    ],
    master_segments: &["NSECM", "NSECD", "NSEFO", "BSECM", "BSEFO", "MCXFO"],
    stream_mode_codes: [1512, 1501, 1502],
    binary_decoder: None,
    hooks: XtsHooks {
        funds_balance_header: None,
        margin_details: false,
        multiquote_oi: false,
        split_duplicate_batch: false,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
