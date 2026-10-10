//! Zebu (MYNT), a Noren member (web `broker/zebu/`). Shoonya's dialect on
//! `go.mynt.in`; MPP on MARKET only, raw tick sizes, exact-name index
//! table, no BSE indices, no margin endpoint, single history request.

use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::*;

pub static HOOKS: NorenHooks = NorenHooks {
    parse_login: hooks::login_access_token,
    holding_qty: hooks::holding_qty_npoadt1,
    collateral: hooks::collateral_brkcollamt,
    margin_total: hooks::margin_used,
    cancel_error: hooks::cancel_message,
};

/// `plugin.json`: no BSE_INDEX.
pub const EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::NseIndex,
];

pub static CONFIG: NorenConfig = NorenConfig {
    id: "zebu",
    name: "Zebu",
    logo: "/logos/zebu.svg",
    rest_url: "https://go.mynt.in/NorenWClientAPI",
    ws_url: "wss://go.mynt.in/NorenWSAPI/",
    dialect: Dialect::BearerJData,
    chart_dialect: None,
    login: Login::GenAcsTok {
        authorize_url: "https://go.mynt.in/OAuthlogin/authorize/oauth",
    },
    exchanges: EXCHANGES,
    master_files: &[
        MasterFile {
            exchange: "NSE",
            url: "https://go.mynt.in/NSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BSE",
            url: "https://go.mynt.in/BSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "NFO",
            url: "https://go.mynt.in/NFO_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "CDS",
            url: "https://go.mynt.in/CDS_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "MCX",
            url: "https://go.mynt.in/MCX_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BFO",
            url: "https://go.mynt.in/BFO_symbols.txt.zip",
            zipped: true,
        },
    ],
    tick_rule: TickRule::Raw,
    index_naming: IndexNaming::ExactName,
    bse_indices: BseIndices::Absent,
    nse_index_brexchange: "NSE",
    index_instrument_type: "INDEX",
    bse_drop_without_exchange: false,
    bfo_from_tsym: true,
    timeframes: TIMEFRAMES_WITH_4H,
    history_window_secs: None,
    eod_index_names: &[],
    history_repair: false,
    eod_widen: false,
    strict_candles: false,
    today_bar_utc: false,
    quote_identity_retries: 0,
    multiquote_batch: 10,
    multiquote_delay_ms: 1000,
    mpp: MppScope::MarketOnly,
    send_mkt_protection: true,
    place_remarks: None,
    modify_market_price_zero: true,
    funds_m2m: FundsM2m::Limits,
    margin: MarginApi::Unsupported,
    margin_mpp: MarginMpp::LtpOrSupplied,
    position_pnl: PositionPnl::NetAverage,
    tradebook_time_only: false,
    orderbook_price_fallback: false,
    depth_oi: true,
    rate: RateLimits {
        order: None,
        data: None,
        quote: None,
    },
    persistent_socket: false,
    order_feed_subscribe: true,
    hooks: &HOOKS,
};

pub fn config() -> &'static NorenConfig {
    &CONFIG
}

pub fn broker(symbols: SymbolResolver) -> NorenBroker {
    NorenBroker::new(&CONFIG, symbols)
}
