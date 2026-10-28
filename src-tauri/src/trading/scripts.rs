//! The trader's OpenScript sources and the compiled program stored beside
//! each (web `blueprints/openscript.py`).
//!
//! **These are sources, not modules.** A `.oscript` file is text a compiler
//! turns into a compiled program, which is data an engine walks. Nothing here
//! is ever executed or imported: a source is served as `text/plain` and a
//! program as `application/json`, so a response the page could import as code
//! never leaves this module.
//!
//! **Why a save carries the program.** The compiler is TypeScript and runs in
//! the page the trader is typing into; that page is the one place a program
//! can be produced. The program is stored byte for byte (its canonical
//! encoding is what its hash is taken over, and re-encoding it here would make
//! the engine refuse it at load). This module checks only what it can: that
//! the program records the hash of the very source it is stored beside, so a
//! stale editor buffer can never leave a runner executing yesterday's
//! strategy while the trader reads today's source.
//!
//! **A save with no program removes the program that was there.** A source
//! with no program beside it is saved, editable and not runnable, which is
//! never wrong; a source beside another source's program is the one state
//! this store must never reach. So the program is removed before the source is
//! replaced and written after it, both files having been staged and flushed
//! first, and one save or delete of a given file runs at a time.

use super::indicators::{mtime_of, plain_file};
use super::names::is_script_name;
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The largest source stored (web `MAX_SOURCE_BYTES`).
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// The largest compiled program stored (web `MAX_PROGRAM_BYTES`).
pub const MAX_PROGRAM_BYTES: usize = 512 * 1024;
/// A program's file name is its source's name plus this. Derived here from a
/// name already checked, never taken off the wire.
pub const PROGRAM_SUFFIX: &str = ".program.json";

/// One stored script as the index lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoredScript {
    pub file: String,
    pub mtime: i64,
    pub bytes: u64,
    pub program: bool,
}

/// A refusal with the status the web answers it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub status: u16,
    pub message: String,
}

impl Refused {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// The text in the language's own normal form: no leading byte order mark,
/// no carriage return before a newline. Exactly the compiler's two rules, so
/// the hash below is the one it stamps into a program.
pub fn normalised(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

/// The identity a compiled program records for its source.
pub fn source_hash(text: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(normalised(text).as_bytes()))
    )
}

/// One lock per file name, forgotten once nothing holds it.
#[derive(Default)]
struct KeyedLocks {
    held: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl KeyedLocks {
    fn hold<T>(&self, key: &str, f: impl FnOnce() -> T) -> T {
        let lock = self.held.lock().entry(key.to_string()).or_default().clone();
        let out = {
            let _guard = lock.lock();
            f()
        };
        let mut held = self.held.lock();
        // The map's copy and ours: nobody else is waiting on this file.
        if Arc::strong_count(&lock) == 2 {
            held.remove(key);
        }
        out
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.held.lock().len()
    }
}

/// The scripts folder. One write path: the /trading editor's route and the
/// Agent's `save_openscript` both save through [`ScriptStore::save`] (web
/// `openscript_store.write_script`), so there is one name rule, one size
/// limit, one per-file lock and one replace sequence.
pub struct ScriptStore {
    dir: PathBuf,
    locks: KeyedLocks,
    /// Every save in order, `(name, with a program)`: the web's spy on
    /// `write_script`, so a test can see who saved through here.
    #[cfg(test)]
    pub(crate) saves: Mutex<Vec<(String, bool)>>,
}

/// What a save stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub bytes: usize,
    pub mtime: i64,
    pub program: bool,
    /// A source of that name was there, and is kept as `<name>.bak`.
    pub replaced: bool,
}

/// What a save may replace (web `write_script`'s `replace` and
/// `replace_compiled`), checked under the file's lock so nothing slips in
/// between the check and the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replace {
    /// A source of that name.
    pub source: bool,
    /// A source that has a compiled program beside it, whatever `source`
    /// says: that program is what a deployed strategy starts from.
    pub compiled: bool,
}

