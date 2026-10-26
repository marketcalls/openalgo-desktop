//! Performance and durability benchmarks against the acceptance targets
//! (`docs/audit/perf.md`; the roadmap's "Performance and durability
//! acceptance targets"). Each runs the real services (`AppState`, the HTTP
//! and 8765 feed servers, the event bus, the scalping monitor, Historify)
//! with `MockBroker` and a synthetic broker feed, in a temporary data
//! folder on ephemeral ports, alone in a child process (`isolated!`).
//!
//! All are `#[ignore]`d: they take seconds to minutes and measure the
//! machine, so they never run in the pull-request suite. On demand:
//!
//! ```text
//! cd src-tauri
//! cargo test --locked --test it perf:: -- --ignored --nocapture --test-threads=1
//! ```
//!
//! One benchmark: `perf::tick_to_risk`, `perf::report_load` (tick to
//! decision while the report pages read a large log),
//! `perf::trading_pages` (the same while the trading pages poll a long
//! history), `perf::reconnect`,
//! `perf::sqlite_wait`, `perf::event_burst`, `perf::disk_growth`; the
//! endurance run is `perf::endurance` (15 minutes unless
//! `OPENALGO_PERF_MINUTES` says otherwise). `.github/workflows/perf.yml`
//! runs them nightly on Linux.
//!
//! Every measurement prints one line in a stable format,
//!
//! ```text
//! PERF bench=<name> <key>=<value> ... target=<what> verdict=<pass|miss>
//! ```
//!
//! and, when `OPENALGO_PERF_OUT` names a file, appends the same fields
//! there as one JSON object per line. A benchmark asserts its target after
//! printing, so a miss still leaves its numbers.

#![allow(dead_code)]

mod browsers;
mod disk_growth;
mod endurance;
mod event_burst;
mod faults;
mod lag_log;
mod reconnect;
mod report_load;
mod rig;
mod sqlite_wait;
mod tick_to_risk;
mod trading_pages;

use std::io::Write;
use std::time::Duration;

/// Latency percentiles of one set of samples.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub count: usize,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
    pub mean: Duration,
}

impl Stats {
    /// Nearest-rank percentiles. Panics on no samples: a benchmark that
    /// measured nothing has failed.
    pub fn of(samples: &[Duration]) -> Stats {
        assert!(!samples.is_empty(), "no samples were measured");
        let mut s = samples.to_vec();
        s.sort();
        let rank = |p: f64| {
            let i = ((p / 100.0) * s.len() as f64).ceil() as usize;
            s[i.clamp(1, s.len()) - 1]
        };
        let total: Duration = s.iter().sum();
        Stats {
            count: s.len(),
            p50: rank(50.0),
            p90: rank(90.0),
            p99: rank(99.0),
            max: s[s.len() - 1],
            mean: total / s.len() as u32,
        }
    }
}

/// Milliseconds with two decimals, the unit every latency is printed in.
pub fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

/// One benchmark's result line.
pub struct Report {
    bench: &'static str,
    fields: Vec<(String, String)>,
}

impl Report {
    pub fn new(bench: &'static str) -> Report {
        Report {
            bench,
            fields: Vec::new(),
        }
    }

    pub fn field(mut self, key: &str, value: impl std::fmt::Display) -> Report {
        self.fields.push((key.to_string(), value.to_string()));
        self
    }

    /// `<prefix>_count`, `_p50_ms`, `_p90_ms`, `_p99_ms`, `_max_ms`,
    /// `_mean_ms`.
    pub fn stats(self, prefix: &str, s: &Stats) -> Report {
        self.field(&format!("{}_count", prefix), s.count)
            .field(&format!("{}_p50_ms", prefix), ms(s.p50))
            .field(&format!("{}_p90_ms", prefix), ms(s.p90))
            .field(&format!("{}_p99_ms", prefix), ms(s.p99))
            .field(&format!("{}_max_ms", prefix), ms(s.max))
            .field(&format!("{}_mean_ms", prefix), ms(s.mean))
    }

    /// Print the line (and the JSON record), then return whether the
    /// target was met so the caller can assert it.
    pub fn verdict(self, target: &str, met: bool) -> bool {
        let mut fields = self.fields;
        fields.extend(machine());
        fields.push(("target".into(), target.into()));
        fields.push(("verdict".into(), if met { "pass" } else { "miss" }.into()));
        let line: Vec<String> = fields
            .iter()
            .map(|(k, v)| format!("{}={}", k, v.replace(' ', "_")))
            .collect();
        eprintln!("PERF bench={} {}", self.bench, line.join(" "));
        record(self.bench, &fields);
        met
    }
}

