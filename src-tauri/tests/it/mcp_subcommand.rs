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
use openalgo_desktop_lib::mcp::store::TokenScope;
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
