//! The Agent's kill switch, in its two halves (the web's `kill_switch`
//! setting and `kill_switch_file`): either one engaged stops every order
//! tool. The setting is what the settings screen toggles; the file is what
//! a trader can create when the app cannot be reached, and it engages the
//! switch even when the database cannot be read.
//!
//! Decision 2 of the 1.0.4 plan: the file defaults to
//! `<app data folder>/agent_kill_switch`, the path stays a setting (as on
//! the web), and the switch on the Trading safety panel creates the file
//! when it engages and removes it when it releases, so the two halves never
//! disagree about a switch the trader turned off. What an engaged switch
//! refuses is the order guard's (the orders lane); this module stores it
//! and says whether it is engaged.

use super::schema::{KILL_SWITCH, KILL_SWITCH_FILE};
use super::{store, Snapshot};
use crate::state::AppState;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What the file says to whoever finds it.
const FILE_NOTE: &str = "While this file exists, the OpenAlgo Agent places, modifies and cancels no orders.\nDelete it, or release the kill switch in Agent settings, to allow them again.\n";

/// The switch as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillSwitchState {
    /// Either half engaged.
    pub engaged: bool,
    pub by_setting: bool,
    pub by_file: bool,
    /// Where the file half lives.
    pub path: PathBuf,
}

impl KillSwitchState {
    pub fn to_json(&self) -> Value {
        json!({
            "engaged": self.engaged,
            "by_setting": self.by_setting,
            "by_file": self.by_file,
            "path": self.path.to_string_lossy(),
        })
    }
}

/// The file's path: `~` is the trader's home, and a relative path is taken
/// inside the app's data folder (the web resolves against its repository
/// root), so a value typed in the app means the same wherever the app runs.
pub fn resolve(setting: &str, data_dir: &Path) -> PathBuf {
    let text = setting.trim();
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            match std::env::home_dir() {
                Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
                None => PathBuf::from(text),
            }
        }
        _ => PathBuf::from(text),
    };
    if text.is_empty() {
        return data_dir.join(super::schema::KILL_SWITCH_FILE_NAME);
    }
    if expanded.is_absolute() {
        expanded
    } else {
        data_dir.join(expanded)
    }
}

/// Whether the switch is engaged now: the setting read fresh, and the file.
pub fn state(app: &AppState) -> KillSwitchState {
    let snapshot = Snapshot::read(app);
    let path = resolve(snapshot.text(KILL_SWITCH_FILE), &app.data_dir);
    let by_setting = snapshot.flag(KILL_SWITCH);
    let by_file = path.exists();
    KillSwitchState {
        engaged: by_setting || by_file,
        by_setting,
        by_file,
        path,
    }
}

/// Engage or release both halves. The setting is written first (it is what
/// a run reads); the file follows. A file that cannot be created is logged
/// (the setting already engages the switch); one that cannot be removed
/// keeps the switch engaged, which the answer shows.
pub fn set(app: &AppState, engaged: bool) -> crate::error::Result<KillSwitchState> {
    let text = if engaged { "true" } else { "false" };
    store::write_all(&*app.sqlite.conn()?, &[(KILL_SWITCH, text.to_string())])?;
    let path = resolve(Snapshot::read(app).text(KILL_SWITCH_FILE), &app.data_dir);
    if engaged {
        if let Err(e) = std::fs::write(&path, FILE_NOTE) {
            tracing::warn!(
                "The Agent kill switch file could not be created at {}: {}",
                path.display(),
                e
            );
        }
    } else if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                "The Agent kill switch file could not be removed at {}: {}",
                path.display(),
                e
            );
        }
    }
    tracing::info!(
        "Agent kill switch {}",
        if engaged { "engaged" } else { "released" }
    );
    Ok(state(app))
}
