//! The `instructions` an MCP client gets with `initialize` (MCP-03): what
//! the desktop computes and the limits it enforces, so a client can plan a
//! call instead of learning them from a refused one.
//!
//! The tool descriptors stay byte-identical to the web's (the contract test
//! pins them), so this is the one place that says where the desktop
//! differs: the indicators it computes (`ta::supported`) and the research
//! and rate limits (`research::limits`, `http`). Every number is read from
//! the constant that enforces it, so the text cannot drift from the code.
//! The web sends no instructions; adding them changes no tool.

use super::http::{DISPATCH_LIMIT, READ_LIMIT, WRITE_LIMIT};
use super::research::{
    MAX_BARS, MAX_DAYS, MAX_FETCH_BARS, MAX_PER_TOKEN, MAX_SCREEN_SYMBOLS, MAX_TIMEFRAMES,
    RESEARCH_DEADLINE, RESEARCH_SLOTS, SLOT_WAIT,
};
use super::ta;

/// The text served in the `initialize` result, over HTTP and stdio.
pub(crate) fn instructions() -> String {
    format!(
        "OpenAlgo Desktop serves the tools of OpenAlgo web. Where it differs:\n\
         \n\
         Indicators: calculate_indicator and the research tools compute {count} indicators: \
         {names}. Any other indicator name is refused with error_type \"not_available\".\n\
         \n\
         Research limits, checked before any data is fetched (a call over one is refused with \
         error_type \"limit_exceeded\"): screen_instruments takes at most {symbols} symbols; \
         multi_timeframe_analysis at most {timeframes} intervals; bars, lookback_bars and limit \
         at most {bars}; lookback_days and a date range at most {days} days; one history fetch \
         at most {fetch} bars. Each token runs at most {per_token} research calls at once and \
         OpenAlgo at most {slots} in all; a call waits up to {wait} seconds for its turn \
         (error_type \"busy\" after that) and stops after {deadline} seconds.\n\
         \n\
         Rate limits per token: {read} calls a minute to market data tools and as many to \
         account tools, {write} calls a minute to order tools, {dispatch} requests a minute \
         in all.",
        count = ta::supported().len(),
        names = ta::supported().join(", "),
        symbols = MAX_SCREEN_SYMBOLS,
        timeframes = MAX_TIMEFRAMES,
        bars = MAX_BARS,
        days = MAX_DAYS,
        fetch = MAX_FETCH_BARS as u64,
        per_token = MAX_PER_TOKEN,
        slots = RESEARCH_SLOTS,
        wait = SLOT_WAIT.as_secs(),
        deadline = RESEARCH_DEADLINE.as_secs(),
        read = READ_LIMIT.0,
        write = WRITE_LIMIT.0,
        dispatch = DISPATCH_LIMIT.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_supported_indicator_and_every_limit() {
        let text = instructions();
        for name in ta::supported() {
            assert!(text.contains(name), "{} missing", name);
        }
        for n in [
            MAX_SCREEN_SYMBOLS.to_string(),
            MAX_BARS.to_string(),
            MAX_DAYS.to_string(),
            RESEARCH_DEADLINE.as_secs().to_string(),
        ] {
            assert!(text.contains(&n), "{} missing", n);
        }
    }
}
