//! Web `blueprints/historify.py`: the Historify page's JSON routes under
//! `/historify/api/`. Every route needs the signed-in user (declared in
//! [`table`]); writes carry the session's CSRF token like every other page
//! route. The upload and schedule routes are in `upload` and `schedules`.

use super::{Access, RouteSpec};
use crate::historify::service::{self as svc, ChainQuery};
use crate::historify::{export, jobs::CreateJob, time::parse_date};
use crate::server::api_v1::send;
use crate::server::envelope::{error, EarlyReply};
use crate::server::middleware::User;
use crate::server::routes::webui::JsonBody;
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{delete, get, post, put, MethodRouter},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

mod schedules;
mod upload;

pub use schedules::*;
pub use upload::upload;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

/// Web `MAX_UPLOAD_SIZE`.
pub const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
/// The upload route's body limit (file plus the multipart framing).
const UPLOAD_BODY_LIMIT: usize = MAX_UPLOAD_BYTES + 1024 * 1024;

macro_rules! h {
    ($m:ident, $path:expr, $f:path) => {
        RouteSpec {
            path: $path,
            method: Method::$m,
            access: Access::User,
            make: || h!(@route $m, $f),
        }
    };
    (@route GET, $f:path) => { get($f) };
    (@route POST, $f:path) => { post($f) };
    (@route PUT, $f:path) => { put($f) };
    (@route DELETE, $f:path) => { delete($f) };
}

fn upload_route() -> MethodRouter<Arc<AppState>> {
    post(upload).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT))
}

/// The Historify routes, each with its access rule.
pub fn table() -> Vec<RouteSpec> {
    let mut t = data_routes();
    t.extend(job_routes());
    t.extend(schedule_routes());
    t
}

/// The watchlist, downloads, data, catalog, export, upload and utility
/// routes.
fn data_routes() -> Vec<RouteSpec> {
    vec![
        h!(GET, "/historify/api/watchlist", watchlist),
        h!(POST, "/historify/api/watchlist", watchlist_add),
        h!(DELETE, "/historify/api/watchlist", watchlist_remove),
        h!(
            POST,
            "/historify/api/watchlist/bulk/delete",
            watchlist_bulk_remove
        ),
        h!(POST, "/historify/api/watchlist/bulk", watchlist_bulk_add),
        h!(POST, "/historify/api/download", download),
        h!(
            POST,
            "/historify/api/download/watchlist",
            download_watchlist
        ),
        h!(GET, "/historify/api/data", data),
        h!(GET, "/historify/api/catalog", catalog),
        h!(GET, "/historify/api/catalog/grouped", catalog_grouped),
        h!(GET, "/historify/api/catalog/metadata", catalog_metadata),
        h!(GET, "/historify/api/symbol-info", symbol_info),
        h!(POST, "/historify/api/export/preview", export_preview),
        h!(POST, "/historify/api/export/bulk", export_bulk),
        h!(GET, "/historify/api/export/bulk/download", export_download),
        h!(GET, "/historify/api/intervals", intervals),
        h!(
            GET,
            "/historify/api/historify-intervals",
            historify_intervals
        ),
        h!(GET, "/historify/api/exchanges", exchanges),
        h!(GET, "/historify/api/stats", stats),
        h!(DELETE, "/historify/api/delete", delete_data),
        h!(POST, "/historify/api/delete/bulk", delete_bulk),
        RouteSpec {
            path: "/historify/api/upload",
            method: Method::POST,
            access: Access::User,
            make: upload_route,
        },
        h!(GET, "/historify/api/sample/{format}", sample),
    ]
}

/// The F&O lookups, download jobs and metadata enrichment.
fn job_routes() -> Vec<RouteSpec> {
    vec![
        h!(GET, "/historify/api/fno/underlyings", fno_underlyings),
        h!(GET, "/historify/api/fno/expiries", fno_expiries),
        h!(GET, "/historify/api/fno/chain", fno_chain),
        h!(GET, "/historify/api/fno/futures", fno_futures),
        h!(GET, "/historify/api/fno/options", fno_options),
        h!(GET, "/historify/api/jobs", jobs_list),
        h!(POST, "/historify/api/jobs", jobs_create),
        h!(GET, "/historify/api/jobs/{id}", job_status),
        h!(DELETE, "/historify/api/jobs/{id}", job_delete),
        h!(POST, "/historify/api/jobs/{id}/cancel", job_cancel),
        h!(POST, "/historify/api/jobs/{id}/pause", job_pause),
        h!(POST, "/historify/api/jobs/{id}/resume", job_resume),
        h!(POST, "/historify/api/jobs/{id}/retry", job_retry),
        h!(POST, "/historify/api/metadata/enrich", metadata_enrich),
    ]
}

