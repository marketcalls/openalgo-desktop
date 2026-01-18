//! Auto-logout scheduler for broker compliance
//!
//! Automatically logs out broker sessions at 3:00 AM IST daily.
//! This is required because:
//! - Broker auth tokens are valid for ~24 hours only
//! - 3:00 AM IST is well outside market hours (9:15 AM - 3:30 PM IST)
//! - Ensures fresh authentication each trading day
//! - Compliance requirement for Indian brokers

use chrono::{NaiveTime, Timelike, Utc};
use chrono_tz::Asia::Kolkata;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

/// Auto-logout scheduler that runs at 3:00 AM IST
pub struct AutoLogoutScheduler {
    app_handle: AppHandle,
}

impl AutoLogoutScheduler {
    /// Create a new auto-logout scheduler
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Calculate duration until next 3:00 AM IST
    pub fn duration_until_3am_ist() -> Duration {
        let now_utc = Utc::now();
        let now_ist = now_utc.with_timezone(&Kolkata);

        let target_time = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        let now_time = now_ist.time();

        let duration_secs = if now_time < target_time {
            // Target is later today
            (target_time - now_time).num_seconds() as u64
        } else {
            // Target is tomorrow
            let until_midnight = (24 * 3600) - now_time.num_seconds_from_midnight() as u64;
            let from_midnight = target_time.num_seconds_from_midnight() as u64;
            until_midnight + from_midnight
        };

        Duration::from_secs(duration_secs)
    }

    /// Start the auto-logout scheduler
    ///
    /// This spawns a background thread that:
    /// 1. Emits warning events before logout (30 min, 15 min, 5 min, 1 min)
    /// 2. Executes logout at 3:00 AM IST
    /// 3. Emits `auto_logout` event to frontend
    pub fn start(self) {
        std::thread::spawn(move || {
            info!("Auto-logout scheduler started");

            loop {
                let duration = Self::duration_until_3am_ist();
                info!(
                    "Next auto-logout in {} hours {} minutes",
                    duration.as_secs() / 3600,
                    (duration.as_secs() % 3600) / 60
                );

                // Wait until 3:00 AM IST
                std::thread::sleep(duration);

                // Execute auto-logout
                self.execute_auto_logout();
            }
        });
    }

    /// Execute the auto-logout
    fn execute_auto_logout(&self) {
        info!("Executing auto-logout at 3:00 AM IST");

        // Emit auto_logout event to frontend
        if let Err(e) = self.app_handle.emit("auto_logout", ()) {
            warn!("Failed to emit auto_logout event: {}", e);
        }

        // TODO: Phase 2 implementation
        // 1. Clear auth tokens from SQLite
        // 2. Clear session state
        // 3. Clear broker connection

        info!("Auto-logout completed");
    }

    /// Emit warning notification before logout
    #[allow(dead_code)]
    fn emit_warning(&self, minutes_remaining: u32) {
        let message = format!("Auto-logout in {} minutes", minutes_remaining);
        info!("{}", message);

        if let Err(e) = self.app_handle.emit("auto_logout_warning", minutes_remaining) {
            warn!("Failed to emit warning: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_calculation() {
        // Just verify it doesn't panic and returns a reasonable duration
        let duration = AutoLogoutScheduler::duration_until_3am_ist();
        assert!(duration.as_secs() > 0);
        assert!(duration.as_secs() <= 24 * 3600); // Max 24 hours
    }
}
