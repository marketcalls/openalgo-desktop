//! RMoney (web `broker/rmoney`): OAuth callback already carries the
//! session token (`brlogin.py:921-943`); socket client on `/apimarketdata`
//! with a login without `source` (`ws:42-43,252-255`), LTP mode on 1501
//! (`ws:65-70`), funds from `ALL|ALL|ALL` (`funds.py:29-38`), margin via
//! `/orders/margindetails` (`margin_api.py`), OI merged into multiquotes.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{BinaryDecoder, XtsBroker, XtsConfig, XtsHooks, XtsLogin};

pub static CONFIG: XtsConfig = XtsConfig {
    id: "rmoney",
    name: "RMoney",
    base_url: "https://xts.rmoneyindia.co.in:3000",
    interactive_path: "/interactive",
    md_rest_path: "/apibinarymarketdata",
    socket_path: "/apimarketdata/socket.io",
    socket_login_path: "/apimarketdata/auth/login",
    socket_login_source: false,
    subscription_path: "/apimarketdata",
    broadcast_mode: "Full",
    login: XtsLogin::OAuthSessionToken,
    supported_exchanges: &[
        Exchange::Nse,
        Exchange::Bse,
        Exchange::Nfo,
        Exchange::Bfo,
        Exchange::NseIndex,
        Exchange::BseIndex,
    ],
    master_segments: &["NSECM", "NSEFO", "BSECM", "BSEFO"],
    stream_mode_codes: [1501, 1501, 1502],
    binary_decoder: Some(BinaryDecoder::Rmoney),
    hooks: XtsHooks {
        funds_balance_header: Some("ALL|ALL|ALL"),
        margin_details: true,
        multiquote_oi: true,
        split_duplicate_batch: true,
        data_stall_watchdog: false,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
