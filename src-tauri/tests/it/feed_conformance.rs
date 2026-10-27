//! Replays every recorded web transcript in
//! `tests/fixtures/web/websocket/*.jsonl` against the desktop feed server
//! with a `FakeSource`.
//!
//! Control frames (acks, errors, pongs, broker info) must match the
//! recording exactly, except values that are inherently per-run
//! (`server_timestamp`) and arrays the web emits in Python set order
//! (`successful` / `failed`, supported brokers), which are compared as sets.
//!
//! Market data frames are produced by publishing, at the point the recording
//! received them, an update built from the recorded frame. The desktop
//! frame must then have the same keys (top level, `data`, depth levels) and
//! the same JSON value types, and the same `data.mode` label; prices are not
//! compared. One tolerance: the
//! desktop always sends `ltt` in mode-1 data, which the web's Zerodha
//! adapter omits on mode-1 copies derived from a richer tick.

use crate::feed_support;

use feed_support::*;
use openalgo_desktop_lib::feed::source::{DepthBook, DepthLevel, QuoteFields};
use openalgo_desktop_lib::feed::{InstrumentKey, MarketUpdate, Mode};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests/fixtures/web/websocket")
}

fn load(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixture_dir().join(name)).expect("transcript exists");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("transcript line is JSON"))
        .collect()
}

fn substitute(v: &Value) -> Value {
    replace_strings(v, &[("<APIKEY>", API_KEY)])
}

