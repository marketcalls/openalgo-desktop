//! The Noren (Finvasia OMS) broker family: one generic adapter for shoonya,
//! flattrade, tradesmart and zebu (web `broker/{shoonya,flattrade,
//! tradesmart,zebu}/**`, audit Part D "NOREN").
//!
//! Every member speaks the same vocabulary (`jData` JSON bodies, `stat` /
//! `emsg` envelopes, `norenordno`, `MKT/LMT/SL-LMT/SL-MKT`, `C/M/I`, the
//! Noren JSON WebSocket). What differs is data, not code, and lives in a
//! `&'static NorenConfig`: hosts, the login variant, the REST dialect,
//! the master-contract file set, index naming, the funds formula, rate
//! limits and a handful of fn-pointer hooks for the few behaviours that
//! differ by more than a value.
//!
//! The stored session token is `uid:::access_token` for every member (the
//! web's tradesmart convention), so the trading user id that every `jData`
//! body needs travels with the token instead of living in an environment
//! variable.

pub mod auth;
pub mod data;
pub mod funds;
pub mod mapping;
pub mod master_contract;
pub mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;
pub mod transport;
pub mod zip;

use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::Result;
use async_trait::async_trait;
use serde_json::Value;
use transport::Limiter;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How authenticated REST calls carry the session token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `Content-Type: text/plain`, `Authorization: Bearer <token>`, body
    /// `jData=<json>` (shoonya, zebu, tradesmart).
    BearerJData,
    /// `Content-Type: application/x-www-form-urlencoded`, body
    /// `jData=<json>&jKey=<token>` (flattrade; shoonya chart endpoints).
    JKeyForm,
}

/// How the trader signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// Redirect to `authorize_url?client_id=`, callback `?code=`, then
    /// `POST {rest}/GenAcsTok` with `jData={code, checksum}` where
    /// `checksum = sha256(client_id + secret + code)` (shoonya, zebu,
    /// tradesmart).
    GenAcsTok { authorize_url: &'static str },
    /// Redirect to `authorize_url?app_key=`, callback `?code=`, then
    /// `POST token_url` JSON `{api_key, request_code, api_secret}` where
    /// `api_secret = sha256(api_key + code + secret)` (flattrade).
    ApiToken {
        authorize_url: &'static str,
        token_url: &'static str,
    },
}

/// One master-contract file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterFile {
    /// Exchange the file holds (`NSE`, `NFO`, ...); NFO/BFO may be split
    /// across several files.
    pub exchange: &'static str,
    pub url: &'static str,
    /// `*.txt.zip` (Noren hosts) or a plain CSV (flattrade S3).
    pub zipped: bool,
}

/// Tick-size column handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickRule {
    /// NSE/BSE tick sizes are paise (divide by 100); derivatives are rupees.
    CashInPaise,
    /// Stored as published.
    Raw,
    /// Not published: 0.05 everywhere, 0.0025 on CDS (flattrade).
    Fixed,
}

/// NSE index rows: how the OpenAlgo symbol is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexNaming {
    /// Uppercase, strip spaces and hyphens, then a short override table
    /// (shoonya, tradesmart, flattrade).
    StripAndOverride,
    /// Exact-name table only, everything else keeps the broker name (zebu).
    ExactName,
}

/// Where BSE index rows come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BseIndices {
    /// Manual SENSEX (token 1) and BANKEX (token 12) rows.
    Manual,
    /// `Instrument == UNDIND` rows of the BSE file (flattrade).
    FromMaster,
    /// None (zebu).
    Absent,
}

/// Market Price Protection: Noren OMS rejects MKT / SL-MKT for API orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MppScope {
    /// MARKET and SL-M convert when a quote is available; SL-M without a
    /// quote still goes as SL-LMT priced off the trigger (shoonya, flattrade).
    MarketAndStop,
    /// MARKET only; SL-M is sent as SL-MKT (zebu).
    MarketOnly,
    /// Always convert, SL-M priced off the trigger (tradesmart).
    AlwaysConvert,
}

/// Where funds read realised / unrealised M2M from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundsM2m {
    /// `-rpnl` and `unmtom` of `/Limits` (shoonya, zebu).
    Limits,
    /// Sum of `rpnl` and `urmtom` over `/PositionBook` (flattrade,
    /// tradesmart).
    PositionBook,
}

