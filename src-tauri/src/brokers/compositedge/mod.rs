//! CompositEdge (web `broker/compositedge`): OAuth front door
//! (`{interactive}/thirdparty`, callback `session` JSON with `accessToken`,
//! `brlogin.py:188-241`), six master segments (`mc:93`).

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "compositedge",
    name: "compositedge",
    base_url: "https://xts.compositedge.com",
    interactive_path: "/interactive",
    md_rest_path: "/apimarketdata",
    socket_path: "/apimarketdata/socket.io",
    socket_login_path: "/apibinarymarketdata/auth/login",
    socket_login_source: true,
    subscription_path: "/apimarketdata",
    broadcast_mode: "FULL",
    login: XtsLogin::OAuthAccessToken,
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
        data_stall_watchdog: false,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
