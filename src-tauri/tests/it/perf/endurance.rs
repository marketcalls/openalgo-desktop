//! Target: memory and descriptors over 8 h stay flat within 5% after the
//! warm-up; the data folder's growth is reported.
//!
//! The app runs a trading day in a loop for `OPENALGO_PERF_MINUTES`
//! (default 15, the smoke; the maintainer runs 480), with the services the
//! app starts besides the servers (monitoring writer, health sampler,
//! wake watch, strategy module):
//!
//! - the broker feed streams 1,000 ticks a second over 1,000 symbols; an
//!   SDK client holds 200 of them and reads everything; `/api/v1` traffic
//!   runs at 20 requests a second over a sample of endpoints;
//! - every minute: five scalping stops are saved, breached by the feed,
//!   exited at the broker, filled and cleared through the order book;
//!   three live and three sandbox orders; a passing SDK client and a
//!   Socket.IO browser connect and leave;
//! - one live leg keeps an exit working at the broker all run, so every
//!   reconnect opens a reconciler gap that must close;
//! - after the warm-up, one fault a minute in rotation (`faults.rs`): broker
//!   feed drop, order-socket drop, sleep and wake, clock set back, locked
//!   database. Each is checked against its expected outcome.
//!
//! Memory (the physical footprint on macOS, the resident set elsewhere),
//! descriptors, database pool connections, live tokio tasks and the data
//! folder are sampled every minute. Warm-up is the first tenth of the run,
//! at least 3 minutes. After it, every sample's memory stays within 5% of
//! the warm-up sample's, and descriptors within 5% or 4, whichever is more
//! (plus two per pooled database connection opened since, the pool's
//! bounded growth).
//! Samples go to `OPENALGO_PERF_OUT` as they are taken.

use super::faults::{self, Outcome, ROTATION};
use super::rig::{Rig, RigOptions, BASE_PRICE};
use super::{folder_bytes, memory_kib, memory_parts, record_sample, MemoryParts, Report};
use crate::broker_session_e2e::settled_fds;
use crate::feed_support::Client;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::types::Order;
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::scalping::store::SlUpsert;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_MINUTES: u64 = 15;
const MINUTE: Duration = Duration::from_secs(60);
const FLAT: f64 = 1.05;
const TASK_SLACK: usize = 8;
/// Descriptors the 5% may round up to: with about 50 open, 5% is two, and
/// the harness's own client sockets (an HTTP keep-alive, a feed client
/// leaving) move the count by that much from one sample to the next.
const FD_SLACK: usize = 4;
/// The leg whose exit works at the broker all run.
const STANDING: &str = "SYM0998";

