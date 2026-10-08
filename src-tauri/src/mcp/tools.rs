//! The tool implementations: each builds the payload the web's tool hands
//! the Python SDK (and the SDK sends), calls `/api/v1` in process
//! ([`super::dispatch`]) and shapes the result as the web's tool does.
//! The research tools live in [`super::research`].

use super::dispatch::{self, sdk_post};
use super::envelope::{self, error, fail, write_result, Output, DEFAULT_VERIFY_WITH};
use super::research;
use super::ToolDef;
use crate::db::sqlite::market_calendar as cal;
use crate::services::market_calendar_service as calendar;
use crate::state::AppState;
use chrono::{Datelike, NaiveDate, Weekday};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Default strategy for orders from MCP (`MCP_STRATEGY`).
pub const MCP_STRATEGY: &str = "python mcp";

/// OpenAlgo standardised index symbols (`NSE_INDEX_SYMBOLS`).
pub const NSE_INDEX_SYMBOLS: &[&str] = &[
    "NIFTY",
    "NIFTYNXT50",
    "FINNIFTY",
    "BANKNIFTY",
    "MIDCPNIFTY",
    "INDIAVIX",
    "HANGSENGBEESNAV",
    "NIFTY100",
    "NIFTY200",
    "NIFTY500",
    "NIFTYALPHA50",
    "NIFTYAUTO",
    "NIFTYCOMMODITIES",
    "NIFTYCONSUMPTION",
    "NIFTYCPSE",
    "NIFTYDIVOPPS50",
    "NIFTYENERGY",
    "NIFTYFMCG",
    "NIFTYGROWSECT15",
    "NIFTYGS10YR",
    "NIFTYGS10YRCLN",
    "NIFTYGS1115YR",
    "NIFTYGS15YRPLUS",
    "NIFTYGS48YR",
    "NIFTYGS813YR",
    "NIFTYGSCOMPSITE",
    "NIFTYINFRA",
    "NIFTYIT",
    "NIFTYMEDIA",
    "NIFTYMETAL",
    "NIFTYMIDLIQ15",
    "NIFTYMIDCAP100",
    "NIFTYMIDCAP150",
    "NIFTYMIDCAP50",
    "NIFTYMIDSML400",
    "NIFTYMNC",
    "NIFTYPHARMA",
    "NIFTYPSE",
    "NIFTYPSUBANK",
    "NIFTYPVTBANK",
    "NIFTYREALTY",
    "NIFTYSERVSECTOR",
    "NIFTYSMLCAP100",
    "NIFTYSMLCAP250",
    "NIFTYSMLCAP50",
    "NIFTY100EQLWGT",
    "NIFTY100LIQ15",
    "NIFTY100LOWVOL30",
    "NIFTY100QUALTY30",
    "NIFTY200QUALTY30",
    "NIFTY50DIVPOINT",
    "NIFTY50EQLWGT",
    "NIFTY50PR1XINV",
    "NIFTY50PR2XLEV",
    "NIFTY50TR1XINV",
    "NIFTY50TR2XLEV",
    "NIFTY50VALUE20",
];

/// `BSE_INDEX_SYMBOLS`.
pub const BSE_INDEX_SYMBOLS: &[&str] = &[
    "SENSEX",
    "BANKEX",
    "SENSEX50",
    "BSE100",
    "BSE150MIDCAPINDEX",
    "BSE200",
    "BSE250LARGEMIDCAPINDEX",
    "BSE400MIDSMALLCAPINDEX",
    "BSE500",
    "BSEAUTO",
    "BSECAPITALGOODS",
    "BSECARBONEX",
    "BSECONSUMERDURABLES",
    "BSECPSE",
    "BSEDOLLEX100",
    "BSEDOLLEX200",
    "BSEDOLLEX30",
    "BSEENERGY",
    "BSEFASTMOVINGCONSUMERGOODS",
    "BSEFINANCIALSERVICES",
    "BSEGREENEX",
    "BSEHEALTHCARE",
    "BSEINDIAINFRASTRUCTUREINDEX",
    "BSEINDUSTRIALS",
    "BSEINFORMATIONTECHNOLOGY",
    "BSEIPO",
    "BSELARGECAP",
    "BSEMETAL",
    "BSEMIDCAP",
    "BSEMIDCAPSELECTINDEX",
    "BSEOIL&GAS",
    "BSEPOWER",
    "BSEPSU",
    "BSEREALTY",
    "BSESENSEXNEXT50",
    "BSESMALLCAP",
    "BSESMALLCAPSELECTINDEX",
    "BSESMEIPO",
    "BSETECK",
    "BSETELECOM",
];

