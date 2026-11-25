//! Symphony XTS broker family (web `broker/{fivepaisaxts,jainamxts,
//! compositedge,rmoney,ibulls,wisdom,iifl}/**`, audit 03 Part D, XTS).
//!
//! One generic `XtsBroker` holding a `&'static XtsConfig`; each member
//! directory holds only its config constant. Two sessions per login, stored
//! the way the web stores them (three separate columns, no `:::`):
//!
//! * interactive token (`auth_token`): `POST /interactive/user/session`,
//!   sent raw in a lowercase `authorization` header (no `Bearer`);
//! * market-data token (`feed_token`) and its `userID` (`user_id`):
//!   `POST {md}/auth/login` with the second key pair.
//!
//! Market-data REST calls use the feed token when present, else the
//! interactive token (web `get_api_response`). An HTTP 200 answer of
//! `{"type":"error","description":"Invalid Token"}` triggers one market
//! re-login and retry when the market keys are known in this process
//! (web iifl `_market_data_request`, issue #1669); the renewed session is
//! kept for the calls after it, and for 5paisa XTS's feed (`session`).

mod auth;
pub mod binary;
mod data;
#[cfg(test)]
mod documented_tests;
mod funds;
pub mod mapping;
pub mod master_contract;
#[cfg(test)]
mod order_data_tests;
mod orders;
#[cfg(test)]
mod parity_tests;
mod protection;
#[cfg(test)]
mod protection_tests;
mod session;
pub mod socketio;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;

/// How the interactive (trading) token is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XtsLogin {
    /// `POST /interactive/user/session {appKey, secretKey, source}`
    /// (fivepaisaxts, ibulls, wisdom, iifl).
    Direct,
    /// Same call with `accessToken: <broker id>` and no `source` (jainamxts
    /// sends the literal plugin name, `brlogin.py:377`).
    DirectAccessToken,
    /// Redirect to `{interactive}/thirdparty?appKey&returnURL`; the callback
    /// `session` JSON carries `accessToken`, then
    /// `POST /user/session {appKey, secretKey, accessToken}` (compositedge).
    OAuthAccessToken,
    /// Same redirect; the callback `session` JSON already holds `token` and
    /// `userID`; no `/user/session` call (rmoney).
    OAuthSessionToken,
}

/// Which `xts-binary-packet` decoder a member uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryDecoder {
    /// jainamxts `_on_xts_binary_packet` (`ws:580-1054`).
    Jainam,
    /// rmoney `_extract_*_from_binary_payload` (`ws:739-844`).
    Rmoney,
}

