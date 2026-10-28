//! Save an OpenScript source the Agent wrote, and list the scripts already
//! stored (the web's `services/agent/tools/openscript_gen.py`, toolkit
//! `openscript_gen`).
//!
//! **Source only, by necessity.** The OpenScript compiler is TypeScript and
//! runs in the page; the Agent can store a script's source and nothing more.
//! A source with no compiled program beside it opens in the /trading
//! editor, which compiles and plots it, and nothing runs it until the
//! trader saves it from that editor with a clean console. Every result says
//! so, because "saved" must not be read as "compiled" or "deployed".
//!
//! **One write path.** The file work is [`ScriptStore::save`], the same
//! function the /trading editor's route calls: one name rule, one size
//! limit, one per-file lock and the same replace sequence with a backup.
//! The Agent asks it never to replace a script that has a compiled program:
//! that program is what a deployed strategy starts from.
//!
//! Saving pauses for the trader's approval (its spec's `needs_approval`): it
//! writes a file the trader may later run.
//!
//! [`ScriptStore::save`]: crate::trading::scripts::ScriptStore::save

use super::lane_specs::web_or_log;
use crate::agent::contract::json::{dumps_capped_ordered, strip_code_fence};
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::tools::service::py_repr;
use crate::trading::names::is_script_name;
use crate::trading::scripts::{Replace, SaveError, Saved, StoredScript, MAX_SOURCE_BYTES};
use async_trait::async_trait;
use chrono_tz::Asia::Kolkata;
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Most scripts described by one listing.
pub const MAX_LISTED_FILES: usize = 200;

/// The extension every stored script carries.
pub const EXTENSION: &str = ".oscript";

/// The toolkit (web `ToolkitSpec` `openscript_gen`): chat and chart, not
/// voice; offered with trading off, since it places nothing.
pub const GROUP: ToolGroup = ToolGroup {
    key: "openscript_gen",
    surfaces: Surfaces::CHAT_AND_CHART,
    requires: &[],
    order: 65,
    description: "Save an OpenScript source to strategies/openscript/ for the /trading editor to compile. Never compiles or runs it.",
    build,
};

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [
        ("list_openscripts", Kind::List),
        ("save_openscript", Kind::Save),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = web_or_log(GROUP.key, name)?;
        Some(Arc::new(ScriptTool { spec, kind }) as Arc<dyn AgentTool>)
    })
    .collect()
}

/// What the trader does next, stated in every successful save.
fn next_step(file: &str) -> String {
    format!(
        "Saved as source only. Nothing has compiled it yet, so it may still have errors, and it cannot run as a strategy until it is compiled. The trader opens /trading and picks {} in the OpenScript editor: it compiles there, plots on the chart and lists any errors in the console. Saving it from that editor once the console is clean stores the compiled program a strategy needs before it can be backtested or deployed.",
        file
    )
}

#[derive(Clone, Copy)]
enum Kind {
    List,
    Save,
}

struct ScriptTool {
    spec: ToolSpec,
    kind: Kind,
}

#[derive(Serialize)]
struct Listed {
    file: String,
    bytes: u64,
    modified: String,
    compiled: bool,
}

#[derive(Serialize)]
struct Listing {
    ok: bool,
    count: usize,
    total_on_disk: usize,
    truncated: bool,
    scripts: Vec<Listed>,
    note: &'static str,
}

#[derive(Serialize)]
struct Failed {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<bool>,
    error: String,
}

#[derive(Serialize)]
struct SavedAnswer {
    ok: bool,
    saved: bool,
    file: String,
    bytes: usize,
    lines: usize,
    replaced: bool,
    backup: Option<String>,
    compiled: bool,
    next_step: String,
}

/// A payload as the model reads it: ordered JSON within the shared cap,
/// labelled as data.
fn result<T: Serialize>(tool: &str, payload: &T) -> ToolOutcome {
    ToolOutcome::Ok(wrap_tool_result(
        tool,
        &dumps_capped_ordered(payload, MAX_RESULT_CHARS),
        &[],
    ))
}

