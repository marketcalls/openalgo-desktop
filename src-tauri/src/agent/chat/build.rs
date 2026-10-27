//! Building one run (the web's `_chat_preconditions`, the conversation
//! lookup, `builder.build_agent` and `build_session_state`).
//!
//! Database reads run on the blocking pool; every failure answers with the
//! web's status and sentence before the stream opens.

use super::{paused_state, Caller, Refusal, NOT_FOUND};
use crate::agent::contract::approval::{Decision, DecisionScope};
use crate::agent::contract::model::{ModelError, ResolvedModel};
use crate::agent::contract::tool::{AgentTool, Capabilities, Surface, ToolCtx};
use crate::agent::contract::viz::VizSink;
use crate::agent::conversations::store::{self, Claim, Conversation};
use crate::agent::prompts::{self, SystemPrompt};
use crate::agent::run::client::{effective_effort, ModelClient};
use crate::agent::run::{history, PausedState};
use crate::agent::tools::build_tools;
use crate::security::Secret;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use genai::chat::ChatMessage;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The setup gate's answer when nothing is configured.
pub const NOT_CONFIGURED: &str = "No model is configured for the agent. Add a model, test its credentials and set it as the default before chatting.";

/// The answer when the trader has no OpenAlgo API key.
pub const NO_API_KEY: &str = "This deployment has no OpenAlgo API key. Generate one at /apikey so the agent's tools can reach the platform.";

fn internal(context: &str, e: impl std::fmt::Display) -> Refusal {
    tracing::error!("{}: {}", context, e);
    Refusal::new(500, "Could not start the agent", None)
}

/// The setup gate and the trader's API key, refused with 409 when either
/// is missing (the browser is not the only thing enforcing the gate).
pub async fn preconditions(app: &Arc<AppState>) -> Result<Secret, Refusal> {
    let resolver = app.agent.resolver();
    let (configured, key) = app
        .blocking(move |ctx| {
            let configured = resolver.is_configured(ctx);
            // Read only to learn whether one exists and to hand it to the
            // tools; never logged (the error is reduced to its kind).
            let key = ApiKeyService::current(ctx).unwrap_or_else(|e| {
                // Only the kind of failure: a decryption error's own text
                // could quote what it choked on.
                let kind = e.to_string();
                tracing::error!(
                    "Could not read the OpenAlgo API key: {}",
                    kind.split(':').next().unwrap_or("error")
                );
                None
            });
            Ok((configured, key))
        })
        .await
        .map_err(|e| internal("Agent preconditions", e))?;
    if !configured {
        return Err(Refusal::new(
            409,
            NOT_CONFIGURED,
            Some(json!({"kind": "config", "configured": false})),
        ));
    }
    key.ok_or_else(|| Refusal::new(409, NO_API_KEY, Some(json!({"kind": "config"}))))
}

/// Python's `int()` of a JSON value.
pub fn whole_number(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// The model the request named (the web's `_as_model_id`): absent or blank
/// is the default; anything that is not an id answers like an unknown one.
pub fn model_id_of(body: &Map<String, Value>) -> Result<Option<i64>, Refusal> {
    let not_an_id = |shown: String| {
        Refusal::new(
            404,
            format!("{} is not a model id", shown),
            Some(json!({"kind": "config"})),
        )
    };
    match body.get("model_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Err(not_an_id(if *b { "True".into() } else { "False".into() })),
        Some(Value::Number(n)) => n.as_i64().map(Some).ok_or_else(|| not_an_id(n.to_string())),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| not_an_id(format!("'{}'", s.trim()))),
        Some(other) => Err(not_an_id(other.to_string())),
    }
}

pub async fn owned_conversation(
    app: &Arc<AppState>,
    id: i64,
    user: &str,
) -> Result<Option<Conversation>, Refusal> {
    let user = user.to_string();
    app.blocking(move |ctx| store::get_conversation(&*ctx.sqlite.conn()?, id, &user))
        .await
        .map_err(|e| internal("Agent conversation read", e))
}

