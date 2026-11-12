//! Noren family members and Firstock end to end against a local fake
//! broker (ephemeral port): sign-in, request dialects (Bearer jData vs
//! jData+jKey), order bodies with price protection, books, funds, margin,
//! quotes (with the shoonya identity guard), history chunking, and master
//! contracts downloaded as zips / CSVs.

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::families::noren::master_contract::parse_file;
use openalgo_desktop_lib::brokers::families::noren::{
    zip, NorenBroker, NorenConfig, NorenEndpoints,
};
use openalgo_desktop_lib::brokers::firstock::FirstockBroker;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{firstock, flattrade, shoonya, tradesmart, zebu};
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

macro_rules! fixture {
    ($b:literal, $name:literal) => {
        include_str!(concat!("../fixtures/brokers/", $b, "/", $name))
    };
}

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    content_type: String,
    authorization: String,
    raw: String,
    jdata: Value,
    jkey: Option<String>,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    quote_calls: AtomicUsize,
    /// First N GetQuotes answer with another instrument (identity guard).
    wrong_quotes: AtomicUsize,
    /// Path leaves answered 404 (a master file or endpoint that is down).
    down: Mutex<Vec<&'static str>>,
    /// Path leaves answered 200 with this body instead (an empty or
    /// header-only master file).
    bodies: Mutex<Vec<(&'static str, &'static str)>>,
    /// TPSeries answered from these bars by the requested range.
    series: Mutex<Option<Series>>,
}

/// Bars as `(epoch seconds, open, high, low, close, volume)`.
type Bars = Vec<(i64, f64, f64, f64, f64, i64)>;

/// Bars from `(IST "YYYY-MM-DD HH:MM", open, high, low, close, volume)`.
fn bars_at(rows: &[(&str, f64, f64, f64, f64, i64)]) -> Bars {
    rows.iter()
        .map(|&(t, o, h, l, c, v)| (ist_ts(t), o, h, l, c, v))
        .collect()
}

/// TPSeries as Shoonya answers it (web test_shoonya_history_aggregation.py
/// `_FakeTPSeries`): data only inside the bars it holds, 1m bars for
/// `intrv` 1 and the native interval otherwise, "no data" for an empty
/// window, and a failure for the 1m chunk starting at `fail_st`.
#[derive(Clone, Default)]
struct Series {
    native: Bars,
    minute: Bars,
    fail_st: Option<i64>,
}

fn ist_ts(text: &str) -> i64 {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
    chrono_tz::Asia::Kolkata
        .from_local_datetime(&naive)
        .unwrap()
        .timestamp()
}

fn series_answer(s: &Series, body: &Value) -> Response {
    let num = |k: &str| body[k].as_str().unwrap().parse::<i64>().unwrap();
    let (st, et) = (num("st"), num("et"));
    let minute = body["intrv"] == "1";
    if minute && s.fail_st == Some(st) {
        return ok(json!({"stat":"Not_Ok","emsg":"Server busy, try again"}));
    }
    let rows = if minute { &s.minute } else { &s.native };
    let hit: Vec<Value> = rows
        .iter()
        .filter(|r| (st..=et).contains(&r.0))
        .map(|r| {
            json!({"stat":"Ok","ssboe": r.0.to_string(), "into": r.1.to_string(),
                   "inth": r.2.to_string(), "intl": r.3.to_string(), "intc": r.4.to_string(),
                   "intv": r.5.to_string(), "oi": "0"})
        })
        .collect();
    if hit.is_empty() {
        return ok(json!({"stat":"Not_Ok","emsg":"Error Occurred : 5 \"no data\""}));
    }
    ok(Value::Array(hit))
}

impl Fake {
    fn calls(&self, leaf: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path.ends_with(leaf))
            .cloned()
            .collect()
    }
}

fn ok(v: impl ToString) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn split_body(raw: &str) -> (Value, Option<String>) {
    if let Some(rest) = raw.strip_prefix("jData=") {
        let (j, key) = match rest.rsplit_once("&jKey=") {
            Some((j, k)) => (j, Some(k.to_string())),
            None => (rest, None),
        };
        (serde_json::from_str(j).unwrap_or(Value::Null), key)
    } else {
        (serde_json::from_str(raw).unwrap_or(Value::Null), None)
    }
}

fn zipped(text: &str, name: &str) -> Response {
    (StatusCode::OK, zip::build(name, text.as_bytes(), true)).into_response()
}

