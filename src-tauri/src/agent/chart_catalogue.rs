//! The indicators the `/trading` chart can draw (the web's
//! `services/agent/tools/chart.py` `_chart_indicator_catalogue`).
//!
//! The chart draws with `openalgo-charts`, a JavaScript library; the
//! Agent computes values with `openalgo.ta`. They are different catalogues
//! that share a domain, so a name in one is not necessarily in the other.
//! `docs/prompt/indicators/chart-indicators.md` is generated from the
//! chart's own registry by `scripts/generate-chart-indicators.mjs`, checked
//! against it by `src/lib/trading/chartIndicators.test.ts`, and compiled
//! in here (decision 17), so the ids the Agent accepts for a chart are the
//! ids the bundled chart draws.
//!
//! The web reads the file at run time and degrades to "any well-formed id"
//! when it cannot; compiled in, it is always readable, and a test pins that
//! it parses to the count the file states.

use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// The generated catalogue, as committed.
const DOC: &str = include_str!("../../../docs/prompt/indicators/chart-indicators.md");

/// One indicator the chart can draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartIndicator {
    /// The `openalgo-charts` id, kebab-case (`parabolic-sar`).
    pub id: String,
    /// The display name (`Parabolic SAR`).
    pub name: String,
    /// The heading it is listed under (`Trend`), `Other` before any.
    pub category: String,
}

/// A catalogue line: `` - `id` Name (placement) `` (the web's
/// `_CATALOGUE_LINE`).
static LINE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^- `([a-z0-9-]+)` (.+?) \((.+)\)$").ok());

/// Parse a catalogue document, in the order it lists the indicators.
fn parse(doc: &str) -> Vec<ChartIndicator> {
    let Some(line_re) = LINE.as_ref() else {
        return Vec::new();
    };
    let mut category = "Other".to_string();
    let mut rows = Vec::new();
    for line in doc.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            category = heading.trim().to_string();
            continue;
        }
        if let Some(c) = line_re.captures(line.trim()) {
            rows.push(ChartIndicator {
                id: c[1].to_string(),
                name: c[2].to_string(),
                category: category.clone(),
            });
        }
    }
    rows
}

static ROWS: LazyLock<Vec<ChartIndicator>> = LazyLock::new(|| parse(DOC));

static IDS: LazyLock<BTreeSet<String>> =
    LazyLock::new(|| ROWS.iter().map(|r| r.id.clone()).collect());

/// Every indicator the chart can draw, in the generated order.
pub fn catalogue() -> &'static [ChartIndicator] {
    &ROWS
}

/// The ids alone, sorted (what the chart tools check an id against).
pub fn ids() -> &'static BTreeSet<String> {
    &IDS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed file parses to exactly the count it states, so a
    /// format change in the generator cannot leave the Agent with an empty
    /// or partial catalogue.
    #[test]
    fn the_committed_catalogue_parses_whole() {
        let stated = DOC
            .lines()
            .find_map(|l| l.strip_suffix(" indicators."))
            .and_then(|n| n.parse::<usize>().ok())
            .expect("the catalogue states its count");
        assert_eq!(catalogue().len(), stated);
        assert_eq!(ids().len(), stated, "ids are unique");
        assert!(stated > 100);
    }

    /// The tier boundary the file exists for: drawable but not in
    /// `openalgo.ta`, and the other way round.
    #[test]
    fn it_lists_what_the_chart_draws_and_nothing_else() {
        for id in [
            "alphatrend",
            "halftrend",
            "bollinger",
            "parabolic-sar",
            "ema",
        ] {
            assert!(ids().contains(id), "{}", id);
        }
        for id in ["bbands", "adxr", "psar"] {
            assert!(!ids().contains(id), "{}", id);
        }
        let ema = catalogue().iter().find(|r| r.id == "ema").expect("ema");
        assert!(!ema.name.is_empty() && ema.category != "Other");
    }

    #[test]
    fn lines_outside_the_format_are_skipped_and_headings_set_the_category() {
        let rows = parse(
            "# Title\n- `orphan` Orphan (own pane)\n## Trend\n\n- `ema` EMA (on the price)\n- not a row\n",
        );
        assert_eq!(
            rows,
            vec![
                ChartIndicator {
                    id: "orphan".into(),
                    name: "Orphan".into(),
                    category: "Other".into()
                },
                ChartIndicator {
                    id: "ema".into(),
                    name: "EMA".into(),
                    category: "Trend".into()
                },
            ]
        );
    }
}
