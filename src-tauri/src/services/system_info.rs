//! Diagnostics page (web `blueprints/admin.py` diagnostics section): the
//! error log views, the system snapshot, connectivity probes and the
//! downloadable report.
//!
//! The snapshot never carries a secret: credentials appear only as
//! set / not set, and there is nothing from the environment except the one
//! development-port switch.

use crate::db::sqlite::monitor::{self as store, ErrorEntry};
use crate::error::Result;
use crate::services::error_log::fingerprint;
use crate::state::{AppState, ServerStatus};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Instant;

pub const MAX_LIMIT: i64 = 200;
pub const LEVELS: &[&str] = &["ERROR", "CRITICAL", "WARNING", "INFO", "DEBUG"];

fn process_start() -> &'static Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now)
}

/// Note the process start (call once at startup; tests may skip it).
pub fn mark_start() {
    let _ = process_start();
}

/// Error entries in the web's shape, timestamps in IST.
fn entry_json(e: &ErrorEntry) -> Value {
    let mut v = e.to_json();
    if let Some(t) = store::parse_ts(&e.ts) {
        v["ts"] = json!(t
            .with_timezone(&Kolkata)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string());
    }
    v
}

fn errors(ctx: &AppState) -> Result<Vec<ErrorEntry>> {
    let _ = crate::services::error_log::flush_queue(ctx);
    let conn = ctx.logs.conn()?;
    store::all_errors(&conn)
}

/// `GET /admin/api/errors` (newest `limit` matching entries, oldest first).
pub fn errors_list(
    ctx: &AppState,
    limit: i64,
    level: Option<&str>,
    q: Option<&str>,
) -> Result<Value> {
    let all = errors(ctx)?;
    let q = q.map(|s| s.to_lowercase());
    let mut out = Vec::new();
    let mut scanned = 0;
    for e in all.iter().rev() {
        scanned += 1;
        if let Some(l) = level {
            if e.level != l {
                continue;
            }
        }
        if let Some(q) = &q {
            let exc = e
                .exception
                .as_ref()
                .map(|v| v.to_string().to_lowercase())
                .unwrap_or_default();
            if !e.message.to_lowercase().contains(q) && !exc.contains(q) {
                continue;
            }
        }
        out.push(entry_json(e));
        if out.len() as i64 >= limit {
            break;
        }
    }
    out.reverse();
    Ok(json!({
        "status": "success",
        "data": out,
        "count": out.len(),
        "scanned": scanned,
        "total_in_window": all.len(),
    }))
}

fn summary(all: &[ErrorEntry], now: DateTime<Utc>) -> (BTreeMap<String, i64>, i64, i64) {
    let mut by_level = BTreeMap::new();
    let (mut d1, mut h1) = (0, 0);
    for e in all {
        *by_level.entry(e.level.clone()).or_insert(0) += 1;
        if let Some(t) = store::parse_ts(&e.ts) {
            if t >= now - Duration::hours(24) {
                d1 += 1;
            }
            if t >= now - Duration::hours(1) {
                h1 += 1;
            }
        }
    }
    (by_level, d1, h1)
}

/// `GET /admin/api/errors/stats`.
pub fn errors_stats(ctx: &AppState) -> Result<Value> {
    let all = errors(ctx)?;
    let (by_level, d1, h1) = summary(&all, ctx.now());
    Ok(json!({
        "status": "success",
        "total": all.len(),
        "by_level": by_level,
        "last_24h": d1,
        "last_1h": h1,
    }))
}