/// Exchanges the holiday check accepts (web `SUPPORTED_EXCHANGES`).
const CALENDAR_EXCHANGES: &[&str] = &[
    "NSE", "BSE", "NFO", "BFO", "MCX", "BCD", "CDS", "NCO", "CRYPTO",
];

type Args = Map<String, Value>;

fn present<'a>(a: &'a Args, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null())
}

/// A string argument as given.
fn st(a: &Args, k: &str) -> String {
    match present(a, k) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn up(a: &Args, k: &str) -> String {
    st(a, k).to_uppercase()
}

/// Python `str(value)` of an argument (numbers keep their JSON spelling).
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

fn payload(pairs: &[(&str, Value)]) -> Args {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// The SDK's `**kwargs` loop: each non-None value as `str(value)`.
fn kwargs_str(p: &mut Args, a: &Args, keys: &[&str]) {
    for k in keys {
        if let Some(v) = present(a, k) {
            p.insert((*k).into(), json!(py_str(v)));
        }
    }
}

/// Run one tool with bound arguments; the result is the text the web's tool
/// returns, inside the trust envelope.
pub async fn call(ctx: &Arc<AppState>, tool: &ToolDef, a: &Args) -> String {
    let out = run(ctx, tool.name, a).await;
    envelope::envelope(tool, out)
}

async fn run(ctx: &Arc<AppState>, name: &str, a: &Args) -> Output {
    let v = DEFAULT_VERIFY_WITH;
    let strategy = || json!(st(a, "strategy"));
    match name {
        // ---- orders ----
        "place_order" => {
            let mut p = payload(&[
                ("strategy", strategy()),
                ("symbol", json!(up(a, "symbol"))),
                ("action", json!(up(a, "action"))),
                ("exchange", json!(up(a, "exchange"))),
                ("pricetype", json!(up(a, "price_type"))),
                ("product", json!(up(a, "product"))),
                ("quantity", json!(st(a, "quantity"))),
            ]);
            kwargs_str(&mut p, a, &["price", "trigger_price", "disclosed_quantity"]);
            write_result(sdk_post(ctx, "placeorder", p).await, "placing order", v).into()
        }
        "place_smart_order" => {
            let mut p = payload(&[
                ("strategy", strategy()),
                ("symbol", json!(up(a, "symbol"))),
                ("action", json!(up(a, "action"))),
                ("exchange", json!(up(a, "exchange"))),
                ("pricetype", json!(up(a, "price_type"))),
                ("product", json!(up(a, "product"))),
                ("quantity", json!(st(a, "quantity"))),
                ("position_size", json!(st(a, "position_size"))),
            ]);
            kwargs_str(&mut p, a, &["price", "trigger_price", "disclosed_quantity"]);
            write_result(
                sdk_post(ctx, "placesmartorder", p).await,
                "placing smart order",
                v,
            )
            .into()
        }
        "place_basket_order" => {
            let orders: Vec<Value> = present(a, "orders")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|o| match o {
                    Value::Object(m) => Value::Object(
                        m.into_iter()
                            .map(|(k, x)| {
                                let x = match x {
                                    Value::Number(_) | Value::Bool(_) => json!(py_str(&x)),
                                    other => other,
                                };
                                (k, x)
                            })
                            .collect(),
                    ),
                    other => other,
                })
                .collect();
            let p = payload(&[("strategy", strategy()), ("orders", json!(orders))]);
            write_result(
                sdk_post(ctx, "basketorder", p).await,
                "placing basket order",
                v,
            )
            .into()
        }
        "place_split_order" => {
            let mut p = payload(&[
                ("strategy", strategy()),
                ("symbol", json!(up(a, "symbol"))),
                ("action", json!(up(a, "action"))),
                ("exchange", json!(up(a, "exchange"))),
                ("quantity", json!(st(a, "quantity"))),
                ("splitsize", json!(st(a, "split_size"))),
                ("pricetype", json!(up(a, "price_type"))),
                ("product", json!(up(a, "product"))),
            ]);
            kwargs_str(&mut p, a, &["price", "trigger_price", "disclosed_quantity"]);
            write_result(
                sdk_post(ctx, "splitorder", p).await,
                "placing split order",
                v,
            )
            .into()
        }
        "place_options_order" => {
            let mut p = payload(&[
                ("strategy", strategy()),
                ("underlying", json!(up(a, "underlying"))),
                ("exchange", json!(up(a, "exchange"))),
                ("offset", json!(up(a, "offset"))),
                ("option_type", json!(up(a, "option_type"))),
                ("action", json!(up(a, "action"))),
                ("quantity", json!(st(a, "quantity"))),
                ("pricetype", json!(up(a, "price_type"))),
                ("product", json!(up(a, "product"))),
            ]);
            if let Some(e) = present(a, "expiry_date") {
                p.insert("expiry_date".into(), e.clone());
            }
            kwargs_str(&mut p, a, &["price", "trigger_price", "disclosed_quantity"]);
            write_result(
                sdk_post(ctx, "optionsorder", p).await,
                "placing options order",
                v,
            )
            .into()
        }
        "place_options_multi_order" => match multi_legs(a) {
            Ok(legs) => {
                let mut p = payload(&[
                    ("strategy", strategy()),
                    ("underlying", json!(up(a, "underlying"))),
                    ("exchange", json!(up(a, "exchange"))),
                    ("legs", Value::Array(legs)),
                ]);
                if let Some(e) = present(a, "expiry_date") {
                    p.insert("expiry_date".into(), e.clone());
                }
                write_result(
                    sdk_post(ctx, "optionsmultiorder", p).await,
                    "placing multi-leg options order",
                    v,
                )
                .into()
            }
            Err((msg, kind)) => fail("placing options multi order", &msg, kind).into(),
        },
        "modify_order" => {
            let p = payload(&[
                ("orderid", json!(st(a, "order_id"))),
                ("strategy", strategy()),
                ("symbol", json!(up(a, "symbol"))),
                ("action", json!(up(a, "action"))),
                ("exchange", json!(up(a, "exchange"))),
                ("pricetype", json!(up(a, "price_type"))),
                ("product", json!(up(a, "product"))),
                ("quantity", json!(st(a, "quantity"))),
                ("price", json!(st(a, "price"))),
                ("disclosed_quantity", json!(st(a, "disclosed_quantity"))),
                ("trigger_price", json!(st(a, "trigger_price"))),
            ]);
            write_result(sdk_post(ctx, "modifyorder", p).await, "modifying order", v).into()
        }
        "cancel_order" => {
            let p = payload(&[
                ("orderid", json!(st(a, "order_id"))),
                ("strategy", strategy()),
            ]);
            write_result(sdk_post(ctx, "cancelorder", p).await, "cancelling order", v).into()
        }
        "cancel_all_orders" => {
            let p = payload(&[("strategy", strategy())]);
            write_result(
                sdk_post(ctx, "cancelallorder", p).await,
                "cancelling all orders",
                v,
            )
            .into()
        }
        "close_all_positions" => {
            let p = payload(&[("strategy", strategy())]);
            write_result(
                sdk_post(ctx, "closeposition", p).await,
                "closing positions",
                v,
            )
            .into()
        }
        "analyzer_toggle" => {
            let mode = present(a, "mode").cloned().unwrap_or(json!(false));
            write_result(
                sdk_post(ctx, "analyzer/toggle", payload(&[("mode", mode)])).await,
                "toggling analyzer mode",
                "analyzer_status",
            )
            .into()
        }
        // ---- account ----
        "get_open_position" => sdk_post(
            ctx,
            "openposition",
            payload(&[
                ("strategy", strategy()),
                ("symbol", json!(up(a, "symbol"))),
                ("exchange", json!(up(a, "exchange"))),
                ("product", json!(up(a, "product"))),
            ]),
        )
        .await
        .into(),
        "get_order_status" => sdk_post(
            ctx,
            "orderstatus",
            payload(&[
                ("strategy", strategy()),
                ("orderid", json!(st(a, "order_id"))),
            ]),
        )
        .await
        .into(),
        "get_order_book" => sdk_post(ctx, "orderbook", Map::new()).await.into(),
        "get_trade_book" => sdk_post(ctx, "tradebook", Map::new()).await.into(),
        "get_position_book" => sdk_post(ctx, "positionbook", Map::new()).await.into(),
        "get_holdings" => sdk_post(ctx, "holdings", Map::new()).await.into(),
        "get_funds" => sdk_post(ctx, "funds", Map::new()).await.into(),
        "analyzer_status" => sdk_post(ctx, "analyzer", Map::new()).await.into(),
        "calculate_margin" => {
            let positions: Vec<Value> = present(a, "positions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|pos| match pos {
                    Value::Object(mut m) => {
                        for k in ["quantity", "price", "trigger_price"] {
                            if let Some(x) = m.get(k).filter(|x| !x.is_null()).cloned() {
                                m.insert(k.into(), json!(py_str(&x)));
                            }
                        }
                        m.entry("price").or_insert(json!("0"));
                        m.entry("trigger_price").or_insert(json!("0"));
                        Value::Object(m)
                    }
                    other => other,
                })
                .collect();
            sdk_post(ctx, "margin", payload(&[("positions", json!(positions))]))
                .await
                .into()
        }
        "send_telegram_alert" => {
            let p = payload(&[
                ("username", json!(st(a, "username"))),
                (
                    "message",
                    present(a, "message").cloned().unwrap_or(json!("")),
                ),
                (
                    "priority",
                    present(a, "priority").cloned().unwrap_or(json!(5)),
                ),
            ]);
            write_result(
                sdk_post(ctx, "telegram/notify", p).await,
                "sending telegram alert",
                "the Telegram chat itself (the alert may already have been delivered)",
            )
            .into()
        }
        // ---- market data ----
        "get_quote" | "get_market_depth" => {
            let endpoint = if name == "get_quote" {
                "quotes"
            } else {
                "depth"
            };
            sdk_post(
                ctx,
                endpoint,
                payload(&[
                    ("symbol", json!(up(a, "symbol"))),
                    ("exchange", json!(up(a, "exchange"))),
                ]),
            )
            .await
            .into()
        }
        "get_multi_quotes" => {
            let mut list = Vec::new();
            for s in present(a, "symbols")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let field = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_uppercase);
                match (field("symbol"), field("exchange")) {
                    (Some(sym), Some(ex)) => list.push(json!({"symbol": sym, "exchange": ex})),
                    (None, _) => {
                        return fail("getting multi quotes", "'symbol'", "KeyError").into()
                    }
                    (_, None) => {
                        return fail("getting multi quotes", "'exchange'", "KeyError").into()
                    }
                }
            }
            sdk_post(ctx, "multiquotes", payload(&[("symbols", json!(list))]))
                .await
                .into()
        }
        "get_option_chain" => {
            let mut p = payload(&[
                ("underlying", json!(up(a, "underlying"))),
                ("exchange", json!(up(a, "exchange"))),
            ]);
            if present(a, "expiry_date").is_some() {
                p.insert("expiry_date".into(), json!(up(a, "expiry_date")));
            }
            if let Some(n) = present(a, "strike_count") {
                p.insert("strike_count".into(), n.clone());
            }
            sdk_post(ctx, "optionchain", p).await.into()
        }
        "search_instruments" => {
            let mut exchange = present(a, "exchange").map(|_| st(a, "exchange"));
            if up(a, "instrument_type") == "INDEX" {
                if let Some(e) = exchange.as_deref() {
                    match e.to_uppercase().as_str() {
                        "NSE" => exchange = Some("NSE_INDEX".into()),
                        "BSE" => exchange = Some("BSE_INDEX".into()),
                        _ => {}
                    }
                }
            }
            let mut p = payload(&[("query", present(a, "query").cloned().unwrap_or(json!("")))]);
            if let Some(e) = exchange {
                p.insert("exchange".into(), json!(e.to_uppercase()));
            }
            sdk_post(ctx, "search", p).await.into()
        }
        "get_symbol_info" => {
            let symbol = up(a, "symbol");
            let mut exchange = st(a, "exchange");
            if up(a, "instrument_type") == "INDEX" {
                match exchange.to_uppercase().as_str() {
                    "NSE" => exchange = "NSE_INDEX".into(),
                    "BSE" => exchange = "BSE_INDEX".into(),
                    _ => {}
                }
            }
            if NSE_INDEX_SYMBOLS.contains(&symbol.as_str()) && exchange.to_uppercase() == "NSE" {
                exchange = "NSE_INDEX".into();
            } else if BSE_INDEX_SYMBOLS.contains(&symbol.as_str())
                && exchange.to_uppercase() == "BSE"
            {
                exchange = "BSE_INDEX".into();
            }
            sdk_post(
                ctx,
                "symbol",
                payload(&[
                    ("symbol", json!(symbol)),
                    ("exchange", json!(exchange.to_uppercase())),
                ]),
            )
            .await
            .into()
        }
        "get_index_symbols" => {
            let ex = st(a, "exchange");
            match ex.to_uppercase().as_str() {
                "NSE" => json!({"exchange": "NSE", "exchange_code": "NSE_INDEX", "indices": NSE_INDEX_SYMBOLS}).into(),
                "BSE" => json!({"exchange": "BSE", "exchange_code": "BSE_INDEX", "indices": BSE_INDEX_SYMBOLS}).into(),
                _ => error(format!("Unknown exchange: {}. Use NSE or BSE.", ex), &[]).into(),
            }
        }
        "get_expiry_dates" => sdk_post(
            ctx,
            "expiry",
            payload(&[
                ("symbol", json!(up(a, "symbol"))),
                ("exchange", json!(up(a, "exchange"))),
                (
                    "instrumenttype",
                    json!(st(a, "instrument_type").to_lowercase()),
                ),
            ]),
        )
        .await
        .into(),
        "get_available_intervals" => sdk_post(ctx, "intervals", Map::new()).await.into(),
        "get_option_symbol" => {
            let mut p = payload(&[
                ("underlying", json!(up(a, "underlying"))),
                ("exchange", json!(up(a, "exchange"))),
                ("offset", json!(up(a, "offset"))),
                ("option_type", json!(up(a, "option_type"))),
            ]);
            if let Some(e) = present(a, "expiry_date") {
                p.insert("expiry_date".into(), e.clone());
            }
            sdk_post(ctx, "optionsymbol", p).await.into()
        }
        "get_synthetic_future" => sdk_post(
            ctx,
            "syntheticfuture",
            payload(&[
                ("underlying", json!(up(a, "underlying"))),
                ("exchange", json!(up(a, "exchange"))),
                (
                    "expiry_date",
                    present(a, "expiry_date").cloned().unwrap_or(Value::Null),
                ),
            ]),
        )
        .await
        .into(),
        "get_option_greeks" => {
            let mut p = payload(&[
                ("symbol", json!(up(a, "symbol"))),
                ("exchange", json!(up(a, "exchange"))),
            ]);
            for k in ["interest_rate", "forward_price", "expiry_time"] {
                if let Some(x) = present(a, k) {
                    p.insert(k.into(), x.clone());
                }
            }
            for k in ["underlying_symbol", "underlying_exchange"] {
                if present(a, k).is_some() {
                    p.insert(k.into(), json!(up(a, k)));
                }
            }
            sdk_post(ctx, "optiongreeks", p).await.into()
        }
        "get_holidays" => {
            let mut p = Map::new();
            if let Some(y) = present(a, "year") {
                p.insert("year".into(), y.clone());
            }
            sdk_post(ctx, "market/holidays", p).await.into()
        }
        "get_timings" => {
            let date = match present(a, "date") {
                Some(d) => d.clone(),
                None => json!(calendar::today_ist(ctx.now())
                    .format("%Y-%m-%d")
                    .to_string()),
            };
            sdk_post(ctx, "market/timings", payload(&[("date", date)]))
                .await
                .into()
        }
        "check_holiday" => check_holiday(
            ctx,
            &st(a, "date"),
            present(a, "exchange").map(|_| st(a, "exchange")),
        )
        .into(),
        "get_instruments" => instruments(ctx, a).await.into(),
        // ---- utility ----
        "get_openalgo_version" => {
            Output::Text(format!("OpenAlgo version: {}", env!("CARGO_PKG_VERSION")))
        }
        "validate_order_constants" => json!({
            "exchanges": {
                "NSE": "NSE Equity",
                "NFO": "NSE Futures & Options",
                "CDS": "NSE Currency",
                "BSE": "BSE Equity",
                "BFO": "BSE Futures & Options",
                "BCD": "BSE Currency",
                "MCX": "MCX Commodity",
                "NCDEX": "NCDEX Commodity",
            },
            "product_types": {
                "CNC": "Cash & Carry for equity",
                "NRML": "Normal for futures and options",
                "MIS": "Intraday Square off",
            },
            "price_types": {
                "MARKET": "Market Order",
                "LIMIT": "Limit Order",
                "SL": "Stop Loss Limit Order",
                "SL-M": "Stop Loss Market Order",
            },
            "actions": {"BUY": "Buy", "SELL": "Sell"},
            "intervals": ["1m", "3m", "5m", "10m", "15m", "30m", "1h", "D"],
        })
        .into(),
        // ---- research ----
        "get_historical_data"
        | "calculate_indicator"
        | "get_trend_snapshot"
        | "get_momentum_snapshot"
        | "get_volatility_snapshot"
        | "get_support_resistance"
        | "detect_signals"
        | "screen_instruments"
        | "multi_timeframe_analysis"
        | "correlation_beta" => research::run(ctx, name, a).await.into(),
        other => error(format!("unknown tool '{}'", other), &[]).into(),
    }
}

