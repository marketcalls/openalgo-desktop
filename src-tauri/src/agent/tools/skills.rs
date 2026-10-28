//! Read a skill's instructions and reference pages, a page at a time (the
//! web's `services/agent/tools/skills.py`, toolkit `skills`).
//!
//! The SKILLS prompt section lists each skill offered on the run's surface;
//! these two tools are how the model reads one. Both are read-only, and
//! neither can name a skill the run's surface is not offered or a file
//! outside the skill's reference folders. Nothing here runs anything.
//!
//! **Long text is paged, never truncated.** A reference page runs to 34k
//! characters and a result is capped at [`SKILL_RESULT_CHARS`]. Cutting it
//! to fit would hand the model the first third of a name table and let it
//! believe it had read all of it. So each call returns a slice that fits,
//! with `total_chars` and `next_offset` beside it, and the model continues
//! from `next_offset` until it is null. The slices are contiguous, so the
//! pages join back to the page exactly.

use super::lane_specs::web_or_log;
use crate::agent::contract::json::dumps_capped_ordered;
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::skills::loader::LoadedSkill;
use crate::agent::skills::{shipped, SkillSet};
use crate::agent::tools::service::py_repr;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::Arc;

/// Most characters one skill result carries: larger than the shared 12,000
/// on purpose, since a skill page is the project's own document the model
/// reads in full either way (at the shared cap one OpenScript study took
/// eighteen calls of paging).
pub const SKILL_RESULT_CHARS: usize = 32_000;

/// Characters tried for one page before the result is measured.
pub const PAGE_CHARS: usize = 30_000;

/// Characters kept free under [`SKILL_RESULT_CHARS`] for the envelope.
const PAGE_HEADROOM: usize = 200;

/// The toolkit (web `ToolkitSpec` `skills`): chat and chart, not voice,
/// since authoring is a typing job.
pub const GROUP: ToolGroup = ToolGroup {
    key: "skills",
    surfaces: Surfaces::CHAT_AND_CHART,
    requires: &[],
    order: 58,
    description: "Read a skill's authoring instructions and reference pages, live from disk.",
    build,
};

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    tools(shipped())
}

/// The two tools over a set of skills (the shipped set; a test's own).
pub(crate) fn tools(skills: Arc<SkillSet>) -> Vec<Arc<dyn AgentTool>> {
    [
        ("get_skill_instructions", Kind::Instructions),
        ("get_skill_reference", Kind::Reference),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = web_or_log(GROUP.key, name)?;
        Some(Arc::new(SkillTool {
            spec,
            kind,
            skills: skills.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[derive(Clone, Copy)]
enum Kind {
    Instructions,
    Reference,
}

struct SkillTool {
    spec: ToolSpec,
    kind: Kind,
    skills: Arc<SkillSet>,
}

/// A skill that is not here, or not readable, and the ones that are.
#[derive(Serialize)]
struct NoSkill<'a> {
    ok: bool,
    error: String,
    available_skills: Vec<&'a str>,
}

/// A reference path that names no page of the skill, and the ones it has.
#[derive(Serialize)]
struct NoPage<'a> {
    ok: bool,
    skill: &'a str,
    error: String,
    references: &'a [String],
}

/// One page, in the web's key order.
#[derive(Serialize)]
struct Page<'a> {
    ok: bool,
    skill: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_notes: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    references: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<&'a str>,
    offset: usize,
    next_offset: Option<usize>,
    total_chars: usize,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// What a page carries beside its text.
struct Extra<'a> {
    agent_notes: Option<&'a str>,
    references: Option<&'a [String]>,
    reference: Option<&'a str>,
}

/// A payload as the model reads it: ordered JSON within the skill cap,
/// labelled as data (skill text is the project's own, but it is still text
/// the model did not write).
fn result<T: Serialize>(tool: &str, payload: &T) -> ToolOutcome {
    ToolOutcome::Ok(wrap_tool_result(
        tool,
        &dumps_capped_ordered(payload, SKILL_RESULT_CHARS),
        &[],
    ))
}

/// Python's truthiness of an argument as sent (`if not offset`).
fn falsy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => !b,
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
    }
}

/// The offset as a whole number (the web's `int(str(offset).strip())`), or
/// -1 when it is not one.
fn offset_of(v: Option<&Value>) -> i64 {
    match v {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .unwrap_or(-1),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(-1),
        Some(_) => -1,
    }
}

