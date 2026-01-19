//! Rate limiting middleware for the REST API
//!
//! Implements token bucket rate limiting to prevent hitting broker API limits.
//! Different rate limits are applied to different endpoint types:
//! - General API: api_rate_limit (default 100/s)
//! - Order placement: order_rate_limit (default 10/s)
//! - Smart orders: smart_order_rate_limit (default 2/s)

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Rate limit type for different endpoint categories
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitType {
    /// General API calls (quotes, positions, etc.)
    General,
    /// Order placement calls
    Order,
    /// Smart order calls (position sizing, multi-leg)
    SmartOrder,
}

/// Token bucket rate limiter
#[derive(Debug)]
pub struct TokenBucket {
    /// Maximum tokens (requests) allowed per period
    capacity: u32,
    /// Current available tokens
    tokens: f64,
    /// Tokens added per second
    refill_rate: f64,
    /// Last refill time
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new token bucket
    pub fn new(rate_per_second: u32) -> Self {
        Self {
            capacity: rate_per_second,
            tokens: rate_per_second as f64,
            refill_rate: rate_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    /// Update the rate limit
    pub fn update_rate(&mut self, rate_per_second: u32) {
        self.capacity = rate_per_second;
        self.refill_rate = rate_per_second as f64;
        // Don't exceed new capacity
        if self.tokens > self.capacity as f64 {
            self.tokens = self.capacity as f64;
        }
    }

    /// Try to consume a token, returns true if allowed
    pub fn try_acquire(&mut self) -> bool {
        // Refill tokens based on elapsed time
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let refill_amount = elapsed.as_secs_f64() * self.refill_rate;

        self.tokens = (self.tokens + refill_amount).min(self.capacity as f64);
        self.last_refill = now;

        // Try to consume a token
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Get time until a token will be available
    pub fn time_until_available(&self) -> Duration {
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            let tokens_needed = 1.0 - self.tokens;
            Duration::from_secs_f64(tokens_needed / self.refill_rate)
        }
    }
}

/// Shared rate limiter state
#[derive(Debug)]
pub struct RateLimiterState {
    /// Rate limiters per type
    limiters: Mutex<HashMap<RateLimitType, TokenBucket>>,
    /// Smart order delay in seconds
    smart_order_delay: Mutex<f64>,
    /// Last smart order time
    last_smart_order: Mutex<Option<Instant>>,
}

impl RateLimiterState {
    /// Create new rate limiter state with default values
    pub fn new(api_rate: u32, order_rate: u32, smart_order_rate: u32, smart_order_delay: f64) -> Self {
        let mut limiters = HashMap::new();
        limiters.insert(RateLimitType::General, TokenBucket::new(api_rate));
        limiters.insert(RateLimitType::Order, TokenBucket::new(order_rate));
        limiters.insert(RateLimitType::SmartOrder, TokenBucket::new(smart_order_rate));

        Self {
            limiters: Mutex::new(limiters),
            smart_order_delay: Mutex::new(smart_order_delay),
            last_smart_order: Mutex::new(None),
        }
    }

    /// Update rate limits from config
    pub fn update_config(&self, api_rate: u32, order_rate: u32, smart_order_rate: u32, smart_order_delay: f64) {
        let mut limiters = self.limiters.lock();
        if let Some(limiter) = limiters.get_mut(&RateLimitType::General) {
            limiter.update_rate(api_rate);
        }
        if let Some(limiter) = limiters.get_mut(&RateLimitType::Order) {
            limiter.update_rate(order_rate);
        }
        if let Some(limiter) = limiters.get_mut(&RateLimitType::SmartOrder) {
            limiter.update_rate(smart_order_rate);
        }
        *self.smart_order_delay.lock() = smart_order_delay;
    }

    /// Try to acquire a token for the given rate limit type
    pub fn try_acquire(&self, rate_type: RateLimitType) -> bool {
        let mut limiters = self.limiters.lock();
        if let Some(limiter) = limiters.get_mut(&rate_type) {
            limiter.try_acquire()
        } else {
            true // If limiter doesn't exist, allow
        }
    }

    /// Get time until rate limit allows a request
    pub fn time_until_available(&self, rate_type: RateLimitType) -> Duration {
        let limiters = self.limiters.lock();
        if let Some(limiter) = limiters.get(&rate_type) {
            limiter.time_until_available()
        } else {
            Duration::ZERO
        }
    }

    /// Check and apply smart order delay
    /// Returns Ok(()) if allowed to proceed, Err(wait_time) if need to wait
    pub fn check_smart_order_delay(&self) -> Result<(), Duration> {
        let delay = *self.smart_order_delay.lock();
        let mut last_order = self.last_smart_order.lock();

        if let Some(last_time) = *last_order {
            let elapsed = last_time.elapsed();
            let required_delay = Duration::from_secs_f64(delay);

            if elapsed < required_delay {
                return Err(required_delay - elapsed);
            }
        }

        // Update last order time
        *last_order = Some(Instant::now());
        Ok(())
    }
}

/// Determine rate limit type based on request path
pub fn get_rate_limit_type(path: &str) -> RateLimitType {
    // Smart order endpoints
    if path.contains("/placesmartorder")
        || path.contains("/optionsorder")
        || path.contains("/optionsmultiorder")
        || path.contains("/basketorder")
        || path.contains("/splitorder")
    {
        return RateLimitType::SmartOrder;
    }

    // Order endpoints
    if path.contains("/placeorder")
        || path.contains("/modifyorder")
        || path.contains("/cancelorder")
        || path.contains("/cancelallorder")
        || path.contains("/closeposition")
    {
        return RateLimitType::Order;
    }

    // Everything else is general
    RateLimitType::General
}

/// Rate limiting middleware
pub async fn rate_limit_middleware(
    State(state): State<Arc<RateLimiterState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let rate_type = get_rate_limit_type(&path);

    // For smart orders, also check the delay between orders
    if rate_type == RateLimitType::SmartOrder {
        if let Err(wait_time) = state.check_smart_order_delay() {
            tracing::warn!(
                "Smart order delay not met, need to wait {:?}ms",
                wait_time.as_millis()
            );
            return rate_limit_response(wait_time, "smart_order_delay");
        }
    }

    // Check rate limit
    if !state.try_acquire(rate_type) {
        let wait_time = state.time_until_available(rate_type);
        tracing::warn!(
            "Rate limit exceeded for {:?}, path: {}, retry after {:?}ms",
            rate_type,
            path,
            wait_time.as_millis()
        );
        return rate_limit_response(wait_time, &format!("{:?}", rate_type).to_lowercase());
    }

    next.run(request).await
}

/// Create a rate limit exceeded response
fn rate_limit_response(retry_after: Duration, limit_type: &str) -> Response {
    let retry_seconds = retry_after.as_secs_f64().ceil() as u64;

    let body = Json(json!({
        "status": "error",
        "error_type": "rate_limit_exceeded",
        "message": format!("Rate limit exceeded for {}. Please retry after {} seconds.", limit_type, retry_seconds),
        "retry_after_ms": retry_after.as_millis()
    }));

    let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();

    // Add Retry-After header
    response.headers_mut().insert(
        "Retry-After",
        retry_seconds.to_string().parse().unwrap(),
    );

    // Add rate limit headers
    response.headers_mut().insert(
        "X-RateLimit-Type",
        limit_type.parse().unwrap(),
    );

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_basic() {
        let mut bucket = TokenBucket::new(10); // 10 per second

        // Should allow first 10 requests
        for _ in 0..10 {
            assert!(bucket.try_acquire());
        }

        // 11th should fail
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn test_token_bucket_refill() {
        let mut bucket = TokenBucket::new(100);

        // Drain all tokens
        for _ in 0..100 {
            bucket.try_acquire();
        }
        assert!(!bucket.try_acquire());

        // Simulate time passing (force refill)
        bucket.last_refill = Instant::now() - Duration::from_millis(100);

        // Should have ~10 tokens now (100/s * 0.1s)
        for _ in 0..10 {
            assert!(bucket.try_acquire());
        }
    }

    #[test]
    fn test_rate_limit_type_detection() {
        assert_eq!(get_rate_limit_type("/api/v1/placeorder"), RateLimitType::Order);
        assert_eq!(get_rate_limit_type("/api/v1/placesmartorder"), RateLimitType::SmartOrder);
        assert_eq!(get_rate_limit_type("/api/v1/quotes"), RateLimitType::General);
        assert_eq!(get_rate_limit_type("/api/v1/basketorder"), RateLimitType::SmartOrder);
        assert_eq!(get_rate_limit_type("/api/v1/cancelorder"), RateLimitType::Order);
    }

    #[test]
    fn test_smart_order_delay() {
        let state = RateLimiterState::new(100, 10, 2, 0.5);

        // First order should pass
        assert!(state.check_smart_order_delay().is_ok());

        // Immediate second order should fail
        let result = state.check_smart_order_delay();
        assert!(result.is_err());

        // Should need to wait ~0.5 seconds
        if let Err(wait_time) = result {
            assert!(wait_time.as_secs_f64() <= 0.5);
        }
    }
}
