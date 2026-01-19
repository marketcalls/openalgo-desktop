//! Options Service
//!
//! Handles options-related operations like option chain, Greeks, and option symbol resolution.
//! Called by both Tauri commands and REST API.

use crate::brokers::types::OrderRequest;
use crate::error::{AppError, Result};
use crate::services::{OrderService, PlaceOrderResult, QuotesService};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Option chain entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionChainEntry {
    pub strike: f64,
    pub call_symbol: Option<String>,
    pub call_token: Option<String>,
    pub call_ltp: Option<f64>,
    pub call_oi: Option<i64>,
    pub call_volume: Option<i64>,
    pub call_iv: Option<f64>,
    pub put_symbol: Option<String>,
    pub put_token: Option<String>,
    pub put_ltp: Option<f64>,
    pub put_oi: Option<i64>,
    pub put_volume: Option<i64>,
    pub put_iv: Option<f64>,
}

/// Option chain result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionChainResult {
    pub success: bool,
    pub underlying: String,
    pub underlying_ltp: f64,
    pub expiry: String,
    pub atm_strike: f64,
    pub strikes: Vec<OptionChainEntry>,
}

/// Option Greeks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionGreeks {
    pub symbol: String,
    pub ltp: f64,
    pub iv: f64,
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}

/// Resolved option symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionSymbolResult {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub strike: f64,
    pub option_type: String,
    pub expiry: String,
}

/// Synthetic future result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticFutureResult {
    pub underlying: String,
    pub underlying_ltp: f64,
    pub expiry: String,
    pub atm_strike: f64,
    pub synthetic_future_price: f64,
}

/// Options order request
#[derive(Debug, Clone, Deserialize)]
pub struct OptionsOrderRequest {
    pub underlying: String,
    pub exchange: String,
    pub option_type: String, // "CE" or "PE"
    pub strike_selection: String, // "ATM", "ITM1", "OTM1", etc.
    pub expiry_date: Option<String>,
    pub action: String,
    pub quantity: i32,
    pub product: String,
    pub pricetype: Option<String>,
}

/// Options multi-order leg
#[derive(Debug, Clone, Deserialize)]
pub struct OptionsLeg {
    pub option_type: String,
    pub strike_selection: String,
    pub action: String,
    pub quantity: i32,
}

/// Options service for business logic
pub struct OptionsService;