fn route(fake: &Fake, path: &str, body: &Value) -> Response {
    let leaf = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    if fake.down.lock().contains(&leaf) {
        return (StatusCode::NOT_FOUND, "down").into_response();
    }
    if let Some((_, text)) = fake.bodies.lock().iter().find(|(l, _)| *l == leaf) {
        return (StatusCode::OK, text.to_string()).into_response();
    }
    if leaf == "TPSeries" {
        if let Some(s) = fake.series.lock().clone() {
            return series_answer(&s, body);
        }
    }
    match leaf {
        "GenAcsTok" => {
            if body["code"] == "bad" {
                ok(json!({"stat":"Not_Ok","emsg":"Invalid code"}))
            } else {
                ok(json!({"stat":"Ok","access_token":"acc-123","actid":"TS9"}))
            }
        }
        "apitoken" => ok(json!({"stat":"Ok","token":"ft-tok","client":"FT1"})),
        "PlaceOrder" => ok(json!({"stat":"Ok","norenordno":"26100300000099"})),
        "ModifyOrder" => ok(json!({"stat":"Ok","result":"26100300000001"})),
        "CancelOrder" => {
            if body["norenordno"] == "26100300000002" {
                ok(json!({"stat":"Not_Ok","message":"Order already cancelled","emsg":"x"}))
            } else {
                ok(json!({"stat":"Ok","result":body["norenordno"]}))
            }
        }
        "OrderBook" => ok(fixture!("shoonya", "order_book.json")),
        "TradeBook" => ok(fixture!("shoonya", "trade_book.json")),
        "PositionBook" => ok(fixture!("shoonya", "positions.json")),
        "Holdings" => ok(fixture!("shoonya", "holdings.json")),
        "Limits" => ok(fixture!("shoonya", "limits.json")),
        "GetBasketMargin" => {
            ok(json!({"stat":"Ok","marginused":"150000.50","marginusedtrade":"90000.25"}))
        }
        "GetOrderMargin" => ok(json!({"stat":"Ok","ordermargin":"1000.5"})),
        "GetQuotes" => {
            fake.quote_calls.fetch_add(1, Ordering::SeqCst);
            if fake
                .wrong_quotes
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return ok(
                    json!({"stat":"Ok","exch":"NSE","token":"26000","tsym":"Nifty 50","lp":"25012.35"}),
                );
            }
            let mut q: Value = serde_json::from_str(fixture!("shoonya", "quote.json")).unwrap();
            q["token"] = body["token"].clone();
            q["exch"] = body["exch"].clone();
            ok(q)
        }
        // Index minute bars as Flattrade sends them: a 09:14 pre-open bar,
        // rows with a missing or NaN price, a negative closing-session
        // volume (web #2198).
        "TPSeries" if body["token"] == "26000" => ok(fixture!("flattrade", "tpseries.json")),
        "TPSeries" => ok(fixture!("shoonya", "tpseries.json")),
        // BSE EOD rows as Flattrade sends them for BSE indices: the close
        // lies outside the day's high/low (web #2196), and a row with no
        // low (web #2198).
        "EODChartData" if body["sym"].as_str().is_some_and(|s| s.starts_with("BSE:")) => ok(json!([
            "{\"time\":\"30-SEP-2026\",\"into\":\"81000.00\",\"inth\":\"81500.00\",\"intl\":\"80800.00\",\"intc\":\"81650.00\",\"ssboe\":\"1790726400\",\"intv\":\"0\"}",
            "{\"time\":\"01-OCT-2026\",\"into\":\"81650.00\",\"inth\":\"81900.00\",\"intl\":\"81400.00\",\"intc\":\"81300.00\",\"ssboe\":\"1790812800\",\"intv\":\"0\"}",
            "{\"time\":\"02-OCT-2026\",\"into\":\"81300.00\",\"inth\":\"81700.00\",\"intl\":null,\"intc\":\"81600.00\",\"ssboe\":\"1790899200\",\"intv\":\"0\"}"
        ])),
        "EODChartData" => ok(fixture!("shoonya", "eod.json")),
        "NSE_symbols.txt.zip" => zipped(fixture!("shoonya", "NSE_symbols.txt"), "NSE_symbols.txt"),
        "BSE_symbols.txt.zip" => zipped(fixture!("shoonya", "BSE_symbols.txt"), "BSE_symbols.txt"),
        "NFO_symbols.txt.zip" => zipped(fixture!("shoonya", "NFO_symbols.txt"), "NFO_symbols.txt"),
        "CDS_symbols.txt.zip" => zipped(fixture!("shoonya", "CDS_symbols.txt"), "CDS_symbols.txt"),
        "MCX_symbols.txt.zip" => zipped(fixture!("shoonya", "MCX_symbols.txt"), "MCX_symbols.txt"),
        "BFO_symbols.txt.zip" => zipped(fixture!("shoonya", "BFO_symbols.txt"), "BFO_symbols.txt"),
        "NSE_Equity.csv" => ok(fixture!("flattrade", "NSE_Equity.csv")),
        "BSE_Equity.csv" => ok(fixture!("flattrade", "BSE_Equity.csv")),
        "Nfo_Index_Derivatives.csv" => ok(fixture!("flattrade", "Nfo_Index_Derivatives.csv")),
        "Nfo_Equity_Derivatives.csv" => ok(fixture!("flattrade", "Nfo_Equity_Derivatives.csv")),
        "Currency_Derivatives.csv" => ok(fixture!("flattrade", "Currency_Derivatives.csv")),
        "Commodity.csv" => ok(fixture!("flattrade", "Commodity.csv")),
        "Bfo_Index_Derivatives.csv" => ok(fixture!("flattrade", "Bfo_Index_Derivatives.csv")),
        "Bfo_Equity_Derivatives.csv" => ok(fixture!("flattrade", "Bfo_Equity_Derivatives.csv")),
        // Firstock
        "login" => {
            let all: Value = serde_json::from_str(fixture!("firstock", "responses.json")).unwrap();
            if body["TOTP"] == "000000" {
                ok(&all["login_failed"])
            } else {
                ok(&all["login_ok"])
            }
        }
        _ => firstock_route(leaf, body),
    }
}

fn firstock_route(leaf: &str, body: &Value) -> Response {
    let all: Value = serde_json::from_str(fixture!("firstock", "responses.json")).unwrap();
    let pick = |k: &str| ok(&all[k]);
    match leaf {
        "orderBook" => pick("order_book"),
        "tradeBook" => pick("trade_book"),
        "positionBook" => pick("positions"),
        "holdings" => pick("holdings"),
        "limit" => pick("limit"),
        "getQuote" => pick("quote"),
        "getMultiQuotes" => pick("multiquotes"),
        "timePriceSeries" => {
            if body["interval"] == "1d" {
                pick("history_day")
            } else {
                pick("history_minute")
            }
        }
        "indexList" => pick("index_list"),
        "basketMargin" => pick("basket_margin"),
        "placeOrder" | "modifyOrder" => pick("place_ok"),
        "cancelOrder" => {
            if body["orderNumber"] == "1234567890112" {
                pick("cancel_failed")
            } else {
                ok(json!({"status":"success","data":{"orderNumber":body["orderNumber"]}}))
            }
        }
        "NSE" => ok(fixture!("firstock", "NSE.csv")),
        "BSE" => ok(fixture!("firstock", "BSE.csv")),
        "NFO" => ok(fixture!("firstock", "NFO.csv")),
        "BFO" => ok(fixture!("firstock", "BFO.csv")),
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let fake = fake.clone();
        async move {
            let raw = String::from_utf8_lossy(&body).to_string();
            let (jdata, jkey) = split_body(&raw);
            let h = |k: &str| {
                headers
                    .get(k)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            let path = uri.path().to_string();
            fake.seen.lock().push(Seen {
                path: path.clone(),
                content_type: h("content-type"),
                authorization: h("authorization"),
                raw,
                jdata: jdata.clone(),
                jkey,
            });
            route(&fake, &path, &jdata)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn master() -> SymbolResolver {
    let cfg = shoonya::config();
    let mut rows = parse_file(cfg, "NSE", fixture!("shoonya", "NSE_symbols.txt"));
    rows.extend(parse_file(
        cfg,
        "BSE",
        fixture!("shoonya", "BSE_symbols.txt"),
    ));
    rows.extend(parse_file(
        cfg,
        "NFO",
        fixture!("shoonya", "NFO_symbols.txt"),
    ));
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

async fn noren(cfg: &'static NorenConfig) -> (NorenBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = NorenBroker::with_endpoints(
        cfg,
        master(),
        NorenEndpoints::rebased(cfg, &host, "ws://127.0.0.1:1"),
    );
    (b, fake, AuthToken::new("<USER_ID>:::sess-tok"))
}

fn order(symbol: &str, exchange: &str, pricetype: &str, price: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

// ---------------------------------------------------------------------------
// Shoonya: Bearer dialect, GenAcsTok, identity guard, chunked history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shoonya_login_exchanges_the_code_with_a_checksum() {
    let (b, fake, _) = noren(shoonya::config()).await;
    let creds = BrokerCredentials {
        api_key: "<USER_ID>:::CLIENT_U".into(),
        api_secret: Some("secret".into()),
        auth_code: Some("code1".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    // Shoonya answers no user id: the one from the key is kept.
    assert_eq!(r.auth_token, "<USER_ID>:::acc-123");
    let s = &fake.calls("/GenAcsTok")[0];
    assert_eq!(s.path, "/NorenWClientAPI/GenAcsTok");
    assert_eq!(s.content_type, "text/plain");
    assert_eq!(s.authorization, "");
    assert_eq!(s.jdata["code"], "code1");
    assert_eq!(
        s.jdata["checksum"],
        openalgo_desktop_lib::brokers::families::noren::auth::sha256_hex(&[
            "CLIENT_U", "secret", "code1"
        ])
    );
    let bad = BrokerCredentials {
        auth_code: Some("bad".into()),
        ..creds
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert!(e.client_message().contains("Invalid code"));
}

#[tokio::test]
async fn shoonya_market_order_is_protected_and_sent_with_bearer() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "MARKET", 0.0))
        .await
        .unwrap();
    assert_eq!(r.order_id, "26100300000099");
    let q = &fake.calls("/GetQuotes")[0];
    assert_eq!(
        q.jdata,
        json!({"exch":"NSE","token":"3045","uid":"<USER_ID>"})
    );
    let p = &fake.calls("/PlaceOrder")[0];
    assert_eq!(p.content_type, "text/plain");
    assert_eq!(p.authorization, "Bearer sess-tok");
    assert!(p.jkey.is_none());
    assert_eq!(p.jdata["prctyp"], "LMT");
    // 812.40 * 1.005 on a 0.05 tick.
    assert_eq!(p.jdata["prc"], "816.45");
    assert_eq!(p.jdata["prd"], "I");
    assert_eq!(p.jdata["tsym"], "SBIN-EQ");
    assert_eq!(p.jdata["mkt_protection"], "0");
    // Ampersand symbol: %26 in tsym, no raw & anywhere in the body.
    b.place_order(&auth, &order("M&M", "NSE", "LIMIT", 2900.5))
        .await
        .unwrap();
    let p = &fake.calls("/PlaceOrder")[1];
    assert_eq!(p.jdata["tsym"], "M%26M-EQ");
    assert!(!p.raw.contains('&'));
}

#[tokio::test]
async fn shoonya_quote_identity_guard_retries() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    fake.wrong_quotes.store(2, Ordering::SeqCst);
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 812.4);
    assert_eq!(fake.quote_calls.load(Ordering::SeqCst), 3);
    fake.wrong_quotes.store(3, Ordering::SeqCst);
    let e = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("different instrument"));
    // Depth and multiquotes ride the same endpoint.
    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((d.bids[0].price, d.oi), (812.35, 0));
    let m = b
        .get_multiquotes(
            &auth,
            &[QuoteKey::new("NSE", "SBIN"), QuoteKey::new("NSE", "NOPE")],
        )
        .await
        .unwrap();
    assert_eq!(m[0].data.as_ref().unwrap().ltp, 812.4);
    assert_eq!(
        m[1].error.as_deref(),
        Some("Could not resolve broker symbol")
    );
}

#[tokio::test]
async fn shoonya_history_is_chunked_on_the_jkey_form() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    let calls = fake.calls("/TPSeries");
    // 11 days in 5-day windows.
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].content_type, "application/x-www-form-urlencoded");
    assert_eq!(calls[0].jkey.as_deref(), Some("sess-tok"));
    assert_eq!(calls[0].authorization, "");
    assert_eq!(calls[0].jdata["intrv"], "1");
    assert_eq!(calls[0].jdata["token"], "3045");
    // Same three candles from every chunk: deduplicated, repaired, sorted.
    assert_eq!(c.len(), 3);
    assert!(c.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
    assert!(c
        .iter()
        .all(|x| x.low <= x.open.min(x.close) && x.volume >= 0));
    // Daily: EODChartData with the index display name.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let d = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(fake.calls("/EODChartData")[0].jdata["sym"], "NSE:Nifty 50");
    assert_eq!(d.len(), 2);
    let e = b
        .get_history(
            &auth,
            &HistoryRequest {
                interval: "1w".into(),
                ..req
            },
        )
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not supported"));
}

