//! Web `blueprints/pnltracker.py` (`/pnltracker/api/pnl`) and
//! `blueprints/chart_test.py` (`/chart/test/api/history`): chart data for
//! the signed-in trader, built from the books and broker history.

use crate::server::api_v1::send;
use crate::server::envelope::{error, json_response};
use crate::server::routes::webui::ok;
use crate::services::apikey_service::ApiKeyService;
use crate::services::core::Reply;
use crate::services::{
    account_service, chart_test_service as chart, market_data_service, pnl_capital,
    pnl_tracker_service,
};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::NaiveDate;
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

type Ctx = State<Arc<AppState>>;

fn has_api_key(ctx: &AppState) -> bool {
    matches!(ApiKeyService::current(ctx), Ok(Some(_)))
}

/// History calls from the tracker are spaced (web: 2 per second) so a
/// portfolio of many symbols stays under the broker's history limit.
async fn pace() {
    static LAST: OnceLock<tokio::sync::Mutex<Option<Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if let Some(t) = *last {
        let gap = Duration::from_millis(500);
        let since = t.elapsed();
        if since < gap {
            tokio::time::sleep(gap - since).await;
        }
    }
    *last = Some(Instant::now());
}

fn rows(r: &Reply) -> Vec<Value> {
    r.body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// POST /pnltracker/api/pnl
pub async fn pnl(State(ctx): Ctx) -> Response {
    if !ctx.is_broker_connected() {
        return error(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    if !has_api_key(&ctx) {
        return error(
            StatusCode::UNAUTHORIZED,
            "API key not configured. Please generate an API key in /apikey",
        );
    }
    let tradebook = account_service::tradebook(&ctx).await;
    if !tradebook.is_success() {
        return send(tradebook);
    }
    let trades = rows(&tradebook);
    let positions_reply = account_service::positionbook(&ctx).await;
    let positions = if positions_reply.is_success() {
        rows(&positions_reply)
    } else {
        tracing::warn!(
            "P&L tracker: positions unavailable ({})",
            positions_reply.message()
        );
        Vec::new()
    };
    let c = ctx.clone();
    let history = move |symbol: String, exchange: String, date: NaiveDate| {
        let c = c.clone();
        async move {
            pace().await;
            let r =
                market_data_service::history(&c, &symbol, &exchange, "1m", date, date, "api").await;
            if !r.is_success() {
                tracing::warn!("P&L tracker: no history for {}:{}", exchange, symbol);
                return None;
            }
            Some(
                rows(&r)
                    .iter()
                    .filter_map(|b| {
                        Some((b.get("timestamp")?.as_i64()?, b.get("close")?.as_f64()?))
                    })
                    .collect::<Vec<_>>(),
            )
        }
    };
    let mut body = pnl_tracker_service::compute(&trades, &positions, ctx.now(), history).await;
    pnl_capital::attach(&ctx, &mut body).await;
    ok(body)
}

/// GET /chart/test/api/history?symbol&exchange&interval&date
pub async fn chart_history(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    if !has_api_key(&ctx) {
        return error(
            StatusCode::NOT_FOUND,
            "No API key found. Generate one at /apikey first.",
        );
    }
    let up = |k: &str, n: usize| -> String {
        q.get(k)
            .map(|s| s.trim().to_uppercase().chars().take(n).collect())
            .unwrap_or_default()
    };
    let (symbol, exchange) = (up("symbol", 50), up("exchange", 20));
    if symbol.is_empty() || exchange.is_empty() {
        return error(StatusCode::BAD_REQUEST, "symbol and exchange are required");
    }
    let (interval, keep) =
        chart::interval_days(q.get("interval").map(|s| s.trim()).unwrap_or("1m"));
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let (start, end, keep) =
        chart::window(today, q.get("date").map(|s| s.trim()).unwrap_or(""), keep);
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d");
    let (Ok(start), Ok(end)) = (parse(&start), parse(&end)) else {
        return error(StatusCode::BAD_REQUEST, "Choose the date as YYYY-MM-DD.");
    };
    let r =
        market_data_service::history(&ctx, &symbol, &exchange, interval, start, end, "api").await;
    if !r.is_success() {
        let msg = r.message();
        return json_response(
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            json!({"status": "error", "message": if msg.is_empty() { "History fetch failed".to_string() } else { msg }}),
        );
    }
    ok(chart::candles(
        &symbol,
        &exchange,
        interval,
        &rows(&r),
        keep,
    ))
}
