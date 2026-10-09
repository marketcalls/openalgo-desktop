//! Shoonya (Finvasia), the canonical Noren member (web `broker/shoonya/`).
//! Bearer REST dialect, chart endpoints on the jKey form, GenAcsTok login,
//! quote-identity retries, per-interval history windows.

use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::*;

pub static HOOKS: NorenHooks = NorenHooks {
    parse_login: hooks::login_access_token,
    holding_qty: hooks::holding_qty_full,
    collateral: hooks::collateral_brkcollamt,
    margin_total: hooks::margin_used,
    cancel_error: hooks::cancel_message,
};

pub static CONFIG: NorenConfig = NorenConfig {
    id: "shoonya",
    name: "Shoonya",
    logo: "/logos/shoonya.svg",
    rest_url: "https://api.shoonya.com/NorenWClientAPI",
    ws_url: "wss://api.shoonya.com/NorenWSAPI/",
    dialect: Dialect::BearerJData,
    chart_dialect: Some(Dialect::JKeyForm),
    login: Login::GenAcsTok {
        authorize_url: "https://api.shoonya.com/OAuthlogin/authorize/oauth",
    },
    exchanges: NOREN_EXCHANGES,
    master_files: &[
        MasterFile {
            exchange: "NSE",
            url: "https://api.shoonya.com/NSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BSE",
            url: "https://api.shoonya.com/BSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "NFO",
            url: "https://api.shoonya.com/NFO_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "CDS",
            url: "https://api.shoonya.com/CDS_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "MCX",
            url: "https://api.shoonya.com/MCX_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BFO",
            url: "https://api.shoonya.com/BFO_symbols.txt.zip",
            zipped: true,
        },
    ],
    tick_rule: TickRule::CashInPaise,
    index_naming: IndexNaming::StripAndOverride,
    bse_indices: BseIndices::Manual,
    nse_index_brexchange: "NSE_INDEX",
    bfo_from_tsym: true,
    timeframes: TIMEFRAMES_WITH_4H,
    history_window_secs: Some(shoonya_history_window),
    eod_index_names: &[
        (("NSE_INDEX", "NIFTY"), "Nifty 50"),
        (("NSE_INDEX", "BANKNIFTY"), "Nifty Bank"),
        (("NSE_INDEX", "FINNIFTY"), "Nifty Financial Services"),
        (("NSE_INDEX", "MIDCPNIFTY"), "Nifty Midcap Select"),
        (("NSE_INDEX", "NIFTYNXT50"), "Nifty Next 50"),
        (("NSE_INDEX", "INDIAVIX"), "India VIX"),
    ],
    history_repair: true,
    eod_widen: false,
    today_bar_utc: true,
    quote_identity_retries: 3,
    multiquote_batch: 20,
    multiquote_delay_ms: 1000,
    mpp: MppScope::MarketAndStop,
    send_mkt_protection: true,
    place_remarks: None,
    modify_market_price_zero: false,
    funds_m2m: FundsM2m::Limits,
    margin: MarginApi::Basket,
    position_pnl: PositionPnl::NetAverage,
    tradebook_time_only: true,
    orderbook_price_fallback: false,
    depth_oi: false,
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