/// The schedule routes.
fn schedule_routes() -> Vec<RouteSpec> {
    vec![
        h!(GET, "/historify/api/schedules", schedules_list),
        h!(POST, "/historify/api/schedules", schedule_create),
        h!(GET, "/historify/api/schedules/{id}", schedule_get),
        h!(PUT, "/historify/api/schedules/{id}", schedule_update),
        h!(DELETE, "/historify/api/schedules/{id}", schedule_delete),
        h!(
            POST,
            "/historify/api/schedules/{id}/enable",
            schedule_enable
        ),
        h!(
            POST,
            "/historify/api/schedules/{id}/disable",
            schedule_disable
        ),
        h!(POST, "/historify/api/schedules/{id}/pause", schedule_pause),
        h!(
            POST,
            "/historify/api/schedules/{id}/resume",
            schedule_resume
        ),
        h!(
            POST,
            "/historify/api/schedules/{id}/trigger",
            schedule_trigger
        ),
        h!(
            GET,
            "/historify/api/schedules/{id}/executions",
            schedule_executions
        ),
    ]
}

fn arg<'a>(q: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn upper(body: &JsonBody, k: &str) -> String {
    body.str(k).unwrap_or_default().to_uppercase()
}

fn ok_or_400(r: Result<String, String>) -> Response {
    match r {
        Ok(m) => send(Reply::ok(json!({"status": "success", "message": m}))),
        Err(m) => error(StatusCode::BAD_REQUEST, m),
    }
}

/// The connected broker is needed to download.
fn broker_ready(ctx: &AppState) -> Result<(), EarlyReply> {
    ctx.historify
        .jobs
        .source()
        .ready()
        .map_err(|m| error(StatusCode::BAD_REQUEST, m).into())
}

// --------------------------------------------------------------- watchlist

/// GET /historify/api/watchlist
pub async fn watchlist(State(ctx): Ctx) -> Response {
    send(ctx.historify.watchlist().await)
}

/// POST /historify/api/watchlist (json: symbol, exchange, display_name?)
pub async fn watchlist_add(State(ctx): Ctx, body: JsonBody) -> Response {
    let display = body
        .0
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    send(
        ctx.historify
            .add_watchlist(
                &ctx.symbols,
                &upper(&body, "symbol"),
                &upper(&body, "exchange"),
                display,
            )
            .await,
    )
}

/// DELETE /historify/api/watchlist (json: symbol, exchange)
pub async fn watchlist_remove(State(ctx): Ctx, body: JsonBody) -> Response {
    send(
        ctx.historify
            .remove_watchlist(&upper(&body, "symbol"), &upper(&body, "exchange"))
            .await,
    )
}

/// POST /historify/api/watchlist/bulk/delete (json: symbols)
pub async fn watchlist_bulk_remove(State(ctx): Ctx, body: JsonBody) -> Response {
    let items = svc::parse_symbol_reqs(body.0.get("symbols"));
    if items.is_empty() {
        return error(StatusCode::BAD_REQUEST, "No symbols provided");
    }
    send(ctx.historify.bulk_remove_watchlist(items).await)
}

/// POST /historify/api/watchlist/bulk (json: symbols)
pub async fn watchlist_bulk_add(State(ctx): Ctx, body: JsonBody) -> Response {
    let items = svc::parse_symbol_reqs(body.0.get("symbols"));
    send(ctx.historify.bulk_add_watchlist(&ctx.symbols, items).await)
}

// --------------------------------------------------------------- download

fn date_pair(body: &JsonBody) -> Result<(String, String), EarlyReply> {
    let (s, e) = (body.str("start_date"), body.str("end_date"));
    match (
        s.as_deref().and_then(parse_date),
        e.as_deref().and_then(parse_date),
    ) {
        (Some(a), Some(b)) => Ok((
            a.format("%Y-%m-%d").to_string(),
            b.format("%Y-%m-%d").to_string(),
        )),
        _ => Err(error(
            StatusCode::BAD_REQUEST,
            "Choose a start and end date in YYYY-MM-DD format.",
        )
        .into()),
    }
}

