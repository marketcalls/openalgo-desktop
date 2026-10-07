//! Process isolation for tests that measure the whole process (descriptor
//! count, RSS). In the single test binary other tests run in parallel
//! threads and would disturb those numbers, so such a test re-runs itself
//! alone in a child process of the same binary.

/// Environment variable naming the one test a child process runs.
const ISOLATED: &str = "OPENALGO_IT_ISOLATED";

/// Returns true when the caller is the isolated child and should run its
/// body. Otherwise runs `test` (the libtest path, such as
/// `feed_hygiene::reconnect_loop_does_not_leak_descriptors_or_memory`) in a
/// child process, fails if the child fails, and returns false.
pub fn run_isolated(test: &str) -> bool {
    if std::env::var(ISOLATED).as_deref() == Ok(test) {
        return true;
    }
    // Descriptors other tests opened without close-on-exec (SQLite and
    // DuckDB files) would otherwise be inherited and counted by the child.
    close_on_exec_all();
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            test,
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "--include-ignored",
        ])
        .env(ISOLATED, test)
        .output()
        .expect("spawn isolated test process");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprint!("{}", stderr);
    assert!(
        out.status.success(),
        "isolated test {} failed\n{}\n{}",
        test,
        stdout,
        stderr
    );
    assert!(
        stdout.contains("1 passed"),
        "isolated test {} did not run\n{}",
        test,
        stdout
    );
    false
}

/// Mark every open descriptor above stderr close-on-exec.
#[cfg(unix)]
fn close_on_exec_all() {
    let fds: Vec<i32> = std::fs::read_dir("/dev/fd")
        .map(|d| {
            d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .filter(|fd| *fd > 2)
                .collect()
        })
        .unwrap_or_default();
    for fd in fds {
        // SAFETY: fcntl on a descriptor number only reads and sets its
        // flags; a number that was closed meanwhile fails with EBADF.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

#[cfg(not(unix))]
fn close_on_exec_all() {}

/// `isolated!(fn_name)` at the top of a test body: the parent returns early
/// once the child process passed; the child runs the body.
#[macro_export]
macro_rules! isolated {
    ($name:ident) => {
        let path = concat!(module_path!(), "::", stringify!($name));
        let path = path.split_once("::").map(|(_, rest)| rest).unwrap_or(path);
        if !$crate::isolate::run_isolated(path) {
            return;
        }
    };
}