/// Web #2161 (shoonya `_repair_candles`): a bar with a NaN price is dropped
/// rather than sent to the chart, which refuses the whole series over it;
/// the rest are repaired.
#[tokio::test]
async fn shoonya_history_drops_bars_without_a_price() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    fake.bodies.lock().push((
        "TPSeries",
        r#"[{"stat":"Ok","time":"01-10-2026 09:15:00","ssboe":"1790826300","into":"809","inth":"808","intl":"808.5","intc":"809.5","intv":"-10"},
            {"stat":"Ok","time":"01-10-2026 09:16:00","ssboe":"1790826360","into":"NaN","inth":"811","intl":"809","intc":"810","intv":"10"}]"#,
    ));
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].timestamp, 1790826300);
    assert_eq!((c[0].low, c[0].high, c[0].volume), (808.0, 809.5, 0));
}

// Web test/test_shoonya_history_aggregation.py (marketcalls/openalgo#2343,
// #2360, 0e4a488cc): Shoonya folds the next trading day's first 1m bar into
// each day's last intraday bar when one TPSeries request spans several
// days; each day's last bar is rebuilt from 1m bars, fetched only for the
// span the repair reads.

/// Shoonya's 5m SBIN reply for 30-10 to 02-11 (last bars contaminated).
fn sbin_5m() -> Bars {
    bars_at(&[
        ("2026-10-30 15:20", 960.05, 960.05, 960.05, 960.05, 0),
        ("2026-10-30 15:25", 960.05, 965.0, 960.05, 965.0, 6971359),
        ("2026-11-02 09:15", 968.75, 976.4, 964.65, 976.35, 453904),
        ("2026-11-02 15:25", 985.0, 992.7, 985.0, 992.7, 9608879),
    ])
}

fn sbin_1m() -> Bars {
    bars_at(&[
        ("2026-10-30 15:24", 960.05, 960.05, 960.05, 960.05, 2653),
        ("2026-10-30 15:25", 960.05, 960.05, 960.05, 960.05, 495),
        ("2026-10-30 15:26", 960.05, 960.05, 960.05, 960.05, 1201),
        ("2026-10-30 15:27", 960.05, 960.05, 960.05, 960.05, 10506),
        ("2026-10-30 15:28", 961.85, 961.85, 961.85, 961.85, 6959157),
        ("2026-10-30 15:29", 961.85, 961.85, 961.85, 961.85, 0),
        ("2026-11-02 09:14", 965.0, 965.0, 965.0, 965.0, 0),
        ("2026-11-02 09:15", 968.75, 973.75, 964.65, 973.7, 102611),
        ("2026-11-02 15:25", 985.0, 985.0, 985.0, 985.0, 3184),
        ("2026-11-02 15:26", 985.0, 985.0, 985.0, 985.0, 531),
        ("2026-11-02 15:27", 985.0, 985.0, 985.0, 985.0, 0),
        ("2026-11-02 15:28", 985.0, 985.0, 985.0, 985.0, 2357),
        ("2026-11-02 15:29", 987.0, 987.0, 987.0, 987.0, 9602870),
        ("2026-11-03 09:14", 992.7, 992.7, 992.7, 992.7, 0),
    ])
}

/// 5m SBIN history from `start` to `end` against `series`: the bars, and
/// the 1m TPSeries calls made.
async fn shoonya_5m(
    series: Series,
    start: (i32, u32, u32),
    end: (i32, u32, u32),
) -> (Vec<Candle>, Vec<Value>) {
    let (b, fake, auth) = noren(shoonya::config()).await;
    *fake.series.lock() = Some(series);
    let day = |(y, m, d): (i32, u32, u32)| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: day(start),
        end: day(end),
    };
    let out = b.get_history(&auth, &req).await.unwrap();
    let minute = fake
        .calls("/TPSeries")
        .into_iter()
        .map(|c| c.jdata)
        .filter(|j| j["intrv"] == "1")
        .collect();
    (out, minute)
}

