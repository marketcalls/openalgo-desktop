//! 5paisa XTS (web `broker/fivepaisaxts`, the family template). Its base
//! URL has no trailing slash, as the web's has had since #2504.
//!
//! Since web #2490 and #2504 it departs from the template where the other
//! members' web code has not moved: it reads XTS answers as the XTS v2
//! docs describe them, asks for history 365 days at a time and takes the
//! consolidated `ALL|ALL|ALL` funds entry.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{EventCode, XtsBroker, XtsConfig, XtsHooks, XtsLogin};

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
        funds_balance_header: Some("ALL|ALL|ALL"),
        margin_details: false,
        multiquote_oi: false,
        split_refused_batch: crate::brokers::families::xts::SplitRefused::Never,
        data_stall_watchdog: true,
        event_code: EventCode::Payload,
        text_1105_as_ltp: false,
        text_1105_doubles: false,
        touchline_by_holding: false,
        payload_variants: false,
        oi_events: false,
        price_protection: false,
        answers_as_documented: true,
        history_window_days: 365,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