/// The conversation a new turn goes to: a new one titled by the message,
/// or the named one (404 when it is not the trader's). The flag says
/// whether this request opened it.
pub async fn conversation_for(
    app: &Arc<AppState>,
    caller: &Caller,
    body: &Map<String, Value>,
    message: &str,
    surface: Surface,
) -> Result<(Conversation, bool), Refusal> {
    let title: String = message.chars().take(80).collect();
    let raw = body.get("conversation_id");
    if matches!(raw, None | Some(Value::Null)) || raw == Some(&Value::String(String::new())) {
        let user = caller.user.clone();
        let now = app.now();
        let created = app
            .blocking(move |ctx| {
                store::create_conversation(
                    &*ctx.sqlite.conn()?,
                    &user,
                    Some(&title),
                    surface.as_str(),
                    now,
                )
            })
            .await
            .map_err(|e| {
                tracing::error!("Could not create an agent conversation: {}", e);
                Refusal::new(500, "Could not create the conversation", None)
            })?;
        return Ok((created, true));
    }
    let id = whole_number(raw)
        .ok_or_else(|| Refusal::bad_request("conversation_id must be a whole number"))?;
    let conv = owned_conversation(app, id, &caller.user)
        .await?
        .ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    if conv.title.as_deref().unwrap_or("").is_empty() {
        let (user, now) = (caller.user.clone(), app.now());
        let _ = app
            .blocking(move |ctx| {
                store::update_conversation(&*ctx.sqlite.conn()?, id, &user, Some(&title), None, now)
            })
            .await;
    }
    Ok((conv, false))
}

/// Remove a conversation this request opened when the run could not be
/// built, so a failed attempt leaves no empty row.
pub async fn discard_conversation(app: &Arc<AppState>, id: i64, user: &str) {
    let user = user.to_string();
    if let Err(e) = app
        .blocking(move |ctx| store::delete_conversation(&*ctx.sqlite.conn()?, id, &user))
        .await
    {
        tracing::error!("Could not discard the empty conversation {}: {}", id, e);
    }
}

/// Everything one run is built from.
pub struct Turnkey {
    pub caller: Caller,
    pub conversation: Conversation,
    pub surface: Surface,
    pub api_key: Secret,
    pub operator_message: String,
    pub trading_asked: bool,
    pub web_search: bool,
    pub chart_context: Option<Map<String, Value>>,
    pub runtime_lines: Vec<String>,
    pub model_id: Option<i64>,
    pub effort: Option<String>,
    pub require_vision: bool,
    /// A resume: a failure reads "Could not resume the run".
    pub resuming: bool,
}

/// One run, built.
pub struct RunParts {
    pub ctx: ToolCtx,
    pub model: ResolvedModel,
    pub effort: Option<&'static str>,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub history: Vec<ChatMessage>,
    pub client: Arc<ModelClient>,
    /// The browser session that started it.
    pub session: String,
}

fn model_refusal(e: ModelError) -> Refusal {
    let kind = serde_json::to_value(e.kind()).unwrap_or(Value::Null);
    Refusal::new(e.status(), e.message(), Some(json!({"kind": kind})))
}

/// What a run reads from the database before it starts.
struct Reads {
    settings: crate::agent::contract::policy::RunSettings,
    analyzer: bool,
    history: Vec<String>,
}

async fn reads(
    app: &Arc<AppState>,
    surface: Surface,
    session: Option<String>,
) -> Result<Reads, Refusal> {
    let policy = app.agent.policy();
    app.blocking(move |ctx| {
        let settings = policy.settings(ctx, surface);
        // Live is the safe reading of an unreadable analyzer switch: an
        // order that needs analyzer mode is then refused, never let through.
        let analyzer = ctx.sqlite.get_analyze_mode().unwrap_or(false);
        let history = match session {
            Some(s) => store::history(&*ctx.sqlite.conn()?, &s, history::HISTORY_RUNS)?,
            None => Vec::new(),
        };
        Ok(Reads {
            settings,
            analyzer,
            history,
        })
    })
    .await
    .map_err(|e| internal("Agent run reads", e))
}

