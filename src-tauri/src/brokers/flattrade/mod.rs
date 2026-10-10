//! Flattrade (PiConnect), a Noren member (web `broker/flattrade/`).
//! jKey form dialect, `/trade/apitoken` login, S3 CSV masters with fixed
//! tick sizes and BSE `UNDIND` indices (index rows typed `EQ`, stale BSE
//! rows with no exchange dropped, every file required), hardened candle
//! parsing, `marginusedtrade` basket margin, funds M2M from the position
//! book, a dual rolling-window limiter, and one socket per session (order
//! updates ride the market socket).

use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::*;

pub static HOOKS: NorenHooks = NorenHooks {
    parse_login: hooks::login_token,
    holding_qty: hooks::holding_qty_npoad,
    collateral: hooks::collateral_preferred,
    margin_total: hooks::margin_used_trade,
    cancel_error: hooks::cancel_message,
};

pub static CONFIG: NorenConfig = NorenConfig {
    id: "flattrade",
    name: "Flattrade",
    logo: "/logos/flattrade.svg",
    rest_url: "https://piconnect.flattrade.in/PiConnectAPI",
    ws_url: "wss://piconnect.flattrade.in/PiConnectWSAPI/",
    dialect: Dialect::JKeyForm,
    chart_dialect: None,
    login: Login::ApiToken {
        authorize_url: "https://auth.flattrade.in/",
        token_url: "https://authapi.flattrade.in/trade/apitoken",
    },
    exchanges: NOREN_EXCHANGES,
    master_files: &[
        MasterFile { exchange: "NSE", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/NSE_Equity.csv", zipped: false },
        MasterFile { exchange: "BSE", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/BSE_Equity.csv", zipped: false },
        MasterFile { exchange: "NFO", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Nfo_Equity_Derivatives.csv", zipped: false },
        MasterFile { exchange: "NFO", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Nfo_Index_Derivatives.csv", zipped: false },
        MasterFile { exchange: "CDS", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Currency_Derivatives.csv", zipped: false },
        MasterFile { exchange: "MCX", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Commodity.csv", zipped: false },
        MasterFile { exchange: "BFO", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Bfo_Index_Derivatives.csv", zipped: false },
        MasterFile { exchange: "BFO", url: "https://flattrade.s3.ap-south-1.amazonaws.com/scripmaster/Bfo_Equity_Derivatives.csv", zipped: false },
    ],
    tick_rule: TickRule::Fixed,
    index_naming: IndexNaming::StripAndOverride,
    bse_indices: BseIndices::FromMaster,
    nse_index_brexchange: "NSE_INDEX",
    index_instrument_type: "EQ",
    bse_drop_without_exchange: true,
    bfo_from_tsym: false,
    timeframes: TIMEFRAMES_NO_4H,
    history_window_secs: None,
    eod_index_names: &[],
    history_repair: false,
    eod_widen: true,
    strict_candles: true,
    today_bar_utc: true,
    quote_identity_retries: 0,
    multiquote_batch: 10,
    multiquote_delay_ms: 0,
    mpp: MppScope::MarketAndStop,
    send_mkt_protection: true,
    place_remarks: None,
    modify_market_price_zero: false,
    funds_m2m: FundsM2m::PositionBook,
    margin: MarginApi::Basket,
    margin_mpp: MarginMpp::TriggerFirst,
    position_pnl: PositionPnl::RealisedPlusUnrealised,
    tradebook_time_only: false,
    orderbook_price_fallback: true,
    depth_oi: true,
    rate: RateLimits {
        order: Some(Window { per_second: 9, per_minute: 38 }),
        data: Some(Window { per_second: 9, per_minute: 110 }),
        quote: None,
    },
    persistent_socket: true,
    order_feed_subscribe: true,
    hooks: &HOOKS,
};

pub fn config() -> &'static NorenConfig {
    &CONFIG
}

pub fn broker(symbols: SymbolResolver) -> NorenBroker {
    NorenBroker::new(&CONFIG, symbols)
}