/// The SDK's `optionsmultiorder` leg processing (`int()` / `float()` of the
/// numeric fields; unknown keys dropped). Errors as Python would raise them.
fn multi_legs(a: &Args) -> Result<Vec<Value>, (String, &'static str)> {
    let legs = present(a, "legs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let to_int = |v: &Value| -> Result<i64, (String, &'static str)> {
        match v {
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
                .ok_or_else(|| (format!("invalid literal for int() with base 10: '{}'", n), "ValueError")),
            Value::String(s) => s
                .trim()
                .parse()
                .map_err(|_| (format!("invalid literal for int() with base 10: '{}'", s), "ValueError")),
            other => Err((
                format!("int() argument must be a string, a bytes-like object or a real number, not '{}'", kind(other)),
                "TypeError",
            )),
        }
    };
    let to_float = |v: &Value| -> Result<f64, (String, &'static str)> {
        match v {
            Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => s.trim().parse().map_err(|_| {
                (
                    format!("could not convert string to float: '{}'", s),
                    "ValueError",
                )
            }),
            other => Err((
                format!(
                    "float() argument must be a string or a real number, not '{}'",
                    kind(other)
                ),
                "TypeError",
            )),
        }
    };
    let mut out = Vec::new();
    for leg in legs {
        let get = |k: &str| {
            leg.get(k)
                .cloned()
                .ok_or_else(|| (format!("'{}'", k), "KeyError"))
        };
        let mut m = Map::new();
        m.insert("offset".into(), get("offset")?);
        m.insert("option_type".into(), get("option_type")?);
        m.insert("action".into(), get("action")?);
        m.insert("quantity".into(), json!(to_int(&get("quantity")?)?));
        for k in ["expiry_date", "pricetype", "product"] {
            if let Some(v) = leg.get(k) {
                m.insert(k.into(), v.clone());
            }
        }
        for k in ["price", "trigger_price"] {
            if let Some(v) = leg.get(k) {
                m.insert(k.into(), json!(to_float(v)?));
            }
        }
        if let Some(v) = leg.get("disclosed_quantity") {
            m.insert("disclosed_quantity".into(), json!(to_int(v)?));
        }
        out.push(Value::Object(m));
    }
    Ok(out)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The web's tool posts to `/api/v1/checkholiday`, a route the web never
/// shipped, so it answers 404 there. The desktop answers with the web's
/// `market_calendar_service.check_holiday` reply, which is what the tool's
/// description documents.
fn check_holiday(ctx: &AppState, date: &str, exchange: Option<String>) -> Value {
    let Ok(d) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return json!({"status": "error", "message": "Invalid date format. Use YYYY-MM-DD"});
    };
    if !calendar::supported_date(d) {
        return json!({"status": "error", "message": "Date must be between 2020-01-01 and 2050-12-31"});
    }
    let ex = exchange.filter(|e| !e.is_empty()).map(|e| e.to_uppercase());
    if let Some(e) = &ex {
        if !CALENDAR_EXCHANGES.contains(&e.as_str()) {
            return json!({
                "status": "error",
                "message": format!("Exchange must be one of: {}", CALENDAR_EXCHANGES.join(", ")),
            });
        }
    }
    let weekend = matches!(d.weekday(), Weekday::Sat | Weekday::Sun);
    let is_holiday = if ex.as_deref() == Some("CRYPTO") {
        false
    } else {
        let found = ctx
            .sqlite
            .conn()
            .and_then(|c| cal::holidays_by_year(&c, d.year()))
            .map(|list| {
                let key = d.format("%Y-%m-%d").to_string();
                list.into_iter().find(|h| h.date == key)
            });
        match found {
            Ok(Some(h)) if h.holiday_type == "SPECIAL_SESSION" => false,
            Ok(_) if weekend => true,
            Ok(None) => false,
            Ok(Some(h)) => match &ex {
                Some(e) => {
                    if h.open.iter().any(|w| &w.exchange == e) {
                        false
                    } else {
                        h.closed.iter().any(|c| c == e)
                    }
                }
                None => true,
            },
            Err(err) => {
                tracing::debug!("Holiday check fell back to the weekend rule: {}", err);
                weekend
            }
        }
    };
    json!({
        "status": "success",
        "data": {
            "date": date,
            "exchange": ex.unwrap_or_else(|| "ALL".into()),
            "is_holiday": is_holiday,
        },
    })
}

