//! Auto-logout scheduler for broker compliance
//!
//! Automatically logs out broker sessions at a configurable time (default 3:00 AM IST).
//! This is required because:
//! - Broker auth tokens are valid for ~24 hours only
//! - Default 3:00 AM IST is well outside market hours (9:15 AM - 3:30 PM IST)
//! - Ensures fresh authentication each trading day
//! - Compliance requirement for Indian brokers
//!
//! Configuration is stored in SQLite settings table and can be changed via GUI.

use crate::db::sqlite::AutoLogoutConfig;
use crate::state::AppState;
use chrono::{NaiveTime, Timelike, Utc};
use chrono_tz::Asia::Kolkata;
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tracing::{error, info, warn};

/// Default warning schedule (minutes before logout)
const DEFAULT_WARNINGS: &[u32] = &[30, 15, 5, 1];

/// Default logout time
const DEFAULT_HOUR: u32 = 3;
const DEFAULT_MINUTE: u32 = 0;

/// Auto-logout event payload
#[derive(Clone, Serialize)]
pub struct AutoLogoutEvent {
    pub reason: String,
    pub timestamp: String,
}

/// Warning event payload
#[derive(Clone, Serialize)]
pub struct WarningEvent {
    pub minutes_remaining: u32,
    pub message: String,
}

/// Auto-logout scheduler that runs at configurable time (default 3:00 AM IST)
pub struct AutoLogoutScheduler {
    app_handle: AppHandle,
}

impl AutoLogoutScheduler {
    /// Create a new auto-logout scheduler
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Get auto-logout configuration from database
    fn get_config(&self) -> AutoLogoutConfig {
        match self.app_handle.try_state::<AppState>() {
            Some(state) => {
                match state.sqlite.get_auto_logout_config() {
                    Ok(config) => config,
                    Err(e) => {
                        warn!("Failed to get auto-logout config, using defaults: {}", e);
                        AutoLogoutConfig {
                            enabled: true,
                            hour: DEFAULT_HOUR,
                            minute: DEFAULT_MINUTE,
                            warnings: DEFAULT_WARNINGS.to_vec(),
                        }
                    }
                }
            }
            None => {
                warn!("AppState not available, using default auto-logout config");
                AutoLogoutConfig {
                    enabled: true,
                    hour: DEFAULT_HOUR,
                    minute: DEFAULT_MINUTE,
                    warnings: DEFAULT_WARNINGS.to_vec(),
                }
            }
        }
    }