impl Replace {
    /// The /trading editor: whatever is stored is replaced.
    pub const ANY: Replace = Replace {
        source: true,
        compiled: true,
    };
}

/// Why a save did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The name, the sizes, the program or the disk refused it.
    Refused(Refused),
    /// Something is stored under the name and [`Replace`] said not to
    /// replace it (web `ScriptConflict`); nothing was written.
    Taken { compiled: bool },
}

impl SaveError {
    /// The refusal a route answers with (web `ScriptConflict`'s message for
    /// a taken name; the editor's route replaces, so never sees one).
    pub fn into_refused(self, name: &str) -> Refused {
        match self {
            SaveError::Refused(r) => r,
            SaveError::Taken { compiled } => Refused::new(
                409,
                format!(
                    "{} already has {} stored",
                    name,
                    if compiled {
                        "a compiled program"
                    } else {
                        "a source"
                    }
                ),
            ),
        }
    }
}

impl ScriptStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            locks: KeyedLocks::default(),
            #[cfg(test)]
            saves: Mutex::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn program_name(name: &str) -> String {
        format!("{}{}", name, PROGRAM_SUFFIX)
    }

    /// Whether a source by this name is stored.
    pub fn has_source(&self, name: &str) -> bool {
        is_script_name(name) && plain_file(&self.dir, name).is_some()
    }

    /// Whether a compiled program is stored beside this source.
    pub fn has_program(&self, name: &str) -> bool {
        is_script_name(name) && plain_file(&self.dir, &Self::program_name(name)).is_some()
    }

    /// Every stored source, by name, with whether a program sits beside it.
    /// A folder that cannot be read lists nothing (logged).
    pub fn index(&self) -> Vec<StoredScript> {
        self.list().unwrap_or_else(|e| {
            tracing::warn!("Could not list the OpenScript folder: {}", e);
            Vec::new()
        })
    }

    /// [`ScriptStore::index`], telling a folder that cannot be read from one
    /// that does not exist yet (web `list_scripts`: a missing folder lists
    /// nothing, an unreadable one is an error).
    pub fn list(&self) -> std::io::Result<Vec<StoredScript>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_script_name(n))
            .collect();
        names.sort();
        Ok(names
            .into_iter()
            .filter_map(|file| {
                let meta = plain_file(&self.dir, &file)?;
                Some(StoredScript {
                    mtime: mtime_of(&meta),
                    bytes: meta.len(),
                    program: self.has_program(&file),
                    file,
                })
            })
            .collect())
    }

    /// One source's bytes, or `None` when there is no such file.
    pub fn source(&self, name: &str) -> Option<Vec<u8>> {
        if !self.has_source(name) {
            return None;
        }
        fs::read(self.dir.join(name)).ok()
    }

    /// The compiled program stored beside one source.
    pub fn program(&self, name: &str) -> Option<Vec<u8>> {
        if !self.has_program(name) {
            return None;
        }
        fs::read(self.dir.join(Self::program_name(name))).ok()
    }

    /// Check a save before anything is touched: the size of each half, and
    /// that the program was compiled from this very source.
    pub fn check_save(source: &str, program: Option<&str>) -> Result<(), Refused> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(Refused::new(
                413,
                format!(
                    "This script is {} bytes and the limit is {}",
                    source.len(),
                    MAX_SOURCE_BYTES
                ),
            ));
        }
        let Some(program) = program else {
            return Ok(());
        };
        if program.len() > MAX_PROGRAM_BYTES {
            return Err(Refused::new(
                413,
                format!(
                    "The compiled program for this script is {} bytes and the limit is {}",
                    program.len(),
                    MAX_PROGRAM_BYTES
                ),
            ));
        }
        // serde_json refuses nesting past its recursion limit with an error
        // rather than running out of stack, so a hostile body is a refusal.
        let parsed: Value = serde_json::from_str(program).map_err(|_| {
            Refused::new(
                400,
                "The compiled program that came with this script could not be read. Save it again.",
            )
        })?;
        let Some(recorded) = parsed
            .get("source")
            .and_then(|s| s.get("hash"))
            .and_then(Value::as_str)
        else {
            return Err(Refused::new(
                400,
                "What came with this script is not a compiled program. Save it again.",
            ));
        };
        if recorded != source_hash(source) {
            return Err(Refused::new(
                400,
                "The compiled program that came with this script was built from different text. Save it again.",
            ));
        }
        Ok(())
    }

    /// Create or replace one source, and the program beside it, replacing
    /// only what `replace` allows.
    pub fn save(
        &self,
        name: &str,
        source: &str,
        program: Option<&str>,
        replace: Replace,
    ) -> Result<Saved, SaveError> {
        if !is_script_name(name) {
            return Err(SaveError::Refused(Refused::new(
                400,
                super::names::script_name_refusal(name),
            )));
        }
        Self::check_save(source, program).map_err(SaveError::Refused)?;
        if let Err(e) = fs::create_dir_all(&self.dir) {
            tracing::error!("Could not create the OpenScript folder: {}", e);
            return Err(SaveError::Refused(Refused::new(
                500,
                "Could not create the scripts folder. Check that the app's data folder can be written to.",
            )));
        }
        #[cfg(test)]
        self.saves
            .lock()
            .push((name.to_string(), program.is_some()));
        let replaced = match self
            .locks
            .hold(name, || self.write(name, source, program, replace))
        {
            Ok(Ok(replaced)) => replaced,
            Ok(Err(taken)) => return Err(taken),
            Err(e) => {
                tracing::error!("Could not save the OpenScript source {}: {}", name, e);
                return Err(SaveError::Refused(Refused::new(
                    500,
                    "Could not save this script. Check that the app's data folder can be written to.",
                )));
            }
        };
        let mtime = fs::metadata(self.dir.join(name))
            .map(|m| mtime_of(&m))
            .unwrap_or(0);
        tracing::info!(
            "Saved OpenScript source {} ({} bytes, program {})",
            name,
            source.len(),
            if program.is_some() { "stored" } else { "none" }
        );
        Ok(Saved {
            bytes: source.len(),
            mtime,
            program: program.is_some(),
            replaced,
        })
    }

    /// The replace sequence, under the file's lock: refuse what `replace`
    /// does not allow, back the old source up, stage both halves, then one
    /// unlink and two renames. Every state in between is a source with no
    /// program, which is never wrong. The inner result says whether a
    /// source was replaced, or that the name was taken (nothing written).
    fn write(
        &self,
        name: &str,
        source: &str,
        program: Option<&str>,
        replace: Replace,
    ) -> std::io::Result<Result<bool, SaveError>> {
        let target = self.dir.join(name);
        let program_target = self.dir.join(Self::program_name(name));
        let existing = fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_file());
        if !replace.compiled && fs::symlink_metadata(&program_target).is_ok() {
            return Ok(Err(SaveError::Taken { compiled: true }));
        }
        if !replace.source && fs::symlink_metadata(&target).is_ok() {
            return Ok(Err(SaveError::Taken { compiled: false }));
        }
        let mut staged: Vec<PathBuf> = Vec::new();
        let out = (|| -> std::io::Result<()> {
            if existing {
                let backup = self.dir.join(format!("{}.bak", name));
                fs::write(&backup, fs::read(&target)?)?;
            }
            let source_tmp = stage(&self.dir, source.as_bytes())?;
            staged.push(source_tmp.clone());
            let program_tmp = match program {
                Some(p) => {
                    let t = stage(&self.dir, p.as_bytes())?;
                    staged.push(t.clone());
                    Some(t)
                }
                None => None,
            };
            match fs::remove_file(&program_target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            fs::rename(&source_tmp, &target)?;
            staged.retain(|p| p != &source_tmp);
            if let Some(t) = program_tmp {
                fs::rename(&t, &program_target)?;
                staged.retain(|p| p != &t);
            }
            Ok(())
        })();
        // Whatever is still staged was never renamed into place.
        for leftover in staged {
            let _ = fs::remove_file(leftover);
        }
        out.map(|()| Ok(existing))
    }

    /// Delete one source, its backup and its program. Deleting one that is
    /// not there succeeds: the caller asked for it to be gone and it is.
    pub fn delete(&self, name: &str) -> Result<(), Refused> {
        if !is_script_name(name) {
            return Err(Refused::new(400, super::names::script_name_refusal(name)));
        }
        let result = self.locks.hold(name, || -> std::io::Result<()> {
            for path in [
                self.dir.join(name),
                self.dir.join(format!("{}.bak", name)),
                self.dir.join(Self::program_name(name)),
            ] {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                tracing::info!("Deleted OpenScript source {}", name);
                Ok(())
            }
            Err(e) => {
                tracing::error!("Could not delete the OpenScript source {}: {}", name, e);
                Err(Refused::new(
                    500,
                    "Could not delete this script. Check that the app's data folder can be written to.",
                ))
            }
        }
    }
}

