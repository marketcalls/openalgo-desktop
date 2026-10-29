//! Skills: written authoring guidance the Agent reads on demand (the web's
//! `services/agent/skills`).
//!
//! - [`registry`]: the skills and where each is offered.
//! - [`embedded`]: the skill folders compiled into the app.
//! - [`frontmatter`]: a `SKILL.md`'s frontmatter and body.
//! - [`loader`]: one skill read, its references listed and served.
//! - [`prompt`]: the SKILLS prompt section.
//!
//! The tools that serve them are `agent::tools::skills`; the tool that saves
//! what the OpenScript skill teaches is `agent::tools::openscript_gen`.
//!
//! The web reads the folders live from disk so an edited `SKILL.md` applies
//! on the next message. The desktop embeds them when it is built, so an
//! edit applies with the next build, and a test pins the embedded files to
//! the repository's folders. Everything is loaded once, on first use.

pub mod embedded;
pub mod frontmatter;
pub mod loader;
pub mod prompt;
pub mod registry;

#[cfg(test)]
mod flow_builder;
#[cfg(test)]
mod tests;

use crate::agent::contract::tool::Surface;
use loader::{load_skill, Library, LoadedSkill};
use registry::SkillSpec;
use std::sync::{Arc, LazyLock};

/// The registered skills, each loaded once (or `None`: skipped, logged).
pub struct SkillSet {
    specs: Vec<SkillSpec>,
    loaded: Vec<Option<LoadedSkill>>,
}

impl SkillSet {
    /// Load every spec from `library`; a skill that cannot be read is
    /// skipped with one warning.
    pub fn new(specs: &[SkillSpec], library: &Library) -> SkillSet {
        SkillSet {
            specs: specs.to_vec(),
            loaded: specs.iter().map(|s| load_skill(s, library)).collect(),
        }
    }

    /// The skills offered on a surface that loaded, in registry order (the
    /// web's `available_skills`).
    pub fn available(&self, surface: Surface) -> Vec<&LoadedSkill> {
        self.specs
            .iter()
            .zip(&self.loaded)
            .filter(|(spec, _)| spec.offered_on(surface))
            .filter_map(|(_, loaded)| loaded.as_ref())
            .collect()
    }

    /// The names of the skills offered on a surface, loaded or not.
    pub fn offered(&self, surface: Surface) -> Vec<&'static str> {
        registry::skills_for_surface(&self.specs, surface)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    /// A skill offered on the surface, by name: its spec, and the skill
    /// when it loaded.
    pub fn find(&self, name: &str, surface: Surface) -> Option<(&SkillSpec, Option<&LoadedSkill>)> {
        let spec = registry::find_skill(&self.specs, name, surface)?;
        let at = self.specs.iter().position(|s| s == spec)?;
        Some((spec, self.loaded.get(at).and_then(Option::as_ref)))
    }
}

static SHIPPED: LazyLock<Arc<SkillSet>> =
    LazyLock::new(|| Arc::new(SkillSet::new(registry::SKILLS, &Library::shipped())));

/// The skills the app ships, loaded once.
pub fn shipped() -> Arc<SkillSet> {
    SHIPPED.clone()
}
