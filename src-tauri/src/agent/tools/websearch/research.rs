//! `web_research`: Perplexity's cited answer.

use super::args::constrained;
use super::{blocked, busy, failure, json_text, mark_used, stored_key, WebTool};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::payloads::{Answer, NotOk};
use crate::agent::websearch::providers;
use crate::agent::websearch::PERPLEXITY;
use serde_json::{Map, Value};

const NO_PERPLEXITY_KEY: &str = "No Perplexity key is stored in the agent settings, so cited research is unavailable. Use web_search instead, which needs no key, and tell the operator they can add a Perplexity key in the agent settings.";

impl WebTool {
    /// `web_research`.
    pub(super) async fn research(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let q = match constrained(
            args,
            "question",
            &self.shared.operator_message,
            &self.shared.operator_history,
        ) {
            Ok(q) => q,
            Err(refused) => return refused,
        };
        if q.blocked() {
            return blocked("web_research");
        }
        let Some(key) = stored_key(&ctx.app, PERPLEXITY).await else {
            tracing::info!("Web research refused before dispatch: no Perplexity key configured");
            return ToolOutcome::json_ordered(
                &NotOk::new("not_configured", NO_PERPLEXITY_KEY).by(PERPLEXITY),
            );
        };
        if let Some(refusal) = self.shared.budget.refusal(&ctx.app).await {
            tracing::info!("Web research refused before dispatch: {}", refusal.error);
            return ToolOutcome::json_ordered(&refusal);
        }
        let config = self.shared.budget.config(&ctx.app).await;
        let o =
            providers::perplexity(&self.shared.net, &q.text, &config.perplexity_model, &key).await;
        mark_used(&ctx.app, &o).await;
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
            self.shared.budget.record_attempt();
            let p = NotOk::new("provider_failed", o.error.clone())
                .by(PERPLEXITY)
                .with_cost(o.cost_usd);
            return failure(PERPLEXITY, &p);
        }
        self.shared.budget.record(&ctx.app).await;
        let payload = Answer {
            ok: true,
            provider: PERPLEXITY,
            model: &config.perplexity_model,
            question_sent: &q.text,
            question_source: q.decision.as_str(),
            answer: &o.answer,
            citation_count: o.citations.len(),
            citations: &o.citations,
            cost_usd: o.cost_usd,
        };
        ToolOutcome::Ok(wrap_web_result(
            PERPLEXITY,
            &json_text(&payload),
            &[("kind", Some("synthesised_answer"))],
        ))
    }
}