/// Per-member behaviour that differs from the fivepaisaxts template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XtsHooks {
    /// Funds: pick the `BalanceList` entry with this `limitHeader` (rmoney
    /// `ALL|ALL|ALL`), else the first.
    pub funds_balance_header: Option<&'static str>,
    /// `POST /interactive/orders/margindetails` is implemented (rmoney).
    pub margin_details: bool,
    /// Multiquotes merge a second 1510 open-interest call (rmoney).
    pub multiquote_oi: bool,
    /// Which refused subscription batches are sent again one instrument at
    /// a time, so the others still start ([`SplitRefused`]).
    pub split_refused_batch: SplitRefused,
    /// Reconnect the market-data socket after 90 s without market data
    /// while a subscribed segment's session is open (fivepaisaxts, web
    /// #2155). XTS answers pings on an otherwise silent socket, so the
    /// manager's frame watchdog alone never sees such a stall.
    pub data_stall_watchdog: bool,
    /// Where a payload's `MessageCode` comes from (audit FB-05).
    pub event_code: EventCode,
    /// A 1105 text event is an LTP (1512) message: rmoney
    /// `_process_binary_format` marks it so and its adapter publishes it on
    /// the LTP topic alone. Otherwise the desktop reads it as the touchline
    /// its codes 110-119 are: Jainam's client marks it 1512 too, but its
    /// adapter publishes every message to every held mode in that mode's
    /// dict, which is the frames a touchline makes under its per-mode fold;
    /// the template members' web adapters drop it for want of a code.
    pub text_1105_as_ltp: bool,
    /// A 1105 text event's counts are JSON doubles, as jainamxts
    /// `_process_1105_data` and rmoney `_process_binary_format` parse its
    /// values with `float()` (`NumberForm::Double` for that packet).
    pub text_1105_doubles: bool,
    /// A 1501 touchline of an instrument held at LTP only is an LTP
    /// message: RMoney subscribes LTP with 1501, and its adapter publishes
    /// a 1501 at mode 1 unless a Quote or Depth subscription of the symbol
    /// is active (rmoney_adapter.py _process_json_data).
    pub touchline_by_holding: bool,
    /// RMoney's payload variants (audit FB-04, `streaming/variants.rs`).
    pub payload_variants: bool,
    /// A 1510 open-interest event updates the instrument's OI (jainamxts
    /// `_process_json_data` publishes 1505 and 1510 to every subscribed
    /// mode; a 1505 candle carries no trade and is not read).
    pub oi_events: bool,
    /// MARKET goes out as a LIMIT off the LTP and SL-M as a STOPLIMIT off
    /// its trigger, priced with the MPP slabs (ibulls, web #2466; its RMS
    /// may refuse a native MARKET or SL-M from an API session).
    pub price_protection: bool,
    /// XTS answers are read as the XTS v2 docs describe them (5paisa XTS,
    /// web #2504; the other members' web code still reads the older
    /// shapes): a trade-book row is one fill (`LastTradedQuantity`,
    /// `LastTradedPrice`, `LastExecutionTransactTime`); `Replaced` and the
    /// `Pending` states are open, and cancel-all cancels what is still
    /// working, not a `PendingCancel` already on its way; a `BalanceList`
    /// sent as one entry is read; F&O master rows are typed by
    /// `InstrumentType` (a future at strike 0, a spread left out); and the
    /// smallest OHLC compression is 60, so there is no `1s` interval.
    pub answers_as_documented: bool,
    /// Days one `/instruments/ohlc` call asks for, each window whole days
    /// from 00:00:00 to 23:59:59 IST: 365 for 5paisa XTS (web #2490), 6 for
    /// the others, as their web clients ask.
    pub history_window_days: i64,
    /// The feed connects with the market-data session quotes and history
    /// use (one renewed in this process, else the one stored at sign-in),
    /// read again at every connect, and logs in itself only when none is
    /// known (5paisa XTS, web #2484: its own login at
    /// `/apibinarymarketdata` cancelled the `/apimarketdata` session history
    /// used, and every reconnect cancelled it again). The others log in at
    /// every connect, as their web clients do.
    pub feed_on_stored_session: bool,
}

/// Which subscribe batches of several instruments a member's web client
/// sends again one instrument at a time when the broker refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitRefused {
    /// None: the refusal is non-fatal and nothing is retried, as in the
    /// template members' web adapters.
    #[default]
    Never,
    /// One refused because an instrument in it is already subscribed
    /// (`e-session-0002`; rmoney `subscribe_batch`, web #2176).
    AlreadySubscribed,
    /// One refused for any reason, already subscribed or not accepted
    /// (wisdom `subscribe_batch`, web #2469). A first subscribe a client
    /// waits on is searched by halves instead (`run_command`), which
    /// leaves the broker holding the same instruments in fewer calls.
    Any,
}

/// Where a member's web client takes a payload's `MessageCode` from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventCode {
    /// The payload's own; one without is a touchline (the template
    /// adapters).
    #[default]
    Payload,
    /// The event's (`1502` in `1502-json-full`) when the payload has none
    /// (jainamxts `_process_xts_data`).
    WhenMissing,
    /// Always the event's (rmoney `_process_market_data`).
    Event,
}

/// Static configuration of one XTS white-label.
#[derive(Debug)]
pub struct XtsConfig {
    pub id: &'static str,
    /// Plugin display name (`plugin.json`).
    pub name: &'static str,
    /// `scheme://host[:port]`, no trailing slash (the web's fivepaisaxts URL
    /// has one and builds `//apimarketdata`).
    pub base_url: &'static str,
    pub interactive_path: &'static str,
    /// Market-data REST root: `/apimarketdata` or `/apibinarymarketdata`.
    pub md_rest_path: &'static str,
    /// Socket.IO path of the market-data socket.
    pub socket_path: &'static str,
    /// Market-data login the socket client performs before connecting.
    pub socket_login_path: &'static str,
    /// Whether that login body carries `source: "WebAPI"` (not rmoney).
    pub socket_login_source: bool,
    /// Root of `/instruments/subscription` for the socket client.
    pub subscription_path: &'static str,
    /// `broadcastMode` query value (`FULL`; rmoney urlencodes `Full`).
    pub broadcast_mode: &'static str,
    pub login: XtsLogin,
    pub supported_exchanges: &'static [Exchange],
    /// Segments requested from `/instruments/master`.
    pub master_segments: &'static [&'static str],
    /// XTS message code per OpenAlgo mode (LTP, Quote, Depth).
    pub stream_mode_codes: [u16; 3],
    pub binary_decoder: Option<BinaryDecoder>,
    pub hooks: XtsHooks,
}

