//! The skill folders the app ships, compiled into the binary (maintainer,
//! 2026-10-27: the desktop serves its own `.claude/skills/openscript`,
//! read-only; 1.0.5 adds `.claude/skills/flow-builder`).
//!
//! The web reads its skill folders live from the repository on every call.
//! The desktop has no repository at run time, so the pages a skill may serve
//! (its `SKILL.md` and the files of its reference folders with an allowed
//! type) are embedded when the app is built, and nothing is read from disk
//! afterwards. A test walks each folder in the repository and pins it
//! against this list, file for file and byte for byte, so a page added,
//! removed or edited there fails the build's tests until it is listed here:
//! a skill edit cannot ship stale. Scripts in the folders (`*.mjs`,
//! `*.py`) are never embedded, served or run.
//!
//! A page may be borrowed: served as a reference of a skill while its one
//! copy lives elsewhere in the repository (the Flow import-format contract,
//! `docs/prompt/flow-import-format.md`), so the document the Flow tools cite
//! and the page the skill serves cannot drift apart.

/// One shipped skill folder: its path from the repository root, and each
/// file as `(path inside the folder, text)`.
pub struct EmbeddedFolder {
    pub folder: &'static str,
    pub files: &'static [(&'static str, &'static str)],
    /// The files of `files` whose one copy lives elsewhere in the
    /// repository: `(path inside the folder, path from the repository
    /// root)`.
    pub borrowed: &'static [(&'static str, &'static str)],
}

/// One file of the OpenScript skill, as it is in the repository.
macro_rules! openscript {
    ($path:literal) => {
        (
            $path,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../.claude/skills/openscript/",
                $path
            )),
        )
    };
}

/// The OpenScript skill: its instructions, the reference pages and the
/// worked examples (served as text).
const OPENSCRIPT: EmbeddedFolder = EmbeddedFolder {
    folder: ".claude/skills/openscript",
    files: &[
        openscript!("SKILL.md"),
        openscript!("examples/intraday-session.oscript"),
        openscript!("examples/long-only.oscript"),
        openscript!("examples/short-only.oscript"),
        openscript!("examples/stop-and-reverse.oscript"),
        openscript!("examples/stop-and-target.oscript"),
        openscript!("examples/study-bands.oscript"),
        openscript!("reference/library.md"),
        openscript!("reference/pitfalls.md"),
        openscript!("reference/porting-from-javascript.md"),
        openscript!("reference/strategies.md"),
    ],
    borrowed: &[],
};

/// One file of the Flow skill, as it is in the repository.
macro_rules! flow_builder {
    ($path:literal) => {
        (
            $path,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../.claude/skills/flow-builder/",
                $path
            )),
        )
    };
}

/// The Flow skill: its instructions, the web's node table, and the
/// import-format contract the Flow tools cite, borrowed from `docs/prompt`.
const FLOW_BUILDER: EmbeddedFolder = EmbeddedFolder {
    folder: ".claude/skills/flow-builder",
    files: &[
        flow_builder!("SKILL.md"),
        (
            "reference/flow-import-format.md",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../docs/prompt/flow-import-format.md"
            )),
        ),
        flow_builder!("reference/nodes.md"),
    ],
    borrowed: &[(
        "reference/flow-import-format.md",
        "docs/prompt/flow-import-format.md",
    )],
};

/// Every shipped skill folder.
pub const SHIPPED: &[EmbeddedFolder] = &[OPENSCRIPT, FLOW_BUILDER];