/// `get_instruments`: the SDK's `instruments()` (every exchange in turn
/// when none is given) trimmed to `limit` rows.
async fn instruments(ctx: &Arc<AppState>, a: &Args) -> Value {
    let limit = present(a, "limit").and_then(Value::as_i64).unwrap_or(500);
    let exchange = present(a, "exchange").map(|_| up(a, "exchange"));
    let exchanges: Vec<String> = match &exchange {
        Some(e) => vec![e.clone()],
        None => [
            "NSE",
            "BSE",
            "NFO",
            "BFO",
            "MCX",
            "CDS",
            "BCD",
            "NSE_INDEX",
            "BSE_INDEX",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };
    let mut rows: Vec<Value> = Vec::new();
    for ex in &exchanges {
        let reply = match dispatch::get_raw(ctx, "instruments", &[("exchange", ex.as_str())]).await
        {
            Ok(raw) => dispatch::shape(&raw),
            Err(t) => dispatch::transport_reply(t),
        };
        let ok = reply.get("status").and_then(Value::as_str) == Some("success");
        match reply.get("data").and_then(Value::as_array) {
            Some(data) if ok => rows.extend(data.iter().cloned()),
            _ if exchange.is_some() => return reply,
            _ => {}
        }
    }
    if rows.is_empty() {
        return if exchange.is_some() {
            json!({
                "status": "error",
                "message": "No instruments available for the specified exchange",
                "error_type": "no_data",
            })
        } else {
            json!({
                "status": "error",
                "message": "Failed to fetch instruments from any exchange",
                "error_type": "no_data",
            })
        };
    }
    let total = rows.len() as i64;
    // `df.head(limit)`: a negative limit drops that many rows from the end.
    let take = if limit >= 0 {
        limit.min(total)
    } else {
        (total + limit).max(0)
    } as usize;
    rows.truncate(take);
    json!({
        "exchange": exchange.unwrap_or_else(|| "ALL".into()),
        "count": total,
        "returned": rows.len(),
        "truncated": total > limit,
        "limit": limit,
        "data": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_string_conversion() {
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(100.5)), "100.5");
        assert_eq!(py_str(&json!(100.0)), "100.0");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!("7")), "7");
    }

    #[test]
    fn multi_leg_processing_matches_the_sdk() {
        let a = json!({"legs": [{"offset": "ATM", "option_type": "CE", "action": "BUY",
            "quantity": "75", "price": "250", "extra": 1}]});
        let legs = multi_legs(a.as_object().unwrap()).unwrap();
        assert_eq!(
            legs[0],
            json!({"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 75, "price": 250.0})
        );
        let bad = json!({"legs": [{"option_type": "CE", "action": "BUY", "quantity": 75}]});
        assert_eq!(
            multi_legs(bad.as_object().unwrap()).unwrap_err(),
            ("'offset'".to_string(), "KeyError")
        );
    }
}