/// `v` with every string equal to a placeholder replaced by its value.
fn replace_strings(v: &Value, with: &[(&str, &str)]) -> Value {
    match v {
        Value::String(s) => match with.iter().find(|(from, _)| s == from) {
            Some((_, to)) => Value::String((*to).into()),
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| replace_strings(x, with)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), replace_strings(x, with)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Kind {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Arr,
    Obj,
}

fn kind(v: &Value) -> Kind {
    match v {
        Value::Null => Kind::Null,
        Value::Bool(_) => Kind::Bool,
        Value::Number(n) if n.is_f64() => Kind::Float,
        Value::Number(_) => Kind::Int,
        Value::String(_) => Kind::Str,
        Value::Array(_) => Kind::Arr,
        Value::Object(_) => Kind::Obj,
    }
}

/// Keys that carry prices: Python sends an int when the value is whole
/// (`price_change: 0`), the desktop always sends a float. Both are JSON
/// numbers to every client.
const PRICE_KEYS: &[&str] = &[
    "ltp",
    "open",
    "high",
    "low",
    "close",
    "average_price",
    "price",
    "price_change",
    "price_change_percent",
];

fn assert_same_type(path: &str, key: &str, exp: &Value, got: &Value) {
    let (ke, kg) = (kind(exp), kind(got));
    let ok = ke == kg || (PRICE_KEYS.contains(&key) && ke == Kind::Int && kg == Kind::Float);
    assert!(
        ok,
        "{}.{}: recorded {:?} ({}) but desktop sent {:?} ({})",
        path, key, ke, exp, kg, got
    );
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

fn compare_market(exp: &Value, got: &Value) {
    assert_eq!(keys(exp), keys(got), "market_data keys\n{}\n{}", exp, got);
    for k in ["type", "symbol", "exchange", "mode", "broker"] {
        assert_eq!(exp[k], got[k], "market_data.{}", k);
    }
    let (ed, gd) = (&exp["data"], &got["data"]);
    let (ek, gk) = (keys(ed), keys(gd));
    let missing: Vec<_> = ek.difference(&gk).collect();
    let extra: Vec<_> = gk.difference(&ek).filter(|k| *k != "ltt").collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "data keys differ: missing {:?} extra {:?}\nrecorded {}\ndesktop  {}",
        missing,
        extra,
        ed,
        gd
    );
    for k in &ek {
        assert_same_type("data", k, &ed[k], &gd[k]);
    }
    assert_eq!(ed["symbol"], gd["symbol"]);
    assert_eq!(ed["exchange"], gd["exchange"]);
    // A label, not a price: compared by value (docs-7: Depth frames said
    // `depth` where the recording says `full`).
    assert_eq!(ed["mode"], gd["mode"], "data.mode");
    if let Some(book) = ed.get("depth") {
        for side in ["buy", "sell"] {
            let (e, g) = (
                book[side].as_array().expect("recorded side"),
                gd["depth"][side].as_array().expect("desktop side"),
            );
            assert_eq!(e.len(), g.len(), "depth.{} levels", side);
            for (el, gl) in e.iter().zip(g) {
                assert_eq!(keys(el), keys(gl));
                for k in keys(el) {
                    assert_same_type("depth", &k, &el[&k], &gl[&k]);
                }
            }
        }
    }
}

fn sorted_items(v: &Value) -> Vec<String> {
    let mut items: Vec<String> = v
        .as_array()
        .map(|a| a.iter().map(|x| x.to_string()).collect())
        .unwrap_or_default();
    items.sort();
    items
}

fn compare_control(exp: &Value, got: &Value) {
    assert_eq!(
        keys(exp),
        keys(got),
        "frame keys\nrecorded {}\ndesktop  {}",
        exp,
        got
    );
    for k in keys(exp) {
        let (e, g) = (&exp[&k], &got[&k]);
        match (exp["type"].as_str(), k.as_str()) {
            (Some("pong"), "server_timestamp") => assert_eq!(kind(g), Kind::Int),
            (Some("supported_brokers"), "brokers") => {
                assert_eq!(sorted_items(e), sorted_items(g), "brokers")
            }
            (Some("unsubscribe"), "successful") | (Some("unsubscribe"), "failed") => {
                assert_eq!(sorted_items(e), sorted_items(g), "{}", k)
            }
            _ => assert_eq!(e, g, "field '{}'\nrecorded {}\ndesktop  {}", k, exp, got),
        }
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64()
}

/// The update a broker would have sent for the recorded frame.
fn update_from(frame: &Value) -> MarketUpdate {
    let d = &frame["data"];
    let mode = Mode::from_u8(frame["mode"].as_u64().unwrap_or(1) as u8).unwrap_or(Mode::Ltp);
    let ts = int(&d["timestamp"]).unwrap_or(0);
    let quote = (mode >= Mode::Quote).then(|| QuoteFields {
        volume: int(&d["volume"]).unwrap_or(0),
        last_quantity: int(&d["last_quantity"]).unwrap_or(0),
        average_price: num(&d["average_price"]).unwrap_or(0.0),
        total_buy_quantity: int(&d["total_buy_quantity"]).unwrap_or(0),
        total_sell_quantity: int(&d["total_sell_quantity"]).unwrap_or(0),
        open: num(&d["open"]),
        high: num(&d["high"]),
        low: num(&d["low"]),
        close: num(&d["close"]),
        oi: int(&d["oi"]),
        price_change: num(&d["price_change"]),
        price_change_percent: num(&d["price_change_percent"]),
    });
    let side = |s: &str| -> Vec<DepthLevel> {
        d["depth"][s]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| DepthLevel {
                        price: num(&l["price"]).unwrap_or(0.0),
                        quantity: int(&l["quantity"]).unwrap_or(0),
                        orders: int(&l["orders"]).unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let depth = d.get("depth").map(|_| DepthBook {
        buy: side("buy"),
        sell: side("sell"),
    });
    MarketUpdate {
        key: InstrumentKey::new(
            d["symbol"].as_str().unwrap_or_default(),
            d["exchange"].as_str().unwrap_or_default(),
        ),
        mode,
        ltp: num(&d["ltp"]).unwrap_or(0.0),
        ltt: int(&d["ltt"]).or(Some(ts)),
        timestamp: ts,
        quote,
        depth,
        exact_mode: false,
    }
}

fn is_market(line: &Value) -> bool {
    line["direction"] == "recv" && line["message"]["type"] == "market_data"
}

/// Replay one transcript; returns the number of frames compared.
async fn replay(name: &str) -> usize {
    let h = start_default().await;
    let lines = load(name);
    let mut c = Client::connect(&h.url).await;
    let mut compared = 0;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        match line["direction"].as_str() {
            Some("send") => {
                if let Some(raw) = line.get("raw").and_then(Value::as_str) {
                    c.send_raw(raw).await;
                } else if line["raw_text"] == true {
                    c.send_raw(&line["message"].to_string()).await;
                } else {
                    c.send(&substitute(&line["message"])).await;
                }
            }
            Some("recv") if is_market(line) => {
                // One broker tick fans out to every mode the client holds,
                // richest first: group the recorded burst.
                let head = &line["message"];
                let mut burst = vec![head.clone()];
                let mut j = i + 1;
                while j < lines.len() && is_market(&lines[j]) {
                    let m = &lines[j]["message"];
                    let prev = &burst[burst.len() - 1];
                    if m["symbol"] != head["symbol"]
                        || m["exchange"] != head["exchange"]
                        || m["mode"].as_u64() >= prev["mode"].as_u64()
                    {
                        break;
                    }
                    burst.push(m.clone());
                    j += 1;
                }
                assert!(h.source.publish(update_from(head)) > 0);
                for exp in &burst {
                    let got = c.recv().await;
                    compare_market(exp, &got);
                    compared += 1;
                }
                i = j;
                continue;
            }
            Some("recv") => {
                let got = c.recv().await;
                compare_control(&line["message"], &got);
                compared += 1;
            }
            Some("note") if line.get("market_data_total") == Some(&Value::from(0)) => {
                assert_eq!(
                    c.recv_within(Duration::from_millis(150)).await,
                    None,
                    "server sent an unsolicited frame ({})",
                    line["message"]["note"]
                );
            }
            _ => {}
        }
        i += 1;
    }
    // Nothing the recording did not see.
    assert_eq!(c.recv_within(Duration::from_millis(150)).await, None);
    drop(c);
    assert!(
        eventually(Duration::from_secs(5), || h.handle.stats().clients == 0).await,
        "client entry released"
    );
    assert_eq!(h.source.active_count(), 0, "source subscriptions released");
    assert!(
        h.source.violations().is_empty(),
        "{:?}",
        h.source.violations()
    );
    h.handle.stop().await;
    compared
}

#[tokio::test]
async fn transcript_01_connect_auth_subscribe_flow() {
    let n = replay("01_connect_auth_subscribe_flow.jsonl").await;
    assert_eq!(n, 38);
}

#[tokio::test]
async fn transcript_02_subscribe_before_auth() {
    let n = replay("02_subscribe_before_auth.jsonl").await;
    assert_eq!(n, 5);
}

#[tokio::test]
async fn transcript_03_invalid_apikey_auth() {
    let n = replay("03_invalid_apikey_auth.jsonl").await;
    assert_eq!(n, 4);
}

#[tokio::test]
async fn transcript_05_auth_alias_forms_and_mode_labels() {
    let n = replay("05_auth_alias_forms_and_mode_labels.jsonl").await;
    assert_eq!(n, 14);
}

/// 04: a socket that never authenticates is closed with 4401 "auth timeout"
/// after the 15 s grace period. The tokio clock is paused once the socket is
/// open (real I/O under a paused clock lets time jump during the handshake),
/// so the test does not wait 15 s in real time.
#[tokio::test]
async fn transcript_04_auth_grace_timeout() {
    let lines = load("04_auth_grace_timeout.jsonl");
    let closed = lines
        .iter()
        .find(|l| l.get("close_code").is_some())
        .expect("recorded close");
    let want_code = closed["close_code"].as_u64().unwrap() as u16;
    let want_reason = closed["close_reason"].as_str().unwrap().to_string();

    let h = start_default().await;
    let t0 = tokio::time::Instant::now();
    let mut c = Client::connect(&h.url).await;
    tokio::time::pause();
    let got = c.expect_close(Duration::from_secs(60)).await;
    let elapsed = t0.elapsed();
    tokio::time::resume();
    assert_eq!(got, Some((want_code, want_reason)));
    // Only the lower bound is meaningful. A paused clock auto-advances
    // whenever the runtime looks idle, and under load it looks idle while
    // the close frame is still crossing the real loopback socket, so the
    // clock can jump past later timers (seen at 57 s on a busy machine).
    // The exact 4401 "auth timeout" code above already rules out a close
    // for any other reason, such as the ping timeout.
    assert!(
        elapsed >= Duration::from_secs(15),
        "closed before the 15 s grace period: {:?}",
        elapsed
    );
    h.handle.stop().await;
}

/// Only a successful authentication stops the grace timer: an authenticated
/// socket stays open past it, one whose authentication failed is closed with
/// 4401 like a silent one (web behaviour).
#[tokio::test]
async fn auth_timer_stops_only_on_successful_auth() {
    let h = start_default().await;
    let mut ok = Client::connect(&h.url).await;
    ok.auth().await;
    let mut bad = Client::connect(&h.url).await;
    let v = bad
        .request(serde_json::json!({"action": "authenticate", "api_key": "wrong"}))
        .await;
    assert_eq!(v["code"], "AUTHENTICATION_ERROR");

    tokio::time::pause();
    let closed = bad.expect_close(Duration::from_secs(20)).await;
    assert_eq!(closed, Some((4401, "auth timeout".into())));
    // Past the grace period, short of the 40 s keepalive limit.
    tokio::time::sleep(Duration::from_secs(10)).await;
    tokio::time::resume();
    let pong = ok.request(serde_json::json!({"action": "ping"})).await;
    assert_eq!(pong["type"], "pong");
    h.handle.stop().await;
}

/// A broker feed socket that accepts the app's market data connections and
/// never sends a tick.
async fn silent_broker() -> (String, tokio::task::JoinHandle<()>) {
    use futures_util::StreamExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut held = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = listener.accept().await {
            held.spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    while let Some(Ok(_)) = ws.next().await {}
                }
            });
        }
    });
    (url, task)
}

