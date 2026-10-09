//! Firstock adapter (web `broker/firstock/**`, audit Part D "Firstock").
//!
//! Firstock speaks the Noren vocabulary (`C/M/I`, `MKT/LMT/SL-LMT/SL-MKT`,
//! `B/S`, Noren statuses) over its own transport: JSON bodies at
//! `https://api.firstock.in/V1/<camelCase>` carrying `jKey` and `userId`,
//! `{"status":"success","data":..}` / `{"status":"failed","error":{..}}`
//! envelopes, and a WebSocket with the token in the URL. It reuses the
//! Noren enum maps and MPP; it shares neither the request layer nor the
//! feed.
//!
//! Credentials as on the web: API key = vendor code (`<USERID>_API`), API
//! secret = the Firstock API key; the trader signs in with user id,
//! password and TOTP. The stored token is `userId:::susertoken`.

pub mod data;
pub mod mapping;
pub mod master_contract;
pub mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::auth::sha256_hex;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE_URL: &str = "https://api.firstock.in/V1";
pub const WS_URL: &str = "wss://socket.firstock.in/V2/ws";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
];

/// OpenAlgo interval -> Firstock `interval`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1mi"),
    ("3m", "3mi"),
    ("5m", "5mi"),
    ("10m", "10mi"),
    ("15m", "15mi"),
    ("30m", "30mi"),
    ("1h", "60mi"),
    ("2h", "120mi"),
    ("4h", "240mi"),
    ("D", "1d"),
];

pub(crate) const NAME: &str = "Firstock";

/// The stored session.
#[derive(Clone)]
pub struct Session {
    pub uid: String,
    pub jkey: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("uid", &self.uid)
            .field("jkey", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Firstock session has expired. Log in to Firstock again.".into())
}

pub fn session(auth: &AuthToken) -> Result<Session> {
    let raw = auth.raw();
    let (uid, jkey) = match raw.split_once(":::") {
        Some((u, t)) => (u.to_string(), t.to_string()),
        None => (
            auth.user_id().unwrap_or_default().to_string(),
            raw.to_string(),
        ),
    };
    if uid.trim().is_empty() || jkey.trim().is_empty() {
        return Err(session_expired());
    }
    Ok(Session { uid, jkey })
}

/// `<USERID>_API` -> `<USERID>` (web `api_key[:-4]`).
pub fn user_from_vendor(vendor: &str) -> String {
    vendor.strip_suffix("_API").unwrap_or(vendor).to_string()
}

/// A failed envelope's message.
pub fn error_message(v: &Value) -> String {
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .or_else(|| v.get("message").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn is_success(v: &Value) -> bool {
    v.get("status").and_then(Value::as_str) == Some("success")
}

/// Trader-facing error for a refused call.
pub fn firstock_error(v: &Value) -> AppError {
    let m = error_message(v);
    let lower = m.to_ascii_lowercase();
    if lower.contains("session") || lower.contains("jkey") || lower.contains("unauthenticated") {
        return session_expired();
    }
    if m.is_empty() {
        AppError::Broker("Firstock refused the request.".into())
    } else {
        AppError::Broker(m)
    }
}

pub struct FirstockBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) ws_url: String,
    pub(crate) symbols: SymbolResolver,
    pub(crate) quote_pacer: crate::brokers::common::ratelimit::Pacer,
}

impl FirstockBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, WS_URL)
    }

    /// Point the adapter at a local fake (tests).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            ws_url: ws_url.into(),
            symbols,
            // Quote endpoints: 1 request per second (web rate note).
            quote_pacer: crate::brokers::common::ratelimit::Pacer::per_second(1.0),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// POST `{base}{endpoint}` with `jKey` and `userId` added. A plain-text
    /// "rate limit" answer is retried once after a second.
    pub(crate) async fn call(&self, endpoint: &str, mut body: Value, s: &Session) -> Result<Value> {
        if let Some(o) = body.as_object_mut() {
            o.insert("jKey".into(), json!(s.jkey));
            o.insert("userId".into(), json!(s.uid));
        }
        let url = format!("{}{}", self.base_url, endpoint);
        let timeout = if endpoint == "/timePriceSeries" {
            Duration::from_secs(120)
        } else {
            http::REQUEST_TIMEOUT
        };
        for attempt in 0..2 {
            let resp = self
                .http
                .post(&url)
                .timeout(timeout)
                .header("Accept", "application/json")
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            let text = String::from_utf8_lossy(&bytes);
            if text.to_ascii_lowercase().contains("rate limit")
                && !text.trim_start().starts_with('{')
            {
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                return Err(AppError::Broker(
                    "Firstock is limiting requests right now. Wait a moment and try again.".into(),
                ));
            }
            return match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => Ok(v),
                Err(_) => {
                    tracing::warn!(
                        broker = "firstock",
                        status = status.as_u16(),
                        "Unreadable answer from {}",
                        endpoint
                    );
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        return Err(session_expired());
                    }
                    Err(AppError::Broker(
                        "Firstock sent a response OpenAlgo could not read. Try again shortly."
                            .into(),
                    ))
                }
            };
        }
        Err(AppError::Broker("Firstock refused the request.".into()))
    }

    /// `call` and require `status == success`.
    pub(crate) async fn call_ok(&self, endpoint: &str, body: Value, s: &Session) -> Result<Value> {
        let v = self.call(endpoint, body, s).await?;
        if is_success(&v) {
            Ok(v)
        } else {
            tracing::warn!(
                broker = "firstock",
                "Firstock refused {}: {}",
                endpoint,
                error_message(&v)
            );
            Err(firstock_error(&v))
        }
    }
}

