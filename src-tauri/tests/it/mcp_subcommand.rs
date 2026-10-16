//! The real `openalgo-desktop mcp` process: a thin stdio client that opens
//! no listener and touches no data directory.
//!
//! Every child runs with an empty environment whose home and app-data
//! variables point at a fresh temporary directory, so even a build that
//! wrongly started the whole app could not reach the trader's real data.
//! Each child is held by [`Guard`], which kills and reaps it on every path
//! (including a failed assertion), and every wait has a timeout. A probe
//! first checks that the binary really is a thin client and stops it the
//! moment it logs an app start-up line: the integration tests share one
//! target directory with other checkouts, whose builds of the same binary
//! can replace this one.

use crate::mcp_support::M;
use openalgo_desktop_lib::mcp::stdio;
use openalgo_desktop_lib::mcp::store::{self, TokenScope};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const APP_START_LINE: &str = "Starting OpenAlgo Desktop";

/// Kills and reaps the child when dropped.
struct Guard {
    child: Child,
    home: tempfile::TempDir,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<(bool, String)>,
    seen: Vec<(bool, String)>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Guard {
    fn spawn(args: &[&str], token: Option<&str>) -> Guard {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_openalgo-desktop"));
        cmd.args(args)
            .env_clear()
            .env("HOME", h)
            .env("USERPROFILE", h)
            .env("APPDATA", h)
            .env("LOCALAPPDATA", h)
            .env("XDG_DATA_HOME", h)
            .env("XDG_CONFIG_HOME", h)
            .env("TMPDIR", h)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(t) = token {
            cmd.env(stdio::TOKEN_ENV, t);
        }
        // Windows sockets do not initialise in a process without SystemRoot,
        // so every request would fail as if the app were closed. MCP clients
        // on Windows pass it through as well; it points at the OS directory,
        // not at any user data.
        #[cfg(windows)]
        for var in ["SystemRoot", "windir"] {
            if let Some(v) = std::env::var_os(var) {
                cmd.env(var, v);
            }
        }
        let mut child = cmd.spawn().unwrap();
        let (tx, rx) = mpsc::channel();
        // stdout lines are MCP messages (true); stderr lines diagnostics.
        let out = child.stdout.take().unwrap();
        let tx_out = tx.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx_out.send((true, l));
            }
        });
        let mut err = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            for l in s.lines() {
                let _ = tx.send((false, l.to_string()));
            }
        });
        let stdin = child.stdin.take();
        Guard {
            child,
            home,
            stdin,
            lines: rx,
            seen: Vec::new(),
        }
    }

    /// Wait for exit; `None` after `limit` (the guard then kills it).
    fn wait(&mut self, limit: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            self.collect();
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// Every line received so far, failing at once on an app start-up.
    fn drain(&mut self) -> Vec<(bool, String)> {
        self.collect();
        self.seen.clone()
    }

    /// Move received lines into `seen`, stopping a whole-app start at once.
    fn collect(&mut self) {
        let mut fresh = Vec::new();
        while let Ok(l) = self.lines.try_recv() {
            fresh.push(l);
        }
        self.reject_app_start(&fresh);
        self.seen.extend(fresh);
    }

    fn reject_app_start(&mut self, lines: &[(bool, String)]) {
        if lines.iter().any(|(_, l)| l.contains(APP_START_LINE)) {
            let _ = self.child.kill();
            panic!(
                "{} started the whole app instead of the MCP client; it was stopped at once. \
                 The shared target directory probably holds another checkout's build: rebuild and rerun.",
                env!("CARGO_BIN_EXE_openalgo-desktop")
            );
        }
    }

    /// The next MCP message on stdout, within `limit`.
    fn next_message(&mut self, limit: Duration) -> Value {
        let end = Instant::now() + limit;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok((true, l)) => {
                    self.reject_app_start(&[(true, l.clone())]);
                    return serde_json::from_str(&l).unwrap_or_else(|e| panic!("{}: {}", e, l));
                }
                Ok((false, l)) => self.reject_app_start(&[(false, l)]),
                Err(_) => panic!("no MCP reply in time"),
            }
        }
    }

    fn send(&mut self, v: Value) {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{}", v).unwrap();
        w.flush().unwrap();
    }

    /// Nothing was written under the isolated home.
    fn home_is_empty(&self) -> bool {
        std::fs::read_dir(self.home.path())
            .unwrap()
            .next()
            .is_none()
    }
}

