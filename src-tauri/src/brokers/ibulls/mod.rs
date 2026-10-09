//! IBulls (web `broker/ibulls`): XTS in code although the skill table lists
//! it under Noren (audit D-xts 0). The web's MARKET-to-LIMIT rewrite with a
//! hard-coded user fallback (`transform_data.py:22-75`) is not ported.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "ibulls",
    name: "IBulls",
    base_url: "https://xts.ibullssecurities.com",
    interactive_path: "/interactive",
    md_rest_path: "/apibinarymarketdata",
    socket_path: "/apimarketdata/socket.io",
    socket_login_path: "/apibinarymarketdata/auth/login",
    socket_login_source: true,
    subscription_path: "/apibinarymarketdata",
    broadcast_mode: "FULL",
    login: XtsLogin::Direct,
    supported_exchanges: &[
        Exchange::Nse,
        Exchange::Bse,
        Exchange::Nfo,
        Exchange::Bfo,
        Exchange::Mcx,
        Exchange::NseIndex,
        Exchange::BseIndex,
    ],
    master_segments: &["NSECM", "NSEFO", "BSECM", "BSEFO", "MCXFO"],
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