/// POST /historify/api/download: one symbol, now (web `download_data`).
pub async fn download(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Err(r) = broker_ready(&ctx) {
        return *r;
    }
    let (symbol, exchange) = (upper(&body, "symbol"), upper(&body, "exchange"));
    let interval = body.non_empty("interval").unwrap_or_else(|| "D".into());
    let (start, end) = match date_pair(&body) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let (Some(s), Some(e)) = (parse_date(&start), parse_date(&end)) else {
        return error(StatusCode::BAD_REQUEST, "Choose valid dates.");
    };
    match ctx
        .historify
        .jobs
        .download_now(&symbol, &exchange, &interval, s, e)
        .await
    {
        Ok(0) => send(Reply::ok(json!({
            "status": "success", "message": "No data available for the specified period",
            "records": 0,
        }))),
        Ok(n) => send(Reply::ok(json!({
            "status": "success", "symbol": symbol, "exchange": exchange, "interval": interval,
            "start_date": start, "end_date": end, "records": n,
        }))),
        Err(m) => error(StatusCode::BAD_REQUEST, m),
    }
}

/// POST /historify/api/download/watchlist: a job over the whole watchlist.
pub async fn download_watchlist(State(ctx): Ctx, body: JsonBody) -> Response {
    if let Err(r) = broker_ready(&ctx) {
        return *r;
    }
    let (start, end) = match date_pair(&body) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    let symbols = match ctx
        .historify
        .db
        .run(crate::historify::db::watchlist_symbols)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Reading the Historify watchlist failed: {}", e);
            Vec::new()
        }
    };
    send(
        ctx.historify
            .jobs
            .create_and_start(CreateJob {
                job_type: "watchlist".into(),
                symbols,
                interval: body.non_empty("interval").unwrap_or_else(|| "D".into()),
                start_date: Some(start),
                end_date: Some(end),
                config: json!({}),
                incremental: false,
            })
            .await,
    )
}

// ------------------------------------------------------------------- data

/// GET /historify/api/data
pub async fn data(State(ctx): Ctx, Query(q): Q) -> Response {
    let symbol = arg(&q, "symbol").unwrap_or_default().to_uppercase();
    let exchange = arg(&q, "exchange").unwrap_or_default().to_uppercase();
    let interval = q.get("interval").cloned().unwrap_or_else(|| "D".into());
    send(
        ctx.historify
            .chart_data(
                &symbol,
                &exchange,
                &interval,
                arg(&q, "start_date"),
                arg(&q, "end_date"),
            )
            .await,
    )
}

/// GET /historify/api/catalog
pub async fn catalog(State(ctx): Ctx) -> Response {
    send(ctx.historify.catalog().await)
}

/// GET /historify/api/catalog/grouped
pub async fn catalog_grouped(State(ctx): Ctx, Query(q): Q) -> Response {
    let by = arg(&q, "group_by").unwrap_or("underlying").to_string();
    send(ctx.historify.catalog_grouped(&by).await)
}

/// GET /historify/api/catalog/metadata
pub async fn catalog_metadata(State(ctx): Ctx) -> Response {
    send(ctx.historify.catalog_metadata().await)
}

/// GET /historify/api/symbol-info
pub async fn symbol_info(State(ctx): Ctx, Query(q): Q) -> Response {
    let symbol = arg(&q, "symbol").unwrap_or_default().to_uppercase();
    let exchange = arg(&q, "exchange").unwrap_or_default().to_uppercase();
    send(
        ctx.historify
            .symbol_info(&symbol, &exchange, arg(&q, "interval"))
            .await,
    )
}

// ----------------------------------------------------------------- export

/// POST /historify/api/export/preview
pub async fn export_preview(State(ctx): Ctx, body: JsonBody) -> Response {
    send(ctx.historify.export_preview(&body.0).await)
}

/// POST /historify/api/export/bulk
pub async fn export_bulk(State(ctx): Ctx, User(u): User, body: JsonBody) -> Response {
    send(ctx.historify.export_bulk(&u.session_id, &body.0).await)
}

/// Deletes the file once the response body is done with it.
struct SentFile(PathBuf);

