//! `/api/v1/market/holidays` and `/api/v1/market/timings` (web
//! `restx_api/market_holidays.py`, `market_timings.py`), served from the
//! market calendar tables. Shapes follow `tests/fixtures/web/rest/market`.
//!
//! The web validates the body but never checks the key on these two. The
//! desktop checks it like every other `/api/v1` route (a valid key, no
//! broker session needed), so a client that works against the web with
//! its real key works here unchanged.

use crate::server::envelope::{error, json_response, read_json_object, FieldErrors};
use crate::server::middleware::ClientIp;
use crate::services::market_calendar_service as svc;
use crate::state::AppState;
use crate::webhook::handlers::INVALID_API_KEY;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use chrono::NaiveDate;
use serde_json::{Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn check_apikey(f: &mut FieldErrors, body: &Map<String, Value>) -> Option<String> {
    match body.get("apikey") {
        None => {
            f.add("apikey", "Missing data for required field.");
            None
        }
        Some(Value::Null) => {
            f.add("apikey", "Field may not be null.");
            None
        }
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 || n > 256 {
                f.add("apikey", "Length must be between 1 and 256.");
                None
            } else {
                Some(s.clone())
            }
        }
        Some(_) => {
            f.add("apikey", "Not a valid string.");
            None
        }
    }
}

fn unknown(f: &mut FieldErrors, body: &Map<String, Value>, allowed: &[&str]) {
    for k in body.keys() {
        if !allowed.contains(&k.as_str()) {
            f.add(k, "Unknown field.");
        }
    }
}

/// A valid API key; no broker session is needed for calendar data. Same
/// failure throttle as every other `/api/v1` route.
fn key_ok(ctx: &AppState, key: &str, ip: IpAddr) -> bool {
    crate::server::api_v1::authorize(ctx, key, ip, crate::server::api_v1::Auth::KeyOnly)
}

fn unexpected(e: impl std::fmt::Display) -> Response {
    tracing::error!("Market calendar request failed: {}", e);
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "An unexpected error occurred",
    )
}

/// POST /api/v1/market/holidays (json: apikey, year?)
pub async fn holidays(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let mut f = FieldErrors::default();
    unknown(&mut f, &body, &["apikey", "year"]);
    let key = check_apikey(&mut f, &body);
    let year = match body.get("year") {
        None => None,
        Some(Value::Null) => {
            f.add("year", "Field may not be null.");
            None
        }
        Some(Value::Number(n)) => match n
            .as_i64()
            .or_else(|| n.as_f64().filter(|x| x.fract() == 0.0).map(|x| x as i64))
        {
            Some(y) => Some(y),
            None => {
                f.add("year", "Not a valid integer.");
                None
            }
        },
        Some(Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(y) => Some(y),
            Err(_) => {
                f.add("year", "Not a valid integer.");
                None
            }
        },
        Some(_) => {
            f.add("year", "Not a valid integer.");
            None
        }
    };
    if let Some(y) = year {
        if !(2020..=2050).contains(&y) {
            f.add(
                "year",
                "Must be greater than or equal to 2020 and less than or equal to 2050.",
            );
        }
    }
    if !f.is_empty() {
        return f.into_response();
    }
    let Some(key) = key else {
        return f.into_response();
    };
    if !key_ok(&ctx, &key, ip) {
        return error(StatusCode::FORBIDDEN, INVALID_API_KEY);
    }
    match svc::holidays_api(&ctx, year.map(|y| y as i32)) {
        Ok(v) => json_response(StatusCode::OK, v),
        Err(e) => unexpected(e),
    }
}

/// POST /api/v1/market/timings (json: apikey, date)
pub async fn timings(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    let body = match read_json_object(req, &()).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let mut f = FieldErrors::default();
    unknown(&mut f, &body, &["apikey", "date"]);
    let key = check_apikey(&mut f, &body);
    let date = match body.get("date") {
        None => {
            f.add("date", "Missing data for required field.");
            None
        }
        Some(Value::Null) => {
            f.add("date", "Field may not be null.");
            None
        }
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            f.add("date", "Not a valid string.");
            None
        }
    };
    if !f.is_empty() {
        return f.into_response();
    }
    let (Some(key), Some(date)) = (key, date) else {
        return f.into_response();
    };
    if !key_ok(&ctx, &key, ip) {
        return error(StatusCode::FORBIDDEN, INVALID_API_KEY);
    }
    let Ok(d) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid date format. Use YYYY-MM-DD",
        );
    };
    if !svc::supported_date(d) {
        return error(
            StatusCode::BAD_REQUEST,
            "Date must be between 2020-01-01 and 2050-12-31",
        );
    }
    match svc::timings_for(&ctx, d) {
        Ok(w) => json_response(
            StatusCode::OK,
            serde_json::json!({"status": "success", "data": svc::timings_api_json(&w)}),
        ),
        Err(e) => unexpected(e),
    }
}