/// `GET /admin/api/errors/groups`.
pub fn errors_groups(ctx: &AppState, limit: i64) -> Result<Value> {
    let all = errors(ctx)?;
    let mut groups: BTreeMap<String, (i64, ErrorEntry, String, String)> = BTreeMap::new();
    for e in &all {
        let fp = fingerprint(e);
        let g = groups
            .entry(fp)
            .or_insert_with(|| (0, e.clone(), e.ts.clone(), e.ts.clone()));
        g.0 += 1;
        if e.ts < g.2 {
            g.2 = e.ts.clone();
        }
        if e.ts >= g.3 {
            g.3 = e.ts.clone();
            g.1 = e.clone();
        }
    }
    let total_groups = groups.len();
    let ist = |ts: &str| {
        store::parse_ts(ts)
            .map(|t| {
                t.with_timezone(&Kolkata)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| ts.to_string())
    };
    let mut ordered: Vec<(String, (i64, ErrorEntry, String, String))> =
        groups.into_iter().collect();
    ordered.sort_by(|a, b| (b.1 .0, &b.1 .3).cmp(&(a.1 .0, &a.1 .3)));
    let list: Vec<Value> = ordered
        .into_iter()
        .take(limit as usize)
        .map(|(fp, (count, sample, first, last))| {
            json!({
                "fingerprint": fp,
                "count": count,
                "level": sample.level,
                "logger": sample.logger,
                "module": sample.module,
                "first_seen": ist(&first),
                "last_seen": ist(&last),
                "sample": entry_json(&sample),
            })
        })
        .collect();
    Ok(json!({
        "status": "success",
        "groups": list,
        "total_entries": all.len(),
        "total_groups": total_groups,
    }))
}

fn db_snapshot(ctx: &AppState) -> Vec<Value> {
    [
        ("openalgo", "openalgo.db"),
        ("logs", "logs.db"),
        ("historify", "historify.duckdb"),
    ]
    .iter()
    .map(|(name, file)| {
        let p = ctx.data_dir.join(file);
        match std::fs::metadata(&p) {
            Ok(md) => json!({
                "name": name,
                "exists": true,
                "size_mb": (md.len() as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0,
                "modified": md.modified().ok().map(|m| {
                    DateTime::<Utc>::from(m).with_timezone(&Kolkata).format("%Y-%m-%d %H:%M:%S").to_string()
                }),
            }),
            Err(_) => json!({"name": name, "exists": false, "size_mb": 0, "modified": Value::Null}),
        }
    })
    .collect()
}

fn disk_for(path: &std::path::Path) -> Value {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let best = disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());
    match best {
        Some(d) if d.total_space() > 0 => {
            let gb = |b: u64| (b as f64 / 1e9 * 100.0).round() / 100.0;
            json!({
                "total_gb": gb(d.total_space()),
                "free_gb": gb(d.available_space()),
                "used_percent": ((1.0 - d.available_space() as f64 / d.total_space() as f64) * 1000.0).round() / 10.0,
            })
        }
        _ => Value::Null,
    }
}

fn hardware(ctx: &AppState) -> Value {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    let mb = |b: u64| (b / (1024 * 1024)) as i64;
    let total = sys.total_memory();
    let disk = disk_for(&ctx.data_dir);
    json!({
        "cpu_count": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "cpu_model": sys.cpus().first().map(|c| c.brand().trim().to_string()).filter(|s| !s.is_empty()),
        "memory_total_mb": mb(total),
        "memory_available_mb": mb(sys.available_memory()),
        "memory_percent": if total > 0 {
            ((1.0 - sys.available_memory() as f64 / total as f64) * 1000.0).round() / 10.0
        } else { 0.0 },
        "disk_log": disk.clone(),
        "disk_db": disk,
    })
}

fn is_raspberry_pi() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::fs::read_to_string("/proc/device-tree/model")
        .ok()
        .map(|m| m.trim_end_matches('\0').trim().to_string())
        .filter(|m| m.contains("Raspberry Pi"))
}

fn listeners(ctx: &AppState) -> Vec<Value> {
    let cfg = ctx.server_config();
    let http = match &*ctx.server_status.read() {
        ServerStatus::Running { host, port } => {
            json!({"name": "App and API", "address": format!("{}:{}", host, port), "state": "running"})
        }
        ServerStatus::PortInUse { port, .. } => {
            json!({"name": "App and API", "address": format!("{}:{}", cfg.bind_host, port), "state": "port in use"})
        }
        ServerStatus::Failed { .. } => {
            json!({"name": "App and API", "address": format!("{}:{}", cfg.bind_host, cfg.http_port), "state": "stopped"})
        }
        ServerStatus::Starting => {
            json!({"name": "App and API", "address": format!("{}:{}", cfg.bind_host, cfg.http_port), "state": "starting"})
        }
    };
    vec![
        http,
        json!({"name": "Market data feed", "address": format!("{}:{}", cfg.bind_host, cfg.ws_port), "state": "configured"}),
    ]
}

