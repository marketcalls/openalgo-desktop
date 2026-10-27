//! Freed memory given back to the operating system (Linux with glibc).
//!
//! glibc keeps the memory the app frees in its allocator's arenas (one per
//! group of threads) and returns only what sits free at the top of each
//! heap, so pages left between live allocations stay resident. Over the
//! endurance run (180 minutes of a trading day with faults) the resident
//! set climbed about 3.5 MiB an hour without settling, while the memory in
//! use stayed flat: in use 21.4 to 25.3 MiB, held free 20.2 to 31.5 MiB,
//! code pages flat. `malloc_trim(0)` gives every whole free page of every
//! arena back. The macOS and Windows allocators return freed memory
//! themselves.

use std::collections::VecDeque;
use std::time::Duration;

/// How long the last trims took, newest last: a day of them at one a
/// minute.
static TRIMS: parking_lot::Mutex<VecDeque<Duration>> = parking_lot::const_mutex(VecDeque::new());
const TRIMS_KEPT: usize = 1440;

/// Give the heap's free pages back to the system. Called once a minute,
/// on the blocking pool beside the health sample (`health_service`),
/// never on the async runtime: it walks every arena under its lock.
pub fn give_back_free_memory() {
    let started = std::time::Instant::now();
    if !trim() {
        return;
    }
    let mut trims = TRIMS.lock();
    if trims.len() == TRIMS_KEPT {
        trims.pop_front();
    }
    trims.push_back(started.elapsed());
}

/// How long each of the last trims took (the endurance run reports them).
pub fn trim_times() -> Vec<Duration> {
    TRIMS.lock().iter().copied().collect()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() -> bool {
    // SAFETY: malloc_trim takes no pointers; it walks glibc's own arenas
    // under their locks and releases pages that hold no live allocation.
    unsafe {
        libc::malloc_trim(0);
    }
    true
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim() -> bool {
    false
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;

    /// The resident anonymous memory of this process, in KiB.
    fn anon_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("RssAnon:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap()
    }

    /// Memory freed between allocations that stay live stays resident:
    /// glibc does not return it on its own (the endurance run's climb).
    /// Given back, it leaves the resident set. In a thread of its own, so
    /// the blocks share one arena: 64 MiB in 4 KiB blocks, every 32nd kept.
    #[test]
    fn freed_memory_between_live_blocks_goes_back_to_the_system() {
        let (returned_by_free, given_back) = std::thread::spawn(|| {
            let mut blocks: Vec<Vec<u8>> = (0..16_384).map(|i| vec![i as u8; 4096]).collect();
            let kept: Vec<Vec<u8>> = blocks
                .iter_mut()
                .enumerate()
                .filter(|(i, _)| i % 32 == 31)
                .map(|(_, b)| std::mem::take(b))
                .collect();
            let full = anon_kib();
            drop(blocks);
            let freed = anon_kib();
            give_back_free_memory();
            let given = anon_kib();
            drop(kept);
            (full.saturating_sub(freed), freed.saturating_sub(given))
        })
        .join()
        .unwrap();
        assert!(
            returned_by_free < 16 * 1024,
            "freeing alone returned {} KiB: glibc gives such pages back on its own",
            returned_by_free
        );
        assert!(
            given_back > 32 * 1024,
            "only {} KiB of about 62 MiB freed went back",
            given_back
        );
    }
}