/// ISO 8601 in IST, as the web's `datetime.fromtimestamp(..., IST)`.
fn modified(mtime: i64) -> String {
    chrono::DateTime::from_timestamp(mtime, 0)
        .map(|t| {
            t.with_timezone(&Kolkata)
                .format("%Y-%m-%dT%H:%M:%S%:z")
                .to_string()
        })
        .unwrap_or_default()
}

async fn list(ctx: &ToolCtx) -> ToolOutcome {
    let tool = "list_openscripts";
    let store = ctx.app.trading.scripts.clone();
    let entries: std::io::Result<Vec<StoredScript>> =
        tokio::task::spawn_blocking(move || store.list())
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
    let entries = match entries {
        Ok(entries) => entries,
        Err(e) => {
            tracing::error!("Could not list the OpenScript folder: {}", e);
            return result(
                tool,
                &Failed {
                    ok: false,
                    saved: None,
                    error: "The OpenScript folder could not be read. This is a problem on the server; tell the operator rather than calling again.".into(),
                },
            );
        }
    };
    let scripts: Vec<Listed> = entries
        .iter()
        .take(MAX_LISTED_FILES)
        .map(|e| Listed {
            file: e.file.clone(),
            bytes: e.bytes,
            modified: modified(e.mtime),
            compiled: e.program,
        })
        .collect();
    result(
        tool,
        &Listing {
            ok: true,
            count: scripts.len(),
            total_on_disk: entries.len(),
            truncated: entries.len() > MAX_LISTED_FILES,
            scripts,
            note: "A script with compiled=false opens in the /trading OpenScript editor but cannot run as a strategy until it is saved from there with a clean console.",
        },
    )
}

/// The script name checked against the store's rule, `.oscript` added when
/// missing (the web's `_require_filename`).
fn require_filename(raw: Option<&Value>) -> Result<String, ToolOutcome> {
    let Some(given) = raw.and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
        return Err(ToolOutcome::invalid_argument(
            "filename",
            "it is empty.",
            Some("Pass a short name such as 'ema_cross'."),
        ));
    };
    let mut name = given.trim().to_string();
    if !name.to_lowercase().ends_with(EXTENSION) {
        name.push_str(EXTENSION);
    }
    if !is_script_name(&name) {
        return Err(ToolOutcome::invalid_argument(
            "filename",
            &format!("{} is not a valid script name.", py_repr(&Value::String(given.to_string()))),
            Some("Use letters, digits, dot, dash or underscore, starting with a letter or digit, at most 64 characters before .oscript, for example 'ema_cross'."),
        ));
    }
    Ok(name)
}

/// Python's `type(x).__name__` of a decoded JSON value.
fn python_type(v: Option<&Value>) -> &'static str {
    match v {
        None | Some(Value::Null) => "NoneType",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(n)) if n.is_f64() => "float",
        Some(Value::Number(_)) => "int",
        Some(Value::String(_)) => "str",
        Some(Value::Array(_)) => "list",
        Some(Value::Object(_)) => "dict",
    }
}

/// The source: a string, a fence around the whole of it removed, not
/// empty, within the store's size limit (the web's `_require_source`).
fn require_source(raw: Option<&Value>) -> Result<String, ToolOutcome> {
    let Some(source) = raw.and_then(Value::as_str) else {
        return Err(ToolOutcome::invalid_argument(
            "source",
            &format!("it is a {}, not a string.", python_type(raw)),
            Some("Pass the complete OpenScript source as one string."),
        ));
    };
    let text = if source.trim_start().starts_with("```") {
        strip_code_fence(source)
    } else {
        source.to_string()
    };
    if text.trim().is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "source",
            "it is empty.",
            Some("Pass the complete OpenScript source."),
        ));
    }
    if text.len() > MAX_SOURCE_BYTES {
        return Err(ToolOutcome::invalid_argument(
            "source",
            &format!(
                "it is {} bytes and the limit is {}.",
                text.len(),
                MAX_SOURCE_BYTES
            ),
            Some("Write a shorter script."),
        ));
    }
    Ok(text)
}