impl XtsConfig {
    /// Message code the socket is subscribed with for an OpenAlgo mode.
    pub fn mode_code(&self, mode: u8) -> u16 {
        match mode {
            1 => self.stream_mode_codes[0],
            3 => self.stream_mode_codes[2],
            _ => self.stream_mode_codes[1],
        }
    }
}

/// Every timeframe the template's `/instruments/ohlc` call offers, with its
/// `compressionValue` (web `BrokerData.timeframe_map`, `data.py:85-96`).
/// `1s` comes first, so a member without it takes the rest (`timeframes`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1s", "1"),
    ("1m", "60"),
    ("2m", "120"),
    ("3m", "180"),
    ("5m", "300"),
    ("10m", "600"),
    ("15m", "900"),
    ("30m", "1800"),
    ("60m", "3600"),
    ("D", "D"),
];

/// The timeframes a member offers: the template's, or from `1m` for a
/// member that reads XTS answers as documented, where the smallest
/// compression is 60 (web fivepaisaxts #2504).
pub(crate) fn timeframes(cfg: &XtsConfig) -> &'static [(&'static str, &'static str)] {
    if cfg.hooks.answers_as_documented {
        &TIMEFRAME_MAP[1..]
    } else {
        TIMEFRAME_MAP
    }
}

/// XTS third-party (OAuth) login URL for compositedge / rmoney
/// (`frontend/src/pages/BrokerSelect.tsx:174`, `brlogin.py:967-979`). The
/// `state` rides on the return URL so the callback can be verified.
pub fn thirdparty_url(
    broker: &str,
    api_key: &str,
    redirect_url: &str,
    state: &str,
) -> Option<String> {
    let cfg: &XtsConfig = match broker {
        "compositedge" => &crate::brokers::compositedge::CONFIG,
        "rmoney" => &crate::brokers::rmoney::CONFIG,
        _ => return None,
    };
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    let ret = format!("{}?state={}", redirect_url, enc(state));
    Some(format!(
        "{}{}/thirdparty?appKey={}&returnURL={}",
        cfg.base_url,
        cfg.interactive_path,
        enc(api_key),
        enc(&ret)
    ))
}

/// Market keys kept in memory after a login, for the feed re-login and the
/// "Invalid Token" refresh. Never persisted by the adapter.
#[derive(Clone)]
pub(crate) struct MarketKeys {
    pub key: Secret,
    pub secret: Secret,
}

pub struct XtsBroker {
    cfg: &'static XtsConfig,
    http: reqwest::Client,
    base_url: String,
    symbols: SymbolResolver,
    market_keys: parking_lot::Mutex<Option<MarketKeys>>,
    /// The market-data session an "Invalid Token" refresh in this process
    /// renewed, shared with the feed (`session`).
    market_session: Arc<session::MarketSessions>,
    order_pacer: Pacer,
    socket_base: Option<String>,
}

impl XtsBroker {
    pub fn new(cfg: &'static XtsConfig, symbols: SymbolResolver) -> Self {
        Self::with_base_url(cfg, symbols, cfg.base_url)
    }

