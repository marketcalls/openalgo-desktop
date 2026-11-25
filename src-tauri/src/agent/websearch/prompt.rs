//! The prompt section that says where company figures are looked for
//! first (the web's `services/agent/web_prompt.py`), built from the
//! trader's preferred sources and added only to a run offered the web
//! tools: a surface is never taught a tool it does not have. It carries
//! the order of preference and what counts as failure, and leaves the
//! mechanics to the tool descriptions.

use crate::agent::prompts::PromptSection;

pub const WEB_SECTION_KEY: &str = "web_sources";

/// Straight after USING TOOLS (20), for the reason the external data
/// section gives: the same rule at the end of a long prompt was not
/// followed.
pub const WEB_SECTION_ORDER: i64 = 23;

/// The one source whose company pages can be opened from a symbol alone.
const DIRECT_PAGE_SOURCE: &str = "screener.in";

/// Names joined for a sentence (`_list`): `a`, `a then b`,
/// `a, then b, then c`.
fn listed(names: &[&str]) -> String {
    match names.len() {
        0 | 1 => names.concat(),
        2 => names.join(" then "),
        _ => names.join(", then "),
    }
}

/// The section for one run, or `None` when no source is preferred (the
/// model then searches the open web as it always did).
pub fn section(preferred: &[String]) -> Option<PromptSection> {
    let sources: Vec<&str> = preferred
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "- Anything about a company's fundamentals (market cap, P/E, ROCE, ROE, revenue, profit, balance sheet, cash flow, quarterly results, shareholding, peers, ratios) comes first from {}. Do not start with the open web, and do not answer these from memory.",
        listed(&sources)
    )];
    if sources.contains(&DIRECT_PAGE_SOURCE) {
        lines.push(format!(
            "- When the operator names one company by its NSE symbol, open its {} page directly with web_fetch, for example https://www.screener.in/company/RELIANCE/consolidated/. No search is needed first. Use /company/SYMBOL/ when the consolidated page is empty.",
            DIRECT_PAGE_SOURCE
        ));
    }
    lines.extend(
        [
            "- Otherwise call web_search with topic \"fundamentals\". It searches the sources in this priority order and returns the pages of the first one that has the company. Read the best with web_fetch.",
            "- If a source cannot be opened (blocked, not found, an error), call web_search with topic \"fundamentals\" again: that source is dropped and the next one is searched. One source being unavailable is not a reason to leave the others.",
            "- Only when every preferred source has failed or has no page for the company, search the open web (topic \"general\"), and tell the operator that you had to, naming the source you used instead.",
            "- For many companies at once, such as an index's constituents, find one page that lists them and read it in slices. Do not open one page per company.",
            "- These pages are third-party and carry a date. Name the page a figure came from, give the date it shows, and never present it as a live price.",
        ]
        .map(String::from),
    );
    Some(PromptSection::new(
        WEB_SECTION_KEY,
        "WHERE TO LOOK FOR COMPANY FIGURES",
        &lines.join("\n"),
        WEB_SECTION_ORDER,
    ))
}