fn minutes() -> u64 {
    std::env::var("OPENALGO_PERF_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|m| *m >= 5)
        .unwrap_or(DEFAULT_MINUTES)
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    minute: u64,
    mem_kib: u64,
    fds: usize,
    pool: usize,
    tasks: usize,
    folder_kib: u64,
    /// What SQLite's allocator holds (page caches, statements), in KiB:
    /// tells caches filling from the rest of the growth.
    sqlite_kib: u64,
    /// The resident set's parts and the allocator's in-use and free
    /// memory, where the system says (Linux).
    parts: Option<MemoryParts>,
    threads: Option<i64>,
}

fn sqlite_kib() -> u64 {
    // SAFETY: sqlite3_memory_used takes no arguments and only reads
    // SQLite's global allocator statistics.
    let used = unsafe { rusqlite::ffi::sqlite3_memory_used() };
    (used.max(0) as u64) / 1024
}

async fn sample(rig: &Rig, minute: u64) -> Sample {
    let (fds, pool) = settled_fds(&rig.ctx).await;
    let s = Sample {
        minute,
        mem_kib: memory_kib(),
        fds,
        pool,
        tasks: tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks(),
        folder_kib: folder_bytes(rig.dir.path()) / 1024,
        sqlite_kib: sqlite_kib(),
        parts: memory_parts(),
        threads: openalgo_desktop_lib::services::health_service::process_stats().threads,
    };
    let p = s.parts.unwrap_or_default();
    record_sample(
        "endurance_sample",
        &[
            ("minute", s.minute.to_string()),
            ("mem_kib", s.mem_kib.to_string()),
            ("fds", s.fds.to_string()),
            ("pool", s.pool.to_string()),
            ("tasks", s.tasks.to_string()),
            ("folder_kib", s.folder_kib.to_string()),
            ("sqlite_kib", s.sqlite_kib.to_string()),
            ("anon_kib", p.anon_kib.to_string()),
            ("file_kib", p.file_kib.to_string()),
            ("shmem_kib", p.shmem_kib.to_string()),
            ("heap_in_use_kib", p.heap_in_use_kib.to_string()),
            ("heap_free_kib", p.heap_free_kib.to_string()),
            ("threads", s.threads.unwrap_or(-1).to_string()),
        ],
    );
    s
}

fn stop_for(symbol: &str) -> SlUpsert {
    SlUpsert {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        mode: "live".into(),
        side: Some("BUY".into()),
        entry_price: Some(BASE_PRICE),
        quantity: Some(1),
        initial_sl: Some(BASE_PRICE - 10.0),
        is_active: Some(true),
        ..Default::default()
    }
}

fn book_row(order_id: &str, symbol: &str, done: bool, at: &str) -> Order {
    Order {
        order_id: order_id.into(),
        exchange_order_id: None,
        symbol: symbol.into(),
        exchange: "NSE".into(),
        side: "SELL".into(),
        quantity: 1,
        filled_quantity: i32::from(done),
        pending_quantity: i32::from(!done),
        price: 0.0,
        trigger_price: 0.0,
        average_price: if done { BASE_PRICE - 12.0 } else { 0.0 },
        order_type: "MARKET".into(),
        product: "MIS".into(),
        status: if done { "complete" } else { "open" }.into(),
        validity: "DAY".into(),
        order_timestamp: at.into(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }
}

fn ist_now(rig: &Rig) -> String {
    rig.clock
        .now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// The exit order id of a managed leg, once its exit was placed.
fn exit_id(rig: &Rig, symbol: &str) -> Option<String> {
    rig.ctx
        .scalping
        .monitor
        .exit_attempt("live", symbol, "NSE", "MIS")
        .and_then(|a| a.children.first().and_then(|c| c.order_id.clone()))
}

async fn wait_for(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while !f() {
        if t0.elapsed() > limit {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// The standing leg: its exit is placed and stays open at the broker.
async fn standing_leg(rig: &Rig) -> Order {
    rig.set_positions(&[(STANDING.to_string(), 1)]);
    rig.ctx
        .scalping
        .store
        .upsert_sl(&stop_for(STANDING))
        .unwrap();
    rig.ctx.scalping.monitor.request_sync();
    assert!(wait_for(MINUTE, || rig.ctx.scalping.monitor.leg_count() == 1).await);
    let t0 = Instant::now();
    while exit_id(rig, STANDING).is_none() {
        assert!(
            t0.elapsed() < MINUTE,
            "the standing leg's exit was not placed"
        );
        rig.market.send_stamped(STANDING, BASE_PRICE - 15.0);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let row = book_row(
        &exit_id(rig, STANDING).unwrap(),
        STANDING,
        false,
        &ist_now(rig),
    );
    *rig.mock.order_book.lock() = Some(Ok(vec![row.clone()]));
    row
}

/// Five stops saved, breached through the feed, exited, filled and cleared
/// through the order book; the step that did not happen, if one did not.
async fn scalping_round(rig: &Rig, round: u64, standing: &Order) -> Result<(), String> {
    let ctx = &rig.ctx;
    let symbols: Vec<String> = (0..5)
        .map(|i| format!("SYM{:04}", 100 + (round * 5 + i) % 800))
        .collect();
    let before = ctx.scalping.monitor.leg_count();
    let mut held: Vec<(String, i32)> = symbols.iter().map(|s| (s.clone(), 1)).collect();
    held.push((STANDING.to_string(), 1));
    rig.set_positions(&held);
    for s in &symbols {
        ctx.scalping.store.upsert_sl(&stop_for(s)).unwrap();
    }
    ctx.scalping.monitor.request_sync();
    if !wait_for(MINUTE, || ctx.scalping.monitor.leg_count() == before + 5).await {
        return Err(format!(
            "stops not managed ({} legs)",
            ctx.scalping.monitor.leg_count()
        ));
    }
    // The price stays under the stops, as a market that fell through them
    // does: a tick sent before the symbol's subscription reached the broker
    // socket is not delivered, and the next one is.
    let t0 = Instant::now();
    while !symbols.iter().all(|s| exit_id(rig, s).is_some()) {
        if t0.elapsed() > MINUTE {
            return Err("exits not placed".into());
        }
        for s in symbols.iter().filter(|s| exit_id(rig, s).is_none()) {
            rig.market.send_stamped(s, BASE_PRICE - 15.0);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let at = ist_now(rig);
    let mut book = vec![standing.clone()];
    for s in &symbols {
        book.push(book_row(&exit_id(rig, s).unwrap(), s, true, &at));
    }
    *rig.mock.order_book.lock() = Some(Ok(book));
    rig.set_positions(&[(STANDING.to_string(), 1)]);
    let cleared = wait_for(MINUTE, || ctx.scalping.monitor.leg_count() == before).await;
    // The broker's call log is the mock's bookkeeping, not the app's.
    rig.mock.calls.lock().clear();
    if cleared {
        Ok(())
    } else {
        Err(format!(
            "legs not cleared ({} left)",
            ctx.scalping.monitor.leg_count()
        ))
    }
}

/// Three live and three sandbox orders over `/api/v1/placeorder`.
async fn orders_round(rig: &Rig) -> bool {
    let mut ok = true;
    for analyze in [false, true] {
        rig.ctx.sqlite.set_analyze_mode(analyze).unwrap();
        for _ in 0..3 {
            ok &= faults::place_live(rig).await;
        }
    }
    rig.ctx.sqlite.set_analyze_mode(false).unwrap();
    ok
}

/// Requests that did not answer 200, by status (or `error` when no answer
/// came), as `status:count,...`.
#[derive(Default)]
struct Failures(parking_lot::Mutex<std::collections::BTreeMap<String, u64>>);

impl Failures {
    fn note(&self, what: String) {
        *self.0.lock().entry(what).or_insert(0) += 1;
    }

    fn summary(&self) -> String {
        let m = self.0.lock();
        if m.is_empty() {
            return "none".into();
        }
        m.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// `/api/v1` requests at `per_second` until `stop`, over a sample of
/// endpoints, never faster than that after a stall (a burst would trip the
/// 100-a-second limit); the ones that did not answer 200, by status.
fn api_traffic(
    rig: &Rig,
    per_second: u64,
    stop: Arc<AtomicBool>,
) -> (Arc<Failures>, tokio::task::JoinHandle<()>) {
    let failures = Arc::new(Failures::default());
    let (f, base, key) = (failures.clone(), rig.base.clone(), rig.key.clone());
    let task = tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let sym = json!({"symbol": "SYM0005", "exchange": "NSE"});
        let calls: Vec<(&str, Value)> = vec![
            ("ping", json!({})),
            ("funds", json!({})),
            ("orderbook", json!({})),
            ("positionbook", json!({})),
            ("quotes", sym.clone()),
            ("depth", sym.clone()),
            ("symbol", sym),
            ("search", json!({"query": "SYM00", "exchange": "NSE"})),
        ];
        let mut pace = tokio::time::interval(Duration::from_millis(1000 / per_second));
        pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut i = 0usize;
        while !stop.load(Ordering::Relaxed) {
            pace.tick().await;
            let (path, mut body) = calls[i % calls.len()].clone();
            i += 1;
            body["apikey"] = json!(key);
            match http
                .post(format!("{}/api/v1/{}", base, path))
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => f.note(format!("{}_{}", path, r.status().as_u16())),
                Err(e) if e.is_timeout() => f.note(format!("{}_timeout", path)),
                Err(_) => f.note(format!("{}_error", path)),
            }
        }
    });
    (failures, task)
}

/// An SDK client on the 8765 feed holding `symbols`, reading everything.
async fn sdk_client(
    rig: &Rig,
    symbols: &[String],
) -> (Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    let mut c = Client::connect(&rig.ws_url).await;
    let v = c
        .request(json!({"action": "authenticate", "api_key": rig.key}))
        .await;
    assert_eq!(v["status"], "success", "{}", v);
    let list: Vec<_> = symbols
        .iter()
        .map(|s| json!({"symbol": s, "exchange": "NSE"}))
        .collect();
    c.send(&json!({"action": "subscribe", "symbols": list, "mode": 1}))
        .await;
    let frames = Arc::new(AtomicU64::new(0));
    let n = frames.clone();
    let task = tokio::spawn(async move {
        while let Some(v) = c.recv_within(Duration::from_secs(60)).await {
            if v["type"] == "market_data" {
                n.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    (frames, task)
}

/// A passing SDK client and a signed-in Socket.IO browser, each connecting
/// and leaving.
async fn passing_clients(rig: &Rig, round: u64) -> bool {
    let symbols: Vec<String> = (0..50)
        .map(|i| format!("SYM{:04}", (round * 50 + i) % 1000))
        .collect();
    let (frames, task) = sdk_client(rig, &symbols).await;
    let got = wait_for(Duration::from_secs(10), || {
        frames.load(Ordering::Relaxed) > 0
    })
    .await;
    task.abort();
    got && socketio_visit(rig).await
}

/// Engine.IO v4 over WebSocket: open, join the default namespace, leave.
async fn socketio_visit(rig: &Rig) -> bool {
    let url = format!(
        "{}/socket.io/?EIO=4&transport=websocket",
        rig.base.replacen("http", "ws", 1)
    );
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("cookie", rig.cookie.parse().unwrap());
    let Ok((mut ws, _)) = tokio_tungstenite::connect_async(req).await else {
        return false;
    };
    let mut joined = false;
    let _ = ws.send(Message::Text("40".into())).await;
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if let Message::Text(t) = m {
            if t.starts_with("40") {
                joined = true;
                break;
            }
        }
    }
    let _ = ws.send(Message::Text("41".into())).await;
    let _ = ws.close(None).await;
    while let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {}
    joined
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn memory_and_descriptors_stay_flat_through_a_trading_day_with_faults() {
    crate::isolated!(memory_and_descriptors_stay_flat_through_a_trading_day_with_faults);
    let skipped = super::lag_log::install();
    let total = minutes();
    let warm_up = (total / 10).max(3);
    let rig = Rig::start(RigOptions {
        symbols: 1000,
        tick_rate: 1000,
        quiet: vec![STANDING.to_string()],
        trend_per_s: 0.0,
    })
    .await;
    let ctx = rig.ctx.clone();
    // What the app starts beside the servers (lib.rs `start_services`).
    openalgo_desktop_lib::services::monitor::start(&ctx);
    openalgo_desktop_lib::services::health_service::start(&ctx);
    openalgo_desktop_lib::services::lifecycle::wake::start(&ctx);
    ctx.strategy.start().await;

    let held: Vec<String> = (0..200).map(|i| format!("SYM{:04}", i)).collect();
    let (frames, reader) = sdk_client(&rig, &held).await;
    let standing = standing_leg(&rig).await;
    let stop = Arc::new(AtomicBool::new(false));
    let (api_failures, traffic) = api_traffic(&rig, 20, stop.clone());

    let started = Instant::now();
    let mut samples: Vec<Sample> = Vec::new();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut round_failures = Vec::new();
    for minute in 0..total {
        let minute_end = started + MINUTE * (minute as u32 + 1);
        if let Err(e) = scalping_round(&rig, minute, &standing).await {
            round_failures.push(format!("scalping round {}: {}", minute, e));
        }
        if !orders_round(&rig).await {
            round_failures.push(format!("orders round {}", minute));
        }
        if !passing_clients(&rig, minute).await {
            round_failures.push(format!("passing clients {}", minute));
        }
        if minute >= warm_up {
            let fault = ROTATION[(minute - warm_up) as usize % ROTATION.len()];
            let o = faults::inject(&rig, fault, &frames).await;
            record_sample(
                "endurance_fault",
                &[
                    ("minute", minute.to_string()),
                    ("fault", fault.name().to_string()),
                    ("met", o.met.to_string()),
                    ("detail", o.detail.clone()),
                ],
            );
            outcomes.push(o);
        }
        tokio::time::sleep_until(minute_end.into()).await;
        samples.push(sample(&rig, minute + 1).await);
    }
    stop.store(true, Ordering::Relaxed);
    traffic.await.unwrap();
    reader.abort();

    eprintln!(
        "minute  mem_kib  sqlite_kib  fds  pool  tasks  folder_kib  anon_kib  file_kib  \
         shmem_kib  heap_in_use_kib  heap_free_kib  threads"
    );
    for s in &samples {
        let p = s.parts.unwrap_or_default();
        eprintln!(
            "{:>6} {:>8} {:>11} {:>4} {:>5} {:>6} {:>11} {:>9} {:>9} {:>10} {:>16} {:>14} {:>8}",
            s.minute,
            s.mem_kib,
            s.sqlite_kib,
            s.fds,
            s.pool,
            s.tasks,
            s.folder_kib,
            p.anon_kib,
            p.file_kib,
            p.shmem_kib,
            p.heap_in_use_kib,
            p.heap_free_kib,
            s.threads.unwrap_or(-1)
        );
    }
    for o in &outcomes {
        eprintln!(
            "fault {}: {} ({})",
            o.fault.name(),
            if o.met {
                "as expected"
            } else {
                "NOT as expected"
            },
            o.detail
        );
    }
    let warm = samples[warm_up as usize - 1];
    let after = &samples[warm_up as usize..];
    let mem_peak = after
        .iter()
        .map(|s| s.mem_kib)
        .max()
        .unwrap_or(warm.mem_kib);
    let fd_peak = after.iter().map(|s| s.fds).max().unwrap_or(warm.fds);
    let pool_peak = after.iter().map(|s| s.pool).max().unwrap_or(warm.pool);
    let task_peak = after.iter().map(|s| s.tasks).max().unwrap_or(warm.tasks);
    let end = *samples.last().unwrap();
    // A plateau (a cache filling) grows less in the second half after the
    // warm-up than in the first; a leak grows as much.
    let mid = after[after.len() / 2];
    let first_half = mid.mem_kib as i64 - warm.mem_kib as i64;
    let second_half = end.mem_kib as i64 - mid.mem_kib as i64;
    let fd_allowed = ((warm.fds as f64 * FLAT) as usize).max(warm.fds + FD_SLACK)
        + 2 * pool_peak.saturating_sub(warm.pool);
    let mem_flat = mem_peak as f64 <= warm.mem_kib as f64 * FLAT;
    let fd_flat = fd_peak <= fd_allowed;
    let tasks_flat = task_peak <= warm.tasks + TASK_SLACK;
    let faults_met = outcomes.iter().all(|o| o.met);
    let hours = started.elapsed().as_secs_f64() / 3600.0;
    let folder_growth = (end.folder_kib as f64 - warm.folder_kib as f64) / 1024.0;
    // The heap given back once a minute (Linux, glibc): how long each
    // trim held the arenas.
    let mut trims = openalgo_desktop_lib::services::heap::trim_times();
    trims.sort();
    let trim_ms = |d: Option<&Duration>| d.map_or("n/a".to_string(), |d| super::ms(*d));
    let failed_faults: Vec<&str> = outcomes
        .iter()
        .filter(|o| !o.met)
        .map(|o| o.fault.name())
        .collect();
    let met = Report::new("endurance")
        .field("minutes", total)
        .field("warm_up_minutes", warm_up)
        .field("mem_kib_warm", warm.mem_kib)
        .field("mem_kib_peak", mem_peak)
        .field("mem_kib_end", end.mem_kib)
        .field(
            "mem_pct",
            format!(
                "{:+.1}",
                100.0 * (mem_peak as f64 / warm.mem_kib as f64 - 1.0)
            ),
        )
        .field("sqlite_kib_warm", warm.sqlite_kib)
        .field("sqlite_kib_end", end.sqlite_kib)
        .field("mem_kib_first_half", first_half)
        .field("mem_kib_second_half", second_half)
        .field("heap_trims", trims.len())
        .field("heap_trim_median_ms", trim_ms(trims.get(trims.len() / 2)))
        .field("heap_trim_max_ms", trim_ms(trims.last()))
        .field("fds_warm", warm.fds)
        .field("fds_peak", fd_peak)
        .field("pool_warm", warm.pool)
        .field("pool_peak", pool_peak)
        .field("tasks_warm", warm.tasks)
        .field("tasks_peak", task_peak)
        .field(
            "folder_mb_growth_after_warm_up",
            format!("{:.2}", folder_growth),
        )
        .field(
            "folder_mb_per_hour",
            format!(
                "{:.2}",
                folder_growth / (hours - warm_up as f64 / 60.0).max(0.01)
            ),
        )
        .field("faults", outcomes.len())
        .field(
            "faults_not_as_expected",
            if failed_faults.is_empty() {
                "none".into()
            } else {
                failed_faults.join(",")
            },
        )
        .field("activity_failures", round_failures.len())
        .field("api_failures", api_failures.summary())
        .field("ticks_skipped", skipped.summary())
        .verdict(
            "mem_and_fds_within_5pct_after_warm_up",
            mem_flat && fd_flat && tasks_flat,
        );
    rig.stop().await;
    assert!(faults_met, "faults not handled as expected: {:?}", outcomes);
    assert!(
        round_failures.is_empty(),
        "activity failed: {:?}",
        round_failures
    );
    assert!(
        met,
        "not flat after warm-up: memory {} -> peak {} KiB, descriptors {} -> peak {} (allowed {}), tasks {} -> peak {}",
        warm.mem_kib, mem_peak, warm.fds, fd_peak, fd_allowed, warm.tasks, task_peak
    );
}
