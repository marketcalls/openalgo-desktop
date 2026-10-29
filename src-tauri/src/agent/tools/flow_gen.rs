//! Validate Flow workflow JSON the Agent wrote, and import a valid one as an
//! inactive workflow (the web's `services/agent/tools/flow_gen.py`, toolkit
//! `flow_gen`).
//!
//! **The schema lives in one place.** `docs/prompt/flow-import-format.md`
//! is the contract for the workflow shape: every node type, every edge
//! variant, the interpolation grammar and the handle vocabulary. Both tool
//! descriptions and every message here send the model to it by that path,
//! and the flow-builder skill serves that file as its
//! `reference/flow-import-format.md` page, so the model can read what it is
//! pointed at (the web points it at a file nothing serves; parity A-35, lead
//! decision 17). Nothing here holds a copy of the schema.
//!
//! **The validator is the ground truth.** [`validate_workflow`] is called
//! directly, always strict, after the same legacy migration the `/flow`
//! import route applies, so a workflow this accepts is one the import would
//! accept. Its `errors[]` go back to the model verbatim, inside a Retry, so
//! it corrects its own JSON instead of shipping a graph that fails later at
//! activation or mid-run. **An invalid workflow is never saved**: the check
//! runs before the row is created.
//!
//! **One write path, inactive.** The row is made by [`create_workflow`], the
//! function the import route uses (new webhook token and secret, inactive).
//! Nothing here activates, runs, schedules or monitors a workflow, or touches
//! its webhook: the trader reviews and activates it in /flow. Saving pauses
//! for the trader's approval (its spec's `needs_approval`), and the approval
//! card states the workflow's size, never its text ([`FlowTool::card_args`]).
//!
//! Desktop departures, each recorded in the lane report: JSON that does not
//! parse is described in our own words with the line and the column in
//! characters (Python's decoder messages have no serde_json counterpart;
//! lead decision 17), and `NaN` or `Infinity`, which Python's decoder takes,
//! is refused as not JSON (lead decision 15).

use super::specs::web_or_log;
use super::values::type_name;
use crate::agent::contract::json::{dumps_capped_ordered, strip_code_fence};
use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::redact::{redact, redact_map};
use crate::agent::safety::audit::{self, Entry, Phase};
use crate::flow::contract::pyfmt;
use crate::flow::service::create_workflow;
use crate::flow::validator::{
    self, migrate_legacy_node_data, validate_workflow, ValidateOptions, ValidationError,
    TRIGGER_CONFIG_FIELDS,
};
use async_trait::async_trait;
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Where the model is sent for the schema. Never paraphrased here.
pub const SCHEMA_DOC: &str = "docs/prompt/flow-import-format.md";

/// Upper bound on one workflow document, in characters. The validator caps
/// nodes and edges itself; this stops a runaway string before it is parsed.
pub const MAX_JSON_CHARS: usize = 400_000;

/// The longest name: `flow_workflows.name` is 255 wide on the web and the
/// import suffix costs 11 more.
pub const MAX_NAME_CHARS: usize = 200;

/// The suffix the `/flow` import appends, so an Agent-imported workflow
/// reads the same as a hand-imported one in the list.
pub const IMPORT_SUFFIX: &str = " (imported)";

/// Most validator errors handed back in one message: a graph with hundreds
/// of errors has one root cause, and the first few carry it.
pub const MAX_REPORTED_ERRORS: usize = 50;

/// The toolkit (web `ToolkitSpec` `flow_gen`): chat only, never the chart
/// panel or voice; offered with trading off, since it places nothing.
pub const GROUP: ToolGroup = ToolGroup {
    key: "flow_gen",
    surfaces: Surfaces::CHAT_ONLY,
    requires: &[],
    order: 70,
    description: "Validate Flow workflow JSON, and import a valid one as an inactive workflow.",
    build,
};

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    [("validate_flow", Kind::Validate), ("save_flow", Kind::Save)]
        .into_iter()
        .filter_map(|(name, kind)| {
            let spec = web_or_log(GROUP.key, name)?;
            Some(Arc::new(FlowTool { spec, kind }) as Arc<dyn AgentTool>)
        })
        .collect()
}

const VALIDATED_NEXT_STEP: &str =
    "Nothing was saved. Show the workflow to the user, then call save_flow with a name to import it.";

