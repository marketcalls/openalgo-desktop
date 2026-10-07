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
//! (web iifl `_market_data_request`, issue #1669).

mod auth;
pub mod binary;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod socketio;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::Exchange;
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

/// Every timeframe the XTS `/instruments/ohlc` call accepts, with its
/// `compressionValue` (web `BrokerData.timeframe_map`, `data.py:85-96`).
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
    /// A feed token issued by an "Invalid Token" refresh in this process.
    feed_override: parking_lot::RwLock<Option<Secret>>,
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
            feed_override: parking_lot::RwLock::new(None),
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

    pub(crate) fn remember_market_keys(&self, keys: Option<MarketKeys>) {
        *self.market_keys.lock() = keys;
        *self.feed_override.write() = None;
    }

    pub(crate) fn market_keys(&self) -> Option<MarketKeys> {
        self.market_keys.lock().clone()
    }

    /// Token for market-data REST: a refreshed one, else the stored feed
    /// token, else the interactive token (web behaviour).
    pub(crate) fn md_token(&self, auth: &AuthToken) -> String {
        if let Some(t) = self.feed_override.read().as_ref() {
            return t.expose().to_string();
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
        if description.is_empty() {
            return Err(AppError::Broker(format!(
                "{} refused the request.",
                self.cfg.name
            )));
        }
        Err(AppError::Broker(description))
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
        loop {
            let token = self.md_token(auth);
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
                if let Some(keys) = self.market_keys() {
                    retried = true;
                    match auth::market_login(self, &keys, &self.md_url("/auth/login"), true).await {
                        Ok(s) => {
                            tracing::info!(broker = self.cfg.id, "Market data session renewed");
                            *self.feed_override.write() = Some(Secret::new(s.token));
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

    /// The feed's credentials: market keys for a fresh socket login, or
    /// the stored feed token and user id.
    fn feed_source(&self, auth: &AuthToken) -> streaming::FeedSource {
        streaming::FeedSource {
            keys: self.market_keys(),
            token: auth
                .feed()
                .filter(|t| !t.is_empty())
                .map(Secret::new)
                .or_else(|| self.feed_override.read().clone()),
            user_id: auth.user_id().map(str::to_string),
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
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(self, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        self.order_pacer.acquire().await;
        orders::place_order(self, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        self.order_pacer.acquire().await;
        orders::modify_order(self, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        self.order_pacer.acquire().await;
        orders::cancel_order(self, auth, order_id).await
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(self, auth).await
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
