//! Market data and symbol endpoints.

use super::{authorize, check, load, send, Auth, Style};
use crate::server::envelope::json_response;
use crate::server::middleware::ClientIp;
use crate::services::market_data_service as market;
use crate::services::schemas;
use crate::services::symbol_service as symbols;
use crate::state::AppState;
use axum::{
    extract::{Path, RawQuery, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::NaiveDate;
use serde_json::{json, Map, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn date(v: &Value, k: &str) -> NaiveDate {
    NaiveDate::parse_from_str(&s(v, k), "%Y-%m-%d").unwrap_or_default()
}

macro_rules! simple {
    ($name:ident, $schema:path, $auth:expr, |$ctx:ident, $v:ident| $body:expr) => {
        pub async fn $name(State($ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
            match load(&$ctx, ip, req, $schema(), Style::Object, $auth).await {
                Ok($v) => send($body),
                Err(r) => r,
            }
        }
    };
}

simple!(quotes, schemas::quotes, Auth::Broker, |ctx, v| {
    market::quotes(&ctx, &s(&v, "symbol"), &s(&v, "exchange")).await
});

simple!(depth, schemas::quotes, Auth::Broker, |ctx, v| {
    market::depth(&ctx, &s(&v, "symbol"), &s(&v, "exchange")).await
});

simple!(multiquotes, schemas::multiquotes, Auth::Broker, |ctx, v| {
    let pairs: Vec<(String, String)> = v
        .get("symbols")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| (s(i, "symbol"), s(i, "exchange")))
                .collect()
        })
        .unwrap_or_default();
    market::multiquotes(&ctx, &pairs).await
});

simple!(history, schemas::history, Auth::Broker, |ctx, v| {
    market::history(
        &ctx,
        &s(&v, "symbol"),
        &s(&v, "exchange"),
        &s(&v, "interval"),
        date(&v, "start_date"),
        date(&v, "end_date"),
        &s(&v, "source"),
    )
    .await
});

simple!(intervals, schemas::apikey_only, Auth::Broker, |ctx, _v| {
    market::intervals(&ctx)
});

simple!(symbol, schemas::quotes, Auth::Broker, |ctx, v| {
    symbols::symbol(&ctx, &s(&v, "symbol"), &s(&v, "exchange"))
});

simple!(search, schemas::search, Auth::KeyOnly, |ctx, v| {
    let ex = v
        .get("exchange")
        .and_then(Value::as_str)
        .map(str::to_string);
    symbols::search(&ctx, &s(&v, "query"), ex.as_deref())
});

simple!(expiry, schemas::expiry, Auth::KeyOnly, |ctx, v| {
    symbols::expiry(
        &ctx,
        &s(&v, "symbol"),
        &s(&v, "exchange"),
        &s(&v, "instrumenttype"),
    )
});

/// POST /api/v1/margin (string-form validation errors, no analyzer branch).
pub async fn margin(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    match load(&ctx, ip, req, schemas::margin(), Style::Py, Auth::Broker).await {
        Ok(v) => {
            let positions = v
                .get("positions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            send(market::margin(&ctx, &positions).await)
        }
        Err(r) => r,
    }
}

/// Query string as the web's `request.args` (`None` for absent keys).
fn query_map(raw: Option<String>) -> Map<String, Value> {
    let pairs: Vec<(String, String)> = raw
        .as_deref()
        .and_then(|q| serde_urlencoded::from_str(q).ok())
        .unwrap_or_default();
    let mut m = Map::new();
    for (k, v) in pairs {
        m.entry(k).or_insert(Value::String(v));
    }
    m
}

fn text(status: StatusCode, content_type: &'static str, body: String) -> Response {
    let mut r = (status, body).into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}

/// GET /api/v1/instruments?apikey=&exchange=&format=json|csv
pub async fn instruments(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    RawQuery(q): RawQuery,
) -> Response {
    let args = query_map(q);
    let get = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("json")
        .to_ascii_lowercase();
    let mut body = Map::new();
    body.insert("apikey".into(), get("apikey"));
    body.insert("exchange".into(), get("exchange"));
    body.insert("format".into(), json!(format));
    let csv = format == "csv";
    let loaded = match schemas::instruments().load(&body) {
        Ok(l) => l,
        Err(e) if csv => {
            return text(
                StatusCode::BAD_REQUEST,
                "text/plain; charset=utf-8",
                e.to_python(),
            )
        }
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": e.to_json()}),
            )
        }
    };
    let key = loaded
        .get("apikey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !authorize(&ctx, key, ip, Auth::KeyOnly) {
        return send(crate::services::Reply::error(
            403,
            crate::services::core::INVALID_API_KEY,
        ));
    }
    let exchange = loaded
        .get("exchange")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match symbols::instruments(&ctx, exchange, csv) {
        symbols::Instruments::Json(r) => send(r),
        symbols::Instruments::Csv { filename, body } => {
            let mut r = text(StatusCode::OK, "text/csv", body);
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename={}", filename)) {
                r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            r
        }
    }
}

/// GET /api/v1/ticker/{EXCH:SYMBOL}?apikey=&interval=&from=&to=&format=
///
/// Validated with the history schema, as on the web (`from` and `to` map to
/// `start_date` and `end_date`). Text errors are answered as text with
/// their own status (the web turns every text error into a 500).
pub async fn ticker(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    Path(path): Path<String>,
    RawQuery(q): RawQuery,
) -> Response {
    let args = query_map(q);
    let get = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    let txt = args
        .get("format")
        .and_then(Value::as_str)
        .map(|f| f.eq_ignore_ascii_case("txt"))
        .unwrap_or(false);
    let (exchange, symbol) = market::split_ticker(&path);
    let mut body = Map::new();
    body.insert("apikey".into(), get("apikey"));
    body.insert("symbol".into(), json!(symbol));
    body.insert("exchange".into(), json!(exchange));
    body.insert(
        "interval".into(),
        args.get("interval").cloned().unwrap_or(json!("D")),
    );
    body.insert("start_date".into(), get("from"));
    body.insert("end_date".into(), get("to"));
    let plain = |status: StatusCode, msg: String| text(status, "text/plain", msg);
    let v = match check(
        &ctx,
        ip,
        &body,
        schemas::history(),
        Style::Object,
        Auth::Broker,
    ) {
        Ok(v) => v,
        Err(r) if txt => {
            let status = r.status();
            let msg = if status == StatusCode::FORBIDDEN {
                "Invalid openalgo apikey\n".to_string()
            } else {
                "Invalid request: check apikey, interval, from and to (YYYY-MM-DD)\n".to_string()
            };
            return plain(status, msg);
        }
        Err(r) => return r,
    };
    let interval = s(&v, "interval");
    match market::ticker_candles(
        &ctx,
        &symbol,
        &exchange,
        &interval,
        date(&v, "start_date"),
        date(&v, "end_date"),
    )
    .await
    {
        Ok(c) if txt => plain(
            StatusCode::OK,
            market::ticker_text(&exchange, &symbol, &interval, &c),
        ),
        Ok(c) => send(crate::services::Reply::ok(json!({
            "status": "success",
            "data": c.iter().map(market::candle_json).collect::<Vec<_>>(),
        }))),
        Err(r) if txt => plain(
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            format!("{}\n", r.message()),
        ),
        Err(r) => send(r),
    }
}