const SAVED_NEXT_STEP: &str = "The workflow was imported and is INACTIVE, so it is not running. The user opens /flow to review and activate it. This tool cannot activate or execute a workflow.";

const WRITE_FAILED: &str = "The workflow passed validation but could not be written to the database. This is a platform failure, not a problem with the JSON. Report it to the user rather than calling the tool again.";

#[derive(Clone, Copy)]
enum Kind {
    Validate,
    Save,
}

struct FlowTool {
    spec: ToolSpec,
    kind: Kind,
}

/// A map written in the order its entries are listed: the web's dicts keep
/// insertion order and the model reads them so (a `Value` map is sorted).
struct InOrder<V>(Vec<(String, V)>);

impl<V: Serialize> Serialize for InOrder<V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct Validated<'a> {
    ok: bool,
    valid: bool,
    saved: bool,
    name: Option<&'a Value>,
    node_count: usize,
    edge_count: usize,
    trigger: InOrder<InOrder<Value>>,
    migrations: &'a [String],
    next_step: &'static str,
}

#[derive(Serialize)]
struct Rejected<'a> {
    valid: bool,
    saved: bool,
    error_count: usize,
    errors: &'a [ValidationError],
}

#[derive(Serialize)]
struct SavedAnswer<'a> {
    ok: bool,
    saved: bool,
    workflow_id: i64,
    name: &'a str,
    is_active: bool,
    node_count: usize,
    edge_count: usize,
    migrations: &'a [String],
    next_step: &'static str,
}

#[derive(Serialize)]
struct Failed {
    ok: bool,
    saved: bool,
    error: &'static str,
}

/// A payload as the model reads it: ordered JSON within the shared cap,
/// labelled as data (the web's `_result`).
fn result<T: Serialize>(tool: &str, payload: &T) -> ToolOutcome {
    ToolOutcome::Ok(wrap_tool_result(
        tool,
        &dumps_capped_ordered(payload, MAX_RESULT_CHARS),
        &[],
    ))
}

/// The parsed workflow (an object) and the migration notes (the web's
/// `_prepare`).
struct Prepared {
    payload: Map<String, Value>,
    migrations: Vec<String>,
}

/// Parse the argument and upgrade legacy node payloads, as the import
/// route does before it validates.
fn prepare(raw: Option<&Value>) -> Result<Prepared, ToolOutcome> {
    let mut payload = parse(raw)?;
    let mut migrations = Vec::new();
    if let Some(Value::Array(nodes)) = payload.get_mut("nodes") {
        let (migrated, notes) = migrate_legacy_node_data(std::mem::take(nodes));
        *nodes = migrated;
        migrations = notes;
    }
    Ok(Prepared {
        payload,
        migrations,
    })
}

fn invalid_workflow_json(reason: &str, fix: &str) -> ToolOutcome {
    ToolOutcome::invalid_argument("workflow_json", reason, Some(fix))
}

/// The `workflow_json` argument as an object: a shallow copy of an object
/// the runtime already parsed, or JSON text within the size limit, a code
/// fence around the whole of it removed (the web's `_parse`).
fn parse(raw: Option<&Value>) -> Result<Map<String, Value>, ToolOutcome> {
    let text = match raw {
        Some(Value::Object(m)) => return Ok(m.clone()),
        Some(Value::String(s)) => s,
        other => {
            return Err(invalid_workflow_json(
                &format!(
                    "it is a {}, not a JSON string.",
                    type_name(other.unwrap_or(&Value::Null))
                ),
                &format!(
                    "Send the workflow object as JSON text, as described in {}.",
                    SCHEMA_DOC
                ),
            ))
        }
    };
    let chars = text.chars().count();
    if chars > MAX_JSON_CHARS {
        return Err(invalid_workflow_json(
            &format!(
                "it is {} characters, over the {} limit.",
                chars, MAX_JSON_CHARS
            ),
            "Build a smaller workflow.",
        ));
    }
    let text = strip_code_fence(text);
    if text.is_empty() {
        return Err(invalid_workflow_json(
            "it is empty.",
            &format!(
                "Send the whole workflow object as JSON text, as described in {}.",
                SCHEMA_DOC
            ),
        ));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(other) => Err(invalid_workflow_json(
            &format!("it parses to a JSON {}, not an object.", type_name(&other)),
            &format!(
                "The workflow must be {{\"name\": ..., \"nodes\": [...], \"edges\": [...]}}. See {}.",
                SCHEMA_DOC
            ),
        )),
        Err(e) => Err(ToolOutcome::Retry(not_json(&text, &e))),
    }
}

