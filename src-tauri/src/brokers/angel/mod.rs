//! Angel One broker adapter

#![allow(non_snake_case)]

use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "https://apiconnect.angelone.in";
const MASTER_CONTRACT_URL: &str = "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

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

// Angel One API response structures
#[derive(Deserialize)]
struct AngelResponse<T> {
    status: bool,
    message: String,
    data: Option<T>,
}

// Order book response
#[derive(Deserialize)]
struct AngelOrderData {
    orderid: String,
    #[serde(default)]
    exchange_orderid: Option<String>,
    tradingsymbol: String,
    exchange: String,
    transactiontype: String,
    #[serde(default)]
    quantity: StringOrInt,
    #[serde(default)]
    filledshares: StringOrInt,
    #[serde(default)]
    unfilledshares: StringOrInt,
    #[serde(default)]
    price: StringOrFloat,
    #[serde(default)]
    triggerprice: StringOrFloat,
    #[serde(default)]
    averageprice: StringOrFloat,
    #[serde(default)]
    ordertype: String,
    #[serde(default)]
    producttype: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    duration: String,
    #[serde(default)]
    updatetime: String,
    #[serde(default)]
    exchtime: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

// Position response
#[derive(Deserialize)]
struct AngelPositionData {
    tradingsymbol: String,
    exchange: String,
    #[serde(default)]
    producttype: String,
    #[serde(default)]
    netqty: StringOrInt,
    #[serde(default)]
    cfbuyqty: StringOrInt,
    #[serde(default)]
    avgnetprice: StringOrFloat,
    #[serde(default)]
    ltp: StringOrFloat,
    #[serde(default)]
    pnl: StringOrFloat,
    #[serde(default)]
    realised: StringOrFloat,
    #[serde(default)]
    unrealised: StringOrFloat,
    #[serde(default)]
    buyqty: StringOrInt,
    #[serde(default)]
    buyvalue: StringOrFloat,
    #[serde(default)]
    sellqty: StringOrInt,
    #[serde(default)]
    sellvalue: StringOrFloat,
}

// Holdings response
#[derive(Deserialize)]
struct AngelHoldingsResponse {
    holdings: Option<Vec<AngelHoldingData>>,
    #[serde(default)]
    totalholding: Option<AngelTotalHolding>,
}

#[derive(Deserialize)]
struct AngelHoldingData {
    tradingsymbol: String,
    exchange: String,
    #[serde(default)]
    isin: Option<String>,
    #[serde(default)]
    quantity: StringOrInt,
    #[serde(default)]
    t1quantity: StringOrInt,
    #[serde(default)]
    averageprice: StringOrFloat,
    #[serde(default)]
    ltp: StringOrFloat,
    #[serde(default)]
    close: StringOrFloat,
    #[serde(default)]
    profitandloss: StringOrFloat,
    #[serde(default)]
    pnlpercentage: StringOrFloat,
}

#[derive(Deserialize)]
struct AngelTotalHolding {
    #[serde(default)]
    totalholdingvalue: StringOrFloat,
}

// Funds/RMS response
#[derive(Deserialize)]
struct AngelFundsData {
    #[serde(default)]
    availablecash: StringOrFloat,
    #[serde(default)]
    utilisedmargin: StringOrFloat,
    #[serde(default)]
    net: StringOrFloat,
    #[serde(default)]
    availableintradaypayin: StringOrFloat,
    #[serde(default)]
    utilisedpayout: StringOrFloat,
    #[serde(default)]
    utiliseddebits: StringOrFloat,
    #[serde(default)]
    span: StringOrFloat,
    #[serde(default)]
    exposure: StringOrFloat,
    #[serde(default)]
    collateral: StringOrFloat,
}

// Quote response
#[derive(Deserialize)]
struct AngelQuoteResponse {
    fetched: Option<Vec<AngelQuoteData>>,
}

#[derive(Deserialize)]
struct AngelQuoteData {
    #[serde(default)]
    tradingSymbol: String,
    #[serde(default)]
    exchange: String,
    #[serde(default)]
    symbolToken: String,
    #[serde(default)]
    ltp: StringOrFloat,
    #[serde(default)]
    open: StringOrFloat,
    #[serde(default)]
    high: StringOrFloat,
    #[serde(default)]
    low: StringOrFloat,
    #[serde(default)]
    close: StringOrFloat,
    #[serde(default)]
    tradeVolume: StringOrInt64,
    #[serde(default)]
    opnInterest: StringOrInt64,
    #[serde(default)]
    depth: Option<AngelDepthData>,
}

#[derive(Deserialize, Default)]
struct AngelDepthData {
    #[serde(default)]
    buy: Vec<AngelDepthLevel>,
    #[serde(default)]
    sell: Vec<AngelDepthLevel>,
}

#[derive(Deserialize)]
struct AngelDepthLevel {
    #[serde(default)]
    price: StringOrFloat,
    #[serde(default)]
    quantity: StringOrInt,
    #[serde(default)]
    orders: StringOrInt,
}

// Master contract response
#[derive(Deserialize)]
struct AngelSymbolData {
    token: String,
    symbol: String,
    name: String,
    #[serde(default)]
    exch_seg: String,
    #[serde(default)]
    instrumenttype: String,
    #[serde(default)]
    lotsize: StringOrInt,
    #[serde(default)]
    tick_size: StringOrFloat,
    #[serde(default)]
    expiry: Option<String>,
    #[serde(default)]
    strike: StringOrFloat,
}

// Helper types for flexible deserialization
#[derive(Deserialize, Default, Clone)]
#[serde(untagged)]
enum StringOrInt {
    #[default]
    None,
    Str(String),
    Int(i32),
}

impl StringOrInt {
    fn to_i32(&self) -> i32 {
        match self {
            StringOrInt::None => 0,
            StringOrInt::Str(s) => s.parse().unwrap_or(0),
            StringOrInt::Int(i) => *i,
        }
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(untagged)]
enum StringOrInt64 {
    #[default]
    None,
    Str(String),
    Int(i64),
}

impl StringOrInt64 {
    fn to_i64(&self) -> i64 {
        match self {
            StringOrInt64::None => 0,
            StringOrInt64::Str(s) => s.parse().unwrap_or(0),
            StringOrInt64::Int(i) => *i,
        }
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(untagged)]
enum StringOrFloat {
    #[default]
    None,
    Str(String),
    Float(f64),
    Int(i64),
}

impl StringOrFloat {
    fn to_f64(&self) -> f64 {
        match self {
            StringOrFloat::None => 0.0,
            StringOrFloat::Str(s) => s.parse().unwrap_or(0.0),
            StringOrFloat::Float(f) => *f,
            StringOrFloat::Int(i) => *i as f64,
        }
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
        auth_token: &str,
        order_id: &str,
        order: ModifyOrderRequest,
    ) -> Result<OrderResponse> {
        #[derive(Serialize)]
        struct ModifyRequest {
            variety: String,
            orderid: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            quantity: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            price: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            ordertype: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            triggerprice: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            duration: Option<String>,
        }

        let request = ModifyRequest {
            variety: "NORMAL".to_string(),
            orderid: order_id.to_string(),
            quantity: order.quantity.map(|q| q.to_string()),
            price: order.price.map(|p| p.to_string()),
            ordertype: order.order_type,
            triggerprice: order.trigger_price.map(|p| p.to_string()),
            duration: order.validity,
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/secure/angelbroking/order/v1/modifyOrder",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        #[derive(Deserialize)]
        struct ModifyResult {
            status: bool,
            message: String,
            data: Option<ModifyData>,
        }

        #[derive(Deserialize)]
        struct ModifyData {
            orderid: String,
        }

        let result: ModifyResult = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let data = result.data;
        let final_order_id = data.map(|d| d.orderid).unwrap_or_else(|| order_id.to_string());

        Ok(OrderResponse {
            order_id: final_order_id,
            message: Some(result.message),
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
        let response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getOrderBook",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        let result: AngelResponse<Vec<AngelOrderData>> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let orders = result.data.unwrap_or_default();

        Ok(orders
            .into_iter()
            .map(|o| {
                let order_type = match o.ordertype.as_str() {
                    "STOPLOSS_LIMIT" => "SL".to_string(),
                    "STOPLOSS_MARKET" => "SL-M".to_string(),
                    other => other.to_string(),
                };

                let product = match (o.exchange.as_str(), o.producttype.as_str()) {
                    ("NSE" | "BSE", "DELIVERY") => "CNC".to_string(),
                    (_, "INTRADAY") => "MIS".to_string(),
                    ("NFO" | "MCX" | "BFO" | "CDS", "CARRYFORWARD") => "NRML".to_string(),
                    _ => o.producttype,
                };

                Order {
                    order_id: o.orderid,
                    exchange_order_id: o.exchange_orderid,
                    symbol: o.tradingsymbol,
                    exchange: o.exchange,
                    side: o.transactiontype,
                    quantity: o.quantity.to_i32(),
                    filled_quantity: o.filledshares.to_i32(),
                    pending_quantity: o.unfilledshares.to_i32(),
                    price: o.price.to_f64(),
                    trigger_price: o.triggerprice.to_f64(),
                    average_price: o.averageprice.to_f64(),
                    order_type,
                    product,
                    status: o.status,
                    validity: o.duration,
                    order_timestamp: o.updatetime,
                    exchange_timestamp: o.exchtime,
                    rejection_reason: o.text,
                }
            })
            .collect())
    }

    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>> {
        let response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getTradeBook",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        let result: AngelResponse<Vec<AngelOrderData>> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let trades = result.data.unwrap_or_default();

        Ok(trades
            .into_iter()
            .map(|o| {
                let product = match (o.exchange.as_str(), o.producttype.as_str()) {
                    ("NSE" | "BSE", "DELIVERY") => "CNC".to_string(),
                    (_, "INTRADAY") => "MIS".to_string(),
                    ("NFO" | "MCX" | "BFO" | "CDS", "CARRYFORWARD") => "NRML".to_string(),
                    _ => o.producttype,
                };

                Order {
                    order_id: o.orderid,
                    exchange_order_id: o.exchange_orderid,
                    symbol: o.tradingsymbol,
                    exchange: o.exchange,
                    side: o.transactiontype,
                    quantity: o.quantity.to_i32(),
                    filled_quantity: o.filledshares.to_i32(),
                    pending_quantity: o.unfilledshares.to_i32(),
                    price: o.price.to_f64(),
                    trigger_price: o.triggerprice.to_f64(),
                    average_price: o.averageprice.to_f64(),
                    order_type: o.ordertype,
                    product,
                    status: o.status,
                    validity: o.duration,
                    order_timestamp: o.updatetime,
                    exchange_timestamp: o.exchtime,
                    rejection_reason: o.text,
                }
            })
            .collect())
    }

    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>> {
        let response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/order/v1/getPosition",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        let result: AngelResponse<Vec<AngelPositionData>> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let positions = result.data.unwrap_or_default();

        Ok(positions
            .into_iter()
            .map(|p| {
                let product = match (p.exchange.as_str(), p.producttype.as_str()) {
                    ("NSE" | "BSE", "DELIVERY") => "CNC".to_string(),
                    (_, "INTRADAY") => "MIS".to_string(),
                    ("NFO" | "MCX" | "BFO" | "CDS", "CARRYFORWARD") => "NRML".to_string(),
                    _ => p.producttype,
                };

                Position {
                    symbol: p.tradingsymbol,
                    exchange: p.exchange,
                    product,
                    quantity: p.netqty.to_i32(),
                    overnight_quantity: p.cfbuyqty.to_i32(),
                    average_price: p.avgnetprice.to_f64(),
                    ltp: p.ltp.to_f64(),
                    pnl: p.pnl.to_f64(),
                    realized_pnl: p.realised.to_f64(),
                    unrealized_pnl: p.unrealised.to_f64(),
                    buy_quantity: p.buyqty.to_i32(),
                    buy_value: p.buyvalue.to_f64(),
                    sell_quantity: p.sellqty.to_i32(),
                    sell_value: p.sellvalue.to_f64(),
                }
            })
            .collect())
    }

    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>> {
        let response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/portfolio/v1/getAllHolding",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        let result: AngelResponse<AngelHoldingsResponse> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let holdings_data = result.data.unwrap_or(AngelHoldingsResponse {
            holdings: None,
            totalholding: None,
        });

        let holdings = holdings_data.holdings.unwrap_or_default();

        Ok(holdings
            .into_iter()
            .map(|h| {
                let quantity = h.quantity.to_i32();
                let ltp = h.ltp.to_f64();
                let current_value = quantity as f64 * ltp;

                Holding {
                    symbol: h.tradingsymbol,
                    exchange: h.exchange,
                    isin: h.isin,
                    quantity,
                    t1_quantity: h.t1quantity.to_i32(),
                    average_price: h.averageprice.to_f64(),
                    ltp,
                    close_price: h.close.to_f64(),
                    pnl: h.profitandloss.to_f64(),
                    pnl_percentage: h.pnlpercentage.to_f64(),
                    current_value,
                }
            })
            .collect())
    }

    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        let response = self
            .client
            .get(format!(
                "{}/rest/secure/angelbroking/user/v1/getRMS",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .send()
            .await?;

        let result: AngelResponse<AngelFundsData> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let data = result.data.unwrap_or_default();
        let available_cash = data.availablecash.to_f64();
        let used_margin = data.utilisedmargin.to_f64();
        let payout = data.utilisedpayout.to_f64();

        // Calculate collateral as availablecash - utilisedpayout
        let collateral = available_cash - payout;

        Ok(Funds {
            available_cash,
            used_margin,
            total_margin: data.net.to_f64(),
            opening_balance: data.availableintradaypayin.to_f64(),
            payin: data.availableintradaypayin.to_f64(),
            payout,
            span: data.span.to_f64(),
            exposure: data.exposure.to_f64(),
            collateral,
        })
    }

    async fn get_quote(
        &self,
        auth_token: &str,
        symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>> {
        // Group symbols by exchange
        let mut exchange_tokens: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        for (symbol, exchange) in &symbols {
            // TODO: Get token from symbol cache
            // For now, using symbol as placeholder
            let api_exchange = match exchange.as_str() {
                "NSE_INDEX" => "NSE",
                "BSE_INDEX" => "BSE",
                "MCX_INDEX" => "MCX",
                other => other,
            };

            exchange_tokens
                .entry(api_exchange.to_string())
                .or_default()
                .push(symbol.clone()); // Should be token, not symbol
        }

        #[derive(Serialize)]
        struct QuoteRequest {
            mode: String,
            exchangeTokens: std::collections::HashMap<String, Vec<String>>,
        }

        let request = QuoteRequest {
            mode: "FULL".to_string(),
            exchangeTokens: exchange_tokens,
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/secure/angelbroking/market/v1/quote/",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        let result: AngelResponse<AngelQuoteResponse> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let quote_data = result.data.unwrap_or(AngelQuoteResponse { fetched: None });
        let quotes = quote_data.fetched.unwrap_or_default();

        Ok(quotes
            .into_iter()
            .map(|q| {
                let depth = q.depth.unwrap_or_default();
                let bid = depth.buy.first();
                let ask = depth.sell.first();
                let ltp = q.ltp.to_f64();
                let close = q.close.to_f64();
                let change = ltp - close;
                let change_percent = if close > 0.0 { (change / close) * 100.0 } else { 0.0 };

                Quote {
                    symbol: q.tradingSymbol,
                    exchange: q.exchange,
                    ltp,
                    open: q.open.to_f64(),
                    high: q.high.to_f64(),
                    low: q.low.to_f64(),
                    close,
                    volume: q.tradeVolume.to_i64(),
                    bid: bid.map(|b| b.price.to_f64()).unwrap_or(0.0),
                    ask: ask.map(|a| a.price.to_f64()).unwrap_or(0.0),
                    bid_qty: bid.map(|b| b.quantity.to_i32()).unwrap_or(0),
                    ask_qty: ask.map(|a| a.quantity.to_i32()).unwrap_or(0),
                    oi: q.opnInterest.to_i64(),
                    change,
                    change_percent,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                }
            })
            .collect())
    }

    async fn get_market_depth(
        &self,
        auth_token: &str,
        exchange: &str,
        symbol: &str,
    ) -> Result<MarketDepth> {
        // Use the same quote endpoint with FULL mode for depth
        let quotes = self.get_quote(auth_token, vec![(symbol.to_string(), exchange.to_string())]).await?;

        if quotes.is_empty() {
            return Ok(MarketDepth {
                symbol: symbol.to_string(),
                exchange: exchange.to_string(),
                bids: vec![],
                asks: vec![],
            });
        }

        // Get full depth data
        let mut exchange_tokens: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        let api_exchange = match exchange {
            "NSE_INDEX" => "NSE",
            "BSE_INDEX" => "BSE",
            "MCX_INDEX" => "MCX",
            other => other,
        };

        exchange_tokens.insert(api_exchange.to_string(), vec![symbol.to_string()]);

        #[derive(Serialize)]
        struct QuoteRequest {
            mode: String,
            exchangeTokens: std::collections::HashMap<String, Vec<String>>,
        }

        let request = QuoteRequest {
            mode: "FULL".to_string(),
            exchangeTokens: exchange_tokens,
        };

        let response = self
            .client
            .post(format!(
                "{}/rest/secure/angelbroking/market/v1/quote/",
                BASE_URL
            ))
            .headers(self.get_headers("", Some(auth_token)))
            .json(&request)
            .send()
            .await?;

        let result: AngelResponse<AngelQuoteResponse> = response.json().await?;

        if !result.status {
            return Err(AppError::Broker(result.message));
        }

        let quote_data = result.data.unwrap_or(AngelQuoteResponse { fetched: None });
        let quotes_vec = quote_data.fetched.unwrap_or_default();

        if quotes_vec.is_empty() {
            return Ok(MarketDepth {
                symbol: symbol.to_string(),
                exchange: exchange.to_string(),
                bids: vec![],
                asks: vec![],
            });
        }

        let quote = &quotes_vec[0];
        let depth = quote.depth.as_ref();

        let bids: Vec<DepthLevel> = depth
            .map(|d| {
                d.buy
                    .iter()
                    .take(5)
                    .map(|b| DepthLevel {
                        price: b.price.to_f64(),
                        quantity: b.quantity.to_i32(),
                        orders: b.orders.to_i32(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let asks: Vec<DepthLevel> = depth
            .map(|d| {
                d.sell
                    .iter()
                    .take(5)
                    .map(|a| DepthLevel {
                        price: a.price.to_f64(),
                        quantity: a.quantity.to_i32(),
                        orders: a.orders.to_i32(),
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

    async fn download_master_contract(&self, _auth_token: &str) -> Result<Vec<SymbolData>> {
        let response = self
            .client
            .get(MASTER_CONTRACT_URL)
            .send()
            .await?;

        let symbols: Vec<AngelSymbolData> = response.json().await?;

        Ok(symbols
            .into_iter()
            .map(|s| {
                let option_type = if s.symbol.ends_with("CE") {
                    Some("CE".to_string())
                } else if s.symbol.ends_with("PE") {
                    Some("PE".to_string())
                } else {
                    None
                };

                let strike = s.strike.to_f64();
                let strike_opt = if strike > 0.0 { Some(strike / 100.0) } else { None };

                SymbolData {
                    symbol: s.symbol,
                    token: s.token,
                    exchange: s.exch_seg,
                    name: s.name,
                    lot_size: s.lotsize.to_i32(),
                    tick_size: s.tick_size.to_f64() / 100.0,
                    instrument_type: s.instrumenttype,
                    expiry: s.expiry,
                    strike: strike_opt,
                    option_type,
                }
            })
            .collect())
    }
}

impl Default for AngelFundsData {
    fn default() -> Self {
        Self {
            availablecash: StringOrFloat::None,
            utilisedmargin: StringOrFloat::None,
            net: StringOrFloat::None,
            availableintradaypayin: StringOrFloat::None,
            utilisedpayout: StringOrFloat::None,
            utiliseddebits: StringOrFloat::None,
            span: StringOrFloat::None,
            exposure: StringOrFloat::None,
            collateral: StringOrFloat::None,
        }
    }
}
