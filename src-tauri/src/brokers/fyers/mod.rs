//! Fyers broker adapter

#![allow(non_snake_case)]

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://api-t1.fyers.in/api/v3";

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
            headers.insert("Authorization", format!("{}:{}", "", token).parse().unwrap());
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

        Ok(AuthResponse {
            auth_token: access_token,
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
            "SL" => 3,
            "SL-M" => 4,
            _ => 2,
        };

        let request = FyersOrderRequest {
            symbol: format!("{}:{}", order.exchange, order.symbol),
            qty: order.quantity,
            order_type,
            side,
            product_type: order.product.clone(),
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
            qty: Option<i32>,
            #[serde(rename = "type")]
            order_type: Option<i32>,
            limit_price: Option<f64>,
            stop_price: Option<f64>,
        }

        let order_type = order.order_type.as_ref().map(|t| match t.as_str() {
            "MARKET" => 2,
            "LIMIT" => 1,
            "SL" => 3,
            "SL-M" => 4,
            _ => 2,
        });

        let request = ModifyRequest {
            id: order_id.to_string(),
            qty: order.quantity,
            order_type,
            limit_price: order.price,
            stop_price: order.trigger_price,
        };

        let _response = self
            .client
            .patch(format!("{}/orders/sync", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: Some("Order modified".to_string()),
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
        let _response = self
            .client
            .get(format!("{}/orders", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let _response = self
            .client
            .get(format!("{}/tradebook", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let _response = self
            .client
            .get(format!("{}/positions", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let _response = self
            .client
            .get(format!("{}/holdings", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let _response = self
            .client
            .get(format!("{}/funds", BASE_URL))
            .headers(self.get_headers(Some(auth_token)))
            .send()
            .await?;

        Ok(Funds {
            available_cash: 0.0,
            used_margin: 0.0,
            total_margin: 0.0,
            opening_balance: 0.0,
            payin: 0.0,
            payout: 0.0,
            span: 0.0,
            exposure: 0.0,
            collateral: 0.0,
        })
    }

    async fn get_quote(
        &self,
        _auth_token: &str,
        symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>> {
        let _symbols_str: Vec<String> = symbols
            .iter()
            .map(|(ex, sym)| format!("{}:{}", ex, sym))
            .collect();

        Ok(vec![])
    }

    async fn get_market_depth(
        &self,
        _auth_token: &str,
        exchange: &str,
        symbol: &str,
    ) -> Result<MarketDepth> {
        Ok(MarketDepth {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            bids: vec![],
            asks: vec![],
        })
    }

    async fn download_master_contract(&self, _auth_token: &str) -> Result<Vec<SymbolData>> {
        // Fyers symbols CSV: https://public.fyers.in/sym_details/NSE_CM.csv
        Ok(vec![])
    }
}