/// The refusal for a taken name, saying what to do instead.
fn taken(name: &str, compiled: bool) -> ToolOutcome {
    if compiled {
        return ToolOutcome::invalid_argument(
            "filename",
            &format!("{} has been compiled and saved from the /trading editor, so it may be deployed and running as a strategy, and saving source over it would stop it from starting next time. replace=true does not override this.", name),
            Some("Save under a new name, or tell the operator to make this change in the OpenScript editor on /trading."),
        );
    }
    ToolOutcome::invalid_argument(
        "filename",
        &format!("a script named {} already exists.", name),
        Some("Pick a new name. If the operator asked you to change that script, tell them it will be replaced (the previous version is kept as a .bak file) and call again with replace=true."),
    )
}

/// The save's audit rows (the web's `audited` attempt and result: the file
/// name, the size and the replace flag, never the source text). Written to
/// the log; the Agent's audit trail joins here when it is in the same build.
fn audit(phase: &str, name: &str, bytes: usize, replace: bool, outcome: &str) {
    tracing::info!(
        "Agent audit save_openscript {}: filename={} source_bytes={} replace={} {}",
        phase,
        name,
        bytes,
        replace,
        outcome
    );
}

async fn save(ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
    let tool = "save_openscript";
    let name = match require_filename(args.get("filename")) {
        Ok(n) => n,
        Err(refusal) => return refusal,
    };
    let text = match require_source(args.get("source")) {
        Ok(t) => t,
        Err(refusal) => return refusal,
    };
    let bytes = text.len();
    let replace = args.get("replace") == Some(&Value::Bool(true));
    audit("attempt", &name, bytes, replace, "");
    let store = ctx.app.trading.scripts.clone();
    let (file, source) = (name.clone(), text.clone());
    let rule = Replace {
        source: replace,
        compiled: false,
    };
    let saved: Result<Saved, SaveError> =
        tokio::task::spawn_blocking(move || store.save(&file, &source, None, rule))
            .await
            .unwrap_or_else(|e| {
                Err(SaveError::Refused(crate::trading::scripts::Refused {
                    status: 500,
                    message: e.to_string(),
                }))
            });
    match saved {
        Ok(saved) => {
            audit(
                "result",
                &name,
                bytes,
                replace,
                &format!("ok replaced={}", saved.replaced),
            );
            tracing::info!("Agent saved OpenScript source {}", name);
            result(
                tool,
                &SavedAnswer {
                    ok: true,
                    saved: true,
                    lines: text.matches('\n').count() + usize::from(!text.ends_with('\n')),
                    bytes: saved.bytes,
                    replaced: saved.replaced,
                    backup: saved.replaced.then(|| format!("{}.bak", name)),
                    compiled: false,
                    next_step: next_step(&name),
                    file: name,
                },
            )
        }
        Err(SaveError::Taken { compiled }) => {
            let reason = if compiled { "compiled" } else { "exists" };
            audit(
                "result",
                &name,
                bytes,
                replace,
                &format!("refused reason={}", reason),
            );
            taken(&name, compiled)
        }
        Err(SaveError::Refused(r)) => {
            tracing::error!(
                "Could not save the Agent's OpenScript source {}: {}",
                name,
                r.message
            );
            audit("result", &name, bytes, replace, "error reason=write failed");
            result(
                tool,
                &Failed {
                    ok: false,
                    saved: Some(false),
                    error: format!("{} could not be written to the scripts folder. This is a problem on the server, not with the script; tell the operator rather than calling again.", name),
                },
            )
        }
    }
}

#[async_trait]
impl AgentTool for ScriptTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        match self.kind {
            Kind::List => list(ctx).await,
            Kind::Save => save(ctx, &args).await,
        }
    }
}
