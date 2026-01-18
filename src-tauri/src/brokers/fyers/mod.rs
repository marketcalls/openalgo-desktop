//! Fyers broker adapter

#![allow(non_snake_case)]

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://api-t1.fyers.in/api/v3";

// ============================================================================
// Flexible Deserialization Helpers
// ============================================================================

/// Deserialize a value that could be either a string or an integer
fn deserialize_string_or_int<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i64),
    }

    match StringOrInt::deserialize(deserializer)? {
        StringOrInt::String(s) => s.parse().map_err(serde::de::Error::custom),
        StringOrInt::Int(i) => Ok(i),
    }
}

/// Deserialize an optional value that could be either a string or an integer
fn deserialize_optional_string_or_int<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i64),
        Null,
    }

    match Option::<StringOrInt>::deserialize(deserializer)? {
        Some(StringOrInt::String(s)) if s.is_empty() => Ok(None),
        Some(StringOrInt::String(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
        Some(StringOrInt::Int(i)) => Ok(Some(i)),
        Some(StringOrInt::Null) | None => Ok(None),
    }
}

/// Deserialize a value that could be either a string or a float
fn deserialize_string_or_float<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        String(String),
        Float(f64),
        Int(i64),
    }

    match StringOrFloat::deserialize(deserializer)? {
        StringOrFloat::String(s) => s.parse().map_err(serde::de::Error::custom),
        StringOrFloat::Float(f) => Ok(f),
        StringOrFloat::Int(i) => Ok(i as f64),
    }
}

/// Deserialize an optional value that could be either a string or a float
fn deserialize_optional_string_or_float<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrFloat {
        String(String),
        Float(f64),
        Int(i64),
        Null,
    }

    match Option::<StringOrFloat>::deserialize(deserializer)? {
        Some(StringOrFloat::String(s)) if s.is_empty() => Ok(None),
        Some(StringOrFloat::String(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
        Some(StringOrFloat::Float(f)) => Ok(Some(f)),
        Some(StringOrFloat::Int(i)) => Ok(Some(i as f64)),
        Some(StringOrFloat::Null) | None => Ok(None),
    }
}

// ============================================================================
// Fyers API Response Types
// ============================================================================

/// Generic Fyers API response wrapper
#[derive(Debug, Deserialize)]
struct FyersResponse<T> {
    s: String,
    #[serde(default)]
    code: Option<i32>,
    message: Option<String>,
    #[serde(flatten)]
    data: Option<T>,
}

/// Order book response data
#[derive(Debug, Deserialize)]
struct OrderBookData {
    orderBook: Option<Vec<FyersOrderData>>,
}

/// Trade book response data
#[derive(Debug, Deserialize)]
struct TradeBookData {
    tradeBook: Option<Vec<FyersTradeData>>,
}

/// Positions response data
#[derive(Debug, Deserialize)]
struct PositionsData {
    netPositions: Option<Vec<FyersPositionData>>,
}

/// Holdings response data
#[derive(Debug, Deserialize)]
struct HoldingsData {
    holdings: Option<Vec<FyersHoldingData>>,
}

/// Funds response data
#[derive(Debug, Deserialize)]
struct FundsResponseData {
    fund_limit: Option<Vec<FundLimitEntry>>,
}

/// Fyers order data from API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FyersOrderData {
    id: Option<String>,
    symbol: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    exchange: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    segment: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    side: Option<i64>,
    #[serde(rename = "type", default, deserialize_with = "deserialize_optional_string_or_int")]
    order_type: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    status: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    qty: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    filledQty: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    limitPrice: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    stopPrice: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    tradedPrice: Option<f64>,
    productType: Option<String>,
    orderDateTime: Option<String>,
    message: Option<String>,
}

/// Fyers trade data from API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FyersTradeData {
    symbol: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    exchange: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    segment: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    side: Option<i64>,
    productType: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    tradedQty: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    tradePrice: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    tradeValue: Option<f64>,
    orderNumber: Option<String>,
    orderDateTime: Option<String>,
}

/// Fyers position data from API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FyersPositionData {
    symbol: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    exchange: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    segment: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    netQty: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    netAvg: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    ltp: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    pl: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    realized_profit: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    unrealized_profit: Option<f64>,
    productType: Option<String>,
}

