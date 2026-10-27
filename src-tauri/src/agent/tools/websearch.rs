//! The `websearch` toolkit (the web's `WebSearchToolkit`): `web_search`
//! for links from DuckDuckGo or Tavily, `web_research` for Perplexity's
//! cited answer. Offered on every surface, only when the turn's Web search
//! switch is on (the composer's per-turn switch; absent means on).
//!
//! The safety envelope is the web's: the outgoing query is built from the
//! trader's own message (`websearch::taint`), a per-turn budget and a
//! daily cap are checked before a call and counted only after one
//! succeeds, results come back inside a `<web_result trust="third-party">`
//! block, and the decision is logged, never the query.
//!
//! D-020: the web returned a provider's failure as bare JSON while its
//! results were wrapped; a provider's failure is third-party text too, so
//! it is wrapped the same way (`kind="error"`).

use crate::agent::contract::json::dumps_capped;
use crate::agent::contract::tool::{
    AgentTool, Capability, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::providers::{self, Net, Outcome};
use crate::agent::websearch::settings::{self, Config};
use crate::agent::websearch::taint::{self, Constrained};
use crate::agent::websearch::{duckduckgo, today, DUCKDUCKGO, PERPLEXITY, TAVILY};
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

pub const GROUP: ToolGroup = ToolGroup {
    key: "websearch",
    surfaces: Surfaces::ALL,
    requires: &[Capability::WebSearch],
    order: 80,
    description: "Web search for links, and cited web research.",
    build,
};

pub const DEFAULT_MAX_RESULTS: i64 = 5;
pub const MIN_MAX_RESULTS: i64 = 1;
pub const MAX_MAX_RESULTS: i64 = 10;

const NO_OPERATOR_MESSAGE: &str = "Nothing was sent to any provider. Every outgoing query is built from the operator's own message for this turn, and that message is not available to this tool, so there was nothing safe to send. Tell the operator that web search is unavailable for this turn and answer from what you already have.";
const NO_PERPLEXITY_KEY: &str = "No Perplexity key is stored in the agent settings, so cited research is unavailable. Use web_search instead, which needs no key, and tell the operator they can add a Perplexity key in the agent settings.";
const PERPLEXITY_TAVILY: &str = "The configured provider is Perplexity, which answers questions rather than returning links, so Tavily answered this search. Use web_research for a Perplexity answer.";
const PERPLEXITY_DUCKDUCKGO: &str = "The configured provider is Perplexity, which answers questions rather than returning links, so DuckDuckGo answered this search. Use web_research for a Perplexity answer.";
const TAVILY_WITHOUT_KEY: &str = "Tavily is the configured provider but no Tavily key is stored in the agent settings, so DuckDuckGo answered instead.";

/// The two tools' specs as the web sends them (copied from the web
/// capture; see `websearch/specs.json`).
static SPECS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../websearch/specs.json")).unwrap_or(Value::Null)
});

fn spec(name: &'static str) -> Option<ToolSpec> {
    let entry = SPECS.get("tools")?.get(name)?;
    Some(ToolSpec {
        name,
        description: Cow::Owned(entry.get("description")?.as_str()?.to_string()),
        parameters: entry.get("parameters")?.clone(),
        needs_approval: entry.get("needs_approval")?.as_bool()?,
    })
}

/// The caps on one run's searches, shared by its two tools.
struct Budget {
    /// Read on the first call (the settings rows), then kept for the run.
    config: tokio::sync::OnceCell<Config>,
    used: Mutex<i64>,
}

/// What both tools share in one run.
struct Shared {
    operator_message: String,
    budget: Budget,
    net: Net,
}

fn build(ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let shared = Arc::new(Shared {
        operator_message: taint::redact(&ctx.operator_message),
        budget: Budget {
            config: tokio::sync::OnceCell::new(),
            used: Mutex::new(0),
        },
        net: providers::net(),
    });
    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
    for (name, research) in [("web_search", false), ("web_research", true)] {
        match spec(name) {
            Some(spec) => tools.push(Arc::new(WebTool {
                spec,
                research,
                shared: shared.clone(),
            })),
            None => tracing::error!(
                "Agent toolkit websearch has no spec for {}; not offered",
                name
            ),
        }
    }
    tools
}

struct WebTool {
    spec: ToolSpec,
    research: bool,
    shared: Arc<Shared>,
}

#[async_trait]
impl AgentTool for WebTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        if self.research {
            self.research(ctx, &args).await
        } else {
            self.search(ctx, &args).await
        }
    }
}

fn json_text(v: &Value) -> String {
    dumps_capped(v, MAX_RESULT_CHARS)
}

/// A provider's failure, wrapped as third-party text (D-020).
fn failure(provider: &str, payload: &Value) -> ToolOutcome {
    ToolOutcome::Ok(wrap_web_result(
        provider,
        &json_text(payload),
        &[("kind", Some("error"))],
    ))
}