fn bar_at(out: &[Candle], t: &str) -> Candle {
    *out.iter()
        .find(|c| c.timestamp == ist_ts(t))
        .unwrap_or_else(|| panic!("no bar at {}", t))
}

/// `test_long_request_fetches_1m_only_where_bars_exist`: five years asked,
/// bars on two days: one 1m call, for the span between them.
#[tokio::test]
async fn shoonya_history_fetches_1m_only_where_bars_exist() {
    let series = Series {
        native: sbin_5m(),
        minute: sbin_1m(),
        fail_st: None,
    };
    let (out, minute) = shoonya_5m(series, (2021, 11, 1), (2026, 11, 3)).await;
    assert_eq!(minute.len(), 1, "{:?}", minute);
    assert_eq!(minute[0]["st"], ist_ts("2026-10-30 15:25").to_string());
    assert_eq!(
        minute[0]["et"],
        (ist_ts("2026-11-02 09:15") - 1).to_string()
    );
    let last = bar_at(&out, "2026-10-30 15:25");
    assert_eq!((last.high, last.close), (961.85, 961.85));
}

/// `test_single_day_request_makes_no_1m_call`
#[tokio::test]
async fn shoonya_history_of_one_day_makes_no_1m_call() {
    let series = Series {
        native: sbin_5m(),
        minute: sbin_1m(),
        fail_st: None,
    };
    let (out, minute) = shoonya_5m(series, (2026, 10, 30), (2026, 10, 30)).await;
    assert!(minute.is_empty(), "{:?}", minute);
    assert_eq!(out.len(), 2);
}

/// Five trading days whose repair span is longer than one 5-day 1m chunk:
/// the first chunk ends at 02-11 15:25:00 and the second starts inside the
/// last bar of 02-11. Each day's last 5m bar carries the next day's 09:14
/// price folded in, as TPSeries sends it.
fn boundary_series(fail_st: Option<i64>) -> Series {
    const DAYS: [(&str, f64); 5] = [
        ("2026-10-28", 100.0),
        ("2026-10-29", 110.0),
        ("2026-10-30", 120.0),
        ("2026-11-02", 130.0),
        ("2026-11-03", 140.0),
    ];
    let at = |day: &str, hm: &str| ist_ts(&format!("{} {}", day, hm));
    let (mut native, mut minute) = (Vec::new(), Vec::new());
    for (i, (day, base)) in DAYS.iter().enumerate() {
        let next = DAYS.get(i + 1).map(|(_, b)| b + 5.0);
        native.push((at(day, "09:15"), *base, base + 1.0, base - 1.0, *base, 10));
        let top = next.map_or(base + 2.0, |n| (base + 2.0).max(n));
        native.push((
            at(day, "15:25"),
            *base,
            top,
            base - 1.0,
            next.unwrap_or(base + 2.0),
            50,
        ));
        minute.push((
            at(day, "09:14"),
            base - 3.0,
            base - 3.0,
            base - 3.0,
            base - 3.0,
            0,
        ));
        minute.push((at(day, "09:15"), *base, base + 1.0, base - 1.0, *base, 10));
        for (m, close) in [
            (25, *base),
            (26, base + 1.0),
            (27, base - 1.0),
            (28, base + 2.0),
            (29, base + 2.0),
        ] {
            minute.push((
                at(day, &format!("15:{}", m)),
                close,
                close,
                close,
                close,
                10,
            ));
        }
    }
    Series {
        native,
        minute,
        fail_st,
    }
}

/// `test_a_failed_1m_chunk_keeps_the_affected_day_end_bar`: the second 1m
/// chunk fails at the boundary, so 02-11's last bar is kept exactly as
/// Shoonya sent it rather than rebuilt from the 15:25 minute alone (close
/// 130); the days the first chunk covered in full are still repaired.
#[tokio::test]
async fn shoonya_history_keeps_a_day_end_bar_whose_1m_chunk_failed() {
    let fail = ist_ts("2026-11-02 15:25") + 1;
    let (out, minute) =
        shoonya_5m(boundary_series(Some(fail)), (2026, 10, 28), (2026, 11, 3)).await;
    assert_eq!(minute.len(), 2, "{:?}", minute);
    let kept = bar_at(&out, "2026-11-02 15:25");
    assert_eq!((kept.high, kept.close, kept.volume), (145.0, 145.0, 50));
    for (day, base) in [
        ("2026-10-28", 100.0),
        ("2026-10-29", 110.0),
        ("2026-10-30", 120.0),
    ] {
        let bar = bar_at(&out, &format!("{} 15:25", day));
        assert_eq!((bar.high, bar.close), (base + 2.0, base + 2.0), "{}", day);
    }
}

/// `test_without_a_failure_every_day_end_bar_is_repaired`
#[tokio::test]
async fn shoonya_history_repairs_every_day_end_bar_without_a_failure() {
    let (out, _) = shoonya_5m(boundary_series(None), (2026, 10, 28), (2026, 11, 3)).await;
    let last = bar_at(&out, "2026-11-02 15:25");
    assert_eq!((last.high, last.close, last.volume), (132.0, 132.0, 50));
}

/// The repair is Shoonya's (`NorenConfig::day_end_repair`): the web
/// repairs no other Noren member, so Zebu's bars stay as sent.
#[tokio::test]
async fn other_noren_members_keep_their_day_end_bars() {
    let (b, fake, auth) = noren(zebu::config()).await;
    *fake.series.lock() = Some(Series {
        native: sbin_5m(),
        minute: sbin_1m(),
        fail_st: None,
    });
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
    };
    let out = b.get_history(&auth, &req).await.unwrap();
    assert!(fake
        .calls("/TPSeries")
        .iter()
        .all(|c| c.jdata["intrv"] != "1"));
    assert_eq!(bar_at(&out, "2026-10-30 15:25").close, 965.0);
}

#[tokio::test]
async fn shoonya_books_funds_and_cancel_all() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    let book = b.get_order_book(&auth).await.unwrap();
    assert_eq!(book[0].symbol, "SBIN");
    assert_eq!(
        fake.calls("/OrderBook")[0].jdata,
        json!({"uid":"<USER_ID>","actid":"<USER_ID>"})
    );
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[0].symbol, "RELIANCE");
    let pos = b.get_positions(&auth).await.unwrap();
    assert_eq!(pos[1].symbol, "NIFTY27OCT26FUT");
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "SBIN");
    assert_eq!(fake.calls("/Holdings")[0].jdata["prd"], "C");
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!((f.available_cash, f.m2m_realized), (85000.0, 150.0));
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
    // Two working orders; the stop's cancel is refused (message, not emsg).
    let r = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(r.cancelled, ["26100300000001"]);
    assert_eq!(r.failed, ["26100300000002"]);
    let e = b.cancel_order(&auth, "26100300000002").await.unwrap_err();
    assert_eq!(e.client_message(), "Order already cancelled");
    // Close-all: two open positions, MARKET exits with price protection.
    let c = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(c.placed.len(), 2, "{:?}", c.failed);
    let exits = fake.calls("/PlaceOrder");
    assert_eq!(exits[0].jdata["trantype"], "S");
    assert_eq!(exits[1].jdata["trantype"], "B");
    assert_eq!(exits[1].jdata["qty"], "75");
}