impl OptionsService {
    /// Get option chain for an underlying
    pub async fn get_option_chain(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        expiry_date: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<OptionChainResult> {
        info!("OptionsService::get_option_chain - {} {}", underlying, exchange);

        // Get underlying LTP
        let underlying_quote = QuotesService::get_quote(state, exchange, underlying, api_key).await?;
        let underlying_ltp = underlying_quote.ltp;

        // Calculate ATM strike (round to nearest strike interval)
        let strike_interval = Self::get_strike_interval(underlying);
        let atm_strike = (underlying_ltp / strike_interval).round() * strike_interval;

        // Find option symbols in cache
        let expiry = expiry_date.unwrap_or("");
        let strikes = Self::build_option_chain(state, underlying, exchange, expiry, atm_strike, api_key).await?;

        Ok(OptionChainResult {
            success: true,
            underlying: underlying.to_string(),
            underlying_ltp,
            expiry: expiry.to_string(),
            atm_strike,
            strikes,
        })
    }

    /// Calculate option Greeks using Black-Scholes
    pub async fn get_option_greeks(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        api_key: Option<&str>,
    ) -> Result<OptionGreeks> {
        info!("OptionsService::get_option_greeks - {} {}", symbol, exchange);

        // Get option quote
        let quote = QuotesService::get_quote(state, exchange, symbol, api_key).await?;

        // For now, return placeholder Greeks
        // Full implementation would use Black-Scholes with:
        // - Underlying price
        // - Strike price
        // - Time to expiry
        // - Risk-free rate
        // - Implied volatility
        Ok(OptionGreeks {
            symbol: symbol.to_string(),
            ltp: quote.ltp,
            iv: 0.0,     // Would calculate IV from option price
            delta: 0.0,  // Would calculate from Black-Scholes
            gamma: 0.0,
            theta: 0.0,
            vega: 0.0,
            rho: 0.0,
        })
    }

    /// Resolve option symbol based on strike selection
    pub fn get_option_symbol(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        option_type: &str,
        strike_selection: &str,
        expiry_date: Option<&str>,
        underlying_ltp: f64,
    ) -> Result<OptionSymbolResult> {
        info!(
            "OptionsService::get_option_symbol - {} {} {} {}",
            underlying, exchange, option_type, strike_selection
        );

        let strike_interval = Self::get_strike_interval(underlying);
        let atm_strike = (underlying_ltp / strike_interval).round() * strike_interval;

        // Parse strike selection (ATM, ITM1, ITM2, OTM1, OTM2, etc.)
        let target_strike = Self::calculate_target_strike(atm_strike, strike_interval, option_type, strike_selection);

        // Find matching symbol in cache
        let symbol = Self::find_option_symbol(state, underlying, exchange, option_type, target_strike, expiry_date)?;

        Ok(symbol)
    }

    /// Calculate synthetic future price
    pub async fn get_synthetic_future(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        expiry_date: &str,
        api_key: Option<&str>,
    ) -> Result<SyntheticFutureResult> {
        info!("OptionsService::get_synthetic_future - {} {}", underlying, exchange);

        // Get underlying LTP
        let underlying_quote = QuotesService::get_quote(state, exchange, underlying, api_key).await?;
        let underlying_ltp = underlying_quote.ltp;

        let strike_interval = Self::get_strike_interval(underlying);
        let atm_strike = (underlying_ltp / strike_interval).round() * strike_interval;

        // Get ATM call and put prices
        let call_symbol = Self::find_option_symbol(state, underlying, exchange, "CE", atm_strike, Some(expiry_date))?;
        let put_symbol = Self::find_option_symbol(state, underlying, exchange, "PE", atm_strike, Some(expiry_date))?;

        let call_quote = QuotesService::get_quote(state, exchange, &call_symbol.symbol, api_key).await?;
        let put_quote = QuotesService::get_quote(state, exchange, &put_symbol.symbol, api_key).await?;

        // Synthetic Future = Strike + Call Price - Put Price
        let synthetic_future_price = atm_strike + call_quote.ltp - put_quote.ltp;

        Ok(SyntheticFutureResult {
            underlying: underlying.to_string(),
            underlying_ltp,
            expiry: expiry_date.to_string(),
            atm_strike,
            synthetic_future_price,
        })
    }

    /// Place options order
    pub async fn place_options_order(
        state: &AppState,
        req: OptionsOrderRequest,
        api_key: Option<&str>,
    ) -> Result<PlaceOrderResult> {
        info!("OptionsService::place_options_order - {} {}", req.underlying, req.option_type);

        // Get underlying LTP for strike calculation
        let underlying_quote = QuotesService::get_quote(state, &req.exchange, &req.underlying, api_key).await?;

        // Resolve option symbol
        let option_symbol = Self::get_option_symbol(
            state,
            &req.underlying,
            &req.exchange,
            &req.option_type,
            &req.strike_selection,
            req.expiry_date.as_deref(),
            underlying_quote.ltp,
        )?;

        // Place order
        let order_request = OrderRequest {
            symbol: option_symbol.symbol,
            exchange: req.exchange,
            side: req.action,
            quantity: req.quantity,
            order_type: req.pricetype.unwrap_or_else(|| "MARKET".to_string()),
            product: req.product,
            price: 0.0,
            trigger_price: None,
            disclosed_quantity: None,
            validity: "DAY".to_string(),
            amo: false,
        };

        OrderService::place_order(state, order_request, api_key).await
    }

    /// Place multi-leg options order
    pub async fn place_options_multi_order(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        expiry_date: Option<&str>,
        product: &str,
        legs: Vec<OptionsLeg>,
        api_key: Option<&str>,
    ) -> Result<Vec<PlaceOrderResult>> {
        info!("OptionsService::place_options_multi_order - {} legs", legs.len());

        // Get underlying LTP
        let underlying_quote = QuotesService::get_quote(state, exchange, underlying, api_key).await?;

        let mut results = Vec::new();

        for leg in legs {
            let option_symbol = Self::get_option_symbol(
                state,
                underlying,
                exchange,
                &leg.option_type,
                &leg.strike_selection,
                expiry_date,
                underlying_quote.ltp,
            )?;

            let order_request = OrderRequest {
                symbol: option_symbol.symbol,
                exchange: exchange.to_string(),
                side: leg.action,
                quantity: leg.quantity,
                order_type: "MARKET".to_string(),
                product: product.to_string(),
                price: 0.0,
                trigger_price: None,
                disclosed_quantity: None,
                validity: "DAY".to_string(),
                amo: false,
            };

            match OrderService::place_order(state, order_request, api_key).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(PlaceOrderResult {
                        success: false,
                        order_id: None,
                        message: e.to_string(),
                        mode: "live".to_string(),
                    });
                }
            }
        }