/// Fail fast unless the binary is this checkout's thin client.
fn probe() {
    let mut g = Guard::spawn(&["mcp", "--help"], None);
    let status = g.wait(Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(50));
    let lines = g.drain();
    assert_eq!(status.and_then(|s| s.code()), Some(2), "{:?}", lines);
    assert!(
        lines
            .iter()
            .any(|(_, l)| l.starts_with("Usage: openalgo-desktop mcp")),
        "{:?}",
        lines
    );
}

#[test]
fn without_a_token_it_refuses_with_the_message_and_touches_nothing() {
    probe();
    let mut g = Guard::spawn(&["mcp", "--url", "http://127.0.0.1:9"], None);
    let status = g.wait(Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(50));
    let lines = g.drain();
    assert_eq!(status.and_then(|s| s.code()), Some(2), "{:?}", lines);
    assert!(
        lines.iter().all(|(stdout, _)| !stdout),
        "stdout is for MCP messages only"
    );
    let err: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
    assert!(err.join("\n").contains(stdio::MISSING_TOKEN), "{:?}", err);
    assert!(g.home_is_empty());
}

#[test]
fn a_token_on_the_command_line_is_refused_and_never_echoed() {
    probe();
    let mut g = Guard::spawn(
        &["mcp", "--token", "oamcp_on_the_command_line"],
        Some("oamcp_from_env"),
    );
    let status = g.wait(Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(50));
    let lines = g.drain();
    assert_eq!(status.and_then(|s| s.code()), Some(2));
    let all: String = lines
        .iter()
        .map(|(_, l)| l.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !all.contains("oamcp_on_the_command_line") && !all.contains("oamcp_from_env"),
        "{}",
        all
    );
}

/// Listening TCP sockets of `pid` (lsof), when lsof is available.
#[cfg(unix)]
fn listening_sockets(pid: u32) -> Option<String> {
    let out = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_process_serves_stdio_with_no_listener_and_no_data_dir() {
    probe();
    let m = M::new().await;
    m.h.analyze(true);
    let token = m.token(TokenScope::Read);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    m.h.ctx.config.write().http_port = addr.port();
    let app = openalgo_desktop_lib::server::app(m.h.ctx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<axum::extract::Request>::into_make_service_with_connect_info::<
                SocketAddr,
            >(app),
        )
        .await;
    });
    let url = format!("http://{}", addr);

    let result = tokio::task::spawn_blocking(move || {
        let mut g = Guard::spawn(&["mcp", "--url", &url], Some(&token));
        g.send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "t", "version": "1"}}}),
        );
        let init = g.next_message(Duration::from_secs(10));
        assert_eq!(init["result"]["serverInfo"]["name"], "openalgo", "{}", init);
        g.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        g.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "get_index_symbols", "arguments": {"exchange": "NSE"}}}));
        let call = g.next_message(Duration::from_secs(10));
        let text = call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{}", call));
        assert!(text.contains("NSE_INDEX"), "{}", text);

        #[cfg(unix)]
        if let Some(listening) = listening_sockets(g.child.id()) {
            assert!(
                listening.trim().is_empty(),
                "the MCP process listens:\n{}",
                listening
            );
        }
        assert!(
            g.home_is_empty(),
            "the MCP process wrote under its home directory"
        );
        let leaked: String = g.drain().iter().map(|(_, l)| l.clone()).collect();
        assert!(!leaked.contains(&token), "token in the process output");

        // Closing stdin ends the session and the process.
        drop(g.stdin.take());
        let status = g.wait(Duration::from_secs(10));
        assert_eq!(status.and_then(|s| s.code()), Some(0));
    })
    .await;
    server.abort();
    m.h.shutdown().await;
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// The app on an ephemeral loopback listener, as the bridge reaches it.
async fn serve_app(m: &M) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    m.h.ctx.config.write().http_port = addr.port();
    let app = openalgo_desktop_lib::server::app(m.h.ctx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<axum::extract::Request>::into_make_service_with_connect_info::<
                SocketAddr,
            >(app),
        )
        .await;
    });
    (format!("http://{}", addr), server)
}