/// BR-03 sentinels: a position book reply that is not a list (an object,
/// an `Ok` without rows, text) is never read as flat by the smart-order
/// read or close all; the broker's own "no data" answer is.
#[tokio::test]
async fn a_malformed_position_book_is_never_flat() {
    for body in [
        "{}",
        "{\"stat\":\"Ok\"}",
        "\"x\"",
        "{\"stat\":\"Not_Ok\",\"emsg\":\"Session Expired : Invalid Session Key\"}",
    ] {
        let (b, fake, auth) = noren(shoonya::config()).await;
        fake.bodies.lock().push(("PositionBook", body));
        assert!(
            b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Mis)
                .await
                .is_err(),
            "{}",
            body
        );
        assert!(b.close_all_positions(&auth).await.is_err(), "{}", body);
        assert!(fake.calls("/PlaceOrder").is_empty(), "{}", body);
    }
    let (b, fake, auth) = noren(shoonya::config()).await;
    fake.bodies
        .lock()
        .push(("PositionBook", "{\"stat\":\"Not_Ok\",\"emsg\":\"no data\"}"));
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn shoonya_modify_and_basket_margin() {
    let (b, fake, auth) = noren(shoonya::config()).await;
    let m = ResolvedModify::resolve(
        "26100300000001",
        &ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 10,
            price: 811.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&auth, &m).await.unwrap();
    assert_eq!(r.order_id, "26100300000001");
    let s = &fake.calls("/ModifyOrder")[0];
    assert_eq!(s.jdata["prc"], "811");
    assert!(s.jdata.get("trgprc").is_none());
    let legs = vec![
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
            action: Action::Sell,
            quantity: 75,
            product: Product::Nrml,
            pricetype: PriceType::Market,
            price: 0.0,
            trigger_price: 0.0,
        },
        MarginLeg {
            key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
            action: Action::Buy,
            quantity: 75,
            product: Product::Nrml,
            pricetype: PriceType::Limit,
            price: 120.0,
            trigger_price: 0.0,
        },
    ];
    let mr = b.calculate_margin(&auth, &legs).await.unwrap();
    assert_eq!(mr.total_margin_required, 150000.5);
    let body = &fake.calls("/GetBasketMargin")[0].jdata;
    assert_eq!(body["tsym"], "NIFTY27OCT26F");
    assert_eq!(body["prctyp"], "LMT");
    assert_eq!(body["basketlists"][0]["tsym"], "NIFTY27OCT26C25000");
    assert_eq!(body["basketlists"][0]["prc"], "120");
}

#[tokio::test]
async fn shoonya_master_download_reads_the_zips() {
    let (b, _fake, auth) = noren(shoonya::config()).await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY"));
    assert!(rows.iter().any(|r| r.symbol == "CRUDEOIL19OCT26FUT"));
    assert!(rows.iter().any(|r| r.symbol == "SENSEX29OCT2682000CE"));
    for ex in ["NSE", "BSE", "NFO", "CDS", "MCX", "BFO"] {
        assert!(rows.iter().any(|r| r.exchange == ex), "{}", ex);
    }
    assert_eq!(rows.iter().filter(|r| r.exchange == "BSE_INDEX").count(), 2);
}

/// MC-02: Shoonya, Zebu and TradeSmart masters are all or nothing like
/// Flattrade's (their web download fails too when a file is missing): a
/// failed or empty file refuses the download by segment, so the stored
/// master is kept instead of being replaced by a partial one.
#[tokio::test]
async fn shoonya_family_masters_are_all_or_nothing() {
    for cfg in [shoonya::config(), zebu::config(), tradesmart::config()] {
        let (b, fake, auth) = noren(cfg).await;
        let leaf = cfg
            .master_files
            .iter()
            .find(|f| f.exchange == "BFO")
            .map(|f| f.url.rsplit('/').next().unwrap())
            .unwrap();
        fake.down.lock().push(leaf);
        let msg = b
            .download_master_contract(&auth)
            .await
            .unwrap_err()
            .client_message();
        assert!(msg.contains("symbol files for BFO"), "{}: {}", cfg.id, msg);
        assert!(msg.contains("existing symbols were kept"), "{}", msg);
    }
    let (b, fake, auth) = noren(shoonya::config()).await;
    fake.bodies.lock().push(("CDS_symbols.txt.zip", " \n"));
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("symbol files for CDS"), "{}", msg);
}

// ---------------------------------------------------------------------------
// Flattrade: jKey form everywhere, apitoken login, flattrade master
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flattrade_login_and_jkey_dialect() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    let r = b
        .authenticate(BrokerCredentials {
            api_key: "FT1:::appkey".into(),
            api_secret: Some("sec".into()),
            request_token: Some("rc".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.auth_token, "FT1:::ft-tok");
    let s = &fake.calls("/apitoken")[0];
    assert_eq!(s.path, "/trade/apitoken");
    assert_eq!(s.content_type, "application/json");
    assert_eq!(s.jdata["api_key"], "appkey");
    assert_eq!(s.jdata["request_code"], "rc");
    b.place_order(&auth, &order("SBIN", "NSE", "LIMIT", 811.0))
        .await
        .unwrap();
    let p = &fake.calls("/PlaceOrder")[0];
    assert_eq!(p.path, "/PiConnectAPI/PlaceOrder");
    assert_eq!(p.content_type, "application/x-www-form-urlencoded");
    assert_eq!(p.jkey.as_deref(), Some("sess-tok"));
    assert_eq!(p.authorization, "");
    // Funds read M2M from the position book.
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.collateral, 9999.0);
    assert_eq!(f.m2m_unrealized, 1534.0);
    assert_eq!(fake.calls("/PositionBook").len(), 1);
    // Basket margin: marginusedtrade (90000.25) less Limits' marginused
    // (20000.00, the margin already in use; web #2156).
    let mr = b
        .calculate_margin(
            &auth,
            &[MarginLeg {
                key: QuoteKey::new("NSE", "SBIN"),
                action: Action::Buy,
                quantity: 1,
                product: Product::Cnc,
                pricetype: PriceType::Limit,
                price: 800.0,
                trigger_price: 0.0,
            }],
        )
        .await
        .unwrap();
    assert_eq!(mr.total_margin_required, 70000.25);
}

