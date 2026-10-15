//! The confirmation the trader reads before a restart: every Live item
//! named in trading terms, or the lighter sandbox-only notice.

use super::activity::Activity;
use serde::Serialize;

/// The confirmation the trader is asked for before a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// `live` (Live trading would go unwatched) or `sandbox` (only sandbox
    /// activity pauses).
    pub level: &'static str,
    pub title: String,
    pub intro: String,
    pub items: Vec<String>,
    pub advice: String,
}

/// The words that change with what ends OpenAlgo for a while.
struct Wording {
    /// When a paused OpenScript run can be started again.
    resume: &'static str,
    /// When to check what OpenAlgo could not read.
    before: &'static str,
    live_intro: &'static str,
    live_advice: &'static str,
    sandbox_intro: &'static str,
}

/// An update's restart.
const UPDATE: Wording = Wording {
    resume: "after the update",
    before: "before you update",
    live_intro: "Updating restarts OpenAlgo. Until it opens again (usually within a minute) and you sign in, nothing watches your Live trades:",
    live_advice: "Update now only if that is acceptable. Otherwise choose Later and update after the market closes.",
    sandbox_intro: "Updating restarts OpenAlgo. Your sandbox activity pauses until it opens again:",
};

impl Activity {
    /// What the trader is told before a restart, or `None` when nothing
    /// runs. `broker` is the logged-in broker's display name.
    pub fn warning(&self, broker: Option<&str>) -> Option<Warning> {
        self.warning_in(&UPDATE, broker)
    }

    /// The warning in `words`, or `None` when nothing runs.
    fn warning_in(&self, words: &Wording, broker: Option<&str>) -> Option<Warning> {
        if self.is_quiet() {
            return None;
        }
        let sandbox = self.sandbox_items(words);
        let warning = if self.has_live() {
            let mut items = self.live_items(words, broker);
            if !sandbox.is_empty() {
                items.push(format!(
                    "Sandbox: {}. Nothing there is sent to your broker.",
                    join_list(&sandbox)
                ));
            }
            live_warning(words, items)
        } else {
            sandbox_warning(words, sandbox)
        };
        Some(warning)
    }

    /// One sentence per kind of Live work a restart leaves unwatched.
    fn live_items(&self, words: &Wording, broker: Option<&str>) -> Vec<String> {
        let at = broker.map(|b| format!(" at {}", b)).unwrap_or_default();
        let mut items = Vec::new();
        let n = self.live_strategies;
        if n > 0 {
            items.push(format!(
                "{}. {} not checked until OpenAlgo opens again; then {} resume.",
                count(n, "running Live strategy", "running Live strategies"),
                pick(
                    n,
                    "Its stops, targets and exits are",
                    "Their stops, targets and exits are"
                ),
                pick(n, "it will", "they will"),
            ));
        }
        let n = self.live_openscript_runs;
        if n > 0 {
            items.push(format!(
                "{}. {} paused when OpenAlgo closes; start {} again {}.",
                count(
                    n,
                    "running Live OpenScript run",
                    "running Live OpenScript runs"
                ),
                pick(n, "It is", "They are"),
                pick(n, "it", "them"),
                words.resume,
            ));
        }
        let n = self.live_scalping_stops;
        if n > 0 {
            items.push(format!(
                "{}. Stops and targets are not checked until OpenAlgo opens again; then protection resumes.",
                count(
                    n,
                    "Live position under scalping protection",
                    "Live positions under scalping protection"
                ),
            ));
        }
        items.extend(self.order_items(words, &at));
        if self.checks_failed {
            items.push(format!(
                "OpenAlgo could not check everything that is running. Check your strategies and positions {}.",
                words.before
            ));
        }
        items
    }

    /// Orders at the broker: open ones, an unreadable book, and placements
    /// whose outcome is not known yet.
    fn order_items(&self, words: &Wording, at: &str) -> Vec<String> {
        let mut items = Vec::new();
        let n = self.live_open_orders;
        if n > 0 {
            items.push(format!(
                "{}{}. {} at your broker; OpenAlgo follows {} again once it opens.",
                count(n, "open order", "open orders"),
                at,
                pick(n, "It stays", "They stay"),
                pick(n, "it", "them"),
            ));
        }
        if self.live_orders_unknown {
            items.push(format!(
                "OpenAlgo could not read your open orders{}. Check your order book {}.",
                at, words.before
            ));
        }
        let n = self.unsettled_orders;
        if n > 0 {
            items.push(format!(
                "{}. OpenAlgo settles {} from your broker's order book once it opens again.",
                count(
                    n,
                    "order whose outcome is not known yet",
                    "orders whose outcome is not known yet"
                ),
                pick(n, "it", "them"),
            ));
        }
        items
    }