/// Replays a transcript's control frames against a feed at `url`: every
/// acknowledgment, error, pong and broker answer must match the recording
/// (with the key and user put in), and nothing unsolicited may arrive.
/// Market data lines are skipped: the app's broker socket sends no tick.
async fn replay_control(url: &str, name: &str, key: &str, user: &str) -> usize {
    let with = [("<APIKEY>", key), ("<USER_ID>", user)];
    let mut c = Client::connect(url).await;
    let mut compared = 0;
    for line in load(name) {
        match line["direction"].as_str() {
            Some("send") => match line.get("raw").and_then(Value::as_str) {
                Some(raw) => c.send_raw(raw).await,
                None if line["raw_text"] == true => c.send_raw(&line["message"].to_string()).await,
                None => c.send(&replace_strings(&line["message"], &with)).await,
            },
            Some("recv") if is_market(&line) => {}
            Some("recv") => {
                let expected = replace_strings(&line["message"], &with);
                let got = c.recv().await;
                compare_control(&expected, &got);
                compared += 1;
            }
            Some("note") if line.get("market_data_total") == Some(&Value::from(0)) => {
                assert_eq!(c.recv_within(Duration::from_millis(150)).await, None);
            }
            _ => {}
        }
    }
    assert_eq!(c.recv_within(Duration::from_millis(150)).await, None);
    compared
}

