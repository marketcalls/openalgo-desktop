//! Which master contract symbol a trader means when adding to the
//! Historify watchlist (web `historify_service.resolve_symbol`,
//! openalgo#2463).
//!
//! OpenAlgo's NSE and BSE symbols keep the exchange's own spelling, hyphen
//! and ampersand included (BAJAJ-AUTO, M&M, J&KBANK). Lists copied from
//! elsewhere can carry those as an underscore, so an underscore spelling is
//! read as the one master contract symbol it can stand for. Nothing else is
//! rewritten: a series suffix is neither added nor dropped here.

/// Characters a list copied from elsewhere may carry as an underscore:
/// TradingView's symbol for NSE BAJAJ-AUTO is NSE:BAJAJ_AUTO, and its chart
/// links write M&M as M_M. A Zerodha master contract checked on 2026-11-17
/// (by the web) held no NSE or BSE symbol containing an underscore.
const UNDERSCORE_STANDS_FOR: [char; 2] = ['-', '&'];

/// Each underscore doubles the lookups. Real symbols carry one or two.
const MAX_UNDERSCORES_TO_RESOLVE: usize = 4;

/// Every spelling of `symbol` with each underscore read as - or &, in the
/// web's order (hyphen first, left to right).
fn underscore_candidates(symbol: &str) -> Vec<String> {
    let mut candidates = vec![String::new()];
    for ch in symbol.chars() {
        if ch == '_' {
            candidates = candidates
                .iter()
                .flat_map(|c| {
                    UNDERSCORE_STANDS_FOR.iter().map(move |sub| {
                        let mut next = c.clone();
                        next.push(*sub);
                        next
                    })
                })
                .collect();
        } else {
            for c in &mut candidates {
                c.push(ch);
            }
        }
    }
    candidates
}

/// The master contract symbol a trader means, upper-cased, or the reason
/// it cannot be added. `listed(symbol, exchange)` says whether the master
/// contract holds that symbol.
///
/// The symbol as written wins. Only when it is not in the master contract
/// and contains an underscore is each underscore read as - or &, and the
/// spelling is accepted only if exactly one reading exists (BAJAJ_AUTO as
/// BAJAJ-AUTO). Two possible symbols are refused rather than guessed
/// between. Index exchanges are not in the master and are not checked.
pub(super) fn resolve_symbol(
    symbol: &str,
    exchange: &str,
    listed: impl Fn(&str, &str) -> bool,
) -> Result<String, String> {
    let symbol = symbol.to_uppercase();
    let exchange = exchange.to_uppercase();
    if exchange == "NSE_INDEX" || exchange == "BSE_INDEX" {
        return Ok(symbol);
    }
    if listed(&symbol, &exchange) {
        return Ok(symbol);
    }
    let underscores = symbol.matches('_').count();
    if (1..=MAX_UNDERSCORES_TO_RESOLVE).contains(&underscores) {
        let mut matches: Vec<String> = underscore_candidates(&symbol)
            .into_iter()
            .filter(|c| listed(c, &exchange))
            .collect();
        if matches.len() == 1 {
            return Ok(matches.remove(0));
        }
        if matches.len() > 1 {
            return Err(format!(
                "Symbol '{}' could be {} on {}. Enter the exact symbol.",
                symbol,
                matches.join(" or "),
                exchange
            ));
        }
    }
    Err(format!(
        "Symbol '{}' not found in {} master contract. Please check the symbol name.",
        symbol, exchange
    ))
}

#[cfg(test)]
mod tests;