impl Drop for SentFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// GET /historify/api/export/bulk/download: stream once, then delete.
pub async fn export_download(State(ctx): Ctx, User(u): User) -> Response {
    let Some(p) = ctx.historify.exports.take(&u.session_id) else {
        return error(StatusCode::NOT_FOUND, "Export file not found");
    };
    let file = match tokio::fs::File::open(&p.path).await {
        Ok(f) => f,
        Err(_) => {
            let _ = std::fs::remove_file(&p.path);
            return error(StatusCode::NOT_FOUND, "Export file not found");
        }
    };
    let guard = SentFile(p.path.clone());
    let stream = futures_util::stream::unfold(
        Some((file, guard, vec![0u8; 64 * 1024])),
        |state| async move {
            use tokio::io::AsyncReadExt;
            let (mut f, g, mut buf) = state?;
            match f.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                    Some((Ok::<_, std::io::Error>(chunk), Some((f, g, buf))))
                }
                Err(e) => {
                    tracing::warn!("Sending an export failed: {}", e);
                    None
                }
            }
        },
    );
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(p.mime));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename={}", p.filename)) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    r
}

/// GET /historify/api/sample/{format}
pub async fn sample(State(ctx): Ctx, Path(format): Path<String>) -> Response {
    let attach = |body: Body, mime: &'static str, name: &str| {
        let mut r = Response::new(body);
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename={}", name)) {
            r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
        r
    };
    match format.as_str() {
        "csv" => attach(
            Body::from(export::sample_csv()),
            "text/csv",
            "sample_ohlcv.csv",
        ),
        "parquet" => {
            let dir = match ctx.historify.work_dir() {
                Ok(d) => d.to_path_buf(),
                Err(e) => {
                    tracing::error!("Could not create the Historify work folder: {}", e);
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not prepare the sample file. Check free disk space.",
                    );
                }
            };
            match tokio::task::spawn_blocking(move || export::sample_parquet(&dir)).await {
                Ok(Ok(bytes)) => attach(
                    Body::from(bytes),
                    "application/octet-stream",
                    "sample_ohlcv.parquet",
                ),
                other => {
                    tracing::error!(
                        "Writing the sample Parquet failed: {:?}",
                        other.map(|r| r.err())
                    );
                    error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not prepare the sample file. Try again.",
                    )
                }
            }
        }
        _ => error(
            StatusCode::BAD_REQUEST,
            "Invalid format. Use csv or parquet",
        ),
    }
}

// ------------------------------------------------------------------ utils

/// GET /historify/api/intervals: the connected broker's timeframes.
pub async fn intervals(State(ctx): Ctx) -> Response {
    let r = crate::services::market_data_service::intervals(&ctx);
    if r.is_success() {
        return send(r);
    }
    error(
        StatusCode::BAD_REQUEST,
        "Connect your broker to see the timeframes it offers.",
    )
}

/// GET /historify/api/historify-intervals
pub async fn historify_intervals(State(ctx): Ctx) -> Response {
    send(ctx.historify.historify_intervals())
}

/// GET /historify/api/exchanges
pub async fn exchanges(State(ctx): Ctx) -> Response {
    send(ctx.historify.exchanges())
}

/// GET /historify/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    send(ctx.historify.stats().await)
}

/// DELETE /historify/api/delete (json: symbol, exchange, interval?)
pub async fn delete_data(State(ctx): Ctx, body: JsonBody) -> Response {
    send(
        ctx.historify
            .delete_data(
                &upper(&body, "symbol"),
                &upper(&body, "exchange"),
                body.non_empty("interval"),
            )
            .await,
    )
}

/// POST /historify/api/delete/bulk (json: symbols)
pub async fn delete_bulk(State(ctx): Ctx, body: JsonBody) -> Response {
    let items = svc::parse_symbol_reqs(body.0.get("symbols"));
    if items.is_empty() {
        return error(StatusCode::BAD_REQUEST, "No symbols provided");
    }
    send(ctx.historify.bulk_delete(items).await)
}

// -------------------------------------------------------------------- F&O

/// GET /historify/api/fno/underlyings
pub async fn fno_underlyings(State(ctx): Ctx, Query(q): Q) -> Response {
    let snap = ctx.symbols.snapshot();
    send(svc::fno_underlyings(snap.rows(), arg(&q, "exchange")))
}

/// GET /historify/api/fno/expiries
pub async fn fno_expiries(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some(und) = arg(&q, "underlying") else {
        return error(StatusCode::BAD_REQUEST, "Underlying is required");
    };
    let snap = ctx.symbols.snapshot();
    send(svc::fno_expiries(
        snap.rows(),
        und,
        arg(&q, "exchange").unwrap_or("NFO"),
    ))
}

