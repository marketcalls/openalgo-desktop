//! Derived option analytics: a combined premium series and a payoff diagram
//! (the web's `services/agent/tools/option_viz.py`, toolkit `option_viz`).
//!
//! Both tools obey the rule of every rendering tool: **the model supplies
//! the selection, never a number.** Which contracts, which side, how many
//! is the model's to say; every price, strike, lot size, spot, premium and
//! expiry is fetched here, and a leg the instrument master does not hold is
//! refused rather than guessed. The leg vocabulary (sides, lots, symbols,
//! expiries, contracts) is shared with the live combination card and lives
//! in [`super::options_common`].
//!
//! - [`premium`]: "chart the straddle" means two different series. With no
//!   legs it is the rolling ATM straddle the Straddle page computes (the
//!   strike follows spot); with legs it is those contracts summed with the
//!   strikes held. Built from closes and drawn as a line: a combined high
//!   is not the sum of the legs' highs.
//! - [`payoff`]: this file resolves and prices the legs; the browser draws
//!   the curve with the Strategy Builder's own maths, so the two are never
//!   two implementations.

mod payoff;
mod premium;
#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::tools::market::BrokerIntervals;
use crate::agent::tools::options::normalise_int;
use crate::agent::tools::options_common::leg_entries;
use crate::agent::tools::specs;
use crate::agent::tools::values::{py_repr, py_str, truthy};
use crate::agent::tools::viz::arg_or;
use crate::agent::tools::viz::indicators::normalise_indicators;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The renderer the payoff tool emits.
pub const PAYOFF_VIZ: &str = "payoff";

/// The renderer the premium series emits: the platform's own chart engine,
/// so a study attaches to the combined series as it does on a price chart.
pub const PREMIUM_VIZ: &str = "candles";

/// Candle size when the model names none.
pub const DEFAULT_PREMIUM_INTERVAL: &str = "5m";

/// What to draw when the asked size is not one the broker serves.
pub const FALLBACK_PREMIUM_INTERVAL: &str = "1m";

/// Trading sessions a premium series spans by default, and at most.
pub const DEFAULT_PREMIUM_DAYS: i64 = 1;
pub const MAX_PREMIUM_DAYS: i64 = 5;

/// Most points one premium series carries (newest kept, with a notice).
pub const MAX_PREMIUM_POINTS: usize = 2500;

pub const GROUP: ToolGroup = ToolGroup {
    key: "option_viz",
    surfaces: Surfaces::ALL,
    requires: &[],
    order: 43,
    description: "Draw a combined option premium over time, rolling ATM or on fixed legs, and a payoff diagram of named legs or of the operator's own open positions.",
    build,
};

#[derive(Debug, Clone, Copy)]
enum Kind {
    CombinedPremium,
    Payoff,
}

struct OptionVizTool {
    spec: ToolSpec,
    kind: Kind,
    intervals: Arc<BrokerIntervals>,
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let intervals = Arc::new(BrokerIntervals::default());
    [
        ("plot_combined_premium", Kind::CombinedPremium),
        ("plot_payoff", Kind::Payoff),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let spec = specs::web_or_log(GROUP.key, name)?;
        Some(Arc::new(OptionVizTool {
            spec,
            kind,
            intervals: intervals.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

#[async_trait]
impl AgentTool for OptionVizTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let done = match self.kind {
            Kind::CombinedPremium => plot_combined_premium(ctx, &args, &self.intervals).await,
            Kind::Payoff => payoff::plot_payoff(ctx, &args).await,
        };
        done.unwrap_or_else(|refusal| refusal)
    }
}

/// Settle the candle size for a premium series, falling back rather than
/// refusing: the operator asked to see a combination and the size is
/// incidental. It never substitutes silently; the notice names both sizes.
/// `accepted` is `None` when the intervals lookup failed (passed through:
/// the history service checks again).
pub fn premium_interval(
    value: Option<&Value>,
    accepted: Option<&[String]>,
) -> (String, Option<String>) {
    let given = value
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let cleaned = if given.is_empty() {
        DEFAULT_PREMIUM_INTERVAL.to_string()
    } else {
        given
    };
    let Some(accepted) = accepted.filter(|a| !a.is_empty()) else {
        return (cleaned, None);
    };
    if accepted.contains(&cleaned) {
        return (cleaned, None);
    }
    let repr = |s: &str| py_repr(&json!(s));
    let matches: Vec<&String> = accepted
        .iter()
        .filter(|a| a.to_lowercase() == cleaned.to_lowercase())
        .collect();
    if let [only] = matches.as_slice() {
        return (
            only.to_string(),
            Some(format!(
                "The interval was read as {} rather than {}. Interval names are case sensitive: 'm' is minutes and 'M' is months.",
                repr(only),
                repr(&cleaned)
            )),
        );
    }
    for candidate in [FALLBACK_PREMIUM_INTERVAL, DEFAULT_PREMIUM_INTERVAL] {
        if accepted.iter().any(|a| a == candidate) {
            return (
                candidate.to_string(),
                Some(format!(
                    "This broker does not serve {} candles, so the series was drawn at {} instead. It is a different picture of the same session.",
                    repr(&cleaned),
                    repr(candidate)
                )),
            );
        }
    }
    let first = accepted[0].clone();
    let notice = format!(
        "This broker does not serve {} candles, so the series was drawn at {}, the first size it does serve.",
        repr(&cleaned),
        repr(&first)
    );
    (first, Some(notice))
}

/// A single JSON object a model sent as text, decoded; anything else as it
/// came (an invalid string is left for the normaliser to report).
fn json_text(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(s)) if s.trim().starts_with('{') => {
            Some(serde_json::from_str(s.trim()).unwrap_or_else(|_| Value::String(s.clone())))
        }
        other => other.cloned(),
    }
}

/// `plot_combined_premium`: no legs is the rolling ATM straddle, named legs
/// are those contracts summed with the strikes held.
async fn plot_combined_premium(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    intervals: &BrokerIntervals,
) -> Result<ToolOutcome, ToolOutcome> {
    let entries = leg_entries(args.get("legs"), "legs")?;
    let accepted = intervals.accepted(ctx).await;
    let asked = arg_or(args, "interval", json!(DEFAULT_PREMIUM_INTERVAL));
    let (interval, notice) = premium_interval(Some(&asked), accepted.as_deref());
    let notices: Vec<String> = notice.into_iter().collect();
    let sessions = normalise_int(
        Some(&arg_or(args, "days", json!(DEFAULT_PREMIUM_DAYS))),
        "days",
        1,
        MAX_PREMIUM_DAYS,
        None,
    )?;
    let overlays = normalise_indicators(json_text(args.get("indicators")).as_ref())?;
    let request = premium::Request {
        interval,
        sessions,
        overlays,
        notices,
    };
    if entries.is_empty() {
        premium::rolling_atm(ctx, args, request).await
    } else {
        premium::fixed_legs(ctx, args, &entries, request).await
    }
}