/// Margin calculator endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginApi {
    /// `/GetBasketMargin`, first leg flat plus `basketlists` (shoonya,
    /// flattrade).
    Basket,
    /// `/GetOrderMargin` per leg, summed (tradesmart).
    PerLeg,
    /// No margin endpoint (zebu).
    Unsupported,
}

/// How a MARKET or SL-M basket-margin leg is priced (GetBasketMargin
/// refuses MKT/SL-MKT and, on flattrade, a zero price).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginMpp {
    /// Protected off the LTP; without one, MARKET sends the supplied price
    /// and SL-M the trigger, even 0 (shoonya).
    LtpOrSupplied,
    /// SL-M protected off its trigger (the LTP only without one), MARKET
    /// off the LTP; tick from the quote, else the master; no tick anywhere
    /// sends the base unprotected; no positive price at all refuses the
    /// leg rather than pricing a different basket (flattrade, web #2161).
    TriggerFirst,
}

/// Position-book P&L formula.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionPnl {
    /// `urmtom + rpnl` for open rows (or `(lp - avg) * qty + rpnl`), `rpnl`
    /// for closed rows; closed rows fall back to the day/total buy average
    /// (shoonya, zebu).
    NetAverage,
    /// `rpnl + urmtom`, `urmtom` falling back to `(lp - avg) * qty * prcftr`
    /// (flattrade, tradesmart).
    RealisedPlusUnrealised,
}

/// A rolling-window budget: at most `per_second` in any second and
/// `per_minute` in any minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub per_second: u32,
    pub per_minute: u32,
}

/// Request budgets per category; `None` means unpaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    pub order: Option<Window>,
    pub data: Option<Window>,
    pub quote: Option<Window>,
}

/// Access token and, when the broker returned one, the user id.
pub type LoginToken = (String, Option<String>);

/// Behaviours that differ by more than a value.
pub struct NorenHooks {
    /// Token-exchange response -> (access token, user id if the broker
    /// returned one).
    pub parse_login: fn(&Value) -> Option<LoginToken>,
    /// Holding quantity of one `/Holdings` row.
    pub holding_qty: fn(&Value) -> i64,
    /// Collateral from a `/Limits` response.
    pub collateral: fn(&Value) -> f64,
    /// Total margin from a `GetBasketMargin` / `GetOrderMargin` response.
    pub margin_total: fn(&Value) -> f64,
    /// Message of a refused `/CancelOrder`.
    pub cancel_error: fn(&Value) -> Option<String>,
}

