//! Zerodha Kite broker adapter

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://api.kite.trade";

/// Zerodha Kite broker implementation
pub struct ZerodhaBroker {
    client: Client,
}

impl ZerodhaBroker {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    fn get_headers(&self, api_key: &str, access_token: Option<&str>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/x-www-form-urlencoded".parse().unwrap());
        headers.insert("X-Kite-Version", "3".parse().unwrap());

        if let Some(token) = access_token {
            headers.insert(
                "Authorization",
                format!("token {}:{}", api_key, token).parse().unwrap(),
            );
        }

        headers
    }

    /// Generate checksum for Zerodha auth
    fn generate_checksum(api_key: &str, request_token: &str, api_secret: &str) -> String {
        let input = format!("{}{}{}", api_key, request_token, api_secret);
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

impl Default for ZerodhaBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Broker for ZerodhaBroker {
    fn id(&self) -> &'static str {
        "zerodha"
    }

    fn name(&self) -> &'static str {
        "Zerodha"
    }

    fn logo(&self) -> &'static str {
        "/logos/zerodha.svg"
    }

    fn requires_totp(&self) -> bool {
        false // Zerodha uses request_token from OAuth flow
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        let request_token = credentials
            .request_token
            .ok_or_else(|| AppError::Validation("Request token is required".to_string()))?;

        let api_secret = credentials
            .api_secret
            .ok_or_else(|| AppError::Validation("API secret is required".to_string()))?;

        let checksum = Self::generate_checksum(&credentials.api_key, &request_token, &api_secret);

        let params = [
            ("api_key", credentials.api_key.as_str()),
            ("request_token", request_token.as_str()),
            ("checksum", checksum.as_str()),
        ];

        let response = self
            .client
            .post(format!("{}/session/token", BASE_URL))
            .form(&params)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct SessionResponse {
            status: String,
            data: Option<SessionData>,
            message: Option<String>,
        }

        #[derive(Deserialize)]
        struct SessionData {
            access_token: String,
            public_token: String,
            user_id: String,
            user_name: Option<String>,
        }

        let result: SessionResponse = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Auth(
                result.message.unwrap_or_else(|| "Authentication failed".to_string()),
            ));
        }

        let data = result
            .data
            .ok_or_else(|| AppError::Auth("No data in session response".to_string()))?;

        Ok(AuthResponse {
            auth_token: data.access_token,
            feed_token: Some(data.public_token),
            user_id: data.user_id,
            user_name: data.user_name,
        })
    }

    async fn place_order(&self, auth_token: &str, order: OrderRequest) -> Result<OrderResponse> {
        let variety = if order.amo { "amo" } else { "regular" };

        let mut params = vec![
            ("tradingsymbol", order.symbol.clone()),
            ("exchange", order.exchange.clone()),
            ("transaction_type", order.side.clone()),
            ("order_type", order.order_type.clone()),
            ("quantity", order.quantity.to_string()),
            ("product", order.product.clone()),
            ("validity", order.validity.clone()),
        ];

        if order.price > 0.0 {
            params.push(("price", order.price.to_string()));
        }

        if let Some(tp) = order.trigger_price {
            params.push(("trigger_price", tp.to_string()));
        }

        let response = self
            .client
            .post(format!("{}/orders/{}", BASE_URL, variety))
            .headers(self.get_headers("", Some(auth_token)))
            .form(&params)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct OrderResult {
            status: String,
            data: Option<OrderData>,
            message: Option<String>,
        }

        #[derive(Deserialize)]
        struct OrderData {
            order_id: String,
        }

        let result: OrderResult = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Order placement failed".to_string()),
            ));
        }

        let data = result
            .data
            .ok_or_else(|| AppError::Broker("No order ID in response".to_string()))?;

        Ok(OrderResponse {
            order_id: data.order_id,
            message: Some("Order placed successfully".to_string()),
        })
    }

    async fn modify_order(
        &self,
        auth_token: &str,
        order_id: &str,
        order: ModifyOrderRequest,
    ) -> Result<OrderResponse> {
        let mut params: Vec<(&str, String)> = vec![];

        if let Some(q) = order.quantity {
            params.push(("quantity", q.to_string()));
        }
        if let Some(p) = order.price {
            params.push(("price", p.to_string()));
        }
        if let Some(t) = order.order_type {
            params.push(("order_type", t));
        }
        if let Some(tp) = order.trigger_price {
            params.push(("trigger_price", tp.to_string()));
        }

        let _response = self
            .client
            .put(format!("{}/orders/regular/{}", BASE_URL, order_id))
            .headers(self.get_headers("", Some(auth_token)))
            .form(&params)
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
        variety: Option<&str>,
    ) -> Result<()> {
        let variety = variety.unwrap_or("regular");

        let response = self
            .client
            .delete(format!("{}/orders/{}/{}", BASE_URL, variety, order_id))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        #[derive(Deserialize)]
        struct CancelResult {
            status: String,
            message: Option<String>,
        }

        let result: CancelResult = response.json().await?;

        if result.status != "success" {
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
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let _response = self
            .client
            .get(format!("{}/trades", BASE_URL))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let _response = self
            .client
            .get(format!("{}/portfolio/positions", BASE_URL))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let _response = self
            .client
            .get(format!("{}/portfolio/holdings", BASE_URL))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        Ok(vec![])
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let _response = self
            .client
            .get(format!("{}/user/margins", BASE_URL))
            .headers(self.get_headers("", Some(auth_token)))
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
        // Build query params like i=NSE:RELIANCE&i=NSE:TCS
        let _instruments: Vec<String> = symbols
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
        // Zerodha instruments CSV: https://api.kite.trade/instruments
        Ok(vec![])
    }
}
