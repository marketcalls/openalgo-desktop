//! TradeSmart's master contract and history (marketcalls/openalgo#2475,
//! 9a12bd0f7). The web commit adds no test of its own; these cases are the
//! examples its commit messages give (QA MC-03, MC-04, MC-11, HS-07), on
//! the synthetic scrip-master rows in `tests/fixtures/brokers/tradesmart/`
//! (no account data). Each runs through the adapter's public paths
//! (`parse_file`, `Broker::get_history`, `Broker::on_logout`), and the
//! other Noren members are held to the rules they had.

use super::config;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::families::noren::master_contract::parse_file;
use crate::brokers::families::noren::{NorenBroker, NorenConfig, NorenEndpoints};
use crate::brokers::types::{AuthToken, HistoryRequest, QuoteKey};
use crate::brokers::{flattrade, shoonya, zebu, Broker};
use axum::http::StatusCode;
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../tests/fixtures/brokers/tradesmart/",
            $name
        ))
    };
}

fn row<'a>(rows: &'a [SymToken], exchange: &str, symbol: &str) -> &'a SymToken {
    rows.iter()
        .find(|r| r.exchange == exchange && r.symbol == symbol)
        .unwrap_or_else(|| panic!("no {exchange}:{symbol} in {rows:?}"))
}

/// Every master row TradeSmart's files give, as the adapter stores them.
fn tradesmart_master() -> Vec<SymToken> {
    let cfg = config();
    let mut rows = parse_file(cfg, "NSE", fixture!("NSE_symbols.txt"));
    rows.extend(parse_file(cfg, "BSE", fixture!("BSE_symbols.txt")));
    rows.extend(parse_file(cfg, "BFO", fixture!("BFO_symbols.txt")));
    rows
}

