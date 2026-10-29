//! The skills the Agent may read, and where each one is offered (the web's
//! `services/agent/skills/registry.py`).
//!
//! A skill is a folder of written guidance (a `SKILL.md` plus reference
//! pages) that teaches how to author one kind of thing for OpenAlgo. The
//! folders live in the repository under `.claude/skills/` and are shared
//! with Claude Code, which is why they talk about running validators and
//! writing files: that is how Claude Code applies them. The Agent cannot run
//! anything, so each entry carries `agent_notes`, a few sentences that map
//! those steps onto the Agent's own tools.
//!
//! Adding a skill is one [`SkillSpec`] in [`SKILLS`] plus its files in
//! `embedded`. The one thing an entry cannot widen on its own is the
//! surfaces the skills toolkit is built for (`agent::tools::skills::GROUP`):
//! a test pins that every surface named here is one that toolkit is offered
//! on, so a skill is never listed in a prompt whose tools cannot read it.
//!
//! Voice is deliberately absent. Authoring a script or a workflow is a
//! typing job.

use crate::agent::contract::tool::{Surface, Surfaces};

/// Folders, relative to the skill folder, that hold reference pages (the
/// web's `DEFAULT_REFERENCE_DIRS`: both spellings exist).
pub const DEFAULT_REFERENCE_DIRS: &[&str] = &["reference", "references"];

/// File types a reference may be (the web's `DEFAULT_REFERENCE_SUFFIXES`).
/// Text only: nothing in a skill folder is ever executed, and a script is
/// not a reference.
pub const DEFAULT_REFERENCE_SUFFIXES: &[&str] = &[".md", ".txt", ".json"];

/// One skill the Agent may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSpec {
    /// The name the model passes to the skill tools. The spec wins over the
    /// `name` in the skill's frontmatter if they differ.
    pub name: &'static str,
    /// The skill folder, relative to the repository root.
    pub folder: &'static str,
    pub surfaces: Surfaces,
    /// How the skill's own steps map onto the Agent's tools; shown in the
    /// prompt and with the instructions.
    pub agent_notes: &'static str,
    /// Folders inside the skill folder whose files `get_skill_reference`
    /// may serve.
    pub reference_dirs: &'static [&'static str],
    /// File types those folders may serve.
    pub reference_suffixes: &'static [&'static str],
}

impl SkillSpec {
    pub fn offered_on(&self, surface: Surface) -> bool {
        self.surfaces.contains(surface)
    }
}

/// The OpenScript skill: the desktop's own `.claude/skills/openscript`,
/// which documents the OpenScript engine the desktop ships (maintainer,
/// 2026-10-27).
pub const OPENSCRIPT: SkillSpec = SkillSpec {
    name: "openscript",
    folder: ".claude/skills/openscript",
    // Chat and the chart panel: a study is written beside the chart it plots
    // on as often as on the conversation page.
    surfaces: Surfaces::CHAT_AND_CHART,
    agent_notes: "You cannot run validate.mjs, the importer or any node command, and nothing \
compiles here. Check every name against reference/library.md (a planned name \
fails with OS2020) and reference/pitfalls.md, show the operator the full \
source, then save it with save_openscript. It is stored as source only: the \
trader opens it in the OpenScript editor on /trading, where it compiles, and \
saving it there stores the compiled program a strategy needs to run.",
    // The worked examples are the files known to compile, and the skill tells
    // its reader to study them, so they are served as text.
    reference_dirs: &["reference", "examples"],
    reference_suffixes: &[".md", ".txt", ".json", ".oscript"],
};

/// The Flow skill: the web's `flow-builder`, handing off to the Flow
/// toolkit's `validate_flow` and `save_flow` (`agent::tools::flow_gen`).
/// Its reference pages are the web's node table and the import-format
/// contract the Flow tools cite (`docs/prompt/flow-import-format.md`,
/// served as `reference/flow-import-format.md`; lead decision 17: the web
/// points the model at that file and serves it nothing).
pub const FLOW_BUILDER: SkillSpec = SkillSpec {
    name: "flow-builder",
    folder: ".claude/skills/flow-builder",
    // Chat only, matching the Flow toolkit it hands off to.
    surfaces: Surfaces::CHAT_ONLY,
    agent_notes: "You cannot run validate.py, curl or SQL. Validate with validate_flow instead \
of validate.py, and save with save_flow instead of the import route; it \
imports the workflow inactive and the operator activates it in /flow. \
validate_flow does not flag a data key nothing reads, so copy every key from \
reference/nodes.md exactly.",
    reference_dirs: DEFAULT_REFERENCE_DIRS,
    reference_suffixes: DEFAULT_REFERENCE_SUFFIXES,
};

/// Every skill the Agent may read, in the order the prompt lists them.
pub const SKILLS: &[SkillSpec] = &[OPENSCRIPT, FLOW_BUILDER];

/// The skills of `specs` offered on one surface, in registry order.
pub fn skills_for_surface(specs: &[SkillSpec], surface: Surface) -> Vec<&SkillSpec> {
    specs.iter().filter(|s| s.offered_on(surface)).collect()
}

/// A skill by name (case and surrounding spaces ignored) among those
/// offered on the run's surface; one not offered there is not found.
pub fn find_skill<'a>(
    specs: &'a [SkillSpec],
    name: &str,
    surface: Surface,
) -> Option<&'a SkillSpec> {
    let wanted = name.trim().to_lowercase();
    skills_for_surface(specs, surface)
        .into_iter()
        .find(|s| s.name.to_lowercase() == wanted)
}