/// The refusal for text that is not JSON, naming what is wrong and where
/// in our own words (lead decision 17).
fn not_json(text: &str, e: &serde_json::Error) -> String {
    let (column, rest) = char_column(text, e.line(), e.column());
    format!(
        "The 'workflow_json' argument is not valid JSON: {} at line {}, column {}. Nothing was validated and nothing was saved. Emit the JSON object alone, with no code fence, no comments and no trailing commas, as {} requires, and call the tool again.",
        parse_problem(e, rest),
        e.line(),
        column,
        SCHEMA_DOC
    )
}

/// The 1-based column in characters of serde_json's 1-based byte column on
/// `line`, and the text of that line from the column on.
fn char_column(text: &str, line: usize, byte_column: usize) -> (usize, &str) {
    let row = text.split('\n').nth(line.saturating_sub(1)).unwrap_or("");
    let mut at = byte_column.saturating_sub(1).min(row.len());
    while !row.is_char_boundary(at) {
        at -= 1;
    }
    (row[..at].chars().count() + 1, &row[at..])
}

/// What is wrong, in words a model can act on, from serde_json's reason.
fn parse_problem(e: &serde_json::Error, rest: &str) -> String {
    if ["NaN", "Infinity", "-Infinity"]
        .iter()
        .any(|w| rest.starts_with(w))
    {
        return "NaN and Infinity are not JSON values; write a number or null".into();
    }
    let full = e.to_string();
    let suffix = format!(" at line {} column {}", e.line(), e.column());
    let reason = full.strip_suffix(&suffix).unwrap_or(&full);
    let words = match reason {
        r if r.starts_with("EOF while parsing") => "the text ends before the JSON is complete",
        "expected value" | "expected ident" => "a value was expected",
        "expected `:`" => "a colon was expected after the key",
        "expected `,` or `]`" => "a comma or a closing ] was expected",
        "expected `,` or `}`" => "a comma or a closing } was expected",
        "key must be a string" => "an object key must be a double-quoted string",
        "trailing comma" => "a comma comes right before a closing bracket",
        "trailing characters" => "more text follows the JSON object",
        "recursion limit exceeded" => "the JSON is nested too deeply",
        "invalid number" | "number out of range" => "a number is not valid",
        r if r.starts_with("control character") => {
            "a string holds a raw control character such as a line break; escape it"
        }
        r if r.contains("escape") || r.contains("unicode") || r.contains("surrogate") => {
            "a string holds an invalid escape"
        }
        other => return other.to_string(),
    };
    words.to_string()
}

/// Run the real validator, always strict, and refuse the workflow if it
/// reports anything, carrying the first errors verbatim (the web's
/// `_reject_if_invalid`).
fn reject_if_invalid(
    tool: &str,
    payload: &Map<String, Value>,
    require_name: bool,
) -> Result<(), ToolOutcome> {
    let checks = ValidateOptions {
        require_name,
        strict: true,
    };
    let errors = validate_workflow(&Value::Object(payload.clone()), checks);
    if errors.is_empty() {
        return Ok(());
    }
    let reported = &errors[..errors.len().min(MAX_REPORTED_ERRORS)];
    let block = wrap_tool_result(
        tool,
        &dumps_capped_ordered(
            &Rejected {
                valid: false,
                saved: false,
                error_count: errors.len(),
                errors: reported,
            },
            MAX_RESULT_CHARS,
        ),
        &[],
    );
    let omitted = if errors.len() > reported.len() {
        format!(
            " {} further error(s) are not listed.",
            errors.len() - reported.len()
        )
    } else {
        String::new()
    };
    Err(ToolOutcome::Retry(format!(
        "The workflow failed Flow's own validator with {} error(s), so nothing was saved. Each entry names the JSON path, a code and a reason.{}\n{}\nFix exactly those paths and call the tool again. Node types and node data fields are defined in {}; copy them verbatim and never invent a node type. If a requirement has no matching node type, tell the user in prose instead.",
        errors.len(),
        omitted,
        block,
        SCHEMA_DOC
    )))
}

