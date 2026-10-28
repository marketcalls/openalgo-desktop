//! The prompt section that tells the model which skills it can read (the
//! web's `services/agent/skills/prompt.py`).
//!
//! Progressive disclosure: the prompt carries only each skill's name, its
//! description and the notes on how to apply it here. The instructions and
//! the reference pages are fetched with the skill tools when a job calls
//! for them, so a 34k-character reference costs nothing on a turn that does
//! not need it.

use super::{shipped, SkillSet};
use crate::agent::contract::policy::SectionSource;
use crate::agent::contract::tool::{Capabilities, Surface};
use crate::agent::prompts::PromptSection;
use crate::state::AppState;

/// Sort weight: after the code-writing rules (50), before the drawing rules
/// (55).
pub const SKILLS_SECTION_ORDER: i64 = 52;

const PREAMBLE: &str =
    "Some authoring jobs have a skill: written guidance you read on demand. Before
writing anything a skill below covers, call get_skill_instructions for it, then
get_skill_reference for each page those instructions tell you to read. Never
write it from memory. Instructions already read in this conversation need not be
fetched again.

The skills were written for a reader that can run commands and write files. You
cannot, so where a skill says to run something, do what its \"Here\" note says
instead.";

/// The section for one surface, or `None` when no skill offered there
/// loaded, so a surface is never taught tools it does not have.
pub fn skills_section(skills: &SkillSet, surface: Surface) -> Option<PromptSection> {
    let available = skills.available(surface);
    if available.is_empty() {
        return None;
    }
    let entries: Vec<String> = available
        .iter()
        .map(|s| {
            format!(
                "- {}: {}\n  Here: {}",
                s.name(),
                s.description,
                s.spec
                    .agent_notes
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    Some(PromptSection::new(
        "skills",
        "SKILLS",
        &format!("{}\n\n{}", PREAMBLE, entries.join("\n")),
        SKILLS_SECTION_ORDER,
    ))
}

/// The SKILLS section on every run, registered with the Agent at startup.
pub struct SkillsSection;

impl SectionSource for SkillsSection {
    fn sections(
        &self,
        _app: &AppState,
        surface: Surface,
        _caps: &Capabilities,
    ) -> Vec<PromptSection> {
        skills_section(&shipped(), surface).into_iter().collect()
    }
}
