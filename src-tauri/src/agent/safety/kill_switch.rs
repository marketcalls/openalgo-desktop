//! The file half of the Agent's kill switch (the web's `kill_switch_file`
//! setting and `RiskGuard._kill_switch_file_present`).
//!
//! The switch has two halves and either one engages it: the stored
//! `kill_switch` setting, and this file. The file is the half that works
//! when the database or the screen does not: it can be created from a
//! file manager or a shell, and it survives a restart. The guard is told
//! whether it exists; it never looks itself (`risk` does no I/O).
//!
//! Desktop: the default is `<app data folder>/agent_kill_switch` (lead
//! decision 2, 2026-10-27), and the trading safety panel's switch creates
//! and removes it through [`set`].

use crate::state::AppState;
use std::path::{Path, PathBuf};

/// The file's name in the data folder.
pub const FILE_NAME: &str = "agent_kill_switch";

/// The default kill switch file.
pub fn default_file(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// The setting as a path: `~` is the trader's home folder, and a relative
/// path is inside the data folder, so a value typed into the settings
/// means the same thing whatever folder the app was started from.
pub fn resolve(setting: &str, data_dir: &Path) -> PathBuf {
    let text = setting.trim();
    if text.is_empty() {
        return default_file(data_dir);
    }
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => match home() {
            Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(text),
        },
        _ => PathBuf::from(text),
    };
    if expanded.is_absolute() {
        expanded
    } else {
        data_dir.join(expanded)
    }
}

fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var_os(k))
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Whether the file exists. A path that cannot be checked counts as absent,
/// as the web's `Path.exists()` does, and is logged.
pub fn present(path: &Path) -> bool {
    match path.try_exists() {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(
                "The Agent kill switch at {} could not be checked; treating it as absent: {}",
                path.display(),
                e
            );
            false
        }
    }
}

/// Engage (create an empty file) or release (remove it). Releasing a
/// switch that is not engaged is not an error.
pub fn set(path: &Path, engaged: bool) -> std::io::Result<()> {
    if engaged {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|_| ())
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// Engage or release the file at the trader's configured path (the panel
/// switch; the config part stores the `kill_switch` setting beside it).
pub fn set_for(app: &AppState, engaged: bool) -> std::io::Result<PathBuf> {
    let path = super::limits::read(app).kill_switch_path;
    set(&path, engaged)?;
    tracing::info!(
        "Agent kill switch file {} at {}",
        if engaged { "created" } else { "removed" },
        path.display()
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_resolves_inside_the_data_folder() {
        let data = Path::new("/data/openalgo");
        assert_eq!(resolve("", data), data.join(FILE_NAME));
        assert_eq!(resolve("stop", data), data.join("stop"));
        let absolute = std::env::temp_dir().join("stop-all");
        assert_eq!(resolve(absolute.to_str().unwrap(), data), absolute);
        if let Some(home) = home() {
            assert_eq!(resolve("~/stop", data), home.join("stop"));
        }
    }

    #[test]
    fn engaging_creates_the_file_and_releasing_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert!(!present(&path));
        set(&path, true).unwrap();
        assert!(present(&path));
        set(&path, true).unwrap();
        set(&path, false).unwrap();
        assert!(!present(&path));
        set(&path, false).unwrap();
    }
}