/// The `/V1/login` body (password hashed as on the web).
pub fn login_body(user_id: &str, password: &str, totp: &str, vendor: &str, api_key: &str) -> Value {
    json!({
        "userId": user_id,
        "password": sha256_hex(&[password]),
        "TOTP": totp,
        "vendorCode": vendor,
        "apiKey": api_key,
    })
}

async fn authenticate(b: &FirstockBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let vendor = creds.api_key.trim().to_string();
    if vendor.is_empty() {
        return Err(AppError::Validation(
            "Your Firstock vendor code is missing. Add it as the API key on the broker settings page."
                .into(),
        ));
    }
    let api_key = creds.api_secret.clone().filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Firstock API key is missing. Add it as the API secret on the broker settings page."
                .into(),
        )
    })?;
    let uid = creds
        .client_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| user_from_vendor(&vendor));
    let password = creds
        .password
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation("Enter your Firstock password.".into()))?;
    let totp = creds.totp.clone().unwrap_or_default();
    let resp = b
        .http
        .post(format!("{}/login", b.base_url))
        .json(&login_body(&uid, &password, &totp, &vendor, &api_key))
        .send()
        .await?;
    let (_, v): (_, Value) = http::read_json("firstock", resp).await?;
    if !is_success(&v) {
        let field = v
            .get("error")
            .and_then(|e| e.get("field"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let msg = error_message(&v);
        tracing::warn!(
            broker = "firstock",
            "Firstock login refused ({}): {}",
            field,
            msg
        );
        return Err(AppError::Auth(if msg.is_empty() {
            "Firstock did not accept the login. Check your user id, password and TOTP.".into()
        } else {
            format!("Firstock did not accept the login: {}", msg)
        }));
    }
    let data = v.get("data").cloned().unwrap_or_default();
    let token = ["susertoken", "jKey"]
        .iter()
        .find_map(|k| {
            data.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| {
            AppError::Auth(
                "Firstock accepted the login but returned no session. Log in again.".into(),
            )
        })?;
    Ok(AuthResponse {
        auth_token: format!("{}:::{}", uid, token),
        feed_token: None,
        user_id: uid,
        user_name: None,
    })
}

#[async_trait]
impl Broker for FirstockBroker {
    fn id(&self) -> &'static str {
        "firstock"
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn logo(&self) -> &'static str {
        "/logos/firstock.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["userid", "password", "totp"],
        }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: true,
            margin: true,
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
        authenticate(self, credentials).await
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
        orders::get_funds(self, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        orders::calculate_margin(self, auth, legs).await
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

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth, None).await
    }

    /// Index rows are carried forward when `/indexList` fails (web
    /// `get_existing_index_rows`).
    fn carries_stored(&self) -> Option<&'static str> {
        Some("INDEX")
    }

    async fn download_master_carrying(
        &self,
        auth: &AuthToken,
        stored: Vec<SymbolData>,
    ) -> Result<MasterContract> {
        Ok(MasterContract::new(
            master_contract::download(self, auth, Some(stored)).await?,
        ))
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = session(auth)?;
        Ok(Box::new(streaming::FirstockFeed::new(
            &self.ws_url,
            &s.uid,
            &s.jkey,
        )))
    }
}