/// One bridge process: initialize (with `version`), then `initialized`.
fn open_bridge(url: &str, token: &str, version: &str) -> (Guard, Value) {
    let mut g = Guard::spawn(&["mcp", "--url", url], Some(token));
    g.send(
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": version, "capabilities": {},
        "clientInfo": {"name": "every-tool", "version": "1"}}}),
    );
    let init = g.next_message(Duration::from_secs(10));
    g.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    (g, init)
}

/// `tools/call` through the bridge: the tool's output (`data` inside the
/// trust envelope). Every tool must be answered by the app itself: a
/// JSON-RPC result, not an error, carrying the envelope of that tool.
fn call(g: &mut Guard, id: &mut i64, name: &str, args: Value) -> Value {
    *id += 1;
    g.send(json!({"jsonrpc": "2.0", "id": *id, "method": "tools/call",
        "params": {"name": name, "arguments": args}}));
    let reply = g.next_message(Duration::from_secs(60));
    assert_eq!(reply["id"], *id, "{}: {}", name, reply);
    assert!(reply.get("error").is_none(), "{}: {}", name, reply);
    assert_eq!(reply["result"]["isError"], false, "{}: {}", name, reply);
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{}: no text: {}", name, reply));
    let out: Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{}: {}: {}", name, e, text));
    assert_eq!(out["_openalgo_mcp_security"]["tool"], name, "{}", text);
    out["data"].clone()
}

/// What a tool answered, for the table: "success" when the payload says so
/// (or is the tool's own non-error shape), else its error type or message.
fn outcome(data: &Value) -> String {
    if let Some(e) = data.get("error") {
        return format!(
            "error:{}",
            e.get("error_type")
                .and_then(Value::as_str)
                .unwrap_or("tool")
        );
    }
    match data.get("status").and_then(Value::as_str) {
        Some("success") | None => "success".into(),
        Some(other) => format!("{}:{}", other, data["message"]),
    }
}

/// The arguments each read tool is called with (the mock app's master has
/// SBIN, INFY and NIFTY with options; `expiry` is a listed NIFTY expiry).
fn read_calls(expiry: &str, option: &str) -> Vec<(&'static str, Value)> {
    let e = expiry.replace('-', "");
    let sbin = json!({"symbol": "SBIN", "exchange": "NSE"});
    let nifty = json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": e});
    vec![
        ("get_quote", sbin.clone()),
        (
            "get_multi_quotes",
            json!({"symbols": [sbin, {"symbol": "INFY", "exchange": "NSE"}]}),
        ),
        ("get_option_chain", nifty.clone()),
        ("get_market_depth", sbin.clone()),
        (
            "get_historical_data",
            json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D",
            "start_date": "2026-01-01", "end_date": "2026-02-09"}),
        ),
        ("search_instruments", json!({"query": "SBIN"})),
        ("get_symbol_info", sbin.clone()),
        ("get_index_symbols", json!({"exchange": "NSE"})),
        ("get_available_intervals", json!({})),
        (
            "get_option_symbol",
            json!({"underlying": "NIFTY", "exchange": "NSE_INDEX",
            "expiry_date": e, "offset": "ATM", "option_type": "CE"}),
        ),
        ("get_synthetic_future", nifty),
        (
            "get_option_greeks",
            json!({"symbol": option, "exchange": "NFO"}),
        ),
        ("get_holidays", json!({})),
        ("get_timings", json!({"date": "2026-10-05"})),
        ("check_holiday", json!({"date": "2026-10-04"})),
        ("get_instruments", json!({"exchange": "NSE", "limit": 5})),
        (
            "calculate_indicator",
            json!({"symbol": "SBIN", "exchange": "NSE",
            "indicator": "sma", "lookback_days": 60}),
        ),
        ("get_trend_snapshot", sbin.clone()),
        ("get_momentum_snapshot", sbin.clone()),
        ("get_volatility_snapshot", sbin.clone()),
        ("get_support_resistance", sbin.clone()),
        ("detect_signals", sbin.clone()),
        (
            "screen_instruments",
            json!({"symbols": [sbin, {"symbol": "INFY", "exchange": "NSE"}]}),
        ),
        (
            "multi_timeframe_analysis",
            json!({"symbol": "SBIN", "exchange": "NSE", "intervals": ["D"]}),
        ),
        (
            "correlation_beta",
            json!({"symbol1": "SBIN", "exchange1": "NSE",
            "symbol2": "INFY", "exchange2": "NSE"}),
        ),
        ("get_openalgo_version", json!({})),
        ("validate_order_constants", json!({})),
        (
            "get_open_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        ),
        ("get_order_book", json!({})),
        ("get_trade_book", json!({})),
        ("get_position_book", json!({})),
        ("get_holdings", json!({})),
        ("get_funds", json!({})),
        (
            "calculate_margin",
            json!({"positions": [{"symbol": "SBIN", "exchange": "NSE",
            "action": "BUY", "product": "MIS", "pricetype": "MARKET", "quantity": "1"}]}),
        ),
        ("analyzer_status", json!({})),
        (
            "send_telegram_alert",
            json!({"username": "trader", "message": "every-tool test"}),
        ),
    ]
}