/// Everything that makes one Noren broker differ from another.
pub struct NorenConfig {
    pub id: &'static str,
    pub name: &'static str,
    pub logo: &'static str,
    /// REST root including the path prefix, e.g.
    /// `https://api.shoonya.com/NorenWClientAPI`.
    pub rest_url: &'static str,
    /// Market-data WebSocket.
    pub ws_url: &'static str,
    pub dialect: Dialect,
    /// Dialect for `/TPSeries` and `/EODChartData` when it differs.
    pub chart_dialect: Option<Dialect>,
    pub login: Login,
    pub exchanges: &'static [Exchange],
    pub master_files: &'static [MasterFile],
    pub tick_rule: TickRule,
    pub index_naming: IndexNaming,
    pub bse_indices: BseIndices,
    /// `brexchange` of NSE index rows (`NSE_INDEX`, or `NSE` on zebu).
    pub nse_index_brexchange: &'static str,
    /// `instrumenttype` of index rows: `INDEX`, or `EQ` where the web keeps
    /// index rows inside OpenAlgo's EQ/FUT/CE/PE vocabulary and lets the
    /// `NSE_INDEX` / `BSE_INDEX` exchange alone mark them (flattrade, web
    /// #2198, QA MC-04).
    pub index_instrument_type: &'static str,
    /// The master is all or nothing (flattrade, web #2198): a file that
    /// fails or comes back empty, or a segment that yields no rows, fails
    /// the download so the stored master is kept. Otherwise a failed file
    /// is skipped and only every file failing is an error.
    pub master_all_or_nothing: bool,
    /// Drop BSE master rows whose `Exchange` is blank or NULL (flattrade,
    /// web #2198, QA MC-14): stale scrips the broker refuses, with no name,
    /// some repeating a live scrip under an old token.
    pub bse_drop_without_exchange: bool,
    /// BFO rows take the underlying from the leading letters of the
    /// trading symbol and the type from its suffix (shoonya, zebu).
    pub bfo_from_tsym: bool,
    /// OpenAlgo interval -> Noren `intrv` (`D` is the EOD endpoint).
    pub timeframes: &'static [(&'static str, &'static str)],
    /// Per-request history window in seconds; `None` sends one request.
    pub history_window_secs: Option<fn(&str) -> i64>,
    /// `EODChartData` names for indices: ((exchange, symbol), name).
    pub eod_index_names: &'static [((&'static str, &'static str), &'static str)],
    /// Widen high/low to cover open/close and zero negative volume.
    pub history_repair: bool,
    /// Widen the high/low of `EODChartData` rows to cover their open and
    /// close (flattrade: BSE index rows often carry a close outside the
    /// day's range, and a chart refuses the whole history on one such
    /// candle; web #2196).
    pub eod_widen: bool,
    /// Hardened candle parsing (flattrade, web #2198): a candle with a
    /// price that is missing, null, empty, NaN or infinite is skipped
    /// instead of charted as 0; intraday bars stamped before the 09:15
    /// open on NSE/BSE/NFO/BFO (TPSeries' pre-open bar, QA HS-07) are
    /// dropped; intraday volume is floored at 0 (the cumulative volume
    /// switches counters in the closing session).
    pub strict_candles: bool,
    /// Today's synthetic daily bar at UTC midnight of the IST date (true)
    /// or at IST midnight (zebu).
    pub today_bar_utc: bool,
    /// Re-ask a `GetQuotes` answered for another instrument (shoonya).
    pub quote_identity_retries: u8,
    pub multiquote_batch: usize,
    pub multiquote_delay_ms: u64,
    pub mpp: MppScope,
    /// Send `mkt_protection:"0"` on PlaceOrder.
    pub send_mkt_protection: bool,
    /// `remarks` on PlaceOrder (tradesmart `openalgo`).
    pub place_remarks: Option<&'static str>,
    /// Zebu modifies MARKET with `prc:"0"`.
    pub modify_market_price_zero: bool,
    pub funds_m2m: FundsM2m,
    pub margin: MarginApi,
    /// Pricing of MARKET / SL-M legs for `MarginApi::Basket`.
    pub margin_mpp: MarginMpp,
    pub position_pnl: PositionPnl,
    /// Tradebook keeps only `HH:MM:SS` of `norentm` (shoonya).
    pub tradebook_time_only: bool,
    /// Orderbook price falls back to `avgprc` / `rprc` (flattrade).
    pub orderbook_price_fallback: bool,
    /// Depth carries `oi` (shoonya reports 0).
    pub depth_oi: bool,
    pub rate: RateLimits,
    /// The broker allows one socket per session (flattrade): order updates
    /// must ride the market-data socket, never a second one.
    pub persistent_socket: bool,
    /// Send `{"t":"o"}` to receive order updates on the feed (tradesmart
    /// pushes them unasked).
    pub order_feed_subscribe: bool,
    pub hooks: &'static NorenHooks,
}

impl std::fmt::Debug for NorenConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NorenConfig")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Shoonya / tradesmart / flattrade per-request history windows (TPSeries
/// times out on long ranges; EODChartData truncates to 1201 rows).
pub fn shoonya_history_window(interval: &str) -> i64 {
    let days = match interval {
        "1m" => 5,
        "3m" => 10,
        "5m" => 20,
        "10m" => 40,
        "15m" => 60,
        "30m" => 90,
        "1h" | "2h" => 180,
        "4h" => 365,
        "D" => 730,
        _ => 30,
    };
    days * 24 * 3600
}

/// Timeframes with 4h (shoonya, zebu).
pub const TIMEFRAMES_WITH_4H: &[(&str, &str)] = &[
    ("1m", "1"),
    ("3m", "3"),
    ("5m", "5"),
    ("10m", "10"),
    ("15m", "15"),
    ("30m", "30"),
    ("1h", "60"),
    ("2h", "120"),
    ("4h", "240"),
    ("D", "D"),
];

/// Timeframes without 4h (flattrade, tradesmart).
pub const TIMEFRAMES_NO_4H: &[(&str, &str)] = &[
    ("1m", "1"),
    ("3m", "3"),
    ("5m", "5"),
    ("10m", "10"),
    ("15m", "15"),
    ("30m", "30"),
    ("1h", "60"),
    ("2h", "120"),
    ("D", "D"),
];

/// Exchanges of shoonya / tradesmart / flattrade (`plugin.json`).
pub const NOREN_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

// ---------------------------------------------------------------------------
// Default hooks
// ---------------------------------------------------------------------------

pub mod hooks {
    //! Hook implementations members pick from.

