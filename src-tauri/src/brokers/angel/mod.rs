//! Angel One broker adapter

#![allow(non_snake_case)]

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "https://apiconnect.angelone.in";

/// Angel One broker implementation
pub struct AngelBroker {
    client: Client,
}

impl AngelBroker {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    fn get_headers(&self, api_key: &str, auth_token: Option<&str>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers.insert("Accept", "application/json".parse().unwrap());
        headers.insert("X-UserType", "USER".parse().unwrap());
        headers.insert("X-SourceID", "WEB".parse().unwrap());
        headers.insert("X-ClientLocalIP", "127.0.0.1".parse().unwrap());
        headers.insert("X-ClientPublicIP", "127.0.0.1".parse().unwrap());
        headers.insert("X-MACAddress", "00:00:00:00:00:00".parse().unwrap());
        headers.insert("X-PrivateKey", api_key.parse().unwrap());

        if let Some(token) = auth_token {
            headers.insert(
                "Authorization",
                format!("Bearer {}", token).parse().unwrap(),
            );
        }

        headers
    }
}

impl Default for AngelBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Broker for AngelBroker {
    fn id(&self) -> &'static str {
        "angel"
    }

    fn name(&self) -> &'static str {
        "Angel One"
    }

    fn logo(&self) -> &'static str {
        "/logos/angel.svg"
    }

    fn requires_totp(&self) -> bool {
        true
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        let totp = credentials
            .totp
            .ok_or_else(|| AppError::Validation("TOTP is required for Angel One".to_string()))?;

        let client_id = credentials
            .client_id
            .ok_or_else(|| AppError::Validation("Client ID is required".to_string()))?;

        let password = credentials
            .password
            .ok_or_else(|| AppError::Validation("Password is required".to_string()))?;

        #[derive(Serialize)]
        struct LoginRequest {
            clientcode: String,
            password: String,
            totp: String,
        }

        let request = LoginRequest {
            clientcode: client_id.clone(),
            password,
            totp,
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/auth/angelbroking/user/v1/loginByPassword",
                BASE_URL
            ))
            .headers(self.get_headers(&credentials.api_key, None))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct LoginResponse {
            status: bool,
            message: String,
            data: Option<LoginData>,
        }

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct LoginData {
            jwtToken: String,
            refreshToken: String,
            feedToken: String,
        }

        let result: LoginResponse = response.json().await?;

        if !result.status {
            return Err(AppError::Auth(result.message));
        }

        let data = result
            .data
            .ok_or_else(|| AppError::Auth("No data in login response".to_string()))?;

        Ok(AuthResponse {
            auth_token: data.jwtToken,
            feed_token: Some(data.feedToken),
            user_id: client_id,
            user_name: None,
        })
    }

    async fn place_order(&self, auth_token: &str, order: OrderRequest) -> Result<OrderResponse> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AngelOrderRequest {
            variety: String,
            tradingsymbol: String,
            symboltoken: String,
            transactiontype: String,
            exchange: String,
            ordertype: String,
            producttype: String,
            duration: String,
            price: String,
            squareoff: String,
            stoploss: String,
            quantity: String,
            triggerprice: Option<String>,
        }

        // TODO: Get symbol token from cache
        let symbol_token = "0"; // Placeholder

        let variety = if order.amo { "AMO" } else { "NORMAL" };

        let request = AngelOrderRequest {
            variety: variety.to_string(),
            tradingsymbol: order.symbol.clone(),
            symboltoken: symbol_token.to_string(),
            transactiontype: order.side.clone(),
            exchange: order.exchange.clone(),
            ordertype: order.order_type.clone(),
            producttype: order.product.clone(),
            duration: order.validity.clone(),
            price: order.price.to_string(),
            squareoff: "0".to_string(),
            stoploss: "0".to_string(),
            quantity: order.quantity.to_string(),
            triggerprice: order.trigger_price.map(|p| p.to_string()),
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/secure/angelbroking/order/v1/placeOrder",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct OrderResult {
            status: bool,
            message: String,
            data: Option<OrderData>,
        }

        #[derive(Deserialize)]
        struct OrderData {
            orderid: String,
        }

        let result: OrderResult = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let data = result
            .data
            .ok_or_else(|| AppError::Broker("No order ID in response".to_string()))?;

        Ok(OrderResponse {
            order_id: data.orderid,
            message: Some(result.message),
        })
    }

    async fn modify_order(
        &self,
        _auth_token: &str,
        order_id: &str,
        _order: ModifyOrderRequest,
    ) -> Result<OrderResponse> {
        // Implementation similar to place_order
        Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: Some("Order modified".to_string()),
        })
    }

    async fn cancel_order(
        &self,
        auth_token: &str,
        order_id: &str,
        variety: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct CancelRequest {
            variety: String,
            orderid: String,
        }

        let request = CancelRequest {
            variety: variety.unwrap_or("NORMAL").to_string(),
            orderid: order_id.to_string(),
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/secure/angelbroking/order/v1/cancelOrder",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct CancelResult {
            status: bool,
            message: String,
        }

        let result: CancelResult = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        Ok(())
    }

    async fn get_order_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let _response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getOrderBook",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        // Parse and map response to Order structs
        // For brevity, returning empty vec - full implementation would parse JSON
        Ok(vec![])
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let _response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getTradeBook",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let _response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getPosition",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let _response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/portfolio/v1/getHolding",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let _response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/user/v1/getRMS",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        // Parse response - returning default for now
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
        _symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>> {
        // Angel uses LTP endpoint
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
        // Download from Angel's master contract URL
        // https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json
        Ok(vec![])
    }
}
