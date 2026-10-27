//! Tokens, cost and time to first token for one turn (the web's `_Totals`
//! and `EventTranslator._usage_frame`).
//!
//! Every usage frame carries the turn's running total, so the client keeps
//! the latest. A frame of zeros is never sent (several providers report no
//! usage on a stream, and zeros would read as a free turn), nor one that
//! says nothing new. Cost is arithmetic on the model's price table; a plan
//! turn has no cost and says `subscription`.

use crate::agent::contract::frames::{Billing, Frame, Usage};
use crate::agent::contract::model::ResolvedModel;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub total: u64,
    pub cached: u64,
    pub reasoning: u64,
}

impl Totals {
    /// One model call's usage as genai reports it.
    pub fn from_genai(u: &genai::chat::Usage) -> Totals {
        let n = |v: Option<i32>| v.map(|x| x.max(0) as u64).unwrap_or(0);
        Totals {
            input: n(u.prompt_tokens),
            output: n(u.completion_tokens),
            total: n(u.total_tokens),
            cached: n(u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)),
            reasoning: n(u
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)),
        }
    }

    fn add(&mut self, o: Totals) {
        self.input += o.input;
        self.output += o.output;
        self.total += o.total;
        self.cached += o.cached;
        self.reasoning += o.reasoning;
    }

    fn is_empty(&self) -> bool {
        *self == Totals::default()
    }
}

/// The turn's meter.
pub struct UsageMeter {
    totals: Totals,
    last: Option<Totals>,
    began: Instant,
    ttft_ms: Option<f64>,
    model: String,
    subscription: bool,
}

impl UsageMeter {
    pub fn new(model: &ResolvedModel) -> UsageMeter {
        UsageMeter {
            totals: Totals::default(),
            last: None,
            began: Instant::now(),
            ttft_ms: None,
            model: model.catalog_id.clone(),
            subscription: model.pricing.is_subscription(),
        }
    }

    /// The first character the trader could see (prose or reasoning):
    /// measured from the start of the run, tool calls before it included.
    pub fn first_output(&mut self) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(self.began.elapsed().as_secs_f64() * 1000.0);
        }
    }

    /// Add one model call and answer the frame to send, if any.
    pub fn record(&mut self, call: Totals, model: &ResolvedModel) -> Option<Frame> {
        self.totals.add(call);
        if self.totals.is_empty() || self.last == Some(self.totals) {
            return None;
        }
        self.last = Some(self.totals);
        let t = self.totals;
        let (billing, cost) = if self.subscription {
            (Billing::Subscription, None)
        } else {
            (
                Billing::Metered,
                model.pricing.cost(t.input, t.output, t.cached),
            )
        };
        Some(Frame::Usage(Usage {
            input_tokens: t.input,
            output_tokens: t.output,
            total_tokens: t.total,
            cached_tokens: t.cached,
            reasoning_tokens: t.reasoning,
            cost_usd: cost,
            billing,
            model: Some(self.model.clone()),
            ttft_ms: self.ttft_ms,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::model::{ModelCapabilities, NoCredentials, Prices, Pricing};
    use std::sync::Arc;

    pub(crate) fn model(pricing: Pricing) -> ResolvedModel {
        ResolvedModel {
            id: 1,
            provider_kind: "openai".into(),
            model_name: "gpt-x".into(),
            display_name: "GPT X".into(),
            catalog_id: "openai/gpt-x".into(),
            adapter: genai::adapter::AdapterKind::OpenAI,
            endpoint: "http://127.0.0.1:9/v1/".into(),
            credentials: Arc::new(NoCredentials),
            capabilities: ModelCapabilities::default(),
            context_window: None,
            pricing,
        }
    }

    #[test]
    fn frames_carry_the_running_total_and_never_zeros_or_repeats() {
        let m = model(Pricing::Metered(Some(Prices {
            input: Some(1.0),
            output: Some(2.0),
            cache_read: None,
        })));
        let mut meter = UsageMeter::new(&m);
        assert!(meter.record(Totals::default(), &m).is_none());
        let one = Totals {
            input: 10,
            output: 1,
            total: 11,
            ..Default::default()
        };
        let Some(Frame::Usage(u)) = meter.record(one, &m) else {
            panic!()
        };
        assert_eq!(
            (u.input_tokens, u.cost_usd, u.billing),
            (10, Some(12.0), Billing::Metered)
        );
        let Some(Frame::Usage(u)) = meter.record(one, &m) else {
            panic!()
        };
        assert_eq!(u.input_tokens, 20);
        assert!(meter.record(Totals::default(), &m).is_none(), "nothing new");
    }

    #[test]
    fn a_plan_turn_reports_tokens_and_no_cost() {
        let m = model(Pricing::Subscription);
        let mut meter = UsageMeter::new(&m);
        let Some(Frame::Usage(u)) = meter.record(
            Totals {
                input: 5,
                ..Default::default()
            },
            &m,
        ) else {
            panic!()
        };
        assert_eq!((u.cost_usd, u.billing), (None, Billing::Subscription));
    }
}