/// Fyers holding data from API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FyersHoldingData {
    symbol: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    exchange: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    segment: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_int")]
    quantity: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    costPrice: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    ltp: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_float")]
    pl: Option<f64>,
    holdingType: Option<String>,
}

/// Fund limit entry from API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FundLimitEntry {
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    equityAmount: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    commodityAmount: f64,
}

/// Quote data from Fyers /data/quotes endpoint
#[derive(Debug, Deserialize)]
struct QuotesResponse {
    s: String,
    d: Option<Vec<QuoteItem>>,
}

#[derive(Debug, Deserialize)]
struct QuoteItem {
    s: Option<String>,
    n: Option<String>,
    v: Option<QuoteValues>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QuoteValues {
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    bid: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    ask: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    open_price: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    high_price: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    low_price: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    lp: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    prev_close_price: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    volume: i64,
    #[serde(default)]
    ch: Option<f64>,
    #[serde(default)]
    chp: Option<f64>,
}

/// Market depth response from Fyers /data/depth endpoint
#[derive(Debug, Deserialize)]
struct DepthResponse {
    s: String,
    d: Option<std::collections::HashMap<String, DepthData>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct DepthData {
    bids: Option<Vec<FyersDepthLevel>>,
    #[serde(rename = "ask")]
    asks: Option<Vec<FyersDepthLevel>>,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    o: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    h: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    l: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    ltp: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    c: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    v: i64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    oi: i64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    totalbuyqty: i64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    totalsellqty: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct FyersDepthLevel {
    #[serde(default, deserialize_with = "deserialize_string_or_float")]
    price: f64,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    volume: i64,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Map Fyers exchange/segment codes to exchange name
fn get_exchange_name(exchange_code: i64, segment_code: i64) -> String {
    match (exchange_code, segment_code) {
        (10, 10) => "NSE".to_string(),
        (10, 11) => "NFO".to_string(),
        (10, 12) => "CDS".to_string(),
        (12, 10) => "BSE".to_string(),
        (12, 11) => "BFO".to_string(),
        (11, 20) => "MCX".to_string(),
        _ => "NSE".to_string(),
    }
}

/// Map Fyers status code to order status string
fn map_order_status(status: i64) -> String {
    match status {
        1 => "CANCELLED".to_string(),
        2 => "COMPLETE".to_string(),
        4 => "TRIGGER PENDING".to_string(),
        5 => "REJECTED".to_string(),
        6 => "OPEN".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

/// Map Fyers side code to BUY/SELL
fn map_side(side: i64) -> String {
    match side {
        1 => "BUY".to_string(),
        -1 => "SELL".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

/// Map Fyers order type code to order type string
fn map_order_type(order_type: i64) -> String {
    match order_type {
        1 => "LIMIT".to_string(),
        2 => "MARKET".to_string(),
        3 => "SL-M".to_string(),
        4 => "SL".to_string(),
        _ => "MARKET".to_string(),
    }
}

/// Map Fyers product type to standard format
fn map_product_type(product: &str) -> String {
    match product {
        "CNC" => "CNC".to_string(),
        "INTRADAY" => "MIS".to_string(),
        "MARGIN" => "NRML".to_string(),
        "CO" => "CO".to_string(),
        "BO" => "BO".to_string(),
        _ => product.to_string(),
    }
}

/// Map standard product type to Fyers format
fn map_product_to_fyers(product: &str) -> String {
    match product {
        "CNC" => "CNC".to_string(),
        "MIS" => "INTRADAY".to_string(),
        "NRML" => "MARGIN".to_string(),
        "CO" => "CO".to_string(),
        "BO" => "BO".to_string(),
        _ => product.to_string(),
    }
}

// ============================================================================
// FyersBroker Implementation
// ============================================================================

/// Fyers broker implementation
pub struct FyersBroker {
    client: Client,
}

impl FyersBroker {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    fn get_headers(&self, access_token: Option<&str>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse().unwrap());

        if let Some(token) = access_token {
            // Fyers auth format: api_key:access_token
            headers.insert("Authorization", token.parse().unwrap());
        }

        headers
    }

    /// Generate appIdHash for Fyers auth
    fn generate_app_id_hash(client_id: &str, secret: &str) -> String {
        let input = format!("{}-100:{}", client_id, secret);
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Extract symbol name from Fyers format (e.g., "NSE:RELIANCE-EQ" -> "RELIANCE-EQ")
    fn extract_symbol_name(fyers_symbol: &str) -> String {
        if let Some(pos) = fyers_symbol.find(':') {
            fyers_symbol[pos + 1..].to_string()
        } else {
            fyers_symbol.to_string()
        }
    }
}

impl Default for FyersBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Broker for FyersBroker {
    fn id(&self) -> &'static str {
        "fyers"
    }

    fn name(&self) -> &'static str {
        "Fyers"
    }

    fn logo(&self) -> &'static str {
        "/logos/fyers.svg"
    }

    fn requires_totp(&self) -> bool {
        false // Fyers uses auth_code from OAuth flow
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        let auth_code = credentials
            .auth_code
            .ok_or_else(|| AppError::Validation("Auth code is required".to_string()))?;

        let api_secret = credentials
            .api_secret
            .ok_or_else(|| AppError::Validation("API secret is required".to_string()))?;

        let app_id_hash = Self::generate_app_id_hash(&credentials.api_key, &api_secret);

        #[derive(Serialize)]
        struct ValidateRequest {
            grant_type: String,
            appIdHash: String,
            code: String,
        }

        let request = ValidateRequest {
            grant_type: "authorization_code".to_string(),
            appIdHash: app_id_hash,
            code: auth_code,
        };

        let response = self
            .client
            .post(format!("{}/validate-authcode", BASE_URL))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct ValidateResponse {
            s: String,
            code: i32,
            message: Option<String>,
            access_token: Option<String>,
        }

        let result: ValidateResponse = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Auth(
                result.message.unwrap_or_else(|| "Authentication failed".to_string()),
            ));
        }

        let access_token = result
            .access_token
            .ok_or_else(|| AppError::Auth("No access token in response".to_string()))?;

        // Extract client_id from api_key (format: APPID-100)
        let client_id = credentials
            .client_id
            .unwrap_or_else(|| credentials.api_key.split('-').next().unwrap_or("").to_string());

        // Combined auth token format for Fyers: api_key:access_token
        let combined_token = format!("{}:{}", credentials.api_key, access_token);

        Ok(AuthResponse {
            auth_token: combined_token,
            feed_token: None,
            user_id: client_id,
            user_name: None,
        })
    }

    async fn place_order(&self, auth_token: &str, order: OrderRequest) -> Result<OrderResponse> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct FyersOrderRequest {
            symbol: String,
            qty: i32,
            #[serde(rename = "type")]
            order_type: i32,
            side: i32,
            product_type: String,
            limit_price: f64,
            stop_price: f64,
            validity: String,
            disclosed_qty: i32,
            offline_order: bool,
        }

        let side = if order.side == "BUY" { 1 } else { -1 };

        let order_type = match order.order_type.as_str() {
            "MARKET" => 2,
            "LIMIT" => 1,
            "SL" => 4,
            "SL-M" => 3,
            _ => 2,
        };

        let product_type = map_product_to_fyers(&order.product);

        let request = FyersOrderRequest {
            symbol: format!("{}:{}", order.exchange, order.symbol),
            qty: order.quantity,
            order_type,
            side,
            product_type,
            limit_price: order.price,
            stop_price: order.trigger_price.unwrap_or(0.0),
            validity: order.validity.clone(),
            disclosed_qty: order.disclosed_quantity.unwrap_or(0),
            offline_order: order.amo,
        };

        let response = self
            .client
            .post(format!("{}/orders/sync", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OrderResult {
            s: String,
            code: Option<i32>,
            message: Option<String>,
            id: Option<String>,
        }

        let result: OrderResult = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Order placement failed".to_string()),
            ));
        }

        let order_id = result
            .id
            .ok_or_else(|| AppError::Broker("No order ID in response".to_string()))?;

        Ok(OrderResponse {
            order_id,
            message: Some("Order placed successfully".to_string()),
        })
    }