    use crate::brokers::families::noren::mapping::{num_f64, num_i64};
    use serde_json::Value;

    fn text(v: &Value, k: &str) -> Option<String> {
        match v.get(k) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    /// `access_token` only (shoonya, zebu).
    pub fn login_access_token(v: &Value) -> Option<(String, Option<String>)> {
        text(v, "access_token").map(|t| (t, None))
    }

    /// `token` (flattrade `/trade/apitoken`).
    pub fn login_token(v: &Value) -> Option<(String, Option<String>)> {
        text(v, "token").map(|t| (t, None))
    }

    /// Any of the token keys, and the account id when present (tradesmart).
    pub fn login_any(v: &Value) -> Option<(String, Option<String>)> {
        let token = ["access_token", "accesstoken", "token", "susertoken"]
            .iter()
            .find_map(|k| text(v, k))?;
        let uid = ["actid", "uid", "accountId", "actId", "client_id", "uname"]
            .iter()
            .find_map(|k| text(v, k));
        Some((token, uid))
    }

    /// `btstqty + holdqty + brkcolqty + unplgdqty + benqty +
    /// max(npoadqty, dpqty) - usedqty` (shoonya).
    pub fn holding_qty_full(h: &Value) -> i64 {
        let f = |k| num_f64(h.get(k));
        (f("btstqty")
            + f("holdqty")
            + f("brkcolqty")
            + f("unplgdqty")
            + f("benqty")
            + f("npoadqty").max(f("dpqty"))
            - f("usedqty")) as i64
    }

    /// `holdqty + max(npoadqty, dpqty)` (flattrade, tradesmart).
    pub fn holding_qty_npoad(h: &Value) -> i64 {
        num_i64(h.get("holdqty")) + num_i64(h.get("npoadqty")).max(num_i64(h.get("dpqty")))
    }

    /// `holdqty + max(npoadt1qty, dpqty)` (zebu).
    pub fn holding_qty_npoadt1(h: &Value) -> i64 {
        num_i64(h.get("holdqty")) + num_i64(h.get("npoadt1qty")).max(num_i64(h.get("dpqty")))
    }

    /// `brkcollamt`.
    pub fn collateral_brkcollamt(l: &Value) -> f64 {
        num_f64(l.get("brkcollamt"))
    }

    /// `collateral`, else `brkcollamt` (flattrade, web issue #1936).
    pub fn collateral_preferred(l: &Value) -> f64 {
        let c = num_f64(l.get("collateral"));
        if c != 0.0 {
            c
        } else {
            num_f64(l.get("brkcollamt"))
        }
    }

    /// `marginused` (shoonya basket).
    pub fn margin_used(r: &Value) -> f64 {
        num_f64(r.get("marginused"))
    }

    /// `marginusedtrade`, else `marginused` (flattrade basket).
    pub fn margin_used_trade(r: &Value) -> f64 {
        match r.get("marginusedtrade") {
            Some(v) if !v.is_null() && !matches!(v, Value::String(s) if s.is_empty()) => {
                num_f64(Some(v))
            }
            _ => num_f64(r.get("marginused")),
        }
    }

    /// `ordermargin`, else `marginused` (tradesmart per leg).
    pub fn order_margin(r: &Value) -> f64 {
        match r.get("ordermargin") {
            Some(v) if !v.is_null() => num_f64(Some(v)),
            _ => num_f64(r.get("marginused")),
        }
    }

    /// `message` (shoonya, flattrade, zebu read this, not `emsg`).
    pub fn cancel_message(r: &Value) -> Option<String> {
        text(r, "message").or_else(|| text(r, "emsg"))
    }

    /// `emsg` (tradesmart).
    pub fn cancel_emsg(r: &Value) -> Option<String> {
        text(r, "emsg").or_else(|| text(r, "message"))
    }
}

// ---------------------------------------------------------------------------
// Endpoints (overridable for tests)
// ---------------------------------------------------------------------------

/// The URLs one adapter instance talks to. Production takes them from the
/// config; tests rebase every URL onto a local fake server, keeping paths.
#[derive(Debug, Clone)]
pub struct NorenEndpoints {
    pub rest: String,
    pub token: String,
    pub ws: String,
    pub master: Vec<MasterFile>,
    /// Master URLs, rebased (same order as `master`).
    pub master_urls: Vec<String>,
}

impl NorenEndpoints {
    pub fn from_config(cfg: &NorenConfig) -> Self {
        let token = match cfg.login {
            Login::GenAcsTok { .. } => format!("{}/GenAcsTok", cfg.rest_url),
            Login::ApiToken { token_url, .. } => token_url.to_string(),
        };
        Self {
            rest: cfg.rest_url.to_string(),
            token,
            ws: cfg.ws_url.to_string(),
            master: cfg.master_files.to_vec(),
            master_urls: cfg.master_files.iter().map(|f| f.url.to_string()).collect(),
        }
    }