/// QA MC-04 allows only EQ, FUT, CE and PE. The Noren `Instrument` column
/// on NSE holds the series (BE, GB, N0, SG), which was stored as the type;
/// index rows were `INDEX`. Every NSE row is EQ now, and the index rows
/// are EQ on NSE_INDEX, as Kite types them.
#[test]
fn every_nse_row_is_eq_and_index_rows_are_eq_on_the_index_exchange() {
    let rows = parse_file(config(), "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(rows.len(), 14);
    for r in &rows {
        assert_eq!(r.instrument_type, "EQ", "{}:{}", r.exchange, r.symbol);
    }
    for (symbol, series) in [
        ("YESBANK", "YESBANK-BE"),
        ("SGBJUN31I-GB", "SGBJUN31I-GB"),
        ("NHAI-N0", "NHAI-N0"),
        ("735GS2033-SG", "735GS2033-SG"),
    ] {
        assert_eq!(row(&rows, "NSE", symbol).brsymbol, series);
    }
    for (symbol, tsym, token) in [
        ("NIFTY", "NIFTY INDEX", "26000"),
        ("BANKNIFTY", "NIFTY BANK", "26009"),
        ("FINNIFTY", "FINNIFTY", "26037"),
        ("INDIAVIX", "INDIA VIX", "26017"),
    ] {
        let index = row(&rows, "NSE_INDEX", symbol);
        assert_eq!(
            (index.brsymbol.as_str(), index.brexchange.as_str()),
            (tsym, "NSE_INDEX")
        );
        assert_eq!(index.token, token);
    }
}

/// QA MC-03: TradeSmart lists four BSE rows (WEAL, MFLS2, MFLS3, INVE)
/// with the trading symbol `12:` and tick size 0. They cannot be traded or
/// addressed by symbol and were stored as duplicates; they are skipped.
/// The manual SENSEX and BANKEX rows are EQ on BSE_INDEX.
#[test]
fn bse_rows_without_a_tick_are_skipped_and_the_bse_indices_are_eq() {
    let rows = parse_file(config(), "BSE", fixture!("BSE_symbols.txt"));
    assert!(
        rows.iter().all(|r| r.brsymbol != "12:"),
        "placeholder rows stored: {rows:?}"
    );
    assert_eq!(rows.len(), 6, "four equities and two indices");
    for symbol in ["RELIANCE", "BAJAJ-AUTO", "MRF", "IDEA"] {
        assert_eq!(row(&rows, "BSE", symbol).instrument_type, "EQ");
    }
    for (symbol, token) in [("SENSEX", "1"), ("BANKEX", "12")] {
        let index = row(&rows, "BSE_INDEX", symbol);
        assert_eq!(
            (index.token.as_str(), index.instrument_type.as_str()),
            (token, "EQ")
        );
    }
}

/// QA MC-03: the BFO `Symbol` column holds codes (BSXOPT, SX50FUT), and a
/// letters-only match on the trading symbol cut SENSEX50 to SENSEX (its
/// futures collided with SENSEX's), M&M to M, BAJAJ-AUTO to BAJAJ, and
/// NAM-INDIA, GVT&D and 360ONE likewise. The underlying is the trading
/// symbol less its expiry, strike and type, weekly symbols included.
#[test]
fn bfo_underlying_is_the_trading_symbol_less_its_contract() {
    let rows = parse_file(config(), "BFO", fixture!("BFO_symbols.txt"));
    assert_eq!(rows.len(), 10);
    for (tsym, symbol, name, kind) in [
        ("SENSEX5026NOVFUT", "SENSEX5026NOV26FUT", "SENSEX50", "FUT"),
        ("SENSEX26NOVFUT", "SENSEX26NOV26FUT", "SENSEX", "FUT"),
        ("SENSEX26D0389500PE", "SENSEX03DEC2689500PE", "SENSEX", "PE"),
        ("SENSEX26NOV85000CE", "SENSEX26NOV2685000CE", "SENSEX", "CE"),
        ("BANKEX26NOV26000CE", "BANKEX26NOV2626000CE", "BANKEX", "CE"),
        ("M&M26NOVFUT", "M&M26NOV26FUT", "M&M", "FUT"),
        (
            "BAJAJ-AUTO26NOV9000CE",
            "BAJAJ-AUTO26NOV269000CE",
            "BAJAJ-AUTO",
            "CE",
        ),
        (
            "NAM-INDIA26NOVFUT",
            "NAM-INDIA26NOV26FUT",
            "NAM-INDIA",
            "FUT",
        ),
        ("GVT&D26NOV2500PE", "GVT&D26NOV262500PE", "GVT&D", "PE"),
        (
            "360ONE26NOV1012.5CE",
            "360ONE26NOV261012.5CE",
            "360ONE",
            "CE",
        ),
    ] {
        let r = row(&rows, "BFO", symbol);
        assert_eq!(
            (
                r.brsymbol.as_str(),
                r.name.as_str(),
                r.instrument_type.as_str()
            ),
            (tsym, name, kind)
        );
    }
    assert_eq!(
        row(&rows, "BFO", "SENSEX03DEC2689500PE").expiry,
        "03-DEC-26"
    );
    assert_eq!(row(&rows, "BFO", "360ONE26NOV261012.5CE").strike, 1012.5);
}

/// QA MC-03: no (symbol, exchange) pair twice in the whole master.
#[test]
fn the_master_holds_each_symbol_once() {
    let rows = tradesmart_master();
    let mut seen = HashSet::new();
    for r in &rows {
        assert!(
            seen.insert((r.exchange.as_str(), r.symbol.as_str())),
            "{}:{} stored twice",
            r.exchange,
            r.symbol
        );
    }
}

/// The other members keep the rules their own web code has: on the same
/// files Shoonya stores the series as the type, keeps the `12:` rows,
/// reads the BFO underlying from the leading letters (SENSEX50 reads
/// SENSEX, M&M reads M) and types its indices INDEX.
#[test]
fn other_noren_members_keep_their_master_rules() {
    let cfg = shoonya::config();
    let nse = parse_file(cfg, "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(row(&nse, "NSE", "SGBJUN31I-GB").instrument_type, "GB");
    assert_eq!(row(&nse, "NSE_INDEX", "NIFTY").instrument_type, "INDEX");
    let bse = parse_file(cfg, "BSE", fixture!("BSE_symbols.txt"));
    assert_eq!(bse.iter().filter(|r| r.brsymbol == "12:").count(), 4);
    assert_eq!(row(&bse, "BSE_INDEX", "SENSEX").instrument_type, "INDEX");
    let bfo = parse_file(cfg, "BFO", fixture!("BFO_symbols.txt"));
    assert_eq!(row(&bfo, "BFO", "M26NOV26FUT").brsymbol, "M&M26NOVFUT");
    let sensex_futures = bfo
        .iter()
        .filter(|r| r.symbol == "SENSEX26NOV26FUT")
        .count();
    assert_eq!(sensex_futures, 2, "SENSEX50 reads SENSEX on Shoonya");
}

/// QA TS-01/04/05/06, MC-11: TradeSmart's NSE and BSE files give the tick
/// size in rupees (RELIANCE 0.1, BAJAJ-AUTO 1, MRF 5), and dividing it as
/// paise stored RELIANCE at 0.001 and MRF at 0.05. Every file is stored as
/// sent, derivatives as before; Shoonya's paise rule is unchanged.
#[test]
fn nse_and_bse_ticks_are_stored_in_rupees() {
    let rows = tradesmart_master();
    for (exchange, symbol, tick) in [
        ("NSE", "IDEA", 0.01),
        ("NSE", "RELIANCE", 0.1),
        ("NSE", "BAJAJ-AUTO", 1.0),
        ("NSE", "MRF", 5.0),
        ("NSE_INDEX", "NIFTY", 0.05),
        ("BSE", "RELIANCE", 0.05),
        ("BSE", "BAJAJ-AUTO", 1.0),
        ("BSE", "MRF", 5.0),
        ("BSE_INDEX", "SENSEX", 0.05),
        ("BFO", "M&M26NOV26FUT", 0.1),
        ("BFO", "SENSEX26NOV2685000CE", 0.05),
    ] {
        assert_eq!(
            row(&rows, exchange, symbol).tick_size,
            tick,
            "{exchange}:{symbol}"
        );
    }
    let shoonya = parse_file(shoonya::config(), "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(row(&shoonya, "NSE", "MRF").tick_size, 0.05);
}

// ---------------------------------------------------------------------------
// History, against a local fake TradeSmart on an ephemeral port
// ---------------------------------------------------------------------------

/// What the fake answers and what it was asked.
#[derive(Default)]
struct Fake {
    /// Every request: the endpoint's last path segment and its `jData`.
    calls: Mutex<Vec<(String, Value)>>,
    /// TPSeries answers the rows inside the asked range.
    tpseries: Mutex<Vec<Value>>,
    /// EODChartData answers these rows for these `sym` names, "no data"
    /// for any other.
    eod: Mutex<Vec<(&'static str, Vec<Value>)>>,
    /// GetSecurityInfo knows these index names by (exchange, token).
    index_names: Mutex<Vec<((&'static str, &'static str), &'static str)>>,
    /// GetSecurityInfo answers 502.
    security_info_down: AtomicBool,
    /// GetSecurityInfo says it was reached, then waits to be released.
    security_info_gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Fake {
    fn calls(&self, endpoint: &str) -> Vec<Value> {
        self.calls
            .lock()
            .iter()
            .filter(|(leaf, _)| leaf == endpoint)
            .map(|(_, jdata)| jdata.clone())
            .collect()
    }

    fn answer(&self, leaf: &str, jdata: &Value) -> Value {
        let no_data = json!({"stat":"Not_Ok","emsg":"Error Occurred : 5 \"no data\""});
        match leaf {
            "TPSeries" => {
                let at = |k: &str| jdata[k].as_str().and_then(|v| v.parse::<i64>().ok());
                let (Some(st), Some(et)) = (at("st"), at("et")) else {
                    return no_data;
                };
                let rows: Vec<Value> = self
                    .tpseries
                    .lock()
                    .iter()
                    .filter(|r| {
                        r["ssboe"]
                            .as_str()
                            .and_then(|v| v.parse().ok())
                            .is_some_and(|t: i64| (st..=et).contains(&t))
                    })
                    .cloned()
                    .collect();
                if rows.is_empty() {
                    no_data
                } else {
                    Value::Array(rows)
                }
            }
            "EODChartData" => {
                let sym = jdata["sym"].as_str().unwrap_or_default();
                match self.eod.lock().iter().find(|(name, _)| *name == sym) {
                    Some((_, rows)) => Value::Array(rows.clone()),
                    None => no_data,
                }
            }
            "GetSecurityInfo" => {
                let exch = jdata["exch"].as_str().unwrap_or_default();
                let token = jdata["token"].as_str().unwrap_or_default();
                let names = self.index_names.lock();
                match names.iter().find(|(at, _)| *at == (exch, token)) {
                    Some((_, tsym)) => {
                        json!({"stat":"Ok","exch":exch,"token":token,"tsym":tsym,"instname":"UNDIND"})
                    }
                    None => no_data,
                }
            }
            _ => no_data,
        }
    }

    /// GetSecurityInfo's gate and outage, before it answers.
    async fn before_security_info(&self) -> StatusCode {
        let gate = self.security_info_gate.lock().clone();
        if let Some((reached, release)) = gate {
            reached.notify_one();
            release.notified().await;
        }
        if self.security_info_down.load(Ordering::SeqCst) {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::OK
        }
    }
}

/// The `jData` of a Noren body, in either dialect.
fn jdata_of(body: &str) -> Value {
    body.strip_prefix("jData=")
        .map(|rest| rest.split_once("&jKey=").map_or(rest, |(j, _)| j))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(Value::Null)
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri, body: String| {
        let fake = fake.clone();
        async move {
            let leaf = uri
                .path()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let jdata = jdata_of(&body);
            fake.calls.lock().push((leaf.clone(), jdata.clone()));
            let status = if leaf == "GetSecurityInfo" {
                fake.before_security_info().await
            } else {
                StatusCode::OK
            };
            (status, axum::Json(fake.answer(&leaf, &jdata)))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    host
}

/// A member's adapter on TradeSmart's master, talking to `fake`.
async fn adapter(cfg: &'static NorenConfig, fake: &Arc<Fake>) -> NorenBroker {
    let host = serve(fake.clone()).await;
    let symbols = SymbolResolver::new();
    symbols.load(tradesmart_master());
    NorenBroker::with_endpoints(
        cfg,
        symbols,
        NorenEndpoints::rebased(cfg, &host, "ws://127.0.0.1:1"),
    )
}

fn auth() -> AuthToken {
    AuthToken::new("<USER_ID>:::sess-tok")
}

/// `YYYY-MM-DD HH:MM` IST as epoch seconds.
fn ist(text: &str) -> i64 {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
    Kolkata.from_local_datetime(&naive).unwrap().timestamp()
}

/// TPSeries rows as Noren sends them, from `(IST time, o, h, l, c, volume)`.
fn tpseries(bars: &[(&str, f64, f64, f64, f64, i64)]) -> Vec<Value> {
    bars.iter()
        .map(|&(t, o, h, l, c, v)| {
            json!({"stat":"Ok","ssboe": ist(t).to_string(), "into": o.to_string(),
                   "inth": h.to_string(), "intl": l.to_string(), "intc": c.to_string(),
                   "intv": v.to_string(), "oi": "0"})
        })
        .collect()
}

fn history(exchange: &str, symbol: &str, interval: &str) -> HistoryRequest {
    HistoryRequest {
        key: QuoteKey::new(exchange, symbol),
        interval: interval.into(),
        start: NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
    }
}

async fn bar_times(b: &NorenBroker, req: &HistoryRequest) -> Vec<i64> {
    b.get_history(&auth(), req)
        .await
        .unwrap()
        .iter()
        .map(|c| c.timestamp)
        .collect()
}

/// RELIANCE 18-11-2026 1m as TPSeries sends it: each date opens with the
/// pre-open discovered price at 09:14, flat with no volume.
fn reliance_1m() -> Vec<Value> {
    tpseries(&[
        ("2026-11-18 09:14", 1370.0, 1370.0, 1370.0, 1370.0, 0),
        ("2026-11-18 09:15", 1367.0, 1371.0, 1366.2, 1369.5, 182_340),
        ("2026-11-18 09:16", 1369.5, 1369.9, 1368.0, 1368.4, 96_210),
        ("2026-11-19 09:14", 1374.0, 1374.0, 1374.0, 1374.0, 0),
        ("2026-11-19 09:15", 1374.5, 1376.0, 1373.1, 1375.2, 150_002),
    ])
}

/// QA HS-07: every TradeSmart session opened at 09:14 on the pre-open bar.
/// Intraday history leaves it out on cash and index rows alike (an index
/// bar has no volume, but its first trading bar is not flat), so each
/// session opens at 09:15.
#[tokio::test]
async fn intraday_history_leaves_out_the_pre_open_bar_of_each_session() {
    let fake = Arc::new(Fake::default());
    let b = adapter(config(), &fake).await;
    *fake.tpseries.lock() = reliance_1m();
    let times = bar_times(&b, &history("NSE", "RELIANCE", "1m")).await;
    assert_eq!(
        times,
        vec![
            ist("2026-11-18 09:15"),
            ist("2026-11-18 09:16"),
            ist("2026-11-19 09:15")
        ]
    );
    let asked = fake.calls("TPSeries");
    assert_eq!(
        (asked[0]["token"].as_str(), asked[0]["intrv"].as_str()),
        (Some("2885"), Some("1"))
    );
    *fake.tpseries.lock() = tpseries(&[
        (
            "2026-11-18 09:14",
            25_903.1,
            25_903.1,
            25_903.1,
            25_903.1,
            0,
        ),
        (
            "2026-11-18 09:15",
            25_910.4,
            25_951.0,
            25_900.2,
            25_940.7,
            0,
        ),
        (
            "2026-11-18 09:16",
            25_940.7,
            25_948.3,
            25_930.0,
            25_935.1,
            0,
        ),
    ]);
    let times = bar_times(&b, &history("NSE_INDEX", "NIFTY", "1m")).await;
    assert_eq!(
        times,
        vec![ist("2026-11-18 09:15"), ist("2026-11-18 09:16")]
    );
}

/// Shoonya and Zebu keep every bar TPSeries sends: the web drops the
/// pre-open bar by its shape for TradeSmart only (Flattrade's pre-open
/// bars follow its own web rule).
#[tokio::test]
async fn other_noren_members_keep_the_bars_tpseries_sends() {
    for cfg in [shoonya::config(), zebu::config()] {
        let fake = Arc::new(Fake::default());
        let b = adapter(cfg, &fake).await;
        *fake.tpseries.lock() = reliance_1m();
        let times = bar_times(&b, &history("NSE", "RELIANCE", "1m")).await;
        assert_eq!(times.len(), 5, "{}", cfg.id);
        assert_eq!(times[0], ist("2026-11-18 09:14"), "{}", cfg.id);
    }
}

/// Daily rows as EODChartData sends them, from `(YYYY-MM-DD, o, h, l, c)`.
fn eod_rows(days: &[(&str, f64, f64, f64, f64)]) -> Vec<Value> {
    days.iter()
        .map(|&(day, o, h, l, c)| {
            let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
            let ssboe = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
            json!({"stat":"Ok","time": date.format("%d-%b-%Y").to_string().to_uppercase(),
                   "into": o.to_string(), "inth": h.to_string(), "intl": l.to_string(),
                   "intc": c.to_string(), "ssboe": ssboe.to_string(), "intv": "0"})
        })
        .collect()
}

/// A fake that knows NIFTY and SENSEX by the names GetSecurityInfo reports
/// (`Nifty 50` for the master's `NIFTY INDEX`) and serves daily candles
/// under those names, and under RELIANCE's trading symbol, only.
fn index_fake() -> Arc<Fake> {
    let fake = Arc::new(Fake::default());
    *fake.index_names.lock() = vec![(("NSE", "26000"), "Nifty 50"), (("BSE", "1"), "SENSEX")];
    *fake.eod.lock() = vec![
        (
            "NSE:Nifty 50",
            eod_rows(&[
                ("2026-11-18", 25_850.0, 26_010.5, 25_801.2, 25_940.7),
                ("2026-11-19", 25_940.7, 26_102.0, 25_911.3, 26_068.4),
            ]),
        ),
        (
            "BSE:SENSEX",
            eod_rows(&[
                ("2026-11-18", 84_520.0, 84_990.1, 84_310.6, 84_802.3),
                ("2026-11-19", 84_802.3, 85_244.0, 84_700.9, 85_190.5),
            ]),
        ),
        (
            "NSE:RELIANCE-EQ",
            eod_rows(&[
                ("2026-11-18", 1370.0, 1384.2, 1361.0, 1379.6),
                ("2026-11-19", 1374.0, 1391.8, 1370.4, 1388.1),
            ]),
        ),
    ];
    fake
}

fn eod_names(fake: &Fake) -> Vec<String> {
    fake.calls("EODChartData")
        .iter()
        .map(|j| j["sym"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The scrip master's NIFTY INDEX is not the name EODChartData takes, so
/// daily history of every NSE index came back empty. Daily index history
/// asks GetSecurityInfo for the name by token (NSE_INDEX and BSE_INDEX
/// alike); equities and intraday history ask for nothing.
#[tokio::test]
async fn daily_index_history_asks_with_the_name_get_security_info_reports() {
    let fake = index_fake();
    let b = adapter(config(), &fake).await;
    let nifty = b
        .get_history(&auth(), &history("NSE_INDEX", "NIFTY", "D"))
        .await
        .unwrap();
    assert_eq!(nifty.len(), 2, "daily NIFTY came back empty");
    assert_eq!(nifty[1].close, 26_068.4);
    assert_eq!(
        fake.calls("GetSecurityInfo"),
        vec![json!({"uid": "<USER_ID>", "exch": "NSE", "token": "26000"})]
    );
    let sensex = b
        .get_history(&auth(), &history("BSE_INDEX", "SENSEX", "D"))
        .await
        .unwrap();
    assert_eq!(sensex.len(), 2);
    assert_eq!(fake.calls("GetSecurityInfo")[1]["exch"], "BSE");
    let reliance = b
        .get_history(&auth(), &history("NSE", "RELIANCE", "D"))
        .await
        .unwrap();
    assert_eq!(reliance.len(), 2);
    bar_times(&b, &history("NSE_INDEX", "NIFTY", "1m")).await;
    assert_eq!(
        eod_names(&fake),
        ["NSE:Nifty 50", "BSE:SENSEX", "NSE:RELIANCE-EQ"]
    );
    assert_eq!(fake.calls("GetSecurityInfo").len(), 2);
}

/// The name is kept a day (the web's 64-entry, 24-hour LockedTTLCache) and
/// forgotten when the session ends (`on_logout`: logout, a broker switch,
/// the daily boundary).
#[tokio::test]
async fn the_index_name_is_kept_and_forgotten_when_the_session_ends() {
    let fake = index_fake();
    let b = adapter(config(), &fake).await;
    let nifty = history("NSE_INDEX", "NIFTY", "D");
    b.get_history(&auth(), &nifty).await.unwrap();
    b.get_history(&auth(), &nifty).await.unwrap();
    assert_eq!(fake.calls("GetSecurityInfo").len(), 1, "asked once");
    b.on_logout().await;
    b.get_history(&auth(), &nifty).await.unwrap();
    assert_eq!(fake.calls("GetSecurityInfo").len(), 2, "asked again");
    assert_eq!(eod_names(&fake), ["NSE:Nifty 50"; 3]);
}

/// A lookup that fails costs nothing that worked before: daily history
/// asks with the stored broker symbol, and the failure is not kept.
#[tokio::test]
async fn a_failed_lookup_asks_with_the_stored_symbol_and_is_not_kept() {
    let fake = index_fake();
    fake.index_names.lock().clear();
    let b = adapter(config(), &fake).await;
    let nifty = history("NSE_INDEX", "NIFTY", "D");
    assert!(b.get_history(&auth(), &nifty).await.unwrap().is_empty());
    fake.security_info_down.store(true, Ordering::SeqCst);
    assert!(b.get_history(&auth(), &nifty).await.unwrap().is_empty());
    assert_eq!(fake.calls("GetSecurityInfo").len(), 2);
    assert_eq!(eod_names(&fake), ["NSE:NIFTY INDEX"; 2]);
}

/// The cache's race rule (`crate::cache::TtlCache`): a name that answers
/// after the session ended is given to the history that asked for it, but
/// not kept for the next session.
#[tokio::test]
async fn an_index_name_answered_after_the_session_ended_is_not_kept() {
    let fake = index_fake();
    let b = Arc::new(adapter(config(), &fake).await);
    let (reached, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *fake.security_info_gate.lock() = Some((reached.clone(), release.clone()));
    let asking = tokio::spawn({
        let b = b.clone();
        async move {
            b.get_history(&auth(), &history("NSE_INDEX", "NIFTY", "D"))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .expect("daily index history never asked GetSecurityInfo");
    b.on_logout().await;
    release.notify_one();
    let nifty = asking.await.unwrap().unwrap();
    assert_eq!(nifty.len(), 2, "the history that asked gets the name");
    *fake.security_info_gate.lock() = None;
    b.get_history(&auth(), &history("NSE_INDEX", "NIFTY", "D"))
        .await
        .unwrap();
    assert_eq!(
        fake.calls("GetSecurityInfo").len(),
        2,
        "the late answer was kept past the session's end"
    );
}

/// Shoonya, Zebu and Flattrade never ask GetSecurityInfo: Shoonya keeps
/// its own index-name table, the others the broker symbol.
#[tokio::test]
async fn other_noren_members_never_ask_for_the_index_name() {
    for (cfg, name) in [
        (shoonya::config(), "NSE:Nifty 50"),
        (zebu::config(), "NSE:NIFTY INDEX"),
        (flattrade::config(), "NSE:NIFTY INDEX"),
    ] {
        let fake = index_fake();
        let b = adapter(cfg, &fake).await;
        let _ = b
            .get_history(&auth(), &history("NSE_INDEX", "NIFTY", "D"))
            .await;
        assert!(fake.calls("GetSecurityInfo").is_empty(), "{}", cfg.id);
        assert_eq!(eod_names(&fake)[0], name, "{}", cfg.id);
    }
}