fn chain_query(q: &HashMap<String, String>, limit: usize) -> Option<ChainQuery> {
    let und = arg(q, "underlying")?.to_uppercase();
    let num = |k: &str| arg(q, k).and_then(|s| s.parse::<f64>().ok());
    Some(ChainQuery {
        underlying: und,
        exchange: arg(q, "exchange").unwrap_or("NFO").to_uppercase(),
        expiry: arg(q, "expiry").map(str::to_string),
        instrumenttype: arg(q, "instrumenttype").map(str::to_string),
        strike_min: num("strike_min"),
        strike_max: num("strike_max"),
        limit: arg(q, "limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(limit),
    })
}

/// GET /historify/api/fno/chain
pub async fn fno_chain(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some(cq) = chain_query(&q, 1000) else {
        return error(StatusCode::BAD_REQUEST, "Underlying is required");
    };
    let snap = ctx.symbols.snapshot();
    send(svc::fno_chain(snap.rows(), &cq, &ctx.freeze))
}

/// GET /historify/api/fno/futures
pub async fn fno_futures(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some(mut cq) = chain_query(&q, 500) else {
        return error(StatusCode::BAD_REQUEST, "Underlying is required");
    };
    cq.instrumenttype = Some("FUT".into());
    cq.expiry = None;
    cq.strike_min = None;
    cq.strike_max = None;
    cq.limit = 500;
    let snap = ctx.symbols.snapshot();
    send(svc::fno_chain(snap.rows(), &cq, &ctx.freeze))
}

/// GET /historify/api/fno/options
pub async fn fno_options(State(ctx): Ctx, Query(q): Q) -> Response {
    let Some(cq) = chain_query(&q, 2000) else {
        return error(StatusCode::BAD_REQUEST, "Underlying is required");
    };
    let snap = ctx.symbols.snapshot();
    send(svc::fno_options(snap.rows(), &cq, &ctx.freeze))
}

// ------------------------------------------------------------------- jobs

/// GET /historify/api/jobs (status?, limit?)
pub async fn jobs_list(State(ctx): Ctx, Query(q): Q) -> Response {
    let limit = arg(&q, "limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(50);
    send(
        ctx.historify
            .jobs
            .list(arg(&q, "status").map(str::to_string), limit)
            .await,
    )
}

/// POST /historify/api/jobs
pub async fn jobs_create(State(ctx): Ctx, body: JsonBody) -> Response {
    let symbols = match body.0.get("symbols") {
        Some(Value::Array(a)) if !a.is_empty() => {
            let parsed = svc::parse_symbol_list(body.0.get("symbols"));
            if parsed.len() != a.len() || parsed.iter().any(|(s, e)| s.is_empty() || e.is_empty()) {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Each symbol needs a symbol and an exchange.",
                );
            }
            parsed
        }
        _ => return error(StatusCode::BAD_REQUEST, "No symbols provided"),
    };
    if let Err(r) = broker_ready(&ctx) {
        return *r;
    }
    let (start, end) = match date_pair(&body) {
        Ok(d) => d,
        Err(r) => return *r,
    };
    send(
        ctx.historify
            .jobs
            .create_and_start(CreateJob {
                job_type: body
                    .non_empty("job_type")
                    .unwrap_or_else(|| "custom".into()),
                symbols,
                interval: body.non_empty("interval").unwrap_or_else(|| "D".into()),
                start_date: Some(start),
                end_date: Some(end),
                config: body.0.get("config").cloned().unwrap_or(json!({})),
                incremental: body.bool("incremental"),
            })
            .await,
    )
}

/// GET /historify/api/jobs/{id}
pub async fn job_status(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    send(ctx.historify.jobs.status(&id).await)
}

/// DELETE /historify/api/jobs/{id}
pub async fn job_delete(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    send(ctx.historify.jobs.delete(&id).await)
}

/// POST /historify/api/jobs/{id}/cancel
pub async fn job_cancel(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    send(ctx.historify.jobs.cancel(&id).await)
}

/// POST /historify/api/jobs/{id}/pause
pub async fn job_pause(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    send(ctx.historify.jobs.pause(&id).await)
}

/// POST /historify/api/jobs/{id}/resume
pub async fn job_resume(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    send(ctx.historify.jobs.resume(&id).await)
}

/// POST /historify/api/jobs/{id}/retry
pub async fn job_retry(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    if let Err(r) = broker_ready(&ctx) {
        return *r;
    }
    send(ctx.historify.jobs.retry(&id).await)
}

/// POST /historify/api/metadata/enrich (json: symbols)
pub async fn metadata_enrich(State(ctx): Ctx, body: JsonBody) -> Response {
    let items = svc::parse_symbol_reqs(body.0.get("symbols"));
    send(ctx.historify.enrich_metadata(&ctx.symbols, items).await)
}