    /// Every URL moved onto `host` (`http://127.0.0.1:port`), paths kept.
    pub fn rebased(cfg: &NorenConfig, host: &str, ws_host: &str) -> Self {
        let mut e = Self::from_config(cfg);
        e.rest = rebase(&e.rest, host);
        e.token = rebase(&e.token, host);
        e.ws = rebase(&e.ws, ws_host);
        e.master_urls = e.master_urls.iter().map(|u| rebase(u, host)).collect();
        e
    }
}

fn rebase(url: &str, host: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => {
            let mut s = format!("{}{}", host.trim_end_matches('/'), u.path());
            if let Some(q) = u.query() {
                s.push('?');
                s.push_str(q);
            }
            s
        }
        Err(_) => url.to_string(),
    }
}

/// The OAuth authorize URL for a member (`client_id` / `app_key` is the
/// half after `:::` when the stored key is `userid:::key`).
pub fn authorize_url(cfg: &NorenConfig, api_key: &str, state: &str) -> String {
    let key = auth::split_api_key(api_key, None).1;
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    match cfg.login {
        Login::GenAcsTok { authorize_url } => format!(
            "{}?client_id={}&state={}",
            authorize_url,
            enc(&key),
            enc(state)
        ),
        Login::ApiToken { authorize_url, .. } => {
            format!(
                "{}?app_key={}&state={}",
                authorize_url,
                enc(&key),
                enc(state)
            )
        }
    }
}

// ---------------------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------------------

/// The generic Noren adapter. Members construct it with their config.
pub struct NorenBroker {
    pub(crate) cfg: &'static NorenConfig,
    pub(crate) http: reqwest::Client,
    pub(crate) endpoints: NorenEndpoints,
    pub(crate) symbols: SymbolResolver,
    pub(crate) order_limiter: Limiter,
    pub(crate) data_limiter: Limiter,
    pub(crate) quote_limiter: Limiter,
}

impl NorenBroker {
    pub fn new(cfg: &'static NorenConfig, symbols: SymbolResolver) -> Self {
        Self::with_endpoints(cfg, symbols, NorenEndpoints::from_config(cfg))
    }

    /// Point the adapter at other hosts (tests run a local fake Noren).
    pub fn with_endpoints(
        cfg: &'static NorenConfig,
        symbols: SymbolResolver,
        endpoints: NorenEndpoints,
    ) -> Self {
        Self {
            cfg,
            http: crate::brokers::common::http::client(),
            endpoints,
            symbols,
            order_limiter: Limiter::new(cfg.rate.order),
            data_limiter: Limiter::new(cfg.rate.data),
            quote_limiter: Limiter::new(cfg.rate.quote.or(cfg.rate.data)),
        }
    }

    pub fn config(&self) -> &'static NorenConfig {
        self.cfg
    }

    pub fn endpoints(&self) -> &NorenEndpoints {
        &self.endpoints
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }
}

#[async_trait]
impl Broker for NorenBroker {
    fn id(&self) -> &'static str {
        self.cfg.id
    }

    fn name(&self) -> &'static str {
        self.cfg.name
    }

    fn logo(&self) -> &'static str {
        self.cfg.logo
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "code" }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        self.cfg.exchanges
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: false,
            margin: self.cfg.margin != MarginApi::Unsupported,
            gtt: false,
            streaming: true,
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        self.cfg.timeframes
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(self, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        orders::place_order(self, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        orders::modify_order(self, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(self, auth, order_id).await
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(self, auth).await
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(self, auth).await
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(self, auth, symbol, exchange, product).await
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        orders::get_order_book(self, auth).await
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        orders::get_trade_book(self, auth).await
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        orders::get_positions(self, auth).await
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(self, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(self, auth, legs).await
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(self, auth, key).await
    }

    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        data::get_multiquotes(self, auth, keys).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(self, auth, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = transport::session(self.cfg, auth)?;
        Ok(Box::new(streaming::NorenFeed::new(
            self.cfg,
            &self.endpoints.ws,
            &s.uid,
            &s.token,
            self.symbols.clone(),
        )))
    }
}
