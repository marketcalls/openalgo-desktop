//! Reading one skill: its `SKILL.md` and its reference pages (the web's
//! `services/agent/skills/loader.py`).
//!
//! **What the model can name, and what it can reach.** The model chooses a
//! skill name and a reference path, so both are hostile input. A skill is
//! found only through the registry, never by joining a model-chosen name
//! onto a path. A reference path must name a file inside one of the skill's
//! declared reference folders, with an allowed type, and the files are the
//! ones embedded when the app was built ([`super::embedded`]), so neither
//! `../` nor a link pointing elsewhere reaches anything else: there is no
//! file system behind the lookup. Every page is capped at [`MAX_FILE_BYTES`].
//! Nothing in a skill folder is executed.
//!
//! A skill whose folder or `SKILL.md` is missing or malformed is skipped
//! with one warning; it costs that skill and never the run.

use super::embedded::{EmbeddedFolder, SHIPPED};
use super::frontmatter::parse_skill_md;
use super::registry::SkillSpec;
use crate::agent::tools::service::py_repr;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The file holding a skill's frontmatter and instructions.
pub const SKILL_FILE: &str = "SKILL.md";

/// Largest page served (the web's `MAX_FILE_BYTES`): the biggest shipped
/// page is about 34k characters.
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// Most reference pages listed for one skill.
pub const MAX_REFERENCES: usize = 100;

/// Longest description carried into the prompt.
pub const MAX_DESCRIPTION_CHARS: usize = 600;

/// A skill page that could not be served; the text is a sentence for the
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError(pub String);

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One skill folder's files: path inside the folder to text, as built.
pub type Files = Arc<BTreeMap<String, String>>;

/// The skill folders there are to read, by path from the repository root.
pub struct Library {
    folders: HashMap<String, Files>,
    /// The page size cap (a test lowers it).
    max_file_bytes: usize,
}

impl Library {
    /// The folders compiled into the app.
    pub fn shipped() -> Library {
        Library::of(SHIPPED, MAX_FILE_BYTES)
    }

    /// A library of the given folders, pages capped at `max_file_bytes`.
    pub fn of(folders: &[EmbeddedFolder], max_file_bytes: usize) -> Library {
        Library {
            folders: folders
                .iter()
                .map(|f| {
                    let files = f
                        .files
                        .iter()
                        .map(|(p, t)| ((*p).to_string(), (*t).to_string()))
                        .collect();
                    (f.folder.to_string(), Arc::new(files))
                })
                .collect(),
            max_file_bytes,
        }
    }
}

#[cfg(test)]
impl Library {
    /// A library of folders built in a test: `(folder, [(path, text)])`.
    pub fn from_files(folders: Vec<(&str, Vec<(&str, String)>)>, max_file_bytes: usize) -> Library {
        Library {
            folders: folders
                .into_iter()
                .map(|(folder, files)| {
                    let files = files.into_iter().map(|(p, t)| (p.to_string(), t)).collect();
                    (folder.to_string(), Arc::new(files))
                })
                .collect(),
            max_file_bytes,
        }
    }
}

/// One skill as it was built into the app.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub spec: SkillSpec,
    /// The frontmatter description, whitespace collapsed and capped.
    pub description: String,
    /// The body of `SKILL.md` after the frontmatter.
    pub instructions: String,
    /// Reference paths relative to the skill folder, exactly as
    /// `get_skill_reference` accepts them.
    pub references: Vec<String>,
    files: Files,
    max_file_bytes: usize,
}

/// A page's text: refused over the cap, one leading byte order mark
/// removed, line endings made `\n` so offsets are the same on every
/// platform (a Windows checkout embeds CRLF).
fn read_text(path: &str, raw: &str, cap: usize) -> Result<String, SkillError> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if raw.len() > cap {
        return Err(SkillError(format!(
            "{} is {} bytes, over the {} byte limit for a skill page, so it is not served.",
            name,
            raw.len(),
            cap
        )));
    }
    Ok(normalise(raw))
}