/// Web #2161: Flattrade reports the post-hedge `marginusedtrade`, prices an
/// SL-M margin leg off its trigger (a SELL stays below it), and refuses a
/// MARKET leg it cannot price instead of sending it at 0 or dropping it.
#[tokio::test]
async fn flattrade_margin_legs_are_priced_or_refused() {
    let slm = MarginLeg {
        key: QuoteKey::new("NSE", "SBIN"),
        action: Action::Sell,
        quantity: 1,
        product: Product::Mis,
        pricetype: PriceType::SlM,
        price: 0.0,
        trigger_price: 800.0,
    };
    let (b, fake, auth) = noren(flattrade::config()).await;
    let mr = b
        .calculate_margin(&auth, std::slice::from_ref(&slm))
        .await
        .unwrap();
    assert_eq!(mr.total_margin_required, 70000.25);
    let body = &fake.calls("/GetBasketMargin")[0].jdata;
    assert_eq!(
        (&body["prctyp"], &body["prc"], &body["trgprc"]),
        (&json!("SL-LMT"), &json!("796"), &json!("800"))
    );

    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies
        .lock()
        .push(("GetQuotes", r#"{"stat":"Ok","lp":"0"}"#));
    let market = MarginLeg {
        pricetype: PriceType::Market,
        trigger_price: 0.0,
        ..slm.clone()
    };
    let e = b.calculate_margin(&auth, &[market]).await.unwrap_err();
    assert!(
        e.client_message()
            .starts_with("Could not get a live price for SBIN."),
        "{}",
        e.client_message()
    );
    assert!(fake.calls("/GetBasketMargin").is_empty());

    // Shoonya keeps the LTP rule: the same SL-M leg is priced off the LTP.
    let (b, fake, auth) = noren(shoonya::config()).await;
    b.calculate_margin(&auth, &[slm]).await.unwrap();
    assert_eq!(fake.calls("/GetBasketMargin")[0].jdata["prc"], "808.35");
}

fn sbin_leg() -> MarginLeg {
    MarginLeg {
        key: QuoteKey::new("NSE", "SBIN"),
        action: Action::Buy,
        quantity: 1,
        product: Product::Cnc,
        pricetype: PriceType::Limit,
        price: 800.0,
        trigger_price: 0.0,
    }
}

/// The leaves of the margin calls the fake saw, in order.
fn margin_calls(fake: &Fake) -> Vec<String> {
    fake.seen
        .lock()
        .iter()
        .map(|s| s.path.rsplit('/').next().unwrap_or("").to_string())
        .filter(|l| l == "GetBasketMargin" || l == "Limits")
        .collect()
}

/// Web test/test_margin_2156.py `test_flattrade_api_subtracts_limits_marginused`
/// (web #2156): GetBasketMargin's figures are account totals, so the margin
/// already in use (`/Limits` `marginused`) is taken off, read after the
/// basket.
#[tokio::test]
async fn flattrade_margin_is_the_baskets_own() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().extend([
        (
            "GetBasketMargin",
            r#"{"stat":"Ok","marginused":"324330.00","marginusedtrade":"186778.30"}"#,
        ),
        ("Limits", r#"{"stat":"Ok","marginused":"8576.72"}"#),
    ]);
    let mr = b.calculate_margin(&auth, &[sbin_leg()]).await.unwrap();
    assert_eq!(mr.total_margin_required, 178201.58);
    assert_eq!(margin_calls(&fake), ["GetBasketMargin", "Limits"]);
    let limits = &fake.calls("/Limits")[0];
    assert_eq!(
        (&limits.jdata["uid"], &limits.jdata["actid"]),
        (&json!("<USER_ID>"), &json!("<USER_ID>"))
    );
}

/// Web `test_flattrade_api_fails_when_limits_fails` and
/// `test_flattrade_api_skips_limits_when_basket_rejected`.
#[tokio::test]
async fn flattrade_margin_fails_when_either_answer_does() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().extend([
        (
            "GetBasketMargin",
            r#"{"stat":"Ok","marginusedtrade":"186778.30"}"#,
        ),
        ("Limits", r#"{"stat":"Not_Ok","emsg":"Session Expired"}"#),
    ]);
    let e = b.calculate_margin(&auth, &[sbin_leg()]).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Could not read margin already in use: Session Expired"
    );

    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().push((
        "GetBasketMargin",
        r#"{"stat":"Not_Ok","emsg":"Invalid Input"}"#,
    ));
    let e = b.calculate_margin(&auth, &[sbin_leg()]).await.unwrap_err();
    assert_eq!(e.client_message(), "Invalid Input");
    assert_eq!(margin_calls(&fake), ["GetBasketMargin"]);

    // A reply without the post-trade figure is not priced at marginused.
    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().extend([
        (
            "GetBasketMargin",
            r#"{"stat":"Ok","marginused":"315753.18"}"#,
        ),
        ("Limits", r#"{"stat":"Ok","marginused":"0.00"}"#),
    ]);
    let e = b.calculate_margin(&auth, &[sbin_leg()]).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Broker did not return the post-trade margin"
    );
}

/// Web `test_flattrade_unknown_symbol_fails_the_basket` (a live Kotak test
/// priced SILVER27NOV26151000CE alone when SILVER29DEC26151000CE, the
/// December contract dated a day wrong, was dropped): a leg missing from
/// the master fails the basket, named, and nothing is priced.
#[tokio::test]
async fn a_margin_leg_missing_from_the_master_fails_the_basket() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    let missing = MarginLeg {
        key: QuoteKey::new("NFO", "SBIN29DEC26FUT"),
        ..sbin_leg()
    };
    let e = b
        .calculate_margin(&auth, &[sbin_leg(), missing])
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("SBIN29DEC26FUT (NFO)")
            && !e.client_message().contains("SBIN (NSE)"),
        "{}",
        e.client_message()
    );
    assert!(margin_calls(&fake).is_empty());
}