    async fn modify_order(
        &self,
        auth_token: &str,
        order_id: &str,
        order: ModifyOrderRequest,
    ) -> Result<OrderResponse> {
        #[derive(Serialize)]
        struct ModifyRequest {
            id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            qty: Option<i32>,
            #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
            order_type: Option<i32>,
            #[serde(rename = "limitPrice", skip_serializing_if = "Option::is_none")]
            limit_price: Option<f64>,
            #[serde(rename = "stopPrice", skip_serializing_if = "Option::is_none")]
            stop_price: Option<f64>,
        }

        let order_type = order.order_type.as_ref().map(|t| match t.as_str() {
            "MARKET" => 2,
            "LIMIT" => 1,
            "SL" => 4,
            "SL-M" => 3,
            _ => 2,
        });

        let request = ModifyRequest {
            id: order_id.to_string(),
            qty: order.quantity,
            order_type,
            limit_price: order.price,
            stop_price: order.trigger_price,
        };

        let response = self
            .client
            .patch(format!("{}/orders/sync", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct ModifyResult {
            s: String,
            message: Option<String>,
        }

        let result: ModifyResult = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Order modification failed".to_string()),
            ));
        }

        Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: Some("Order modified successfully".to_string()),
        })
    }

    async fn cancel_order(
        &self,
        auth_token: &str,
        order_id: &str,
        _variety: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct CancelRequest {
            id: String,
        }

        let request = CancelRequest {
            id: order_id.to_string(),
        };

        let response = self
            .client
            .delete(format!("{}/orders/sync", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct CancelResult {
            s: String,
            message: Option<String>,
        }

        let result: CancelResult = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Order cancellation failed".to_string()),
            ));
        }

        Ok(())
    }

    async fn get_order_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let response = self
            .client
            .get(format!("{}/orders", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: FyersResponse<OrderBookData> = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch order book".to_string()),
            ));
        }

        let orders = result
            .data
            .and_then(|d| d.orderBook)
            .unwrap_or_default();

        let mapped_orders: Vec<Order> = orders
            .into_iter()
            .map(|o| {
                let exchange = get_exchange_name(
                    o.exchange.unwrap_or(10),
                    o.segment.unwrap_or(10),
                );
                let symbol_name = o.symbol
                    .as_ref()
                    .map(|s| Self::extract_symbol_name(s))
                    .unwrap_or_default();

                Order {
                    order_id: o.id.unwrap_or_default(),
                    exchange_order_id: None,
                    symbol: symbol_name,
                    exchange,
                    side: map_side(o.side.unwrap_or(1)),
                    quantity: o.qty.unwrap_or(0) as i32,
                    filled_quantity: o.filledQty.unwrap_or(0) as i32,
                    pending_quantity: (o.qty.unwrap_or(0) - o.filledQty.unwrap_or(0)) as i32,
                    price: o.limitPrice.unwrap_or(0.0),
                    trigger_price: o.stopPrice.unwrap_or(0.0),
                    average_price: o.tradedPrice.unwrap_or(0.0),
                    order_type: map_order_type(o.order_type.unwrap_or(2)),
                    product: map_product_type(o.productType.as_deref().unwrap_or("INTRADAY")),
                    status: map_order_status(o.status.unwrap_or(0)),
                    validity: "DAY".to_string(),
                    order_timestamp: o.orderDateTime.unwrap_or_default(),
                    exchange_timestamp: None,
                    rejection_reason: o.message,
                }
            })
            .collect();

        Ok(mapped_orders)
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let response = self
            .client
            .get(format!("{}/tradebook", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: FyersResponse<TradeBookData> = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch trade book".to_string()),
            ));
        }

        let trades = result
            .data
            .and_then(|d| d.tradeBook)
            .unwrap_or_default();

        let mapped_trades: Vec<Order> = trades
            .into_iter()
            .map(|t| {
                let exchange = get_exchange_name(
                    t.exchange.unwrap_or(10),
                    t.segment.unwrap_or(10),
                );
                let symbol_name = t.symbol
                    .as_ref()
                    .map(|s| Self::extract_symbol_name(s))
                    .unwrap_or_default();

                Order {
                    order_id: t.orderNumber.clone().unwrap_or_default(),
                    exchange_order_id: t.orderNumber,
                    symbol: symbol_name,
                    exchange,
                    side: map_side(t.side.unwrap_or(1)),
                    quantity: t.tradedQty.unwrap_or(0) as i32,
                    filled_quantity: t.tradedQty.unwrap_or(0) as i32,
                    pending_quantity: 0,
                    price: t.tradePrice.unwrap_or(0.0),
                    trigger_price: 0.0,
                    average_price: t.tradePrice.unwrap_or(0.0),
                    order_type: "MARKET".to_string(),
                    product: map_product_type(t.productType.as_deref().unwrap_or("INTRADAY")),
                    status: "COMPLETE".to_string(),
                    validity: "DAY".to_string(),
                    order_timestamp: t.orderDateTime.unwrap_or_default(),
                    exchange_timestamp: None,
                    rejection_reason: None,
                }
            })
            .collect();

        Ok(mapped_trades)
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let response = self
            .client
            .get(format!("{}/positions", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: FyersResponse<PositionsData> = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch positions".to_string()),
            ));
        }

        let positions = result
            .data
            .and_then(|d| d.netPositions)
            .unwrap_or_default();

        let mapped_positions: Vec<Position> = positions
            .into_iter()
            .map(|p| {
                let exchange = get_exchange_name(
                    p.exchange.unwrap_or(10),
                    p.segment.unwrap_or(10),
                );
                let symbol_name = p.symbol
                    .as_ref()
                    .map(|s| Self::extract_symbol_name(s))
                    .unwrap_or_default();

                let quantity = p.netQty.unwrap_or(0) as i32;
                let avg_price = p.netAvg.unwrap_or(0.0);
                let ltp = p.ltp.unwrap_or(0.0);

                Position {
                    symbol: symbol_name,
                    exchange,
                    product: map_product_type(p.productType.as_deref().unwrap_or("INTRADAY")),
                    quantity,
                    overnight_quantity: 0,
                    average_price: avg_price,
                    ltp,
                    pnl: p.pl.unwrap_or(0.0),
                    realized_pnl: p.realized_profit.unwrap_or(0.0),
                    unrealized_pnl: p.unrealized_profit.unwrap_or(0.0),
                    buy_quantity: if quantity > 0 { quantity } else { 0 },
                    buy_value: if quantity > 0 { quantity as f64 * avg_price } else { 0.0 },
                    sell_quantity: if quantity < 0 { quantity.abs() } else { 0 },
                    sell_value: if quantity < 0 { quantity.abs() as f64 * avg_price } else { 0.0 },
                }
            })
            .collect();

        Ok(mapped_positions)
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let response = self
            .client
            .get(format!("{}/holdings", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: FyersResponse<HoldingsData> = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch holdings".to_string()),
            ));
        }

        let holdings = result
            .data
            .and_then(|d| d.holdings)
            .unwrap_or_default();

        let mapped_holdings: Vec<Holding> = holdings
            .into_iter()
            .map(|h| {
                let exchange = get_exchange_name(
                    h.exchange.unwrap_or(10),
                    h.segment.unwrap_or(10),
                );
                let symbol_name = h.symbol
                    .as_ref()
                    .map(|s| Self::extract_symbol_name(s))
                    .unwrap_or_default();

                let quantity = h.quantity.unwrap_or(0) as i32;
                let avg_price = h.costPrice.unwrap_or(0.0);
                let ltp = h.ltp.unwrap_or(0.0);
                let pnl = h.pl.unwrap_or(0.0);
                let pnl_percentage = if avg_price > 0.0 {
                    (ltp - avg_price) / avg_price * 100.0
                } else {
                    0.0
                };

                Holding {
                    symbol: symbol_name,
                    exchange,
                    isin: None,
                    quantity,
                    t1_quantity: 0,
                    average_price: avg_price,
                    ltp,
                    close_price: avg_price,
                    pnl,
                    pnl_percentage,
                    current_value: quantity as f64 * ltp,
                }
            })
            .collect();

        Ok(mapped_holdings)
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let response = self
            .client
            .get(format!("{}/funds", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: FyersResponse<FundsResponseData> = response.json().await?;

        // Fyers funds API returns code: 200 on success
        let fund_limit = result
            .data
            .and_then(|d| d.fund_limit)
            .unwrap_or_default();

        // Process fund limit entries into a map
        let mut funds_map: std::collections::HashMap<String, (f64, f64)> = std::collections::HashMap::new();
        for fund in fund_limit {
            if let Some(title) = fund.title {
                let key = title.to_lowercase().replace(' ', "_");
                funds_map.insert(key, (fund.equityAmount, fund.commodityAmount));
            }
        }

        // Extract values with defaults
        let available_balance = funds_map.get("available_balance").unwrap_or(&(0.0, 0.0));
        let collaterals = funds_map.get("collaterals").unwrap_or(&(0.0, 0.0));
        let utilized = funds_map.get("utilized_amount").unwrap_or(&(0.0, 0.0));
        let total_balance = funds_map.get("total_balance").unwrap_or(&(0.0, 0.0));

        let available_cash = available_balance.0 + available_balance.1;
        let collateral = collaterals.0 + collaterals.1;
        let used_margin = utilized.0 + utilized.1;
        let total_margin = total_balance.0 + total_balance.1;

        Ok(Funds {
            available_cash,
            used_margin,
            total_margin,
            opening_balance: total_margin,
            payin: 0.0,
            payout: 0.0,
            span: 0.0,
            exposure: 0.0,
            collateral,
        })
    }

    async fn get_quote(
        &self,
        auth_token: &str,
        symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>> {
        if symbols.is_empty() {
            return Ok(vec![]);
        }

        // Build comma-separated symbols list
        let symbols_str: Vec<String> = symbols
            .iter()
            .map(|(ex, sym)| format!("{}:{}", ex, sym))
            .collect();
        let symbols_param = symbols_str.join(",");
        let encoded_symbols = urlencoding::encode(&symbols_param);

        let response = self
            .client
            .get(format!("https://api-t1.fyers.in/data/quotes?symbols={}", encoded_symbols))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: QuotesResponse = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker("Failed to fetch quotes".to_string()));
        }

        let quote_items = result.d.unwrap_or_default();
        let mut quotes = Vec::new();

        for item in quote_items {
            if let (Some(name), Some(values)) = (item.n, item.v) {
                // Extract exchange and symbol from name (format: "NSE:SYMBOL")
                let (exchange, symbol) = if let Some(pos) = name.find(':') {
                    (name[..pos].to_string(), name[pos + 1..].to_string())
                } else {
                    ("NSE".to_string(), name)
                };

                quotes.push(Quote {
                    symbol,
                    exchange,
                    ltp: values.lp,
                    open: values.open_price,
                    high: values.high_price,
                    low: values.low_price,
                    close: values.prev_close_price,
                    volume: values.volume,
                    bid: values.bid,
                    ask: values.ask,
                    bid_qty: 0,
                    ask_qty: 0,
                    oi: 0,
                    change: values.ch.unwrap_or(0.0),
                    change_percent: values.chp.unwrap_or(0.0),
                    timestamp: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                });
            }
        }

        Ok(quotes)
    }

    async fn get_market_depth(
        &self,
        auth_token: &str,
        exchange: &str,
        symbol: &str,
    ) -> Result<MarketDepth> {
        let fyers_symbol = format!("{}:{}", exchange, symbol);
        let encoded_symbol = urlencoding::encode(&fyers_symbol);

        let response = self
            .client
            .get(format!(
                "https://api-t1.fyers.in/data/depth?symbol={}&ohlcv_flag=1",
                encoded_symbol
            ))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        let result: DepthResponse = response.json().await?;

        if result.s != "ok" {
            return Err(AppError::Broker("Failed to fetch market depth".to_string()));
        }

        let depth_data = result
            .d
            .and_then(|d| d.get(&fyers_symbol).cloned())
            .ok_or_else(|| AppError::Broker("No depth data available".to_string()))?;

        let bids: Vec<DepthLevel> = depth_data
            .bids
            .unwrap_or_default()
            .into_iter()
            .take(5)
            .map(|b| DepthLevel {
                price: b.price,
                quantity: b.volume as i32,
                orders: 0,
            })
            .collect();

        let asks: Vec<DepthLevel> = depth_data
            .asks
            .unwrap_or_default()
            .into_iter()
            .take(5)
            .map(|a| DepthLevel {
                price: a.price,
                quantity: a.volume as i32,
                orders: 0,
            })
            .collect();

        // Pad with empty entries if needed
        let bids = Self::pad_depth_levels(bids, 5);
        let asks = Self::pad_depth_levels(asks, 5);

        Ok(MarketDepth {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            bids,
            asks,
        })
    }

    async fn download_master_contract(&self, _auth_token: &str) -> Result<Vec<SymbolData>> {
        // Fyers provides separate CSV files per exchange/segment
        // NSE_CM, NSE_FO, BSE_CM, BSE_FO, MCX_COM, CDS_FO
        let exchanges = [
            ("NSE_CM", "NSE"),
            ("NSE_FO", "NFO"),
            ("BSE_CM", "BSE"),
            ("BSE_FO", "BFO"),
            ("MCX_COM", "MCX"),
            ("CDS_FO", "CDS"),
        ];

        let mut all_symbols = Vec::new();

        for (fyers_exchange, exchange_name) in exchanges {
            let url = format!("https://public.fyers.in/sym_details/{}.csv", fyers_exchange);

            match self.client.get(&url).send().await {
                Ok(response) => {
                    if let Ok(csv_text) = response.text().await {
                        let symbols = Self::parse_fyers_csv(&csv_text, exchange_name);
                        all_symbols.extend(symbols);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to download Fyers master for {}: {}",
                        fyers_exchange,
                        e
                    );
                }
            }
        }

        tracing::info!("Downloaded {} symbols from Fyers", all_symbols.len());
        Ok(all_symbols)
    }
}