    /// The sandbox work a restart pauses, as list entries.
    fn sandbox_items(&self, words: &Wording) -> Vec<String> {
        let mut items = Vec::new();
        if self.sandbox_strategies > 0 {
            items.push(count(
                self.sandbox_strategies,
                "sandbox strategy",
                "sandbox strategies",
            ));
        }
        let n = self.sandbox_openscript_runs;
        if n > 0 {
            items.push(format!(
                "{} (paused; start {} again {})",
                count(n, "sandbox OpenScript run", "sandbox OpenScript runs"),
                pick(n, "it", "them"),
                words.resume,
            ));
        }
        if self.sandbox_scalping_stops > 0 {
            items.push(count(
                self.sandbox_scalping_stops,
                "sandbox position under scalping protection",
                "sandbox positions under scalping protection",
            ));
        }
        if self.sandbox_open_orders > 0 {
            items.push(count(
                self.sandbox_open_orders,
                "open sandbox order",
                "open sandbox orders",
            ));
        }
        items
    }
}

fn live_warning(words: &Wording, items: Vec<String>) -> Warning {
    Warning {
        level: "live",
        title: "Live trading is running".into(),
        intro: words.live_intro.into(),
        items,
        advice: words.live_advice.into(),
    }
}

fn sandbox_warning(words: &Wording, sandbox: Vec<String>) -> Warning {
    Warning {
        level: "sandbox",
        title: "Sandbox activity is running".into(),
        intro: words.sandbox_intro.into(),
        items: sandbox
            .into_iter()
            .map(|s| format!("{}.", capitalise(&s)))
            .collect(),
        advice: "Nothing is sent to your broker. Sandbox orders and positions are kept.".into(),
    }
}

/// "1 open order", "3 open orders".
fn count(n: u32, one: &str, many: &str) -> String {
    format!("{} {}", n, pick(n, one, many))
}

/// The singular or plural wording for `n`.
fn pick<'a>(n: u32, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn join_list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_warning_names_every_item_in_a_traders_words() {
        let a = Activity {
            live_strategies: 2,
            live_openscript_runs: 1,
            live_scalping_stops: 1,
            live_open_orders: 3,
            sandbox_open_orders: 2,
            ..Default::default()
        };
        let w = a.warning(Some("Zerodha")).unwrap();
        assert_eq!(w.level, "live");
        assert_eq!(w.title, "Live trading is running");
        assert_eq!(
            w.items,
            vec![
                "2 running Live strategies. Their stops, targets and exits are not checked until OpenAlgo opens again; then they will resume.",
                "1 running Live OpenScript run. It is paused when OpenAlgo closes; start it again after the update.",
                "1 Live position under scalping protection. Stops and targets are not checked until OpenAlgo opens again; then protection resumes.",
                "3 open orders at Zerodha. They stay at your broker; OpenAlgo follows them again once it opens.",
                "Sandbox: 2 open sandbox orders. Nothing there is sent to your broker.",
            ]
        );
        let unknown = Activity {
            live_orders_unknown: true,
            ..Default::default()
        };
        assert_eq!(
            unknown.warning(Some("Fyers")).unwrap().items,
            vec!["OpenAlgo could not read your open orders at Fyers. Check your order book before you update."]
        );
    }

    #[test]
    fn sandbox_only_activity_gets_the_lighter_warning() {
        let a = Activity {
            sandbox_strategies: 1,
            sandbox_openscript_runs: 2,
            sandbox_open_orders: 1,
            ..Default::default()
        };
        assert!(!a.has_live());
        let w = a.warning(None).unwrap();
        assert_eq!(w.level, "sandbox");
        assert_eq!(
            w.items,
            vec![
                "1 sandbox strategy.",
                "2 sandbox OpenScript runs (paused; start them again after the update).",
                "1 open sandbox order.",
            ]
        );
        assert!(w.advice.starts_with("Nothing is sent to your broker."));
        assert_eq!(Activity::default().warning(None), None);
    }
}