/// Web #2196: Flattrade's EOD rows for BSE indices carry a close outside the
/// day's high/low, and a chart refuses the whole history on one such candle,
/// so Flattrade widens high/low to cover open and close. Members whose web
/// adapters do not (Zebu, TradeSmart) pass the rows through as sent.
#[tokio::test]
async fn flattrade_daily_candles_cover_open_and_close() {
    let req = HistoryRequest {
        key: QuoteKey::new("BSE", "SBIN"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let (b, fake, auth) = noren(flattrade::config()).await;
    let d = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(fake.calls("/EODChartData")[0].jdata["sym"], "BSE:SBIN");
    assert_eq!(d.len(), 2);
    assert_eq!((d[0].low, d[0].high), (80800.0, 81650.0));
    assert_eq!((d[1].low, d[1].high), (81300.0, 81900.0));
    assert!(d
        .iter()
        .all(|c| c.low <= c.open.min(c.close) && c.high >= c.open.max(c.close)));

    let (b, _fake, auth) = noren(zebu::config()).await;
    let d = b.get_history(&auth, &req).await.unwrap();
    assert_eq!((d[0].low, d[0].high), (80800.0, 81500.0));
}

/// Web #2198: index rows are typed EQ and the stale BSE rows with no
/// exchange are dropped; every one of the eight files is fetched.
#[tokio::test]
async fn flattrade_master_csvs() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    let sensex = rows
        .iter()
        .find(|r| r.exchange == "BSE_INDEX" && r.symbol == "SENSEX")
        .unwrap();
    assert_eq!(sensex.instrument_type, "EQ");
    let nifty = rows
        .iter()
        .find(|r| r.exchange == "NSE_INDEX" && r.symbol == "NIFTY")
        .unwrap();
    assert_eq!(nifty.instrument_type, "EQ");
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY27OCT2625000CE" && r.tick_size == 0.05));
    for ex in ["NSE", "BSE", "NFO", "CDS", "MCX", "BFO"] {
        assert!(rows.iter().any(|r| r.exchange == ex), "{}", ex);
    }
    assert!(rows.iter().any(|r| r.symbol == "SENSEX29OCT2682000CE"));
    assert!(!rows
        .iter()
        .any(|r| ["212716", "212255", "212139"].contains(&r.token.as_str())));
    let files: Vec<String> = fake
        .seen
        .lock()
        .iter()
        .filter(|s| s.path.ends_with(".csv"))
        .map(|s| s.path.clone())
        .collect();
    assert_eq!(files.len(), 8, "{:?}", files);
}

/// Web #2198: a Flattrade master file that fails, comes back empty, or a
/// segment that yields no rows refuses the whole download (the stored
/// master is kept), naming the segment (the other members:
/// `shoonya_family_masters_are_all_or_nothing`).
#[tokio::test]
async fn flattrade_master_is_all_or_nothing() {
    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.down.lock().push("Bfo_Equity_Derivatives.csv");
    let e = b.download_master_contract(&auth).await.unwrap_err();
    let msg = e.client_message();
    assert!(msg.contains("Flattrade symbol files for BFO"), "{}", msg);
    assert!(msg.contains("existing symbols were kept"), "{}", msg);

    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().push(("Currency_Derivatives.csv", " \n"));
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("symbol files for CDS"), "{}", msg);

    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.bodies.lock().push((
        "Commodity.csv",
        "Exchange,Token,Lotsize,Symbol,Tradingsymbol,Instrument,Expiry,Strike,Optiontype\n",
    ));
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(
        msg.contains("Flattrade MCX symbol file had no usable rows"),
        "{}",
        msg
    );

    // Both NFO files failing names NFO once.
    let (b, fake, auth) = noren(flattrade::config()).await;
    fake.down
        .lock()
        .extend(["Nfo_Index_Derivatives.csv", "Nfo_Equity_Derivatives.csv"]);
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("symbol files for NFO."), "{}", msg);
}

/// Web #2198: Flattrade drops TPSeries' 09:14 pre-open bar on an NSE index
/// (queried on NSE), skips rows with a missing or NaN price, and floors the
/// negative closing-session volume; Zebu passes the same rows through.
#[tokio::test]
async fn flattrade_intraday_candles_are_hardened() {
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
    };
    let (b, fake, auth) = noren(flattrade::config()).await;
    let c = b.get_history(&auth, &req).await.unwrap();
    let call = &fake.calls("/TPSeries")[0].jdata;
    assert_eq!(
        (call["exch"].as_str(), call["token"].as_str()),
        (Some("NSE"), Some("26000"))
    );
    let ts: Vec<i64> = c.iter().map(|x| x.timestamp).collect();
    assert_eq!(ts, [1790826300, 1790826540, 1790848200]);
    assert!(c
        .iter()
        .all(|x| x.volume >= 0 && x.low > 0.0 && x.close.is_finite()));
    assert_eq!(c[0].volume, 120000);

    let (b, _fake, auth) = noren(zebu::config()).await;
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 7);
    assert_eq!(c[0].timestamp, 1790826240, "zebu keeps the 09:14 bar");
}