/// The subscribe, acknowledgment, mode and authentication transcripts
/// against the running app instead of a `FakeSource`: `AppState` with the
/// mock Zerodha session, its symbol master and its streaming capability,
/// the production bridge and manager connected to a broker socket,
/// `FeedService` on an ephemeral port and the app's own API key check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcripts_against_the_running_app() {
    use openalgo_desktop_lib::brokers::Broker;
    use openalgo_desktop_lib::feed::FeedService;
    use openalgo_desktop_lib::state::ServerStatus;
    let h = crate::api_v1_support::H::new().await;
    let (broker_url, broker_task) = silent_broker().await;
    *h.mock.feed_url.lock() = Some(broker_url);
    // Start the session again so its live feed connects to the socket.
    let session = h.ctx.get_broker_session().expect("the mock session");
    h.ctx.runtime.activate(&h.ctx, &session).await;
    assert!(
        eventually(Duration::from_secs(10), || h.ctx.websocket.is_connected()).await,
        "the session's market data feed connects"
    );
    let free = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    h.ctx.pin_listener_ports(free(), free());
    let feed = FeedService::new(h.ctx.clone());
    assert!(matches!(feed.start().await, ServerStatus::Running { .. }));
    let url = format!("ws://{}", feed.local_addr().await.unwrap());
    // The mock broker streams 5 levels, as Kite does; the Depth 20 and 50
    // subscribes are still acknowledged with 20 and 50, as recorded
    // (maintainer decision 2026-10-16: echo like the web).
    assert_eq!(h.mock.feed_depth_levels("NSE"), vec![5]);

    let mut compared = 0;
    for name in [
        "01_connect_auth_subscribe_flow.jsonl",
        "02_subscribe_before_auth.jsonl",
        "03_invalid_apikey_auth.jsonl",
        "05_auth_alias_forms_and_mode_labels.jsonl",
    ] {
        compared += replay_control(&url, name, &h.key, "trader").await;
    }
    assert_eq!(compared, 26 + 5 + 4 + 8);
    assert!(
        eventually(Duration::from_secs(5), || h.ctx.bridge.applied().is_empty()).await,
        "every subscription released at the broker"
    );
    feed.stop().await;
    broker_task.abort();
    h.shutdown().await;
}
