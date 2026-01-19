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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
    /// Hash is SHA256(api_key:api_secret)
    fn generate_app_id_hash(api_key: &str, api_secret: &str) -> String {
        let input = format!("{}:{}", api_key, api_secret);
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

        tracing::info!("Fyers auth: api_key={}, auth_code_len={}", credentials.api_key, auth_code.len());
        tracing::debug!("Fyers appIdHash: {}", app_id_hash);

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

        // Get raw response text for debugging
        let response_text = response.text().await?;
        tracing::info!("Fyers validate-authcode response: {}", response_text);

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct ValidateResponse {
            s: String,
            code: Option<i32>,
            message: Option<String>,
            access_token: Option<String>,
        }

        let result: ValidateResponse = serde_json::from_str(&response_text)
            .map_err(|e| AppError::Auth(format!("Failed to parse Fyers response: {} - Raw: {}", e, response_text)))?;

        if result.s != "ok" {
            let error_msg = format!(
                "Fyers auth failed: status={}, code={:?}, message={:?}",
                result.s, result.code, result.message
            );
            tracing::error!("{}", error_msg);
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
        // Each file has 21 columns and requires different processing
        let mut all_symbols = Vec::new();

        // Download and process each exchange CSV
        let csv_urls = [
            ("NSE_CM", "https://public.fyers.in/sym_details/NSE_CM.csv"),
            ("NSE_FO", "https://public.fyers.in/sym_details/NSE_FO.csv"),
            ("BSE_CM", "https://public.fyers.in/sym_details/BSE_CM.csv"),
            ("BSE_FO", "https://public.fyers.in/sym_details/BSE_FO.csv"),
            ("NSE_CD", "https://public.fyers.in/sym_details/NSE_CD.csv"),
            ("MCX_COM", "https://public.fyers.in/sym_details/MCX_COM.csv"),
        ];

        for (exchange_key, url) in csv_urls {
            match self.client.get(url).send().await {
                Ok(response) => {
                    if let Ok(csv_text) = response.text().await {
                        let symbols = Self::process_fyers_csv(&csv_text, exchange_key);
                        tracing::info!("Processed {} symbols from {}", symbols.len(), exchange_key);
                        all_symbols.extend(symbols);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to download Fyers master for {}: {}", exchange_key, e);
                }
            }
        }

        tracing::info!("Downloaded {} total symbols from Fyers", all_symbols.len());
        Ok(all_symbols)
    }
}

impl FyersBroker {
    /// Process Fyers CSV with proper 21-column parsing matching Flask implementation
    /// CSV columns: Fytoken, Symbol Details, Exchange Instrument type, Minimum lot size,
    /// Tick size, ISIN, Trading Session, Last update date, Expiry date, Symbol ticker,
    /// Exchange, Segment, Scrip code, Underlying symbol, Underlying scrip code, Strike price,
    /// Option type, Underlying FyToken, Reserved column1, Reserved column2, Reserved column3
    fn process_fyers_csv(csv_text: &str, exchange_key: &str) -> Vec<SymbolData> {
        let mut symbols = Vec::new();

        for line in csv_text.lines() {
            let fields: Vec<&str> = line.split(',').collect();

            // Fyers CSV has 21 columns
            if fields.len() < 17 {
                continue;
            }

            // Parse the 21 columns
            let fytoken = fields.get(0).unwrap_or(&"").trim();
            let symbol_details = fields.get(1).unwrap_or(&"").trim();
            let exchange_instrument_type: i32 = fields.get(2).unwrap_or(&"0").trim().parse().unwrap_or(0);
            let lot_size: i32 = fields.get(3).unwrap_or(&"1").trim().parse().unwrap_or(1);
            let tick_size: f64 = fields.get(4).unwrap_or(&"0.05").trim().parse().unwrap_or(0.05);
            let expiry_timestamp: i64 = fields.get(8).unwrap_or(&"0").trim().parse().unwrap_or(0);
            let symbol_ticker = fields.get(9).unwrap_or(&"").trim();
            let underlying_symbol = fields.get(13).unwrap_or(&"").trim();
            let strike_price: f64 = fields.get(15).unwrap_or(&"0.0").trim().parse().unwrap_or(0.0);
            let option_type = fields.get(16).unwrap_or(&"").trim();

            // Skip invalid rows
            if fytoken.is_empty() || symbol_ticker.is_empty() {
                continue;
            }

            // Process based on exchange key
            let processed = match exchange_key {
                "NSE_CM" => Self::process_nse_cm_row(
                    fytoken, symbol_details, exchange_instrument_type, lot_size, tick_size,
                    symbol_ticker, underlying_symbol,
                ),
                "BSE_CM" => Self::process_bse_cm_row(
                    fytoken, symbol_details, exchange_instrument_type, lot_size, tick_size,
                    symbol_ticker, underlying_symbol,
                ),
                "NSE_FO" => Self::process_fo_row(
                    fytoken, symbol_details, lot_size, tick_size, expiry_timestamp,
                    symbol_ticker, strike_price, option_type, "NFO",
                ),
                "BSE_FO" => Self::process_fo_row(
                    fytoken, symbol_details, lot_size, tick_size, expiry_timestamp,
                    symbol_ticker, strike_price, option_type, "BFO",
                ),
                "NSE_CD" => Self::process_fo_row(
                    fytoken, symbol_details, lot_size, tick_size, expiry_timestamp,
                    symbol_ticker, strike_price, option_type, "CDS",
                ),
                "MCX_COM" => Self::process_fo_row(
                    fytoken, symbol_details, lot_size, tick_size, expiry_timestamp,
                    symbol_ticker, strike_price, option_type, "MCX",
                ),
                _ => None,
            };

            if let Some(symbol_data) = processed {
                symbols.push(symbol_data);
            }
        }

        symbols
    }

    /// Process NSE_CM (cash market) row
    fn process_nse_cm_row(
        fytoken: &str,
        symbol_details: &str,
        exchange_instrument_type: i32,
        lot_size: i32,
        tick_size: f64,
        symbol_ticker: &str,
        underlying_symbol: &str,
    ) -> Option<SymbolData> {
        // Exchange instrument type mapping for NSE_CM:
        // 0, 9 -> EQ (equities)
        // 10 -> INDEX
        // 2 with -GB suffix -> GB (government bonds)
        let (exchange, instrument_type) = match exchange_instrument_type {
            0 | 9 => ("NSE", "EQ"),
            10 => ("NSE_INDEX", "INDEX"),
            2 if symbol_ticker.ends_with("-GB") => ("NSE", "GB"),
            _ => return None, // Skip other instrument types
        };

        Some(SymbolData {
            exchange: exchange.to_string(),
            symbol: underlying_symbol.to_string(),
            token: fytoken.to_string(),
            name: symbol_details.to_string(),
            lot_size,
            tick_size,
            instrument_type: instrument_type.to_string(),
            expiry: None,
            strike: None,
            option_type: None,
            brsymbol: Some(symbol_ticker.to_string()),
            brexchange: Some("NSE".to_string()),
        })
    }

    /// Process BSE_CM (cash market) row
    fn process_bse_cm_row(
        fytoken: &str,
        symbol_details: &str,
        exchange_instrument_type: i32,
        lot_size: i32,
        tick_size: f64,
        symbol_ticker: &str,
        underlying_symbol: &str,
    ) -> Option<SymbolData> {
        // Exchange instrument type mapping for BSE_CM:
        // 0, 4, 50 -> EQ (equities)
        // 10 -> INDEX
        let (exchange, instrument_type) = match exchange_instrument_type {
            0 | 4 | 50 => ("BSE", "EQ"),
            10 => ("BSE_INDEX", "INDEX"),
            _ => return None,
        };

        Some(SymbolData {
            exchange: exchange.to_string(),
            symbol: underlying_symbol.to_string(),
            token: fytoken.to_string(),
            name: symbol_details.to_string(),
            lot_size,
            tick_size,
            instrument_type: instrument_type.to_string(),
            expiry: None,
            strike: None,
            option_type: None,
            brsymbol: Some(symbol_ticker.to_string()),
            brexchange: Some("BSE".to_string()),
        })
    }

    /// Process F&O row (NSE_FO, BSE_FO, NSE_CD, MCX_COM)
    fn process_fo_row(
        fytoken: &str,
        symbol_details: &str,
        lot_size: i32,
        tick_size: f64,
        expiry_timestamp: i64,
        symbol_ticker: &str,
        strike_price: f64,
        option_type: &str,
        exchange: &str,
    ) -> Option<SymbolData> {
        // Convert expiry from Unix timestamp to DD-MMM-YY format
        let expiry = if expiry_timestamp > 0 {
            Self::convert_unix_to_expiry(expiry_timestamp)
        } else {
            None
        };

        // Determine instrument type from option_type field
        // XX -> FUT (futures)
        // CE -> CE (call option)
        // PE -> PE (put option)
        let instrument_type = match option_type {
            "XX" => "FUT",
            "CE" => "CE",
            "PE" => "PE",
            "" => "FUT", // Default to FUT if empty
            _ => option_type,
        };

        // Reformat symbol details to standard format
        // "NIFTY 24 Apr 25 FUT" -> "NIFTY25APR24FUT"
        let symbol = Self::reformat_symbol_detail(symbol_details, option_type);

        Some(SymbolData {
            exchange: exchange.to_string(),
            symbol,
            token: fytoken.to_string(),
            name: symbol_details.to_string(),
            lot_size,
            tick_size,
            instrument_type: instrument_type.to_string(),
            expiry,
            strike: if strike_price > 0.0 { Some(strike_price) } else { None },
            option_type: match option_type {
                "CE" => Some("CE".to_string()),
                "PE" => Some("PE".to_string()),
                _ => None,
            },
            brsymbol: Some(symbol_ticker.to_string()),
            brexchange: Some(exchange.to_string()),
        })
    }

    /// Convert Unix timestamp to DD-MMM-YY format (e.g., "24-APR-25")
    fn convert_unix_to_expiry(timestamp: i64) -> Option<String> {
        use chrono::{TimeZone, Utc};

        if timestamp <= 0 {
            return None;
        }

        match Utc.timestamp_opt(timestamp, 0) {
            chrono::LocalResult::Single(dt) => {
                Some(dt.format("%d-%b-%y").to_string().to_uppercase())
            }
            _ => None,
        }
    }

    /// Reformat symbol details from Fyers format to standard format
    /// Input: "NIFTY 24 Apr 25 FUT" or "NIFTY 24 Apr 25 25000"
    /// Output: "NIFTY25APR24FUT" or "NIFTY25APR2425000CE"
    fn reformat_symbol_detail(symbol_details: &str, option_type: &str) -> String {
        let parts: Vec<&str> = symbol_details.split_whitespace().collect();

        // Expected format: "NAME DD Mon YY SUFFIX"
        // e.g., "NIFTY 24 Apr 25 FUT" or "NIFTY 24 Apr 25 25000"
        if parts.len() >= 5 {
            let name = parts[0];
            let day = parts[1];
            let month = parts[2].to_uppercase();
            let year = parts[3];
            let suffix = parts[4];

            // Build the reformatted symbol: NAME + YY + MON + DD + SUFFIX
            let base = format!("{}{}{}{}{}", name, year, month, day, suffix);

            // Add option type suffix if it's an option
            match option_type {
                "CE" => format!("{}CE", base),
                "PE" => format!("{}PE", base),
                _ => base, // FUT or XX - no additional suffix
            }
        } else if parts.len() >= 4 {
            // Handle shorter format without explicit suffix
            let name = parts[0];
            let day = parts[1];
            let month = parts[2].to_uppercase();
            let year = parts[3];

            let base = format!("{}{}{}{}", name, year, month, day);

            match option_type {
                "CE" => format!("{}CE", base),
                "PE" => format!("{}PE", base),
                "XX" => format!("{}FUT", base),
                _ => base,
            }
        } else {
            // Fallback: return as-is with option suffix
            match option_type {
                "CE" => format!("{}CE", symbol_details.replace(' ', "")),
                "PE" => format!("{}PE", symbol_details.replace(' ', "")),
                _ => symbol_details.replace(' ', ""),
            }
        }
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