fn busy(outcome: &Outcome, notices: &[String]) -> ToolOutcome {
    let mut p = json!({"ok": false, "error": "platform_busy", "provider": outcome.provider,
                       "message": outcome.error});
    if !notices.is_empty() {
        p["notices"] = json!(notices);
    }
    ToolOutcome::json(&p)
}

fn blocked(tool: &str) -> ToolOutcome {
    tracing::warn!(
        "{} refused: no operator message is available for this turn, so no query could be constructed",
        tool
    );
    ToolOutcome::json(
        &json!({"ok": false, "error": "no_operator_message", "message": NO_OPERATOR_MESSAGE}),
    )
}

/// The count the model asked for, clamped to 1..10 with a notice; a value
/// that is not a whole number is refused (the web's
/// `_validated_max_results`).
fn max_results(args: &Map<String, Value>) -> Result<(i64, Option<String>), ToolOutcome> {
    let fix = format!(
        "Pass an integer from {} to {}, for example {}.",
        MIN_MAX_RESULTS, MAX_MAX_RESULTS, DEFAULT_MAX_RESULTS
    );
    let refuse = || {
        ToolOutcome::invalid_argument(
            "max_results",
            "it is not a whole number.",
            Some(fix.as_str()),
        )
    };
    let asked = match args.get("max_results") {
        None => DEFAULT_MAX_RESULTS,
        Some(Value::Number(n)) => n.as_i64().ok_or_else(refuse)?,
        Some(Value::String(s)) => s.trim().parse::<i64>().map_err(|_| refuse())?,
        Some(_) => return Err(refuse()),
    };
    let clamped = asked.clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS);
    let notice = (clamped != asked).then(|| {
        format!(
            "{} results were asked for; searches return {} to {}, so {} were requested.",
            asked, MIN_MAX_RESULTS, MAX_MAX_RESULTS, clamped
        )
    });
    Ok((clamped, notice))
}

/// The taint boundary on one argument; an empty one is refused.
fn constrained(
    args: &Map<String, Value>,
    field: &str,
    operator: &str,
) -> Result<Constrained, ToolOutcome> {
    let text = match args.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(v) if crate::agent::models::truthy(v) => crate::agent::settings::pyrepr::py_str(v),
        _ => String::new(),
    };
    if text.trim().is_empty() {
        return Err(ToolOutcome::invalid_argument(
            field,
            "it is empty.",
            Some("Pass the operator's own question as plain text."),
        ));
    }
    Ok(taint::constrain(&text, operator))
}

impl WebTool {
    async fn config(&self, app: &Arc<AppState>) -> Config {
        self.shared
            .budget
            .config
            .get_or_init(|| async {
                app.blocking(|c| Ok(Config::read(&*c.sqlite.conn()?)))
                    .await
                    .unwrap_or_else(|_| Config::from_rows(&Default::default()))
            })
            .await
            .clone()
    }

    /// Both caps, checked before a call (`_Budget.refusal`).
    async fn refusal(&self, app: &Arc<AppState>) -> Option<Value> {
        let c = self.config(app).await;
        if *self.shared.budget.used.lock() >= c.max_calls_per_turn {
            return Some(
                json!({"ok": false, "error": "per_turn_budget_exhausted", "message": format!(
                "This turn has already used its {} web searches. Answer from what you have and tell the operator you stopped searching.",
                c.max_calls_per_turn)}),
            );
        }
        let day = today(app.now());
        let used = app
            .blocking(move |x| Ok(settings::usage(&*x.sqlite.conn()?, &day)))
            .await
            .unwrap_or(0);
        (used >= c.daily_cap).then(|| json!({"ok": false, "error": "daily_cap_reached", "message": format!(
            "The daily web search cap of {} calls is reached. Tell the operator; they can raise it in the agent settings.",
            c.daily_cap)}))
    }

    /// Count one successful call against both caps.
    async fn record(&self, app: &Arc<AppState>) {
        *self.shared.budget.used.lock() += 1;
        let day = today(app.now());
        if let Err(e) = app
            .blocking(move |x| settings::count_one(&*x.sqlite.conn()?, &day))
            .await
        {
            tracing::error!("Could not count a web search against today's cap: {}", e);
        }
    }

    async fn key(&self, app: &Arc<AppState>, provider: &'static str) -> Option<Secret> {
        app.blocking(move |c| Ok(settings::stored_key(c, provider)))
            .await
            .ok()
            .flatten()
    }

    async fn mark_used(&self, app: &Arc<AppState>, outcome: &Outcome) {
        if outcome.key_used {
            let p = outcome.provider;
            let _ = app
                .blocking(move |c| {
                    settings::mark_used(c, p);
                    Ok(())
                })
                .await;
        }
    }

    fn log(&self, tool: &str, provider: &str, q: &Constrained, results: usize, per_turn: i64) {
        tracing::info!(
            "{}: provider={} decision={} kept={} dropped={} results={} turn={}/{}",
            tool,
            provider,
            q.decision.as_str(),
            q.kept,
            q.dropped,
            results,
            *self.shared.budget.used.lock(),
            per_turn
        );
    }