    /// Calculate duration until a specific time IST (hours, minutes)
    fn duration_until_time_ist(hour: u32, minute: u32) -> Duration {
        let now_utc = Utc::now();
        let now_ist = now_utc.with_timezone(&Kolkata);

        let target_time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap();
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

    /// Calculate duration until next 3:00 AM IST (for backwards compatibility)
    pub fn duration_until_3am_ist() -> Duration {
        Self::duration_until_time_ist(DEFAULT_HOUR, DEFAULT_MINUTE)
    }

    /// Start the auto-logout scheduler
    ///
    /// This spawns a background thread that:
    /// 1. Reads configuration from database
    /// 2. Emits warning events before logout (configurable intervals)
    /// 3. Clears auth tokens from SQLite at configured time
    /// 4. Clears session state
    /// 5. Emits `auto_logout` event to frontend
    pub fn start(self) {
        std::thread::spawn(move || {
            info!("Auto-logout scheduler started");

            loop {
                // Read config from database each cycle (allows runtime changes)
                let config = self.get_config();

                if !config.enabled {
                    info!("Auto-logout is disabled, sleeping for 1 hour before re-checking");
                    std::thread::sleep(Duration::from_secs(3600));
                    continue;
                }

                // Calculate time until configured logout time
                let duration_to_logout = Self::duration_until_time_ist(config.hour, config.minute);
                let minutes_to_logout = duration_to_logout.as_secs() / 60;

                info!(
                    "Next auto-logout at {:02}:{:02} IST in {} hours {} minutes",
                    config.hour,
                    config.minute,
                    minutes_to_logout / 60,
                    minutes_to_logout % 60
                );

                // Emit warnings at configured intervals
                self.schedule_warnings(duration_to_logout, &config.warnings, config.hour, config.minute);

                // Re-check if still enabled before executing
                let config = self.get_config();
                if config.enabled {
                    // Execute auto-logout
                    self.execute_auto_logout(config.hour, config.minute);
                }
            }
        });
    }

    /// Schedule and emit warnings before logout
    fn schedule_warnings(&self, duration_to_logout: Duration, warnings: &[u32], hour: u32, minute: u32) {
        let minutes_to_logout = duration_to_logout.as_secs() / 60;

        // Sort warnings in descending order (30, 15, 5, 1)
        let mut sorted_warnings = warnings.to_vec();
        sorted_warnings.sort_by(|a, b| b.cmp(a));

        for warning_minutes in sorted_warnings {
            if minutes_to_logout > warning_minutes as u64 {
                // Calculate how long to sleep before this warning
                let sleep_minutes = minutes_to_logout - warning_minutes as u64;
                let sleep_duration = Duration::from_secs(sleep_minutes * 60);

                info!(
                    "Sleeping {} minutes until {} minute warning",
                    sleep_minutes, warning_minutes
                );

                std::thread::sleep(sleep_duration);

                // Emit warning
                self.emit_warning(warning_minutes);

                // Update minutes_to_logout for next iteration
                let minutes_to_logout = Self::duration_until_time_ist(hour, minute).as_secs() / 60;
                if minutes_to_logout == 0 {
                    break;
                }
            }
        }

        // Sleep until exactly the target time
        let remaining = Self::duration_until_time_ist(hour, minute);
        if remaining.as_secs() > 0 {
            info!("Sleeping {} seconds until {:02}:{:02} IST", remaining.as_secs(), hour, minute);
            std::thread::sleep(remaining);
        }
    }

    /// Emit warning notification before logout
    fn emit_warning(&self, minutes_remaining: u32) {
        let message = format!(
            "Auto-logout in {} minute{}. Broker session will be cleared for compliance.",
            minutes_remaining,
            if minutes_remaining == 1 { "" } else { "s" }
        );

        info!("{}", message);

        let event = WarningEvent {
            minutes_remaining,
            message,
        };

        if let Err(e) = self.app_handle.emit("auto_logout_warning", event) {
            warn!("Failed to emit warning: {}", e);
        }
    }

    /// Execute the auto-logout
    fn execute_auto_logout(&self, hour: u32, minute: u32) {
        info!("Executing auto-logout at {:02}:{:02} IST", hour, minute);

        // Get AppState to clear auth tokens and session
        match self.app_handle.try_state::<AppState>() {
            Some(state) => {
                // 1. Clear auth tokens from SQLite
                if let Err(e) = state.sqlite.clear_all_auth_tokens() {
                    error!("Failed to clear auth tokens from database: {}", e);
                } else {
                    info!("Cleared auth tokens from database");
                }

                // 2. Clear broker session state
                state.set_broker_session(None);
                info!("Cleared broker session state");

                // Note: We don't clear user session - user stays logged in to the app
                // They just need to re-authenticate with the broker
            }
            None => {
                warn!("AppState not available during auto-logout");
            }
        }

        // 3. Emit auto_logout event to frontend
        let event = AutoLogoutEvent {
            reason: format!(
                "Scheduled auto-logout at {:02}:{:02} IST for broker compliance",
                hour, minute
            ),
            timestamp: Utc::now().to_rfc3339(),
        };

        if let Err(e) = self.app_handle.emit("auto_logout", event) {
            warn!("Failed to emit auto_logout event: {}", e);
        }

        info!("Auto-logout completed");
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

    #[test]
    fn test_duration_to_specific_time() {
        // Test that we can calculate duration to any time
        let duration = AutoLogoutScheduler::duration_until_time_ist(2, 30);
        assert!(duration.as_secs() > 0);
        assert!(duration.as_secs() <= 24 * 3600);
    }

    #[test]
    fn test_default_warnings_order() {
        // Verify default warnings are in descending order (30, 15, 5, 1)
        for i in 1..DEFAULT_WARNINGS.len() {
            assert!(
                DEFAULT_WARNINGS[i - 1] > DEFAULT_WARNINGS[i],
                "Default warnings should be in descending order"
            );
        }
    }
}
