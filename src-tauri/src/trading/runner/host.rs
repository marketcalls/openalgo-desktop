//! Where a run's engine lives: a page the shell opens and closes.
//!
//! The runner owns the lifecycle and the host only does the two things only a
//! shell can: open the runner page for a run, and close it. In the app the
//! host is a hidden Tauri window (`window.rs`, behind the `runner-window`
//! feature); tests and headless runs use [`RecordingHost`], which opens
//! nothing and records what it was asked, so a test can play the page itself
//! over HTTP.

use parking_lot::Mutex;
use std::collections::BTreeSet;

/// One page to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub run_id: String,
    /// The page's path and fragment on the local server, for example
    /// `/openscript-runner.html#run=<id>&token=<secret>`. The secret is in
    /// the fragment, which a browser never sends to a server or a log.
    pub page: String,
    /// A short title for the window (never shown while hidden).
    pub title: String,
}

/// What opens and closes runner pages.
pub trait RunnerHost: Send + Sync {
    /// Open the page. An error is a sentence for the trader.
    fn open(&self, launch: &Launch) -> Result<(), String>;
    /// Close the page for this run, if one is open. Never fails.
    fn close(&self, run_id: &str);
    /// How many runner pages are open right now.
    fn open_count(&self) -> usize;
}

/// A host that opens nothing and remembers what it was asked.
#[derive(Default)]
pub struct RecordingHost {
    open: Mutex<BTreeSet<String>>,
    launches: Mutex<Vec<Launch>>,
    refuse: Mutex<Option<String>>,
}

impl RecordingHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every launch so far, oldest first.
    pub fn launches(&self) -> Vec<Launch> {
        self.launches.lock().clone()
    }

    /// The newest launch for one run.
    pub fn last_launch(&self, run_id: &str) -> Option<Launch> {
        self.launches
            .lock()
            .iter()
            .rev()
            .find(|l| l.run_id == run_id)
            .cloned()
    }

    /// Make the next open fail with this sentence.
    pub fn refuse_next(&self, message: &str) {
        *self.refuse.lock() = Some(message.to_string());
    }

    pub fn is_open(&self, run_id: &str) -> bool {
        self.open.lock().contains(run_id)
    }
}

impl RunnerHost for RecordingHost {
    fn open(&self, launch: &Launch) -> Result<(), String> {
        if let Some(m) = self.refuse.lock().take() {
            return Err(m);
        }
        self.launches.lock().push(launch.clone());
        self.open.lock().insert(launch.run_id.clone());
        Ok(())
    }

    fn close(&self, run_id: &str) {
        self.open.lock().remove(run_id);
    }

    fn open_count(&self) -> usize {
        self.open.lock().len()
    }
}

/// The run id and token a launch's page reads from its fragment.
pub fn fragment_of(page: &str) -> Option<(String, String)> {
    let frag = page.split_once('#')?.1;
    let mut run = None;
    let mut token = None;
    for kv in frag.split('&') {
        match kv.split_once('=') {
            Some(("run", v)) => run = urlencoding::decode(v).ok().map(|s| s.into_owned()),
            Some(("token", v)) => token = Some(v.to_string()),
            _ => {}
        }
    }
    Some((run?, token?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_refuses() {
        let h = RecordingHost::new();
        let l = Launch {
            run_id: "openscript_a".into(),
            page: "/openscript-runner.html#run=openscript_a&token=abc".into(),
            title: "a".into(),
        };
        h.refuse_next("no");
        assert_eq!(h.open(&l), Err("no".into()));
        assert_eq!(h.open_count(), 0);
        h.open(&l).unwrap();
        assert!(h.is_open("openscript_a"));
        assert_eq!(
            fragment_of(&h.last_launch("openscript_a").unwrap().page),
            Some(("openscript_a".into(), "abc".into()))
        );
        h.close("openscript_a");
        assert_eq!(h.open_count(), 0);
    }
}