    /// `web_search`.
    async fn search(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let (count, count_notice) = match max_results(args) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let q = match constrained(args, "query", &self.shared.operator_message) {
            Ok(q) => q,
            Err(refused) => return refused,
        };
        if q.blocked() {
            return blocked("web_search");
        }
        if let Some(refusal) = self.refusal(&ctx.app).await {
            tracing::info!("Web search refused before dispatch: {}", refusal["error"]);
            return ToolOutcome::json(&refusal);
        }
        let config = self.config(&ctx.app).await;
        let mut notices: Vec<String> = count_notice.into_iter().collect();
        let outcome = match self
            .link_search(ctx, &config, &q, count as usize, &mut notices)
            .await
        {
            Ok(o) => o,
            Err(busy_outcome) => return busy(&busy_outcome, &notices),
        };
        self.log(
            "web_search",
            outcome.provider,
            &q,
            outcome.results.len(),
            config.max_calls_per_turn,
        );
        if !outcome.ok {
            return failure(
                outcome.provider,
                &json!({"ok": false, "error": "provider_failed", "provider": outcome.provider,
                        "message": outcome.error, "notices": notices}),
            );
        }
        self.record(&ctx.app).await;
        let payload = json!({
            "ok": true,
            "provider": outcome.provider,
            "query_sent": q.text,
            "query_source": q.decision.as_str(),
            "result_count": outcome.results.len(),
            "results": outcome.results,
            "notices": notices,
        });
        ToolOutcome::Ok(wrap_web_result(
            outcome.provider,
            &json_text(&payload),
            &[("kind", Some("links"))],
        ))
    }

    /// The link provider's answer: Perplexity configured falls back to
    /// Tavily when a Tavily key is stored, else DuckDuckGo; Tavily without
    /// a key, or failing, falls back to DuckDuckGo; a busy gate does not.
    async fn link_search(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        q: &Constrained,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Result<Outcome, Outcome> {
        let mut provider = config.provider;
        let mut tavily_key = None;
        if provider == PERPLEXITY {
            tavily_key = self.key(&ctx.app, TAVILY).await;
            if tavily_key.is_some() {
                notices.push(PERPLEXITY_TAVILY.into());
                provider = TAVILY;
            } else {
                notices.push(PERPLEXITY_DUCKDUCKGO.into());
                provider = DUCKDUCKGO;
            }
        }
        if provider == TAVILY {
            if tavily_key.is_none() {
                tavily_key = self.key(&ctx.app, TAVILY).await;
            }
            match tavily_key {
                None => notices.push(TAVILY_WITHOUT_KEY.into()),
                Some(key) => {
                    let o = providers::tavily(&self.shared.net, &q.text, count, &key).await;
                    self.mark_used(&ctx.app, &o).await;
                    if o.busy {
                        return Err(o);
                    }
                    if o.ok {
                        return Ok(o);
                    }
                    notices.push(format!("{} DuckDuckGo answered instead.", o.error));
                }
            }
        }
        let o = duckduckgo::search(&self.shared.net, &q.text, count).await;
        if o.busy {
            return Err(o);
        }
        Ok(o)
    }

    /// `web_research`.
    async fn research(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let q = match constrained(args, "question", &self.shared.operator_message) {
            Ok(q) => q,
            Err(refused) => return refused,
        };
        if q.blocked() {
            return blocked("web_research");
        }
        let Some(key) = self.key(&ctx.app, PERPLEXITY).await else {
            tracing::info!("Web research refused before dispatch: no Perplexity key configured");
            return ToolOutcome::json(&json!({"ok": false, "error": "not_configured",
                                             "provider": PERPLEXITY, "message": NO_PERPLEXITY_KEY}));
        };
        if let Some(refusal) = self.refusal(&ctx.app).await {
            tracing::info!("Web research refused before dispatch: {}", refusal["error"]);
            return ToolOutcome::json(&refusal);
        }
        let config = self.config(&ctx.app).await;
        let o =
            providers::perplexity(&self.shared.net, &q.text, &config.perplexity_model, &key).await;
        self.mark_used(&ctx.app, &o).await;
        if o.busy {
            return busy(&o, &[]);
        }
        self.log(
            "web_research",
            PERPLEXITY,
            &q,
            o.citations.len(),
            config.max_calls_per_turn,
        );
        if !o.ok {
            return failure(
                PERPLEXITY,
                &json!({"ok": false, "error": "provider_failed", "provider": PERPLEXITY,
                        "message": o.error, "cost_usd": o.cost_usd}),
            );
        }
        self.record(&ctx.app).await;
        let payload = json!({
            "ok": true,
            "provider": PERPLEXITY,
            "model": config.perplexity_model,
            "question_sent": q.text,
            "question_source": q.decision.as_str(),
            "answer": o.answer,
            "citation_count": o.citations.len(),
            "citations": o.citations,
            "cost_usd": o.cost_usd,
        });
        ToolOutcome::Ok(wrap_web_result(
            PERPLEXITY,
            &json_text(&payload),
            &[("kind", Some("synthesised_answer"))],
        ))
    }
}