impl SkillTool {
    /// The skill offered on this run's surface, or the finished result
    /// naming the ones that are.
    fn find<'a>(
        &'a self,
        ctx: &ToolCtx,
        raw: Option<&Value>,
    ) -> Result<&'a LoadedSkill, ToolOutcome> {
        let tool = self.spec.name;
        let name = raw.and_then(Value::as_str).unwrap_or_default();
        let available_skills = self.skills.offered(ctx.surface);
        let error = match self.skills.find(name, ctx.surface) {
            Some((_, Some(skill))) => return Ok(skill),
            Some((spec, None)) => format!(
                "The {} skill could not be read on this server just now. Tell the operator its guidance is unavailable, and carry on carefully without it rather than calling again.",
                spec.name
            ),
            None => format!(
                "There is no skill called {} here. Call again with one of the available_skills names, or write without a skill if none fits.",
                py_repr(raw.unwrap_or(&Value::String(String::new())))
            ),
        };
        Err(result(
            tool,
            &NoSkill {
                ok: false,
                error,
                available_skills,
            },
        ))
    }

    /// The page of `text` that starts at `offset` and fits the budget. It
    /// ends on a line break when one falls in its second half, and shrinks
    /// until the whole result fits, so the cap's truncation never applies.
    fn paged(
        &self,
        skill: &LoadedSkill,
        text: &str,
        offset: Option<&Value>,
        extra: Extra<'_>,
    ) -> ToolOutcome {
        let tool = self.spec.name;
        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        let offset = offset_of(offset);
        if offset < 0 || offset as usize > total {
            return ToolOutcome::invalid_argument(
                "offset",
                &format!(
                    "it is outside the text, which is {} characters long.",
                    total
                ),
                Some("Pass 0 for the first page, or the next_offset the previous page returned."),
            );
        }
        let offset = offset as usize;
        let mut size = PAGE_CHARS.min(total - offset).max(1);
        let budget = SKILL_RESULT_CHARS - PAGE_HEADROOM;
        loop {
            let mut end = total.min(offset + size);
            if end < total {
                if let Some(i) = (offset + size / 2..end).rev().find(|&i| chars[i] == '\n') {
                    end = i + 1;
                }
            }
            let content: String = chars[offset..end].iter().collect();
            let next_offset = (end < total).then_some(end);
            let page = Page {
                ok: true,
                skill: skill.name(),
                agent_notes: extra.agent_notes,
                references: extra.references,
                reference: extra.reference,
                offset,
                next_offset,
                total_chars: total,
                content: &content,
                note: next_offset.map(|e| {
                    format!(
                        "This is characters {} to {} of {}. Call {} again with offset={} to read the next page.",
                        offset, e, total, tool, e
                    )
                }),
            };
            let measured = dumps_capped_ordered(&page, usize::MAX).chars().count();
            if measured <= budget || size <= 1 {
                return result(tool, &page);
            }
            // In proportion to the overshoot, and by at least one character,
            // so the loop ends.
            let shrunk = (size as f64 * budget as f64 / measured as f64 * 0.95) as usize;
            size = (size - 1).min(shrunk).max(1);
        }
    }

    fn instructions(&self, skill: &LoadedSkill, args: &Map<String, Value>) -> ToolOutcome {
        let first = falsy(args.get("offset"));
        let extra = Extra {
            agent_notes: first.then_some(skill.spec.agent_notes),
            references: first.then_some(skill.references.as_slice()),
            reference: None,
        };
        self.paged(skill, &skill.instructions, args.get("offset"), extra)
    }

    fn reference(&self, skill: &LoadedSkill, args: &Map<String, Value>) -> ToolOutcome {
        let path = args
            .get("reference_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match skill.read_reference(path) {
            Ok(text) => {
                let extra = Extra {
                    agent_notes: None,
                    references: None,
                    reference: Some(path),
                };
                self.paged(skill, &text, args.get("offset"), extra)
            }
            Err(e) => result(
                self.spec.name,
                &NoPage {
                    ok: false,
                    skill: skill.name(),
                    error: e.0,
                    references: &skill.references,
                },
            ),
        }
    }
}

#[async_trait]
impl AgentTool for SkillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let skill = match self.find(ctx, args.get("skill_name")) {
            Ok(skill) => skill,
            Err(answer) => return answer,
        };
        match self.kind {
            Kind::Instructions => self.instructions(skill, &args),
            Kind::Reference => self.reference(skill, &args),
        }
    }
}