/// PLATFORM-07, MCP-04: the real `openalgo-desktop mcp` process, as an AI
/// client starts it, against a running app (an `AppState` on ephemeral
/// loopback ports with the mock broker; nothing of the trader's): every
/// one of the 49 tools is called through it, each answered by the app
/// with that tool's envelope; an initialize asking for a protocol version
/// the bridge does not speak still gets one it does; a revoked token is
/// told so; stdout carries JSON-RPC only and the token never appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_tool_through_the_bridge_process_against_the_running_app() {
    probe();
    let m = M::new().await;
    *m.h.mock.history.lock() = Some(Ok(history_candles()));
    m.h.analyze(false);
    // Each token may make 5 order-tool calls a minute (http::WRITE_LIMIT),
    // 60 to each read scope and 120 requests in all. The calls below are
    // split to stay inside: token 0 makes the 39 reads and 4 order calls
    // (place, modify, cancel, cancel all), token 1 the other 5 placements,
    // token 2 close_all_positions and analyzer_toggle. A new order tool or
    // setup call needs its place in that count, or a fourth token.
    let tokens: Vec<String> = (0..3).map(|_| m.token(TokenScope::ReadWrite)).collect();
    let (url, server) = serve_app(&m).await;
    let ctx = m.h.ctx.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut outcomes: Vec<(String, String)> = Vec::new();
        let mut id = 0;
        let (mut g, init) = open_bridge(&url, &tokens[0], "2025-06-18");
        assert_eq!(init["result"]["serverInfo"]["name"], "openalgo", "{}", init);
        g.send(json!({"jsonrpc": "2.0", "id": 100, "method": "tools/list", "params": {}}));
        let list = g.next_message(Duration::from_secs(10));
        let listed: std::collections::BTreeSet<String> = list["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("{}", list))
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(listed.len(), 49, "{:?}", listed);

        let expiries = call(
            &mut g,
            &mut id,
            "get_expiry_dates",
            json!({"symbol": "NIFTY", "exchange": "NFO", "instrument_type": "options"}),
        );
        outcomes.push(("get_expiry_dates".into(), outcome(&expiries)));
        let expiry = expiries["data"][0].as_str().unwrap_or("").to_string();
        let symbol = call(
            &mut g,
            &mut id,
            "get_option_symbol",
            json!({"underlying": "NIFTY",
            "exchange": "NSE_INDEX", "expiry_date": expiry.replace('-', ""),
            "offset": "ATM", "option_type": "CE"}),
        );
        let option = symbol["symbol"].as_str().unwrap_or("NIFTY").to_string();
        for (name, args) in read_calls(&expiry, &option) {
            let data = call(&mut g, &mut id, name, args);
            outcomes.push((name.into(), outcome(&data)));
        }

        // Order tools: a resting LIMIT order is placed, modified, looked up
        // and cancelled; then the rest.
        let order = |price: f64| {
            json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
            "quantity": 1, "price_type": "LIMIT", "price": price, "product": "MIS"})
        };
        let placed = call(&mut g, &mut id, "place_order", order(1.0));
        outcomes.push(("place_order".into(), outcome(&placed)));
        let order_id = placed["orderid"].as_str().unwrap_or("0").to_string();
        let mut modify = order(2.0);
        modify["order_id"] = json!(order_id);
        for (name, args) in [
            ("modify_order", modify),
            ("get_order_status", json!({"order_id": order_id})),
            ("cancel_order", json!({"order_id": order_id})),
            ("cancel_all_orders", json!({})),
        ] {
            let data = call(&mut g, &mut id, name, args);
            outcomes.push((name.into(), outcome(&data)));
        }
        drop(g.stdin.take());
        assert_eq!(
            g.wait(Duration::from_secs(10)).and_then(|s| s.code()),
            Some(0)
        );

        let (mut g, _) = open_bridge(&url, &tokens[1], "2025-06-18");
        let e = expiry.replace('-', "");
        for (name, args) in [
            (
                "place_smart_order",
                json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                "quantity": 1, "position_size": 1, "product": "MIS"}),
            ),
            (
                "place_basket_order",
                json!({"orders": [{"symbol": "SBIN", "exchange": "NSE",
                "action": "BUY", "quantity": 1, "pricetype": "MARKET", "product": "MIS"}]}),
            ),
            (
                "place_split_order",
                json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                "quantity": 2, "split_size": 1, "product": "MIS"}),
            ),
            (
                "place_options_order",
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX",
                "expiry_date": e, "offset": "ATM", "option_type": "CE", "action": "BUY",
                "quantity": 75, "product": "NRML"}),
            ),
            (
                "place_options_multi_order",
                json!({"underlying": "NIFTY", "exchange": "NSE_INDEX",
                "expiry_date": e, "legs": [{"offset": "ATM", "option_type": "CE",
                "action": "BUY", "quantity": 75}]}),
            ),
        ] {
            let data = call(&mut g, &mut id, name, args);
            outcomes.push((name.into(), outcome(&data)));
        }
        drop(g.stdin.take());
        assert_eq!(
            g.wait(Duration::from_secs(10)).and_then(|s| s.code()),
            Some(0)
        );

        // An initialize for a protocol version the bridge does not speak
        // gets one it does (MCP: the server answers with its own).
        let (mut g, init) = open_bridge(&url, &tokens[2], "1999-01-01");
        let agreed = init["result"]["protocolVersion"]
            .as_str()
            .unwrap_or_default();
        assert!(!agreed.is_empty() && agreed != "1999-01-01", "{}", init);
        for (name, args) in [
            ("close_all_positions", json!({})),
            ("analyzer_toggle", json!({"mode": true})),
        ] {
            let data = call(&mut g, &mut id, name, args);
            outcomes.push((name.into(), outcome(&data)));
        }
        // The token is revoked while the client runs: the next call says so.
        let conn = ctx.sqlite.conn().unwrap();
        let row = store::find_token(&conn, &tokens[2]).unwrap().unwrap();
        assert!(store::revoke_token(&conn, row.id, ctx.now()).unwrap());
        drop(conn);
        id += 1;
        g.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "get_funds", "arguments": {}}}));
        let refused = g.next_message(Duration::from_secs(10));
        assert_eq!(refused["result"]["isError"], true, "{}", refused);
        assert_eq!(refused["result"]["content"][0]["text"], stdio::BAD_TOKEN);
        let rest: String = g.drain().iter().map(|(_, l)| l.clone()).collect();
        for t in &tokens {
            assert!(!rest.contains(t.as_str()), "a token reached the output");
        }
        drop(g.stdin.take());
        assert_eq!(
            g.wait(Duration::from_secs(10)).and_then(|s| s.code()),
            Some(0)
        );

        let called: std::collections::BTreeSet<String> =
            outcomes.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(called, listed, "every listed tool is called");
        outcomes
    })
    .await;
    server.abort();
    m.h.shutdown().await;
    let outcomes = match result {
        Ok(o) => o,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    let table: Vec<String> = outcomes
        .iter()
        .map(|(n, o)| format!("{:28} {}", n, o))
        .collect();
    eprintln!("{}", table.join("\n"));
    for (name, o) in &outcomes {
        assert!(
            !o.contains("connection_error") && !o.contains("timeout_error"),
            "{} did not reach the app's answer: {}",
            name,
            o
        );
    }
}

fn history_candles() -> Vec<openalgo_desktop_lib::brokers::types::Candle> {
    (0..40)
        .map(|i| openalgo_desktop_lib::brokers::types::Candle {
            timestamp: 1_767_205_800 + i * 86_400,
            open: 100.0,
            high: 101.0 + i as f64 % 3.0,
            low: 99.0,
            close: 100.0 + (i as f64 * 0.5).sin(),
            volume: 10,
            oi: 0,
        })
        .collect()
}