impl FyersBroker {
    /// Parse Fyers CSV format into SymbolData
    fn parse_fyers_csv(csv_text: &str, exchange: &str) -> Vec<SymbolData> {
        let mut symbols = Vec::new();

        for line in csv_text.lines().skip(1) {
            // Skip header
            let fields: Vec<&str> = line.split(',').collect();

            // Fyers CSV format varies, but typically:
            // Column 0: Fytoken, 1: Symbol, 2: Name, etc.
            if fields.len() >= 3 {
                let fyers_symbol = fields.get(1).unwrap_or(&"").trim();
                let name = fields.get(2).unwrap_or(&"").trim();
                let token = fields.get(0).unwrap_or(&"0").trim();

                // Extract trading symbol from Fyers format (e.g., "NSE:RELIANCE-EQ" -> "RELIANCE-EQ")
                let trading_symbol = if let Some(pos) = fyers_symbol.find(':') {
                    &fyers_symbol[pos + 1..]
                } else {
                    fyers_symbol
                };

                // Determine instrument type based on segment
                let instrument_type = if exchange == "NFO" || exchange == "BFO" || exchange == "CDS" {
                    if trading_symbol.contains("FUT") {
                        "FUT".to_string()
                    } else if trading_symbol.contains("CE") || trading_symbol.contains("PE") {
                        "OPT".to_string()
                    } else {
                        "EQ".to_string()
                    }
                } else if exchange == "MCX" {
                    "FUTCOM".to_string()
                } else {
                    "EQ".to_string()
                };

                if !trading_symbol.is_empty() {
                    symbols.push(SymbolData {
                        exchange: exchange.to_string(),
                        symbol: trading_symbol.to_string(),
                        token: token.to_string(),
                        name: name.to_string(),
                        lot_size: 1,
                        tick_size: 0.05,
                        instrument_type,
                        expiry: None,
                        strike: None,
                        option_type: None,
                    });
                }
            }
        }

        symbols
    }

    /// Pad depth levels to ensure we have the required count
    fn pad_depth_levels(mut entries: Vec<DepthLevel>, count: usize) -> Vec<DepthLevel> {
        while entries.len() < count {
            entries.push(DepthLevel {
                price: 0.0,
                quantity: 0,
                orders: 0,
            });
        }
        entries
    }
}
