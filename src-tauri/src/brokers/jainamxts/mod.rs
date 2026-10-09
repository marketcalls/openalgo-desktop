//! Jainam XTS (web `broker/jainamxts`): binary market-data host only
//! (`baseurl.py:7`, `ws:25`), interactive login sends `accessToken`
//! (`auth_api.py:23-27`), `xts-binary-packet` decoder (`ws:580-1054`).

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{BinaryDecoder, XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "jainamxts",
    name: "jainamxts",
    base_url: "https://jtrade.jainam.in:5000",
    interactive_path: "/interactive",
    md_rest_path: "/apibinarymarketdata",
    socket_path: "/apibinarymarketdata/socket.io",
    socket_login_path: "/apibinarymarketdata/auth/login",
    socket_login_source: true,
    subscription_path: "/apibinarymarketdata",
    broadcast_mode: "FULL",
    login: XtsLogin::DirectAccessToken,
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
    binary_decoder: Some(BinaryDecoder::Jainam),
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