/// Where and how the numbers were taken: the build, the system, its
/// logical CPUs and memory, and its load when the line was printed (the
/// development Mac is shared, so the load says how much else ran).
fn machine() -> Vec<(String, String)> {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    );
    let load = System::load_average();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    vec![
        (
            "build".into(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
        ),
        ("os".into(), std::env::consts::OS.into()),
        ("arch".into(), std::env::consts::ARCH.into()),
        ("cpus".into(), cpus.to_string()),
        (
            "mem_gib".into(),
            format!("{:.1}", sys.total_memory() as f64 / (1u64 << 30) as f64),
        ),
        ("load1".into(), format!("{:.2}", load.one)),
    ]
}

/// The memory the endurance run holds flat, in KiB: the physical
/// footprint on macOS (what Activity Monitor shows: resident plus
/// compressed; macOS compresses idle pages, so the resident set alone
/// moves with no change in use), the resident set elsewhere.
pub fn memory_kib() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: `info` is a valid, writable rusage_info_v2 for the
        // RUSAGE_INFO_V2 flavor; the kernel fills it for this process.
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr() as *mut libc::rusage_info_t,
            )
        };
        if rc == 0 {
            // SAFETY: filled by the successful call above.
            return unsafe { info.assume_init() }.ri_phys_footprint / 1024;
        }
    }
    crate::feed_support::rss_kib()
}

/// What the resident set is made of, in KiB, where Linux and glibc say:
/// anonymous memory (the heap and the stacks), file pages (the test
/// binary's code, read in as code paths first run) and shared memory
/// (SQLite's WAL index) from `/proc/self/status`, and what glibc's
/// allocator holds in use and holds free across all its arenas
/// (`mallinfo2`). Tells a climb in use from memory the allocator or the
/// code pages settle into: the endurance run's resident set grew 5-7% in
/// 15 minutes with descriptors and tasks flat.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryParts {
    pub anon_kib: u64,
    pub file_kib: u64,
    pub shmem_kib: u64,
    pub heap_in_use_kib: u64,
    pub heap_free_kib: u64,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn memory_parts() -> Option<MemoryParts> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    };
    // SAFETY: mallinfo2 takes no arguments and returns a struct by value;
    // it walks glibc's arenas under their own locks.
    let heap = unsafe { libc::mallinfo2() };
    Some(MemoryParts {
        anon_kib: field("RssAnon:"),
        file_kib: field("RssFile:"),
        shmem_kib: field("RssShmem:"),
        heap_in_use_kib: ((heap.uordblks + heap.hblkhd) / 1024) as u64,
        heap_free_kib: (heap.fordblks / 1024) as u64,
    })
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn memory_parts() -> Option<MemoryParts> {
    None
}

/// Every file under `dir`, in bytes.
pub fn folder_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// Append one record to `OPENALGO_PERF_OUT` when it is set (the endurance
/// run's samples go there as they are taken, so a long run can be
/// followed with `tail -f`).
pub fn record_sample(bench: &str, fields: &[(&str, String)]) {
    let owned: Vec<(String, String)> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    record(bench, &owned);
}

/// Append the record to `OPENALGO_PERF_OUT` when it is set.
fn record(bench: &str, fields: &[(String, String)]) {
    let Some(path) = std::env::var_os("OPENALGO_PERF_OUT") else {
        return;
    };
    let mut obj = serde_json::Map::new();
    obj.insert("bench".into(), bench.into());
    for (k, v) in fields {
        obj.insert(k.clone(), v.clone().into());
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{}", serde_json::Value::Object(obj)));
    if let Err(e) = written {
        eprintln!("PERF results not written to {:?}: {}", path, e);
    }
}

#[test]
fn percentiles_use_the_nearest_rank() {
    let samples: Vec<Duration> = (1..=200).map(Duration::from_millis).collect();
    let s = Stats::of(&samples);
    assert_eq!(s.count, 200);
    assert_eq!(s.p50, Duration::from_millis(100));
    assert_eq!(s.p90, Duration::from_millis(180));
    assert_eq!(s.p99, Duration::from_millis(198));
    assert_eq!(s.max, Duration::from_millis(200));
    let one = Stats::of(&[Duration::from_millis(7)]);
    assert_eq!(
        (one.p50, one.p99),
        (Duration::from_millis(7), Duration::from_millis(7))
    );
}