        Ok(results)
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    /// Get strike interval for an underlying
    fn get_strike_interval(underlying: &str) -> f64 {
        match underlying.to_uppercase().as_str() {
            "NIFTY" | "NIFTY50" => 50.0,
            "BANKNIFTY" => 100.0,
            "FINNIFTY" => 50.0,
            "MIDCPNIFTY" => 25.0,
            "SENSEX" => 100.0,
            "BANKEX" => 100.0,
            _ => 50.0, // Default
        }
    }

    /// Calculate target strike based on selection
    fn calculate_target_strike(atm_strike: f64, interval: f64, option_type: &str, selection: &str) -> f64 {
        let selection_upper = selection.to_uppercase();

        if selection_upper == "ATM" {
            return atm_strike;
        }

        // Parse ITM/OTM with offset (e.g., "ITM1", "OTM2")
        let (is_itm, offset) = if selection_upper.starts_with("ITM") {
            (true, selection_upper[3..].parse::<i32>().unwrap_or(1))
        } else if selection_upper.starts_with("OTM") {
            (false, selection_upper[3..].parse::<i32>().unwrap_or(1))
        } else {
            return atm_strike;
        };

        let offset_amount = offset as f64 * interval;

        match (option_type.to_uppercase().as_str(), is_itm) {
            ("CE", true) => atm_strike - offset_amount,   // ITM call = lower strike
            ("CE", false) => atm_strike + offset_amount,  // OTM call = higher strike
            ("PE", true) => atm_strike + offset_amount,   // ITM put = higher strike
            ("PE", false) => atm_strike - offset_amount,  // OTM put = lower strike
            _ => atm_strike,
        }
    }

    /// Find option symbol in cache
    fn find_option_symbol(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        option_type: &str,
        strike: f64,
        expiry_date: Option<&str>,
    ) -> Result<OptionSymbolResult> {
        // Search for matching symbol in cache
        // This is a simplified implementation - actual matching depends on broker symbol format
        for entry in state.symbol_cache.iter() {
            let s = entry.value();
            if s.exchange.eq_ignore_ascii_case(exchange)
                && s.symbol.starts_with(underlying)
                && s.instrument_type.contains(option_type)
            {
                // Check if strike matches (would need to parse from symbol)
                // For now, return first match as placeholder
                return Ok(OptionSymbolResult {
                    symbol: s.symbol.clone(),
                    token: s.token.clone(),
                    exchange: s.exchange.clone(),
                    strike,
                    option_type: option_type.to_string(),
                    expiry: expiry_date.unwrap_or("").to_string(),
                });
            }
        }

        Err(AppError::NotFound(format!(
            "Option symbol not found: {} {} {} {}",
            underlying, exchange, option_type, strike
        )))
    }

    /// Build option chain from cache and quotes
    async fn build_option_chain(
        state: &AppState,
        underlying: &str,
        exchange: &str,
        expiry: &str,
        atm_strike: f64,
        _api_key: Option<&str>,
    ) -> Result<Vec<OptionChainEntry>> {
        let strike_interval = Self::get_strike_interval(underlying);

        // Generate strikes around ATM (10 above and 10 below)
        let mut strikes = Vec::new();
        for i in -10..=10 {
            let strike = atm_strike + (i as f64 * strike_interval);

            // Find CE and PE symbols for this strike
            let call_symbol = Self::find_option_symbol(state, underlying, exchange, "CE", strike, Some(expiry)).ok();
            let put_symbol = Self::find_option_symbol(state, underlying, exchange, "PE", strike, Some(expiry)).ok();

            strikes.push(OptionChainEntry {
                strike,
                call_symbol: call_symbol.as_ref().map(|s| s.symbol.clone()),
                call_token: call_symbol.as_ref().map(|s| s.token.clone()),
                call_ltp: None, // Would fetch quotes
                call_oi: None,
                call_volume: None,
                call_iv: None,
                put_symbol: put_symbol.as_ref().map(|s| s.symbol.clone()),
                put_token: put_symbol.as_ref().map(|s| s.token.clone()),
                put_ltp: None,
                put_oi: None,
                put_volume: None,
                put_iv: None,
            });
        }

        Ok(strikes)
    }
}