/// Write bytes to a fresh file in `dir` and flush them to disk. Same folder,
/// so the rename that follows is atomic.
fn stage(dir: &Path, payload: &[u8]) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("tmp{}.partial", uuid::Uuid::new_v4().simple()));
    let written = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        f.write_all(payload)?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A save as the /trading editor makes it.
    fn save(
        store: &ScriptStore,
        name: &str,
        source: &str,
        program: Option<&str>,
    ) -> Result<Saved, Refused> {
        store
            .save(name, source, program, Replace::ANY)
            .map_err(|e| e.into_refused(name))
    }

    fn program_for(source: &str) -> String {
        json!({"format": 1, "source": {"hash": source_hash(source)}}).to_string()
    }

    #[test]
    fn hash_follows_the_compilers_normal_form() {
        assert_eq!(source_hash("a\r\nb"), source_hash("a\nb"));
        assert_eq!(source_hash("\u{feff}a"), source_hash("a"));
        assert_ne!(source_hash("a\rb"), source_hash("a\nb"));
        assert!(source_hash("x").starts_with("sha256:"));
    }

    #[test]
    fn save_replace_and_delete_keep_source_and_program_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScriptStore::new(dir.path().join("openscript"));
        assert!(store.index().is_empty());

        let saved = save(
            &store,
            "t.oscript",
            "version 1",
            Some(&program_for("version 1")),
        )
        .unwrap();
        assert!(saved.program);
        assert!(store.has_program("t.oscript"));

        // A save with no program removes the one that was there.
        save(&store, "t.oscript", "version 1\n// edit", None).unwrap();
        assert!(!store.has_program("t.oscript"));
        assert_eq!(
            fs::read_to_string(store.dir().join("t.oscript.bak")).unwrap(),
            "version 1"
        );
        let idx = store.index();
        assert_eq!(idx.len(), 1);
        assert!(!idx[0].program);

        store.delete("t.oscript").unwrap();
        store.delete("t.oscript").unwrap();
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 0);
        assert_eq!(store.locks.len(), 0);
    }

    #[test]
    fn a_program_from_other_text_is_refused_and_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScriptStore::new(dir.path().to_path_buf());
        save(&store, "t.oscript", "one", Some(&program_for("one"))).unwrap();
        let e = save(&store, "t.oscript", "two", Some(&program_for("one"))).unwrap_err();
        assert_eq!(e.status, 400);
        assert!(e.message.contains("different text"));
        assert_eq!(store.source("t.oscript").unwrap(), b"one");
        assert!(store.has_program("t.oscript"));
        let e = save(&store, "t.oscript", "two", Some("[1,2")).unwrap_err();
        assert!(e.message.contains("could not be read"));
        let e = save(&store, "t.oscript", "two", Some("{}")).unwrap_err();
        assert!(e.message.contains("not a compiled program"));
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(
            save(&store, "t.oscript", "two", Some(&deep))
                .unwrap_err()
                .status,
            400
        );
    }

    #[test]
    fn sizes_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScriptStore::new(dir.path().to_path_buf());
        let big = "a".repeat(MAX_SOURCE_BYTES + 1);
        assert_eq!(
            save(&store, "t.oscript", &big, None).unwrap_err().status,
            413
        );
        let ok = "a".repeat(MAX_SOURCE_BYTES);
        assert!(save(&store, "t.oscript", &ok, None).is_ok());
        let program = "a".repeat(MAX_PROGRAM_BYTES + 1);
        assert_eq!(
            save(&store, "t.oscript", "x", Some(&program))
                .unwrap_err()
                .status,
            413
        );
        assert_eq!(
            save(&store, "../t.oscript", "x", None).unwrap_err().status,
            400
        );
    }

    const KEEP: Replace = Replace {
        source: false,
        compiled: false,
    };

    /// Web `test_a_refused_write_leaves_nothing_behind`: a save told not to
    /// replace finds the name taken and writes nothing, no backup and no
    /// staged file.
    #[test]
    fn a_refused_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScriptStore::new(dir.path().to_path_buf());
        save(&store, "a.oscript", "old", None).unwrap();
        assert_eq!(
            store.save("a.oscript", "new", None, KEEP),
            Err(SaveError::Taken { compiled: false })
        );
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["a.oscript"]);
        assert_eq!(store.source("a.oscript").unwrap(), b"old");
        assert_eq!(store.locks.len(), 0);
    }

    /// A compiled program is never replaced by a save that does not allow
    /// it, even one allowed to replace the source; the editor's save still
    /// replaces both (web `test_the_route_still_replaces_a_compiled_script`).
    #[test]
    fn a_compiled_script_is_replaced_only_by_a_save_that_allows_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScriptStore::new(dir.path().to_path_buf());
        save(&store, "r.oscript", "one", Some(&program_for("one"))).unwrap();
        let only_source = Replace {
            source: true,
            compiled: false,
        };
        for replace in [KEEP, only_source] {
            assert_eq!(
                store.save("r.oscript", "two", None, replace),
                Err(SaveError::Taken { compiled: true })
            );
        }
        assert!(store.has_program("r.oscript"));
        assert!(!dir.path().join("r.oscript.bak").exists());
        let saved = save(&store, "r.oscript", "two", Some(&program_for("two"))).unwrap();
        assert!(saved.replaced && saved.program);
        assert_eq!(store.source("r.oscript").unwrap(), b"two");
        assert_eq!(
            fs::read_to_string(dir.path().join("r.oscript.bak")).unwrap(),
            "one"
        );
        let fresh = store.save("new.oscript", "x", None, only_source).unwrap();
        assert!(!fresh.replaced);
        assert_eq!(
            SaveError::Taken { compiled: true }.into_refused("r.oscript"),
            Refused::new(409, "r.oscript already has a compiled program stored")
        );
    }

    /// A folder not made yet lists nothing; one that cannot be read is an
    /// error the caller can tell apart (web `list_scripts`).
    #[test]
    fn a_missing_folder_lists_nothing_and_an_unreadable_one_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let missing = ScriptStore::new(dir.path().join("openscript"));
        assert_eq!(missing.list().unwrap(), vec![]);
        let file = dir.path().join("not-a-folder");
        fs::write(&file, "x").unwrap();
        let broken = ScriptStore::new(file);
        assert!(broken.list().is_err());
        assert!(broken.index().is_empty());
    }
}
