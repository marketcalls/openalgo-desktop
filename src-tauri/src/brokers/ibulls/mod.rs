//! IBulls (web `broker/ibulls`), an XTS member. MARKET and SL-M go out
//! priced with Market Price Protection (`families::xts::protection`, web
//! #2466): its RMS may refuse a native MARKET or SL-M from an API session.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::xts::{EventCode, XtsBroker, XtsConfig, XtsHooks, XtsLogin};

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
        split_refused_batch: crate::brokers::families::xts::SplitRefused::Never,
        data_stall_watchdog: false,
        event_code: EventCode::Payload,
        text_1105_as_ltp: false,
        text_1105_doubles: false,
        touchline_by_holding: false,
        payload_variants: false,
        oi_events: false,
        price_protection: true,
        history_window_days: 6,
    },
};

pub fn broker(symbols: SymbolResolver) -> XtsBroker {
    XtsBroker::new(&CONFIG, symbols)
}