    /// Point the adapter at another host (tests run a local fake XTS).
    pub fn with_base_url(
        cfg: &'static XtsConfig,
        symbols: SymbolResolver,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            cfg,
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            symbols,
            market_keys: parking_lot::Mutex::new(None),
            market_session: Arc::default(),
            order_pacer: Pacer::per_second(10.0),
            socket_base: None,
        }
    }

    /// Open the market-data socket on another host (tests).
    pub fn with_socket_base(mut self, socket_base: impl Into<String>) -> Self {
        self.socket_base = Some(socket_base.into().trim_end_matches('/').to_string());
        self
    }

    pub fn config(&self) -> &'static XtsConfig {
        self.cfg
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn interactive_url(&self, path: &str) -> String {
        format!("{}{}{}", self.base_url, self.cfg.interactive_path, path)
    }

    pub(crate) fn md_url(&self, path: &str) -> String {
        format!("{}{}{}", self.base_url, self.cfg.md_rest_path, path)
    }

    /// A sign-in, a resume or a sign-out: the keys of the session that
    /// starts (none for a sign-out). A market-data session renewed under the
    /// one that ended goes with it, cleared after the keys change so a
    /// renewal in flight cannot keep it (`session`).
    pub(crate) fn remember_market_keys(&self, keys: Option<MarketKeys>) {
        *self.market_keys.lock() = keys;
        self.market_session.clear();
    }

    pub(crate) fn market_keys(&self) -> Option<MarketKeys> {
        self.market_keys.lock().clone()
    }

    /// Token for market-data REST: a renewed one, else the stored feed
    /// token, else the interactive token (web behaviour).
    pub(crate) fn md_token(&self, auth: &AuthToken) -> String {
        if let Some(s) = self.market_session.current() {
            return s.token.expose().to_string();
        }
        auth.feed()
            .filter(|f| !f.is_empty())
            .unwrap_or(auth.raw())
            .to_string()
    }

    fn interactive_token(&self, auth: &AuthToken) -> Result<String> {
        let t = auth.raw().trim();
        if t.is_empty() {
            return Err(self.session_expired());
        }
        Ok(t.to_string())
    }

    pub(crate) fn session_expired(&self) -> AppError {
        AppError::Auth(format!(
            "Your {} session has expired. Log in to {} again.",
            self.cfg.name, self.cfg.name
        ))
    }

    /// One interactive (trading) call. `Ok` only for `type == "success"`.
    pub(crate) async fn interactive(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let token = self.interactive_token(auth)?;
        let mut req = self
            .http
            .request(method, self.interactive_url(path))
            .header("authorization", token)
            .header("Content-Type", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let (status, v): (_, Value) = http::read_json(self.cfg.id, resp).await?;
        self.check(status, v, path)
    }

    /// Envelope check shared by both APIs.
    pub(crate) fn check(&self, status: reqwest::StatusCode, v: Value, path: &str) -> Result<Value> {
        if status.is_server_error() {
            return Err(http::server_error(self.cfg.id, status));
        }
        if v.get("type").and_then(Value::as_str) == Some("success") {
            return Ok(v);
        }
        let description = mapping::error_text(&v);
        tracing::warn!(
            broker = self.cfg.id,
            status = status.as_u16(),
            code = %mapping::s(&v, "code"),
            "{} refused {}: {}",
            self.cfg.name,
            path.split('?').next().unwrap_or(""),
            description
        );
        if matches!(status.as_u16(), 401 | 403) || mapping::is_token_error(&description) {
            return Err(self.session_expired());
        }
        // Only XTS's explicit failure (`type: error`) is a refusal inside a
        // placement (LOG-08); a 2xx without one (`{}`) and a 5xx leave the
        // order unknown.
        let failed = v
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("error"));
        let message = if description.is_empty() {
            format!("{} refused the request.", self.cfg.name)
        } else {
            description
        };
        Err(crate::brokers::common::outcome::not_accepted(
            status.as_u16(),
            failed,
            message,
        ))
    }

    /// One market-data call, refreshing the feed token once on
    /// "Invalid Token" when the market keys are known.
    pub(crate) async fn market(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value> {
        let mut retried = false;
        // The session this call renewed: its own retry uses it even when a
        // sign-out meanwhile kept it out of the shared slot.
        let mut renewed: Option<Secret> = None;
        loop {
            let token = match &renewed {
                Some(t) => t.expose().to_string(),
                None => self.md_token(auth),
            };
            let mut req = self
                .http
                .request(method.clone(), self.md_url(path))
                .header("authorization", token)
                .header("Content-Type", "application/json");
            if let Some(q) = query {
                req = req.query(q);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let (status, v): (_, Value) = http::read_json(self.cfg.id, resp).await?;
            let invalid = v.get("type").and_then(Value::as_str) != Some("success")
                && mapping::is_token_error(&mapping::error_text(&v));
            if invalid && !retried {
                // Read before the keys and the login (TtlCache rule): a
                // sign-in, resume or sign-out meanwhile clears the slot, and
                // this renewal is then not kept for anyone else.
                let generation = self.market_session.generation();
                if let Some(keys) = self.market_keys() {
                    retried = true;
                    match auth::market_login(self, &keys, &self.md_url("/auth/login"), true).await {
                        Ok(s) => {
                            tracing::info!(broker = self.cfg.id, "Market data session renewed");
                            let token = Secret::new(s.token.as_str());
                            let shared = session::SharedSession {
                                token: token.clone(),
                                user_id: s.user_id.clone(),
                            };
                            if !self.market_session.renewed(shared, generation) {
                                tracing::debug!(
                                    broker = self.cfg.id,
                                    "Renewed market data session not kept: the broker session changed meanwhile"
                                );
                            }
                            renewed = Some(token);
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(
                                broker = self.cfg.id,
                                "Market data session could not be renewed: {}",
                                e.code()
                            );
                        }
                    }
                }
            }
            if invalid {
                return Err(AppError::Auth(format!(
                    "Your {} market data session has expired. Log in to {} again.",
                    self.cfg.name, self.cfg.name
                )));
            }
            return self.check(status, v, path);
        }
    }

    /// The feed's credentials: market keys for a fresh socket login, the
    /// stored feed token and user id, and the session quotes and history
    /// renew (which a member feeding on the stored session reads at every
    /// connect).
    fn feed_source(&self, auth: &AuthToken) -> streaming::FeedSource {
        streaming::FeedSource {
            keys: self.market_keys(),
            token: auth
                .feed()
                .filter(|t| !t.is_empty())
                .map(Secret::new)
                .or_else(|| self.market_session.current().map(|s| s.token)),
            user_id: auth.user_id().map(str::to_string),
            shared: Some(self.market_session.clone()),
        }
    }
}

#[async_trait]
impl Broker for XtsBroker {
    fn id(&self) -> &'static str {
        self.cfg.id
    }

    fn name(&self) -> &'static str {
        self.cfg.name
    }

    fn logo(&self) -> &'static str {
        // Same convention as the other adapters: `/logos/<id>.svg`.
        match self.cfg.id {
            "fivepaisaxts" => "/logos/fivepaisaxts.svg",
            "jainamxts" => "/logos/jainamxts.svg",
            "compositedge" => "/logos/compositedge.svg",
            "rmoney" => "/logos/rmoney.svg",
            "ibulls" => "/logos/ibulls.svg",
            "wisdom" => "/logos/wisdom.svg",
            "iifl" => "/logos/iifl.svg",
            _ => "/logos/default.svg",
        }
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::ApiKeySecret
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        self.cfg.supported_exchanges
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: true,
            margin: self.cfg.hooks.margin_details,
            gtt: false,
            streaming: true,
            order_feed: false,
            depth_levels: &[5, 20],
        }
    }

    /// Twenty levels on NSE and NFO, five elsewhere, as every member's web
    /// capability registry lists them (`*_mapping.py` `depth_support`,
    /// audit FB-06). XTS has one book subscription (1502) for both depths;
    /// the frames carry the levels its events bring, never padded.
    fn feed_depth_levels(&self, exchange: &str) -> Vec<u8> {
        if matches!(exchange, "NSE" | "NFO") {
            vec![5, 20]
        } else {
            vec![5]
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        timeframes(self.cfg)
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(self, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        self.order_pacer.acquire().await?;
        orders::place_order(self, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        self.order_pacer.acquire().await?;
        orders::modify_order(self, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        self.order_pacer.acquire().await?;
        orders::cancel_order(self, auth, order_id).await
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(self, auth).await
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(self, auth).await
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

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(self, auth, symbol, exchange.as_str(), product.as_str()).await
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(self, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        if !self.cfg.hooks.margin_details {
            return Err(AppError::Unsupported("margin"));
        }
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

    /// After an app restart the market keys are not in memory: the feed
    /// re-login and the token renewal need them from the stored
    /// credentials.
    fn restore_session(&self, credentials: &BrokerCredentials) {
        let keys = match (&credentials.api_key_market, &credentials.api_secret_market) {
            (Some(k), Some(s)) if !k.trim().is_empty() && !s.is_empty() => Some(MarketKeys {
                key: Secret::new(k.trim()),
                secret: Secret::new(s.clone()),
            }),
            _ => None,
        };
        self.remember_market_keys(keys);
    }

    async fn on_logout(&self) {
        self.remember_market_keys(None);
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        self.interactive_token(auth)?;
        Ok(Box::new(streaming::XtsFeed::new(
            self.cfg,
            self.http.clone(),
            self.base_url.clone(),
            self.socket_base.clone(),
            self.feed_source(auth),
        )))
    }
}
