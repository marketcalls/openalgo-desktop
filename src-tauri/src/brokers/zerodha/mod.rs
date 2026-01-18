//! Zerodha Kite broker adapter

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://api.kite.trade";
const MASTER_CONTRACT_URL: &str = "https://api.kite.trade/instruments";

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

    fn get_headers(&self, auth_token: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-Kite-Version", "3".parse().unwrap());
        headers.insert(
            "Authorization",
            format!("token {}", auth_token).parse().unwrap(),
        );
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

// Zerodha API response structures
#[derive(Deserialize)]
struct KiteResponse<T> {
    status: String,
    #[serde(default)]
    data: Option<T>,
    #[serde(default)]
    message: Option<String>,
}

// Order response
#[derive(Deserialize)]
struct KiteOrderData {
    order_id: String,
    #[serde(default)]
    exchange_order_id: Option<String>,
    #[serde(default)]
    tradingsymbol: String,
    #[serde(default)]
    exchange: String,
    #[serde(default)]
    transaction_type: String,
    #[serde(default)]
    quantity: i32,
    #[serde(default)]
    filled_quantity: i32,
    #[serde(default)]
    pending_quantity: i32,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    trigger_price: f64,
    #[serde(default)]
    average_price: f64,
    #[serde(default)]
    order_type: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    validity: String,
    #[serde(default)]
    order_timestamp: Option<String>,
    #[serde(default)]
    exchange_timestamp: Option<String>,
    #[serde(default)]
    status_message: Option<String>,
}

// Positions response
#[derive(Deserialize, Default)]
struct KitePositionsResponse {
    #[serde(default)]
    net: Vec<KitePositionData>,
    #[serde(default)]
    day: Vec<KitePositionData>,
}

#[derive(Deserialize)]
struct KitePositionData {
    tradingsymbol: String,
    exchange: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    quantity: i32,
    #[serde(default)]
    overnight_quantity: i32,
    #[serde(default)]
    average_price: f64,
    #[serde(default)]
    last_price: f64,
    #[serde(default)]
    pnl: f64,
    #[serde(default)]
    realised: f64,
    #[serde(default)]
    unrealised: f64,
    #[serde(default)]
    buy_quantity: i32,
    #[serde(default)]
    buy_value: f64,
    #[serde(default)]
    sell_quantity: i32,
    #[serde(default)]
    sell_value: f64,
}

// Holdings response
#[derive(Deserialize)]
struct KiteHoldingData {
    tradingsymbol: String,
    exchange: String,
    #[serde(default)]
    isin: Option<String>,
    #[serde(default)]
    quantity: i32,
    #[serde(default)]
    t1_quantity: i32,
    #[serde(default)]
    average_price: f64,
    #[serde(default)]
    last_price: f64,
    #[serde(default)]
    close_price: f64,
    #[serde(default)]
    pnl: f64,
    #[serde(default)]
    product: String,
}

// Funds/Margin response
#[derive(Deserialize, Default)]
struct KiteMarginResponse {
    #[serde(default)]
    equity: Option<KiteMarginSegment>,
    #[serde(default)]
    commodity: Option<KiteMarginSegment>,
}

#[derive(Deserialize, Default)]
struct KiteMarginSegment {
    #[serde(default)]
    net: f64,
    #[serde(default)]
    available: KiteMarginAvailable,
    #[serde(default)]
    utilised: KiteMarginUtilised,
}

#[derive(Deserialize, Default)]
struct KiteMarginAvailable {
    #[serde(default)]
    cash: f64,
    #[serde(default)]
    collateral: f64,
    #[serde(default)]
    intraday_payin: f64,
}

#[derive(Deserialize, Default)]
struct KiteMarginUtilised {
    #[serde(default)]
    debits: f64,
    #[serde(default)]
    m2m_realised: f64,
    #[serde(default)]
    m2m_unrealised: f64,
    #[serde(default)]
    span: f64,
    #[serde(default)]
    exposure: f64,
    #[serde(default)]
    payout: f64,
}

// Quote response
#[derive(Deserialize)]
struct KiteQuoteData {
    #[serde(default)]
    last_price: f64,
    #[serde(default)]
    ohlc: KiteOHLC,
    #[serde(default)]
    volume: i64,
    #[serde(default)]
    oi: i64,
    #[serde(default)]
    depth: KiteDepth,
    #[serde(default)]
    last_quantity: i32,
    #[serde(default)]
    last_trade_time: Option<String>,
}

#[derive(Deserialize, Default)]
struct KiteOHLC {
    #[serde(default)]
    open: f64,
    #[serde(default)]
    high: f64,
    #[serde(default)]
    low: f64,
    #[serde(default)]
    close: f64,
}

#[derive(Deserialize, Default)]
struct KiteDepth {
    #[serde(default)]
    buy: Vec<KiteDepthLevel>,
    #[serde(default)]
    sell: Vec<KiteDepthLevel>,
}

#[derive(Deserialize)]
struct KiteDepthLevel {
    #[serde(default)]
    price: f64,
    #[serde(default)]
    quantity: i32,
    #[serde(default)]
    orders: i32,
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

        // Zerodha auth token format: api_key:access_token
        let auth_token = format!("{}:{}", credentials.api_key, data.access_token);

        Ok(AuthResponse {
            auth_token,
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
            .headers(self.get_headers(auth_token))
            .form(&params)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct OrderResult {
            status: String,
            data: Option<OrderIdData>,
            message: Option<String>,
        }

        #[derive(Deserialize)]
        struct OrderIdData {
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
        if let Some(t) = &order.order_type {
            params.push(("order_type", t.clone()));
        }
        if let Some(tp) = order.trigger_price {
            params.push(("trigger_price", tp.to_string()));
        }
        if let Some(v) = &order.validity {
            params.push(("validity", v.clone()));
        }

        let response = self
            .client
            .put(format!("{}/orders/regular/{}", BASE_URL, order_id))
            .headers(self.get_headers(auth_token))
            .form(&params)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct ModifyResult {
            status: String,
            data: Option<ModifyData>,
            message: Option<String>,
        }

        #[derive(Deserialize)]
        struct ModifyData {
            order_id: String,
        }

        let result: ModifyResult = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Modify failed".to_string()),
            ));
        }

        let final_order_id = result.data.map(|d| d.order_id).unwrap_or_else(|| order_id.to_string());

        Ok(OrderResponse {
            order_id: final_order_id,
            message: Some("Order modified successfully".to_string()),
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
            .headers(self.get_headers(auth_token))
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
        let response = self
            .client
            .get(format!("{}/orders", BASE_URL))
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<Vec<KiteOrderData>> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch orders".to_string()),
            ));
        }

        let orders = result.data.unwrap_or_default();

        Ok(orders
            .into_iter()
            .map(|o| Order {
                order_id: o.order_id,
                exchange_order_id: o.exchange_order_id,
                symbol: o.tradingsymbol,
                exchange: o.exchange,
                side: o.transaction_type,
                quantity: o.quantity,
                filled_quantity: o.filled_quantity,
                pending_quantity: o.pending_quantity,
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_price,
                order_type: o.order_type,
                product: o.product,
                status: o.status,
                validity: o.validity,
                order_timestamp: o.order_timestamp.unwrap_or_default(),
                exchange_timestamp: o.exchange_timestamp,
                rejection_reason: o.status_message,
            })
            .collect())
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let response = self
            .client
            .get(format!("{}/trades", BASE_URL))
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<Vec<KiteOrderData>> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch trades".to_string()),
            ));
        }

        let trades = result.data.unwrap_or_default();

        Ok(trades
            .into_iter()
            .map(|o| Order {
                order_id: o.order_id,
                exchange_order_id: o.exchange_order_id,
                symbol: o.tradingsymbol,
                exchange: o.exchange,
                side: o.transaction_type,
                quantity: o.quantity,
                filled_quantity: o.filled_quantity,
                pending_quantity: o.pending_quantity,
                price: o.price,
                trigger_price: o.trigger_price,
                average_price: o.average_price,
                order_type: o.order_type,
                product: o.product,
                status: o.status,
                validity: o.validity,
                order_timestamp: o.order_timestamp.unwrap_or_default(),
                exchange_timestamp: o.exchange_timestamp,
                rejection_reason: o.status_message,
            })
            .collect())
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let response = self
            .client
            .get(format!("{}/portfolio/positions", BASE_URL))
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<KitePositionsResponse> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch positions".to_string()),
            ));
        }

        let positions_data = result.data.unwrap_or(KitePositionsResponse {
            net: vec![],
            day: vec![],
        });

        // Use net positions
        Ok(positions_data
            .net
            .into_iter()
            .map(|p| Position {
                symbol: p.tradingsymbol,
                exchange: p.exchange,
                product: p.product,
                quantity: p.quantity,
                overnight_quantity: p.overnight_quantity,
                average_price: p.average_price,
                ltp: p.last_price,
                pnl: p.pnl,
                realized_pnl: p.realised,
                unrealized_pnl: p.unrealised,
                buy_quantity: p.buy_quantity,
                buy_value: p.buy_value,
                sell_quantity: p.sell_quantity,
                sell_value: p.sell_value,
            })
            .collect())
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let response = self
            .client
            .get(format!("{}/portfolio/holdings", BASE_URL))
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<Vec<KiteHoldingData>> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch holdings".to_string()),
            ));
        }

        let holdings = result.data.unwrap_or_default();

        Ok(holdings
            .into_iter()
            .map(|h| {
                let quantity = h.quantity;
                let ltp = h.last_price;
                let avg_price = h.average_price;
                let current_value = quantity as f64 * ltp;
                let pnl_percentage = if avg_price > 0.0 {
                    ((ltp - avg_price) / avg_price) * 100.0
                } else {
                    0.0
                };

                Holding {
                    symbol: h.tradingsymbol,
                    exchange: h.exchange,
                    isin: h.isin,
                    quantity,
                    t1_quantity: h.t1_quantity,
                    average_price: avg_price,
                    ltp,
                    close_price: h.close_price,
                    pnl: h.pnl,
                    pnl_percentage,
                    current_value,
                }
            })
            .collect())
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let response = self
            .client
            .get(format!("{}/user/margins", BASE_URL))
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<KiteMarginResponse> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch funds".to_string()),
            ));
        }

        let margin_data = result.data.unwrap_or(KiteMarginResponse {
            equity: None,
            commodity: None,
        });

        let equity = margin_data.equity.unwrap_or(KiteMarginSegment {
            net: 0.0,
            available: KiteMarginAvailable::default(),
            utilised: KiteMarginUtilised::default(),
        });
        let commodity = margin_data.commodity.unwrap_or(KiteMarginSegment {
            net: 0.0,
            available: KiteMarginAvailable::default(),
            utilised: KiteMarginUtilised::default(),
        });

        let available_cash = equity.net + commodity.net;
        let used_margin = equity.utilised.debits + commodity.utilised.debits;
        let collateral = equity.available.collateral + commodity.available.collateral;
        let span = equity.utilised.span + commodity.utilised.span;
        let exposure = equity.utilised.exposure + commodity.utilised.exposure;
        let payin = equity.available.intraday_payin + commodity.available.intraday_payin;
        let payout = equity.utilised.payout + commodity.utilised.payout;

        Ok(Funds {
            available_cash,
            used_margin,
            total_margin: available_cash + used_margin,
            opening_balance: payin,
            payin,
            payout,
            span,
            exposure,
            collateral,
        })
    }

    async fn get_quote(
        &self,
        auth_token: &str,
        symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>> {
        // Build query string with multiple 'i' parameters
        let query_params: Vec<String> = symbols
            .iter()
            .map(|(symbol, exchange)| {
                let api_exchange = match exchange.as_str() {
                    "NSE_INDEX" => "NSE",
                    "BSE_INDEX" => "BSE",
                    _ => exchange,
                };
                format!("i={}:{}", api_exchange, symbol)
            })
            .collect();

        let url = format!("{}/quote?{}", BASE_URL, query_params.join("&"));

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<std::collections::HashMap<String, KiteQuoteData>> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch quotes".to_string()),
            ));
        }

        let quotes_data = result.data.unwrap_or_default();

        Ok(symbols
            .iter()
            .filter_map(|(symbol, exchange)| {
                let api_exchange = match exchange.as_str() {
                    "NSE_INDEX" => "NSE",
                    "BSE_INDEX" => "BSE",
                    _ => exchange,
                };
                let key = format!("{}:{}", api_exchange, symbol);
                quotes_data.get(&key).map(|q| {
                    let bid = q.depth.buy.first();
                    let ask = q.depth.sell.first();
                    let ltp = q.last_price;
                    let close = q.ohlc.close;
                    let change = ltp - close;
                    let change_percent = if close > 0.0 { (change / close) * 100.0 } else { 0.0 };

                    Quote {
                        symbol: symbol.clone(),
                        exchange: exchange.clone(),
                        ltp,
                        open: q.ohlc.open,
                        high: q.ohlc.high,
                        low: q.ohlc.low,
                        close,
                        volume: q.volume,
                        bid: bid.map(|b| b.price).unwrap_or(0.0),
                        ask: ask.map(|a| a.price).unwrap_or(0.0),
                        bid_qty: bid.map(|b| b.quantity).unwrap_or(0),
                        ask_qty: ask.map(|a| a.quantity).unwrap_or(0),
                        oi: q.oi,
                        change,
                        change_percent,
                        timestamp: q.last_trade_time.clone().unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
                    }
                })
            })
            .collect())
    }

    async fn get_market_depth(
        &self,
        auth_token: &str,
        exchange: &str,
        symbol: &str,
    ) -> Result<MarketDepth> {
        let api_exchange = match exchange {
            "NSE_INDEX" => "NSE",
            "BSE_INDEX" => "BSE",
            _ => exchange,
        };

        let url = format!("{}/quote?i={}:{}", BASE_URL, api_exchange, symbol);

        let response = self
            .client
            .get(&url)
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let result: KiteResponse<std::collections::HashMap<String, KiteQuoteData>> = response.json().await?;

        if result.status != "success" {
            return Err(AppError::Broker(
                result.message.unwrap_or_else(|| "Failed to fetch depth".to_string()),
            ));
        }

        let quotes_data = result.data.unwrap_or_default();
        let key = format!("{}:{}", api_exchange, symbol);

        let quote = quotes_data.get(&key);

        let bids: Vec<DepthLevel> = quote
            .map(|q| {
                q.depth
                    .buy
                    .iter()
                    .take(5)
                    .map(|b| DepthLevel {
                        price: b.price,
                        quantity: b.quantity,
                        orders: b.orders,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let asks: Vec<DepthLevel> = quote
            .map(|q| {
                q.depth
                    .sell
                    .iter()
                    .take(5)
                    .map(|a| DepthLevel {
                        price: a.price,
                        quantity: a.quantity,
                        orders: a.orders,
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(MarketDepth {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            bids,
            asks,
        })
    }

    async fn download_master_contract(&self, auth_token: &str) -> Result<Vec<SymbolData>> {
        // Zerodha provides CSV format for instruments
        let response = self
            .client
            .get(MASTER_CONTRACT_URL)
            .headers(self.get_headers(auth_token))
            .send()
            .await?;

        let csv_text = response.text().await?;

        // Parse CSV
        let mut symbols = vec![];
        for line in csv_text.lines().skip(1) {
            // Skip header
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() >= 12 {
                let instrument_token = fields[0].to_string();
                let exchange_token = fields[1].to_string();
                let tradingsymbol = fields[2].to_string();
                let name = fields[3].to_string();
                let exchange = fields[11].to_string();
                let instrument_type = fields[9].to_string();
                let lot_size: i32 = fields[5].parse().unwrap_or(1);
                let tick_size: f64 = fields[6].parse().unwrap_or(0.05);
                let expiry = if fields[4].is_empty() { None } else { Some(fields[4].to_string()) };
                let strike: Option<f64> = fields[7].parse().ok();

                let option_type = if tradingsymbol.ends_with("CE") {
                    Some("CE".to_string())
                } else if tradingsymbol.ends_with("PE") {
                    Some("PE".to_string())
                } else {
                    None
                };

                // Token format: instrument_token::::exchange_token
                let token = format!("{}::::{}", instrument_token, exchange_token);

                symbols.push(SymbolData {
                    symbol: tradingsymbol,
                    token,
                    exchange,
                    name,
                    lot_size,
                    tick_size,
                    instrument_type,
                    expiry,
                    strike,
                    option_type,
                });
            }
        }

        Ok(symbols)
    }
}
