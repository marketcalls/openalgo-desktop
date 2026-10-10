//! TradeSmart (Noren v2), a Noren member (web `broker/tradesmart/`).
//! Bearer dialect on `/NorenWClientAPIv2`, `remarks:"openalgo"` and no
//! `mkt_protection` on PlaceOrder, always-convert MPP, per-leg
//! `GetOrderMargin`, funds M2M from the position book, no 4h history,
//! a two-bucket rate limiter, order updates pushed without `{"t":"o"}`.

use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::*;

pub static HOOKS: NorenHooks = NorenHooks {
    parse_login: hooks::login_any,
    holding_qty: hooks::holding_qty_npoad,
    collateral: hooks::collateral_brkcollamt,
    margin_total: hooks::order_margin,
    cancel_error: hooks::cancel_emsg,
};

pub static CONFIG: NorenConfig = NorenConfig {
    id: "tradesmart",
    name: "TradeSmart",
    logo: "/logos/tradesmart.svg",
    rest_url: "https://v2api.tradesmartonline.in/NorenWClientAPIv2",
    ws_url: "wss://v2api.tradesmartonline.in/NorenWSAPI/",
    dialect: Dialect::BearerJData,
    chart_dialect: None,
    login: Login::GenAcsTok {
        authorize_url: "https://v2api.tradesmartonline.in/OAuthlogin/authorize/oauth",
    },
    exchanges: NOREN_EXCHANGES,
    master_files: &[
        MasterFile {
            exchange: "NSE",
            url: "https://v2api.tradesmartonline.in/NSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BSE",
            url: "https://v2api.tradesmartonline.in/BSE_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "NFO",
            url: "https://v2api.tradesmartonline.in/NFO_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "CDS",
            url: "https://v2api.tradesmartonline.in/CDS_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "MCX",
            url: "https://v2api.tradesmartonline.in/MCX_symbols.txt.zip",
            zipped: true,
        },
        MasterFile {
            exchange: "BFO",
            url: "https://v2api.tradesmartonline.in/BFO_symbols.txt.zip",
            zipped: true,
        },
    ],
    tick_rule: TickRule::CashInPaise,
    index_naming: IndexNaming::StripAndOverride,
    bse_indices: BseIndices::Manual,
    nse_index_brexchange: "NSE_INDEX",
    index_instrument_type: "INDEX",
    bse_drop_without_exchange: false,
    bfo_from_tsym: false,
    timeframes: TIMEFRAMES_NO_4H,
    history_window_secs: None,
    eod_index_names: &[],
    history_repair: false,
    eod_widen: false,
    strict_candles: false,
    today_bar_utc: true,
    quote_identity_retries: 0,
    multiquote_batch: 10,
    multiquote_delay_ms: 0,
    mpp: MppScope::AlwaysConvert,
    send_mkt_protection: false,
    place_remarks: Some("openalgo"),
    modify_market_price_zero: false,
    funds_m2m: FundsM2m::PositionBook,
    margin: MarginApi::PerLeg,
    margin_mpp: MarginMpp::LtpOrSupplied,
    position_pnl: PositionPnl::RealisedPlusUnrealised,
    tradebook_time_only: false,
    orderbook_price_fallback: true,
    depth_oi: true,
    rate: RateLimits {
        order: Some(Window {
            per_second: 8,
            per_minute: 110,
        }),
        data: Some(Window {
            per_second: 8,
            per_minute: 110,
        }),
        quote: Some(Window {
            per_second: 90,
            per_minute: 5400,
        }),
    },
    persistent_socket: false,
    order_feed_subscribe: false,
    hooks: &HOOKS,
};

pub fn config() -> &'static NorenConfig {
    &CONFIG
}

pub fn broker(symbols: SymbolResolver) -> NorenBroker {
    NorenBroker::new(&CONFIG, symbols)
}