/// Resolve the model and build the run's context, tools, prompt and
/// history.
pub async fn run_parts(app: &Arc<AppState>, key: Turnkey) -> Result<RunParts, Refusal> {
    let model = app
        .agent
        .resolver()
        .resolve(app, key.model_id)
        .await
        .map_err(model_refusal)?;
    if key.require_vision && !model.capabilities.vision {
        return Err(model_refusal(ModelError::VisionUnsupported(format!(
            "{} cannot read images. Remove the image, or choose a model that supports vision.",
            model.display_name
        ))));
    }
    if model.capabilities.tools_unreliable {
        tracing::warn!(
            "Agent model {} is flagged tools_unreliable; this agent is tool driven and answers from it may be poor",
            model.display_name
        );
    }
    let failed = |r: Refusal| {
        if key.resuming && r.status == 500 {
            Refusal::new(500, "Could not resume the run", None)
        } else {
            r
        }
    };
    let client = app
        .agent
        .client()
        .map_err(|e| failed(internal("Agent model client", e)))?;
    let r = reads(app, key.surface, key.conversation.session_id.clone())
        .await
        .map_err(failed)?;
    let trading = key.trading_asked
        && r.settings.trading_enabled
        && (key.surface != Surface::Voice || r.settings.voice_trading_enabled);
    let caps = Capabilities {
        trading_enabled: trading,
        web_search_enabled: key.web_search,
        mcp_enabled: r.settings.mcp_enabled,
    };
    let ctx = ToolCtx {
        app: app.clone(),
        surface: key.surface,
        conversation_id: Some(key.conversation.id),
        run_id: uuid::Uuid::new_v4().to_string(),
        session_id: key
            .conversation
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        user_id: key.caller.user.clone(),
        api_key: key.api_key,
        caps,
        analyzer_mode: r.analyzer,
        operator_message: key.operator_message,
        chart_context: key.chart_context,
        viz: VizSink::new(),
        cancel: CancellationToken::new(),
    };
    let tools = build_tools(app.agent.toolkits(), &ctx);
    let extra = app
        .agent
        .section_sources()
        .iter()
        .flat_map(|s| s.sections(app, key.surface, &caps))
        .collect();
    let system_prompt = prompts::build_system_prompt(&SystemPrompt {
        surface: Some(key.surface),
        trading_enabled: trading,
        analyzer_mode: r.analyzer,
        now: Some(app.now().with_timezone(&chrono_tz::Asia::Kolkata)),
        override_text: r.settings.system_prompt_override.as_deref(),
        extra_sections: extra,
        extra_runtime_lines: key.runtime_lines,
        voice_agent_name: r.settings.voice_agent_name.as_deref(),
        max_chars: Some(prompts::max_prompt_chars()),
        addresses: Some(addresses(app)),
    });
    tracing::info!(
        "Agent built: model={} surface={} trading={} analyzer={} conversation={}",
        model.display_name,
        key.surface.as_str(),
        trading,
        r.analyzer,
        key.conversation.id
    );
    Ok(RunParts {
        effort: effective_effort(&model, key.effort.as_deref()),
        history: history::replay(&r.history),
        ctx,
        model,
        system_prompt,
        tools,
        client,
        session: key.caller.session,
    })
}

/// The app's own addresses for generated code: the configured App and
/// market data ports, on this computer.
fn addresses(app: &AppState) -> (String, String) {
    let c = app.server_config();
    (
        format!("http://127.0.0.1:{}", app.listening_port()),
        format!("ws://127.0.0.1:{}", c.ws_port),
    )
}

/// Store the trader's own message (never the composed input: the web
/// search boundary reads it back); its id, or none when it could not be
/// stored.
pub async fn store_user_message(
    app: &Arc<AppState>,
    conversation: i64,
    message: &str,
    notices: Option<Value>,
) -> Option<i64> {
    let (message, now) = (message.to_string(), app.now());
    app.blocking(move |ctx| {
        store::add_message(
            &*ctx.sqlite.conn()?,
            conversation,
            "user",
            &message,
            None,
            notices.as_ref(),
            now,
        )
    })
    .await
    .map(|m| m.id)
    .map_err(|e| tracing::error!("Could not store the agent question: {}", e))
    .ok()
}