/// `payload.get(key) or []`.
fn list_or_empty(payload: &Map<String, Value>, key: &str) -> Value {
    match payload.get(key) {
        Some(v) if pyfmt::truthy(v) => v.clone(),
        _ => json!([]),
    }
}

fn count(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// `trigger_config(nodes)` with the web's key order: trigger types in the
/// order the nodes first name them, fields in `TRIGGER_CONFIG_FIELDS`
/// order.
fn trigger_in_order(nodes: &[Value]) -> InOrder<InOrder<Value>> {
    let config = serde_json::to_value(validator::trigger_config(nodes)).unwrap_or_default();
    let mut out: Vec<(String, InOrder<Value>)> = Vec::new();
    for node in nodes {
        let Some(t) = node.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(fields) = config.get(t).and_then(Value::as_object) else {
            continue;
        };
        if out.iter().any(|(k, _)| k == t) {
            continue;
        }
        let ordered = TRIGGER_CONFIG_FIELDS
            .iter()
            .filter_map(|f| fields.get(*f).map(|v| ((*f).to_string(), v.clone())))
            .collect();
        out.push((t.to_string(), InOrder(ordered)));
    }
    InOrder(out)
}

fn validate(args: &Map<String, Value>) -> ToolOutcome {
    let tool = "validate_flow";
    let prepared = match prepare(args.get("workflow_json")) {
        Ok(p) => p,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = reject_if_invalid(tool, &prepared.payload, false) {
        return refusal;
    }
    let nodes = list_or_empty(&prepared.payload, "nodes");
    let edges = list_or_empty(&prepared.payload, "edges");
    result(
        tool,
        &Validated {
            ok: true,
            valid: true,
            saved: false,
            name: prepared.payload.get("name"),
            node_count: count(&nodes),
            edge_count: count(&edges),
            trigger: trigger_in_order(nodes.as_array().map_or(&[][..], Vec::as_slice)),
            migrations: &prepared.migrations,
            next_step: VALIDATED_NEXT_STEP,
        },
    )
}

/// The trimmed name, within the column's width (the web's
/// `_require_name`; trimmed as Python's `str.strip`, counted in code
/// points).
fn require_name(raw: Option<&Value>) -> Result<String, ToolOutcome> {
    let cleaned = raw
        .and_then(Value::as_str)
        .map(validator::py::strip)
        .unwrap_or_default();
    if cleaned.is_empty() {
        return Err(ToolOutcome::invalid_argument(
            "name",
            "it is empty.",
            Some("Pass a short descriptive name such as 'NIFTY opening range breakout'."),
        ));
    }
    let chars = cleaned.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(ToolOutcome::invalid_argument(
            "name",
            &format!(
                "it is {} characters, over the {} limit.",
                chars, MAX_NAME_CHARS
            ),
            Some("Shorten it to a title rather than a description."),
        ));
    }
    Ok(cleaned.to_string())
}

fn audit_entry(ctx: &ToolCtx, phase: Phase) -> Entry {
    let mut e = Entry::new(phase, "save_flow");
    e.conversation_id = ctx.conversation_id;
    e.run_id = Some(ctx.run_id.clone());
    e
}

/// The attempt row, before the store is touched (the web's `audited`):
/// the stored name and the graph's size, never the graph.
async fn audit_attempt(ctx: &ToolCtx, name: &str, nodes: usize, edges: usize) -> Option<i64> {
    let mut e = audit_entry(ctx, Phase::Attempt);
    e.args = Some(json!({"name": name, "node_count": nodes, "edge_count": edges}));
    audit::append(&ctx.app, e).await
}

/// The result row, naming its attempt (`audit.record`).
async fn audit_result(ctx: &ToolCtx, attempt_id: Option<i64>, ok: bool, response: Value) {
    let mut e = audit_entry(ctx, Phase::Result);
    e.attempt_id = attempt_id;
    e.ok = Some(ok);
    e.response = Some(response);
    audit::append(&ctx.app, e).await;
}

async fn save(ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
    let tool = "save_flow";
    let name = match require_name(args.get("name")) {
        Ok(n) => n,
        Err(refusal) => return refusal,
    };
    let mut prepared = match prepare(args.get("workflow_json")) {
        Ok(p) => p,
        Err(refusal) => return refusal,
    };
    prepared
        .payload
        .insert("name".into(), Value::String(name.clone()));
    if let Err(refusal) = reject_if_invalid(tool, &prepared.payload, true) {
        return refusal;
    }
    let nodes = list_or_empty(&prepared.payload, "nodes");
    let edges = list_or_empty(&prepared.payload, "edges");
    let (node_count, edge_count) = (count(&nodes), count(&edges));
    let description = prepared
        .payload
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stored_name = format!("{}{}", name, IMPORT_SUFFIX);
    let attempt_id = audit_attempt(ctx, &stored_name, node_count, edge_count).await;
    match create_workflow(&ctx.app, stored_name.clone(), description, nodes, edges).await {
        Ok(w) => {
            audit_result(
                ctx,
                attempt_id,
                true,
                json!({"workflow_id": w.id, "name": stored_name}),
            )
            .await;
            tracing::info!(
                "Agent imported Flow workflow {} as id {}",
                stored_name,
                w.id
            );
            result(
                tool,
                &SavedAnswer {
                    ok: true,
                    saved: true,
                    workflow_id: w.id,
                    name: &stored_name,
                    is_active: false,
                    node_count,
                    edge_count,
                    migrations: &prepared.migrations,
                    next_step: SAVED_NEXT_STEP,
                },
            )
        }
        Err(e) => {
            tracing::error!(
                "Could not create the agent-generated Flow workflow {:?}: {}",
                stored_name,
                e
            );
            audit_result(
                ctx,
                attempt_id,
                false,
                json!({"status": "error", "message": "create failed"}),
            )
            .await;
            result(
                tool,
                &Failed {
                    ok: false,
                    saved: false,
                    error: WRITE_FAILED,
                },
            )
        }
    }
}

/// What the approval card reads of a workflow: each node's id and type and
/// each edge's ends, redacted, so the card can say "N nodes, M connections"
/// (`src/lib/agent/orderWords.ts`) for a workflow of any size. `None` when
/// the argument is not readable as a workflow object, so the card keeps
/// the redacted text and says it cannot read it.
fn outline(raw: &Value) -> Option<Value> {
    let parsed = match raw {
        Value::Object(_) => None,
        Value::String(s) if s.chars().count() <= MAX_JSON_CHARS => {
            serde_json::from_str::<Value>(&strip_code_fence(s)).ok()
        }
        _ => return None,
    };
    let graph = parsed.as_ref().unwrap_or(raw).as_object()?;
    let pick = |key: &str, fields: [&str; 2]| -> Value {
        let items = graph.get(key).and_then(Value::as_array);
        Value::Array(
            items
                .into_iter()
                .flatten()
                .map(|item| {
                    let kept: Map<String, Value> = fields
                        .iter()
                        .filter_map(|f| item.get(*f).map(|v| ((*f).to_string(), redact(v))))
                        .collect();
                    Value::Object(kept)
                })
                .collect(),
        )
    };
    Some(json!({
        "nodes": pick("nodes", ["id", "type"]),
        "edges": pick("edges", ["source", "target"]),
    }))
}

#[async_trait]
impl AgentTool for FlowTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// The card shows `save_flow`'s workflow by its size, and the name as
    /// it is stored (trimmed); the shared bounding would cut a workflow's
    /// text at 2,000 characters and leave the card nothing to count
    /// (parity A-25).
    fn card_args(&self, args: &Map<String, Value>) -> Map<String, Value> {
        let mut card = redact_map(args);
        if !matches!(self.kind, Kind::Save) {
            return card;
        }
        if let Some(outlined) = args.get("workflow_json").and_then(outline) {
            card.insert("workflow_json".into(), outlined);
        }
        let trimmed = card
            .get("name")
            .and_then(Value::as_str)
            .map(|n| validator::py::strip(n).to_string());
        if let Some(name) = trimmed {
            card.insert("name".into(), Value::String(name));
        }
        card
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        match self.kind {
            Kind::Validate => validate(&args),
            Kind::Save => save(ctx, &args).await,
        }
    }
}