/// `GET /admin/api/system` data.
pub fn system_payload(
    ctx: &AppState,
    active_broker: Option<String>,
    user_logged_in: bool,
) -> Value {
    let cfg = ctx.server_config();
    // The feed's limits: `feed::config_from` builds the running feed on these
    // defaults and sets only its address and handshake policy.
    let feed = crate::feed::FeedConfig::default();
    let analyze = ctx.sqlite.get_analyze_mode().ok();
    let distro = if cfg!(target_os = "linux") {
        json!({
            "name": sysinfo::System::name(),
            "id": sysinfo::System::distribution_id(),
            "version_id": sysinfo::System::os_version(),
        })
    } else {
        Value::Null
    };
    let rpi = is_raspberry_pi();
    let now = ctx.now();
    let creds = ctx
        .sqlite
        .conn()
        .ok()
        .and_then(|c| {
            let mut st = c
                .prepare("SELECT broker_id FROM broker_credentials ORDER BY broker_id")
                .ok()?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))
                .ok()?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            Some(v)
        })
        .unwrap_or_default();
    let mut secrets = Map::new();
    secrets.insert("Broker credentials saved".into(), json!(!creds.is_empty()));
    secrets.insert(
        "API key created".into(),
        json!(crate::services::apikey_service::ApiKeyService::current(ctx)
            .ok()
            .flatten()
            .is_some()),
    );
    let key_mode = ctx.security.mode();
    let mut strength = Map::new();
    strength.insert(
        "Keys held in the OS keychain".into(),
        json!(matches!(key_mode, crate::security::KeyMode::Keychain)),
    );
    json!({
        "mode": {
            "analyze_mode": analyze,
            "label": match analyze { Some(true) => "ANALYZE", Some(false) => "LIVE", None => "UNKNOWN" },
        },
        "host": {
            "system": std::env::consts::OS,
            "release": sysinfo::System::kernel_version(),
            "version": sysinfo::System::long_os_version(),
            "machine": std::env::consts::ARCH,
            "platform": format!("{} {}", sysinfo::System::long_os_version().unwrap_or_default(), std::env::consts::ARCH).trim().to_string(),
            "distro": distro,
            "in_docker": false,
            "is_raspberry_pi": rpi.is_some(),
            "rpi_model": rpi,
            "is_termux": false,
            "is_android": false,
        },
        "runtime": {
            "wsgi_hint": "desktop",
            "worker_class": "OpenAlgo Desktop",
            "process_uptime_seconds": process_start().elapsed().as_secs(),
            "active_threads": crate::services::health_service::process_stats().threads,
            "background_tasks": ctx.task_count(),
            "listeners": listeners(ctx),
            "notes": [],
        },
        "hardware": hardware(ctx),
        "build": {
            "openalgo_version": env!("CARGO_PKG_VERSION"),
            "openalgo_sdk_version": Value::Null,
            "git_branch": Value::Null,
            "git_commit": Value::Null,
            "frontend_build_time": Value::Null,
        },
        "config": {
            "valid_brokers": ctx.brokers.ids(),
            "log_level": if cfg!(debug_assertions) { "DEBUG" } else { "INFO" },
            "log_to_file": false,
            "log_dir": "",
            "websocket_host": cfg.bind_host,
            "websocket_port": cfg.ws_port.to_string(),
            // The web's two keys describe its broker adapter's env defaults
            // (1000 symbols per upstream socket, 3 sockets). The Diagnostics
            // page shows them, so they carry what the desktop's feed
            // enforces: subscriptions (instrument and mode pairs) per client,
            // and client connections, this computer's pool plus the network's.
            "max_symbols_per_websocket": feed.max_subscriptions_per_client.to_string(),
            "max_websocket_connections": (feed.local_connections + feed.max_connections).to_string(),
            "feed_max_subscriptions_per_client": feed.max_subscriptions_per_client,
            "feed_max_connections_this_computer": feed.local_connections,
            "feed_max_connections_network": feed.max_connections,
            "api_rate_limit": "100 per second",
            "flask_debug": false,
            "secrets_present": secrets,
            "secret_strength": strength,
        },
        "brokers": {
            "configured_brokers": creds,
            "active_broker": active_broker,
            "user_logged_in": user_logged_in,
        },
        "databases": db_snapshot(ctx),
        "time": {
            "server_time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "server_tz": chrono::Local::now().format("%:z").to_string(),
            "ist_time": now.with_timezone(&Kolkata).format("%Y-%m-%d %H:%M:%S IST").to_string(),
        },
    })
}