/// The trader's last question in a conversation (the resume rebuilds the
/// run from it); empty when there is none.
pub async fn last_user_row(app: &Arc<AppState>, conversation: i64) -> store::Message {
    app.blocking(move |ctx| store::last_message_of_role(&*ctx.sqlite.conn()?, conversation, "user"))
        .await
        .ok()
        .flatten()
        .unwrap_or(store::Message {
            id: 0,
            conversation_id: conversation,
            role: "user".into(),
            content: String::new(),
            tools: json!([]),
            notices: json!([]),
            created_at: String::new(),
        })
}

/// The answer when a resume finds the approval gone.
pub const EXPIRED: &str =
    "This approval expired after 10 minutes, so nothing was placed. Ask again.";
pub const ALREADY_ANSWERED: &str =
    "This approval was already answered, so nothing more was placed.";
pub const NOTHING_WAITING: &str = "Nothing is waiting for your approval on this answer.";

/// Claim the paused run for this resume (one approval resumes it once)
/// and load its messages.
pub async fn claim(
    app: &Arc<AppState>,
    run_id: &str,
    user: &str,
    conversation_id: i64,
) -> Result<(PausedState, Vec<ChatMessage>), Refusal> {
    let (run, user, now) = (run_id.to_string(), user.to_string(), app.now());
    let (claim, messages) = app
        .blocking(move |ctx| {
            let conn = ctx.sqlite.conn()?;
            let claim = match store::claim_paused(&conn, &run, &user, now)? {
                // Another conversation's run: not this resume's to take.
                Claim::Claimed(row) if row.conversation_id != conversation_id => {
                    store::unclaim_paused(&conn, &run)?;
                    Claim::NotFound
                }
                other => other,
            };
            let messages = match &claim {
                Claim::Claimed(_) => store::run_messages(&conn, &run)?,
                _ => None,
            };
            Ok((claim, messages))
        })
        .await
        .map_err(|e| internal("Agent approval claim", e))?;
    let refuse = |m: &str, kind: &str| Refusal::new(409, m, Some(json!({"kind": kind})));
    match claim {
        Claim::Claimed(row) => {
            let state = paused_state(&row).ok_or_else(|| refuse(NOTHING_WAITING, "input"))?;
            let messages = messages
                .and_then(|m| serde_json::from_str::<Vec<ChatMessage>>(&m).ok())
                .unwrap_or_default();
            Ok((state, messages))
        }
        Claim::NotFound => Err(refuse(NOTHING_WAITING, "input")),
        Claim::Expired => Err(refuse(EXPIRED, "expired")),
        Claim::AlreadyAnswered => Err(refuse(ALREADY_ANSWERED, "input")),
    }
}

/// Hand the decisions to the approval hook before the run resumes, so an
/// approval is in the trail even if the resumed run then fails.
pub async fn record_decisions(
    app: &Arc<AppState>,
    caller: &Caller,
    ctx: &ToolCtx,
    paused: &PausedState,
    decisions: &HashMap<String, bool>,
    names: &HashMap<String, String>,
    note: Option<&str>,
) {
    let list: Vec<Decision> = decisions
        .iter()
        .map(|(id, approved)| {
            let call = paused
                .calls
                .iter()
                .find(|c| &c.call.call_id == id || &c.requirement.id == id);
            Decision {
                tool_call_id: id.clone(),
                tool_name: call
                    .map(|c| c.requirement.tool_name.clone())
                    .or_else(|| names.get(id).cloned())
                    .unwrap_or_else(|| "pending_tool".into()),
                args: call.map(|c| c.requirement.args.clone()).unwrap_or_default(),
                approved: *approved,
            }
        })
        .collect();
    let scope = DecisionScope {
        conversation_id: ctx.conversation_id,
        run_id: ctx.run_id.clone(),
        actor: caller.user.clone(),
        note: note.map(str::to_string),
    };
    app.agent.approvals().decided(app, &scope, &list).await;
}