/// One leading byte order mark removed, CRLF and CR made LF (the web's
/// `_read_text`).
pub fn normalise(raw: &str) -> String {
    raw.strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Whitespace collapsed and capped at [`MAX_DESCRIPTION_CHARS`]: at the last
/// sentence end past half the cap, else cut with "...".
pub fn short_description(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= MAX_DESCRIPTION_CHARS {
        return flat;
    }
    let window: String = chars[..MAX_DESCRIPTION_CHARS].iter().collect();
    if let Some(cut) = window.rfind(". ") {
        let cut_chars = window[..cut].chars().count();
        if cut_chars > MAX_DESCRIPTION_CHARS / 2 {
            return chars[..=cut_chars].iter().collect();
        }
    }
    let head: String = chars[..MAX_DESCRIPTION_CHARS - 3].iter().collect();
    format!("{}...", head.trim_end())
}

/// Whether any part of a relative path is a dot file or dot folder.
fn is_hidden(parts: &[&str]) -> bool {
    parts.iter().any(|p| p.starts_with('.'))
}

/// A file name's type, lower-cased (`.md`), as Python's `Path.suffix`: a
/// dot file or a trailing dot has none.
fn suffix(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

fn allowed_suffix(spec: &SkillSpec, path: &str) -> bool {
    let s = suffix(path);
    spec.reference_suffixes
        .iter()
        .any(|allowed| allowed.to_lowercase() == s)
}

/// The pages a skill may serve: files in its reference folders, folder by
/// folder in the spec's order, each sorted as Python sorts paths (part by
/// part), allowed types only, no hidden part, at most [`MAX_REFERENCES`].
fn list_references(spec: &SkillSpec, files: &BTreeMap<String, String>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for dir in spec.reference_dirs {
        let mut in_dir: Vec<&String> = files
            .keys()
            .filter(|p| p.split('/').next() == Some(*dir) && p.contains('/'))
            .filter(|p| allowed_suffix(spec, p))
            .filter(|p| !is_hidden(&p.split('/').collect::<Vec<_>>()))
            .collect();
        in_dir.sort_by(|a, b| a.split('/').cmp(b.split('/')));
        for p in in_dir {
            found.push(p.clone());
            if found.len() >= MAX_REFERENCES {
                return found;
            }
        }
    }
    found
}

/// Read one skill, or `None` (logged) when its folder or `SKILL.md` is
/// missing or malformed.
pub fn load_skill(spec: &SkillSpec, library: &Library) -> Option<LoadedSkill> {
    let Some(files) = library.folders.get(spec.folder) else {
        tracing::warn!(
            "Agent skill {:?} is registered but its folder {} is missing; it is not offered",
            spec.name,
            spec.folder
        );
        return None;
    };
    let parsed = files
        .get(SKILL_FILE)
        .ok_or_else(|| format!("{} could not be read.", SKILL_FILE))
        .and_then(|raw| read_text(SKILL_FILE, raw, library.max_file_bytes).map_err(|e| e.0))
        .and_then(|text| parse_skill_md(&text).map_err(|e| e.to_string()));
    let md = match parsed {
        Ok(md) => md,
        Err(why) => {
            tracing::warn!(
                "Agent skill {:?} is not offered: {}/{} {}",
                spec.name,
                spec.folder,
                SKILL_FILE,
                why
            );
            return None;
        }
    };
    if let Some(declared) = md.text("name").map(str::trim) {
        if !declared.is_empty() && declared != spec.name {
            tracing::warn!(
                "Agent skill {:?}: SKILL.md names itself {:?}; the registry name is used",
                spec.name,
                declared
            );
        }
    }
    Some(LoadedSkill {
        spec: spec.clone(),
        description: short_description(md.text("description").unwrap_or_default()),
        instructions: md.body.clone(),
        references: list_references(spec, files),
        files: files.clone(),
        max_file_bytes: library.max_file_bytes,
    })
}

/// A path as Python's `PurePosixPath` splits it: whether it is absolute,
/// and its parts with empty and `.` parts dropped.
fn posix_parts(path: &str) -> (bool, Vec<&str>) {
    let parts = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    (path.starts_with('/'), parts)
}

impl LoadedSkill {
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    /// One reference page: a path relative to the skill folder
    /// (`reference/library.md`, `./` and backslashes tolerated), or a bare
    /// file name when exactly one listed page has it.
    pub fn read_reference(&self, reference_path: &str) -> Result<String, SkillError> {
        let refused = || {
            SkillError(format!(
                "{} is not a reference page of the {} skill. Pass one of its reference paths exactly as listed.",
                py_repr(&Value::String(reference_path.to_string())),
                self.spec.name
            ))
        };
        let normalised = reference_path.trim().replace('\\', "/");
        let mut requested = normalised.as_str();
        while let Some(rest) = requested.strip_prefix("./") {
            requested = rest;
        }
        let (absolute, mut parts) = posix_parts(requested);
        if parts.is_empty()
            || absolute
            || parts.contains(&"..")
            || requested.contains(':')
            || is_hidden(&parts)
        {
            return Err(refused());
        }
        if parts.len() == 1 {
            // A bare name, resolved against the listing so it can only name
            // something the listing already vetted.
            let matches: Vec<&String> = self
                .references
                .iter()
                .filter(|r| r.rsplit('/').next() == Some(requested))
                .collect();
            match matches.as_slice() {
                [only] => parts = only.split('/').collect(),
                _ => return Err(refused()),
            }
        }
        let path = parts.join("/");
        if !self.spec.reference_dirs.contains(&parts[0]) || !allowed_suffix(&self.spec, &path) {
            return Err(refused());
        }
        let raw = self.files.get(&path).ok_or_else(refused)?;
        read_text(&path, raw, self.max_file_bytes)
    }
}