fn check(name: &str, f: impl FnOnce() -> std::result::Result<String, String>) -> Value {
    let t = Instant::now();
    match f() {
        Ok(detail) => {
            json!({"name": name, "ok": true, "ms": (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0, "detail": detail})
        }
        Err(detail) => json!({"name": name, "ok": false, "ms": Value::Null, "detail": detail}),
    }
}

fn tcp_probe(name: &str, addr: std::net::SocketAddr) -> Value {
    check(name, || {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
            .map(|_| "Connected".to_string())
            .map_err(|_| "Nothing is answering on this port.".to_string())
    })
}

/// `POST /admin/api/system/diagnostics` checks (fixed targets only).
pub fn diagnostics(ctx: &AppState) -> Value {
    let mut checks = vec![
        check("Database read (openalgo.db)", || {
            ctx.sqlite
                .conn()
                .and_then(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                .map(|_| "OK".to_string())
                .map_err(|_| "The main database could not be read.".to_string())
        }),
        check("Database read (logs.db)", || {
            ctx.logs
                .conn()
                .and_then(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                .map(|_| "OK".to_string())
                .map_err(|_| "The logs database could not be read.".to_string())
        }),
    ];
    let cfg = ctx.server_config();
    let port = ctx.listening_port();
    if let Ok(a) = format!("127.0.0.1:{}", port).parse() {
        checks.push(tcp_probe("App and API listener", a));
    }
    if let Ok(a) = format!("127.0.0.1:{}", cfg.ws_port).parse() {
        checks.push(tcp_probe("Market data feed listener", a));
    }
    checks.push(match ctx.get_broker_session() {
        Some(b) => json!({"name": format!("Broker session ({})", b.broker_id), "ok": true, "ms": Value::Null, "detail": "Connected"}),
        None => json!({"name": "Broker session", "ok": false, "ms": Value::Null, "detail": "No broker is connected. Sign in to your broker from the Broker page."}),
    });
    json!({
        "status": "success",
        "ran_at": ctx.now().with_timezone(&Kolkata).format("%Y-%m-%d %H:%M:%S").to_string(),
        "checks": checks,
    })
}

fn kv(md: bool, label: &str, v: &Value) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let s = if s.is_empty() {
        "not set".to_string()
    } else {
        s
    };
    if md {
        format!("- **{}:** {}", label, s)
    } else {
        format!("{}: {}", label, s)
    }
}

type Section<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// The downloadable report (web `_render_report`), capped at 1 MB.
pub fn render_report(ctx: &AppState, payload: &Value, md: bool) -> Result<String> {
    let all = errors(ctx)?;
    let (by_level, d1, h1) = summary(&all, ctx.now());
    let h1s = if md { "# " } else { "" };
    let h2 = if md { "## " } else { "" };
    let bullet = if md { "- " } else { "  - " };
    let mut l: Vec<String> = Vec::new();
    l.push(format!("{}OpenAlgo Desktop System Report", h1s));
    l.push(String::new());
    l.push(kv(md, "Generated", &payload["time"]["ist_time"]));
    l.push(String::new());
    let sections: &[Section] = &[
        ("Trading Mode", "mode", &[("Mode", "label")]),
        (
            "Host",
            "host",
            &[
                ("System", "system"),
                ("Release", "release"),
                ("Version", "version"),
                ("Machine", "machine"),
                ("Raspberry Pi", "rpi_model"),
            ],
        ),
        (
            "Runtime",
            "runtime",
            &[
                ("Process uptime (s)", "process_uptime_seconds"),
                ("Process threads", "active_threads"),
                ("Background tasks", "background_tasks"),
            ],
        ),
        (
            "Hardware",
            "hardware",
            &[
                ("CPU count", "cpu_count"),
                ("CPU model", "cpu_model"),
                ("Memory total (MB)", "memory_total_mb"),
                ("Memory available (MB)", "memory_available_mb"),
                ("Memory used (%)", "memory_percent"),
            ],
        ),
        (
            "Build",
            "build",
            &[("OpenAlgo Desktop", "openalgo_version")],
        ),
        (
            "Brokers",
            "brokers",
            &[
                ("Active broker", "active_broker"),
                ("User logged in", "user_logged_in"),
            ],
        ),
        (
            "Time",
            "time",
            &[
                ("Local time", "server_time"),
                ("IST time", "ist_time"),
                ("Local offset", "server_tz"),
            ],
        ),
    ];
    for (title, key, fields) in sections {
        l.push(format!("{}{}", h2, title));
        for (label, f) in *fields {
            l.push(kv(md, label, &payload[*key][*f]));
        }
        if *key == "runtime" {
            if let Some(ls) = payload["runtime"]["listeners"].as_array() {
                for x in ls {
                    l.push(format!(
                        "{}{}: {} ({})",
                        bullet,
                        x["name"].as_str().unwrap_or_default(),
                        x["address"].as_str().unwrap_or_default(),
                        x["state"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        if *key == "hardware" {
            if let Some(d) = payload["hardware"]["disk_db"].as_object() {
                l.push(kv(
                    md,
                    "Disk (data folder)",
                    &json!(format!("{} GB free of {} GB", d["free_gb"], d["total_gb"])),
                ));
            }
        }
        l.push(String::new());
    }
    l.push(format!("{}Databases", h2));
    for db in payload["databases"].as_array().cloned().unwrap_or_default() {
        if db["exists"].as_bool().unwrap_or(false) {
            l.push(format!(
                "{}{}: {} MB (modified {})",
                bullet,
                db["name"].as_str().unwrap_or_default(),
                db["size_mb"],
                db["modified"].as_str().unwrap_or_default()
            ));
        } else {
            l.push(format!(
                "{}{}: missing",
                bullet,
                db["name"].as_str().unwrap_or_default()
            ));
        }
    }
    l.push(String::new());
    l.push(format!("{}Errors summary", h2));
    l.push(kv(md, "Total in window", &json!(all.len())));
    l.push(kv(md, "Last 24h", &json!(d1)));
    l.push(kv(md, "Last 1h", &json!(h1)));
    for (lvl, n) in &by_level {
        l.push(format!("{}{}: {}", bullet, lvl, n));
    }
    l.push(String::new());
    if !all.is_empty() {
        l.push(format!("{}Recent errors (latest first, max 50)", h2));
        l.push(String::new());
        for e in all.iter().rev().take(50) {
            let v = entry_json(e);
            let msg: String = e.message.chars().take(500).collect();
            let ts = v["ts"].as_str().unwrap_or("?").to_string();
            let module = e.module.clone().unwrap_or_else(|| "?".into());
            l.push(if md {
                format!(
                    "{}`{}` **{}** in `{}`: {}",
                    bullet, ts, e.level, module, msg
                )
            } else {
                format!("  - [{}] {} in {}: {}", ts, e.level, module, msg)
            });
        }
        l.push(String::new());
    }
    let mut body = l.join("\n");
    if body.len() > 1_000_000 {
        let mut end = 1_000_000;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("\n\n...[report truncated]\n");
    }
    Ok(body)
}
