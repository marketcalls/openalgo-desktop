//! In-app updates, notify only (maintainer decision 2026-10-15).
//!
//! | Step | Started by | What happens |
//! | --- | --- | --- |
//! | Check | every app start (release builds), "Check for Updates" | asks the update address for the newest version; nothing is downloaded |
//! | Notify | the banner and the update dialog | the version, its release notes and an Update button |
//! | Update | the trader's click (`POST /api/update/install`, CSRF) | names what a restart interrupts and asks first; then downloads the update and verifies its signature |
//! | Restart | the desktop window (`restart_to_update`, Tauri) | asks again if new activity appeared since; installs and restarts |
//!
//! Nothing downloads before the trader clicks Update, and OpenAlgo never
//! restarts without the trader's click in the desktop window. A copy that
//! cannot replace itself (a `.deb` or `.rpm` install, a build from source)
//! is told about the update and given the release page instead.
//!
//! The service owns the decisions (which version is newer, what a restart
//! interrupts, what the trader confirmed). The update plugin behind
//! [`UpdateBackend`] fetches the update information, downloads, checks the
//! signature against the public key built into the app, and installs
//! (`crate::updater`).
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `release` | version precedence, how this copy was installed, the offer, failures, the plugin seam |
//! | `activity` | what a restart now would interrupt, read from every module that trades |
//! | `warning` | the confirmation the trader reads before a restart |
//! | `service` | the state of this app's update: check, download, restart |

mod activity;
mod release;
mod service;
mod warning;

pub(crate) use activity::broker_name;
pub use activity::{activity, Activity};
pub use release::{is_newer, Offer, Package, ProgressFn, UpdateBackend, UpdateFailure};
pub use service::{Decision, Progress, UpdateService, UpdateStatus};
pub use warning::Warning;

use std::time::Duration;

/// Where every release is published. The release page of one version is
/// `<RELEASES_URL>/tag/v<version>`.
pub const RELEASES_URL: &str = "https://github.com/marketcalls/openalgo-desktop/releases";

/// Socket.IO event carrying [`UpdateStatus`] whenever it changes.
pub const STATUS_EVENT: &str = "update_status";

/// How long a check may take before it counts as failed.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
