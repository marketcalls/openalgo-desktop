//! The app's runner host: one hidden Tauri window per run, loading the runner
//! page from the local server.
//!
//! The window is never shown and is covered by no capability (the capability
//! files name only `main`), so the page in it has no Tauri IPC at all: it
//! talks HTTP to the local server like any other page, and only to the
//! runner's host routes with its own run's secret. Background throttling is
//! turned off where the platform allows it (macOS 14 and later), because a
//! hidden view is otherwise suspended after a few minutes; elsewhere the page
//! holds a Web Lock, and the runner ends a run whose page stops answering.

use super::host::{Launch, RunnerHost};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub struct WindowHost {
    app: AppHandle,
    origin: Box<dyn Fn() -> String + Send + Sync>,
    labels: Mutex<HashMap<String, String>>,
    next: AtomicU64,
}

impl WindowHost {
    /// `origin` answers the server's origin when a page is opened, so a port
    /// change in Settings is followed.
    pub fn new(app: AppHandle, origin: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self {
            app,
            origin: Box::new(origin),
            labels: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }
}

impl RunnerHost for WindowHost {
    fn open(&self, launch: &Launch) -> Result<(), String> {
        let url = format!("{}{}", (self.origin)(), launch.page);
        let url = url::Url::parse(&url).map_err(|e| {
            tracing::error!("The runner page address is not valid: {}", e);
            "The strategy could not be started because its page address is not valid.".to_string()
        })?;
        let label = format!(
            "openscript-runner-{}",
            self.next.fetch_add(1, Ordering::SeqCst)
        );
        WebviewWindowBuilder::new(&self.app, &label, WebviewUrl::External(url))
            .title(&launch.title)
            .visible(false)
            .focused(false)
            .skip_taskbar(true)
            .inner_size(480.0, 320.0)
            .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
            .build()
            .map_err(|e| {
                tracing::error!("Could not open the runner window: {}", e);
                "The strategy could not be started because its window could not be opened. Restart OpenAlgo and try again.".to_string()
            })?;
        self.labels.lock().insert(launch.run_id.clone(), label);
        Ok(())
    }

    fn close(&self, run_id: &str) {
        let label = self.labels.lock().remove(run_id);
        if let Some(label) = label {
            if let Some(w) = self.app.get_webview_window(&label) {
                if let Err(e) = w.destroy() {
                    tracing::warn!("Could not close the runner window {}: {}", label, e);
                }
            }
        }
    }

    fn open_count(&self) -> usize {
        self.labels.lock().len()
    }
}
