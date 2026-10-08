//! The trader's own chart indicator modules (web
//! `blueprints/custom_indicators.py`).
//!
//! They live in the `indicators/` folder of the app data directory and are
//! served to the /trading chart at runtime, which `import()`s each one after
//! the built-in tier. **They are never bundled**: the app's interface is built
//! from the repository, so a bundled indicator would have to be committed, and
//! an upgrade would replace it. Read from the folder at runtime they need no
//! Node.js, no rebuild, and survive an upgrade untouched.
//!
//! **They are not sandboxed.** A module runs on the app's own origin with the
//! signed-in session, exactly as on the web: it can call every page route and
//! `/api/v1`. That is the trust model of a file the trader put in their own
//! data folder, and it means an indicator from an untrusted source is as
//! dangerous as any program they run.
//!
//! What this module promises is narrower and is enforced here: only plain
//! `.js` names (no separator, no dot segment, no other extension), only
//! regular files inside the folder (a link out of it is not followed), a size
//! bound so one file cannot hold the server's memory, and no listing beyond
//! the index of those names.

use super::names::is_indicator_name;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// The largest module served. A chart indicator is kilobytes; the bound only
/// stops a stray large file being read into memory on every chart load.
pub const MAX_MODULE_BYTES: u64 = 4 * 1024 * 1024;

/// One indicator module as the index lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IndicatorModule {
    pub file: String,
    pub mtime: i64,
}

/// What reading one module found.
#[derive(Debug)]
pub enum Fetch {
    Module(Vec<u8>),
    NoFolder,
    Missing,
    TooLarge,
}

/// Seconds since the epoch of a file's modification time, 0 when unknown.
pub fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A regular file directly inside `dir`, never a link to one elsewhere.
pub fn plain_file(dir: &Path, name: &str) -> Option<fs::Metadata> {
    let meta = fs::symlink_metadata(dir.join(name)).ok()?;
    meta.file_type().is_file().then_some(meta)
}

/// Every module in the folder, by name. A folder that does not exist yet is
/// an empty list, never an error: a trader who has written none has none.
pub fn list(dir: &Path) -> Vec<IndicatorModule> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_indicator_name(n))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|file| {
            // A file that vanished between the listing and the stat is not
            // worth failing the picker over.
            let meta = plain_file(dir, &file)?;
            Some(IndicatorModule {
                mtime: mtime_of(&meta),
                file,
            })
        })
        .collect()
}

/// One module's bytes. `name` must already have passed [`is_indicator_name`].
pub fn read(dir: &Path, name: &str) -> Fetch {
    if !is_indicator_name(name) {
        return Fetch::Missing;
    }
    if !dir.is_dir() {
        return Fetch::NoFolder;
    }
    let Some(meta) = plain_file(dir, name) else {
        return Fetch::Missing;
    };
    if meta.len() > MAX_MODULE_BYTES {
        return Fetch::TooLarge;
    }
    match fs::read(dir.join(name)) {
        Ok(bytes) => Fetch::Module(bytes),
        Err(e) => {
            tracing::warn!("Could not read the custom indicator {}: {}", name, e);
            Fetch::Missing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_plain_js_names_in_order() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.js"), "export default 1").unwrap();
        fs::write(dir.path().join("a.js"), "export default 1").unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        fs::write(dir.path().join(".hidden.js"), "x").unwrap();
        fs::create_dir(dir.path().join("sub.js")).unwrap();
        let names: Vec<String> = list(dir.path()).into_iter().map(|m| m.file).collect();
        assert_eq!(names, vec!["a.js", "b.js"]);
        assert!(list(&dir.path().join("missing")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_folder_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.js"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.js"), dir.path().join("x.js"))
            .unwrap();
        assert!(list(dir.path()).is_empty());
        assert!(matches!(read(dir.path(), "x.js"), Fetch::Missing));
    }

    #[test]
    fn reads_bound_by_size_and_name() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read(&dir.path().join("none"), "a.js"),
            Fetch::NoFolder
        ));
        fs::write(dir.path().join("a.js"), "export default 1").unwrap();
        assert!(matches!(read(dir.path(), "a.js"), Fetch::Module(b) if b == b"export default 1"));
        assert!(matches!(read(dir.path(), "../a.js"), Fetch::Missing));
        let big = vec![b' '; (MAX_MODULE_BYTES + 1) as usize];
        fs::write(dir.path().join("big.js"), big).unwrap();
        assert!(matches!(read(dir.path(), "big.js"), Fetch::TooLarge));
    }
}