/// Web #2198: an EOD row with no low is skipped for Flattrade, so the
/// #2196 widening cannot keep a 0 low; Zebu reads it as 0.
#[tokio::test]
async fn flattrade_daily_rows_without_a_price_are_skipped() {
    let req = HistoryRequest {
        key: QuoteKey::new("BSE", "SBIN"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
    };
    let (b, _fake, auth) = noren(flattrade::config()).await;
    let d = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(d.len(), 2);
    assert!(d.iter().all(|c| c.low > 0.0));
    let (b, _fake, auth) = noren(zebu::config()).await;
    let d = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(d.len(), 3);
    assert_eq!(d[2].low, 0.0);
}

// ---------------------------------------------------------------------------
// TradeSmart and Zebu deltas
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tradesmart_login_keeps_the_returned_account_and_margin_per_leg() {
    let (b, fake, auth) = noren(tradesmart::config()).await;
    let r = b
        .authenticate(BrokerCredentials {
            api_key: "KEY".into(),
            api_secret: Some("sec".into()),
            auth_code: Some("c".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.auth_token, "TS9:::acc-123");
    assert_eq!(
        fake.calls("/GenAcsTok")[0].path,
        "/NorenWClientAPIv2/GenAcsTok"
    );
    b.place_order(&auth, &order("SBIN", "NSE", "LIMIT", 811.0))
        .await
        .unwrap();
    let p = &fake.calls("/PlaceOrder")[0].jdata;
    assert_eq!(p["remarks"], "openalgo");
    assert!(p.get("mkt_protection").is_none());
    let leg = |sym: &str| MarginLeg {
        key: QuoteKey::new("NSE", sym),
        action: Action::Buy,
        quantity: 1,
        product: Product::Cnc,
        pricetype: PriceType::Limit,
        price: 800.0,
        trigger_price: 0.0,
    };
    let mr = b
        .calculate_margin(&auth, &[leg("SBIN"), leg("RELIANCE")])
        .await
        .unwrap();
    assert_eq!(mr.total_margin_required, 2001.0);
    let calls = fake.calls("/GetOrderMargin");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].jdata["rorgqty"], "0");
}

#[tokio::test]
async fn zebu_has_no_margin_and_keeps_sl_m() {
    let (b, fake, auth) = noren(zebu::config()).await;
    assert!(!b.capabilities().margin);
    let e = b.calculate_margin(&auth, &[]).await.unwrap_err();
    assert!(matches!(
        e,
        openalgo_desktop_lib::error::AppError::Unsupported("margin")
    ));
    let mut o = order("SBIN", "NSE", "SL-M", 0.0);
    o.trigger_price = 800.0;
    b.place_order(&auth, &o).await.unwrap();
    assert!(fake.calls("/GetQuotes").is_empty());
    assert_eq!(fake.calls("/PlaceOrder")[0].jdata["prctyp"], "SL-MKT");
    assert_eq!(
        fake.calls("/PlaceOrder")[0].path,
        "/NorenWClientAPI/PlaceOrder"
    );
}

// ---------------------------------------------------------------------------
// Firstock
// ---------------------------------------------------------------------------

async fn firstock_setup() -> (FirstockBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let r = SymbolResolver::new();
    let mut rows = firstock::master_contract::parse_file("NSE", fixture!("firstock", "NSE.csv"));
    rows.extend(firstock::master_contract::parse_file(
        "NFO",
        fixture!("firstock", "NFO.csv"),
    ));
    r.load(rows);
    let b = FirstockBroker::with_urls(r, format!("{}/V1", host), "ws://127.0.0.1:1");
    (b, fake, AuthToken::new("AB1234:::jkey-abc"))
}

#[tokio::test]
async fn firstock_login_orders_and_books() {
    let (b, fake, auth) = firstock_setup().await;
    let creds = BrokerCredentials {
        api_key: "AB1234_API".into(),
        api_secret: Some("apikey".into()),
        client_id: Some("AB1234".into()),
        password: Some("pw".into()),
        totp: Some("123456".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, "AB1234:::jkey-abc");
    let s = &fake.calls("/V1/login")[0];
    assert_eq!(s.jdata["vendorCode"], "AB1234_API");
    assert_eq!(s.jdata["apiKey"], "apikey");
    assert_ne!(s.jdata["password"], "pw");
    let e = b
        .authenticate(BrokerCredentials {
            totp: Some("000000".into()),
            ..creds
        })
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Invalid password"));

    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        price: 0.0,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    let o = ResolvedOrder::resolve(&req, b.symbols().unwrap()).unwrap();
    let r = b.place_order(&auth, &o).await.unwrap();
    assert_eq!(r.order_id, "1234567890200");
    let p = &fake.calls("/placeOrder")[0];
    assert_eq!(p.content_type, "application/json");
    assert_eq!(p.jdata["jKey"], "jkey-abc");
    assert_eq!(p.jdata["userId"], "AB1234");
    assert_eq!(p.jdata["priceType"], "LMT");
    assert_eq!(p.jdata["price"], "816.45");
    assert_eq!(fake.calls("/getQuote")[0].jdata["tradingSymbol"], "SBIN-EQ");

    let book = b.get_order_book(&auth).await.unwrap();
    assert_eq!(book[1].status, "trigger_pending");
    let ca = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(ca.cancelled, ["1234567890111"]);
    assert_eq!(ca.failed, ["1234567890112"]);
    let pos = b.get_positions(&auth).await.unwrap();
    assert_eq!(pos[0].symbol, "NIFTY27OCT26FUT");
    assert_eq!(
        b.get_open_position(&auth, "NIFTY27OCT26FUT", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap(),
        -75
    );
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 85000.0);
    assert_eq!(b.get_holdings(&auth).await.unwrap()[0].symbol, "SBIN");
    assert_eq!(b.get_trade_book(&auth).await.unwrap()[0].quantity, 5);
}

#[tokio::test]
async fn firstock_quotes_history_and_master() {
    let (b, fake, auth) = firstock_setup().await;
    let m = b
        .get_multiquotes(
            &auth,
            &[
                QuoteKey::new("NSE", "SBIN"),
                QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
                QuoteKey::new("NSE", "NOPE"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(m[0].data.as_ref().unwrap().ltp, 812.4);
    assert_eq!(m[1].data.as_ref().unwrap().oi, 9876525);
    assert!(m[2].error.is_some());
    assert_eq!(
        fake.calls("/getMultiQuotes")[0].jdata["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let h = b
        .get_history(
            &auth,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "1m".into(),
                start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            },
        )
        .await
        .unwrap();
    let calls = fake.calls("/timePriceSeries");
    assert_eq!(calls.len(), 2, "1m is fetched one day at a time");
    assert_eq!(calls[0].jdata["startTime"], "00:00:00 01-10-2026");
    assert_eq!(calls[0].jdata["endTime"], "23:59:59 01-10-2026");
    assert_eq!(calls[0].jdata["interval"], "1mi");
    assert_eq!(h.len(), 2);
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "BANKNIFTY"));
    assert!(rows
        .iter()
        .any(|r| r.exchange == "BSE_INDEX" && r.symbol == "SENSEX"));
    assert!(rows.iter().any(|r| r.symbol == "NIFTY27OCT26FUT"));
    let mr = b
        .calculate_margin(
            &auth,
            &[MarginLeg {
                key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
                action: Action::Buy,
                quantity: 75,
                product: Product::Nrml,
                pricetype: PriceType::Limit,
                price: 25000.0,
                trigger_price: 0.0,
            }],
        )
        .await
        .unwrap();
    assert_eq!(mr.total_margin_required, 126783.0);
    assert_eq!(
        fake.calls("/basketMargin")[0].jdata["BasketList_Params"],
        json!([])
    );
}

/// Web #2198: every Firstock symbol file must arrive and yield rows, or the
/// download fails and the stored master is kept.
#[tokio::test]
async fn firstock_master_is_all_or_nothing() {
    let (b, fake, auth) = firstock_setup().await;
    fake.down.lock().push("BFO");
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("Firstock symbol files for BFO"), "{}", msg);
    assert!(msg.contains("existing symbols were kept"), "{}", msg);

    let (b, fake, auth) = firstock_setup().await;
    fake.bodies.lock().push((
        "NFO",
        "Exchange,Token,LotSize,Symbol,TradingSymbol,CompanyName,Expiry,Instrument,OptionType,StrikePrice,TickSize,FreezeQty\n",
    ));
    let msg = b
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(
        msg.contains("Firstock NFO symbol file had no usable rows"),
        "{}",
        msg
    );
}

/// Web #2198 (`get_existing_index_rows`): when the index list cannot be
/// fetched, the index rows of the master in use are carried forward
/// instead of being dropped, each (exchange, token) once.
#[tokio::test]
async fn firstock_index_rows_survive_a_failed_index_list() {
    let (b, fake, auth) = firstock_setup().await;
    let all: Value = serde_json::from_str(fixture!("firstock", "responses.json")).unwrap();
    let mut current = firstock::master_contract::parse_file("NSE", fixture!("firstock", "NSE.csv"));
    current.extend(firstock::master_contract::parse_index_list(
        &all["index_list"],
    ));
    b.symbols().unwrap().load(current);
    fake.down.lock().push("indexList");
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(fake.calls("/indexList").len(), 1);
    // NIFTY BANK exists only in the index list: carried forward.
    let bank: Vec<_> = rows
        .iter()
        .filter(|r| r.exchange == "NSE_INDEX" && r.token == "26009")
        .collect();
    assert_eq!(bank.len(), 1);
    assert_eq!(bank[0].symbol, "BANKNIFTY");
    // Nifty 50 and SENSEX are in the fresh CSVs too: not repeated.
    for (ex, tok) in [("NSE_INDEX", "26000"), ("BSE_INDEX", "1")] {
        assert_eq!(
            rows.iter()
                .filter(|r| r.exchange == ex && r.token == tok)
                .count(),
            1,
            "{} {}",
            ex,
            tok
        );
    }

    // A direct download with an empty master in memory has nothing to
    // carry...
    let (b, fake, auth) = firstock_setup().await;
    b.symbols().unwrap().load(Vec::new());
    fake.down.lock().push("indexList");
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert!(!rows.iter().any(|r| r.token == "26009"));
    // ...but the service's download, the first after a sign-in included,
    // hands over the index rows of the stored table, and those are kept.
    assert_eq!(b.carries_stored(), Some("INDEX"));
    let stored = firstock::master_contract::parse_index_list(&all["index_list"]);
    let m = b.download_master_carrying(&auth, stored).await.unwrap();
    let bank: Vec<_> = m
        .rows
        .iter()
        .filter(|r| r.exchange == "NSE_INDEX" && r.token == "26009")
        .collect();
    assert_eq!(bank.len(), 1);
    assert_eq!(
        m.rows
            .iter()
            .filter(|r| r.exchange == "NSE_INDEX" && r.token == "26000")
            .count(),
        1
    );
}
