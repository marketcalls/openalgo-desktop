//! The index symbol lists of the web's `mcp/mcpserver.py` and the exchange
//! a tool picks for a symbol when the caller named none (web f456b3216).
//!
//! An index is quoted on its index exchange, never the equity one: SENSEX
//! lives on BSE_INDEX and NIFTY on NSE_INDEX, and asking NSE for either is
//! refused as an unknown symbol. No name in either list is also an NSE or
//! BSE equity, so routing on them cannot send a stock to the wrong place.

/// OpenAlgo standardised index symbols (`NSE_INDEX_SYMBOLS`).
pub const NSE_INDEX_SYMBOLS: &[&str] = &[
    "NIFTY",
    "NIFTYNXT50",
    "FINNIFTY",
    "BANKNIFTY",
    "MIDCPNIFTY",
    "INDIAVIX",
    "HANGSENGBEESNAV",
    "NIFTY100",
    "NIFTY200",
    "NIFTY500",
    "NIFTYALPHA50",
    "NIFTYAUTO",
    "NIFTYCOMMODITIES",
    "NIFTYCONSUMPTION",
    "NIFTYCPSE",
    "NIFTYDIVOPPS50",
    "NIFTYENERGY",
    "NIFTYFMCG",
    "NIFTYGROWSECT15",
    "NIFTYGS10YR",
    "NIFTYGS10YRCLN",
    "NIFTYGS1115YR",
    "NIFTYGS15YRPLUS",
    "NIFTYGS48YR",
    "NIFTYGS813YR",
    "NIFTYGSCOMPSITE",
    "NIFTYINFRA",
    "NIFTYIT",
    "NIFTYMEDIA",
    "NIFTYMETAL",
    "NIFTYMIDLIQ15",
    "NIFTYMIDCAP100",
    "NIFTYMIDCAP150",
    "NIFTYMIDCAP50",
    "NIFTYMIDSML400",
    "NIFTYMNC",
    "NIFTYPHARMA",
    "NIFTYPSE",
    "NIFTYPSUBANK",
    "NIFTYPVTBANK",
    "NIFTYREALTY",
    "NIFTYSERVSECTOR",
    "NIFTYSMLCAP100",
    "NIFTYSMLCAP250",
    "NIFTYSMLCAP50",
    "NIFTY100EQLWGT",
    "NIFTY100LIQ15",
    "NIFTY100LOWVOL30",
    "NIFTY100QUALTY30",
    "NIFTY200QUALTY30",
    "NIFTY50DIVPOINT",
    "NIFTY50EQLWGT",
    "NIFTY50PR1XINV",
    "NIFTY50PR2XLEV",
    "NIFTY50TR1XINV",
    "NIFTY50TR2XLEV",
    "NIFTY50VALUE20",
];

/// `BSE_INDEX_SYMBOLS`.
pub const BSE_INDEX_SYMBOLS: &[&str] = &[
    "SENSEX",
    "BANKEX",
    "SENSEX50",
    "BSE100",
    "BSE150MIDCAPINDEX",
    "BSE200",
    "BSE250LARGEMIDCAPINDEX",
    "BSE400MIDSMALLCAPINDEX",
    "BSE500",
    "BSEAUTO",
    "BSECAPITALGOODS",
    "BSECARBONEX",
    "BSECONSUMERDURABLES",
    "BSECPSE",
    "BSEDOLLEX100",
    "BSEDOLLEX200",
    "BSEDOLLEX30",
    "BSEENERGY",
    "BSEFASTMOVINGCONSUMERGOODS",
    "BSEFINANCIALSERVICES",
    "BSEGREENEX",
    "BSEHEALTHCARE",
    "BSEINDIAINFRASTRUCTUREINDEX",
    "BSEINDUSTRIALS",
    "BSEINFORMATIONTECHNOLOGY",
    "BSEIPO",
    "BSELARGECAP",
    "BSEMETAL",
    "BSEMIDCAP",
    "BSEMIDCAPSELECTINDEX",
    "BSEOIL&GAS",
    "BSEPOWER",
    "BSEPSU",
    "BSEREALTY",
    "BSESENSEXNEXT50",
    "BSESMALLCAP",
    "BSESMALLCAPSELECTINDEX",
    "BSESMEIPO",
    "BSETECK",
    "BSETELECOM",
];

fn is_bse_index(symbol: &str) -> bool {
    BSE_INDEX_SYMBOLS.contains(&symbol.to_uppercase().as_str())
}

/// The exchange `get_quote` asks for: the caller's (an empty one counts as
/// omitted, as the web's `exchange or ...`), else the symbol's index
/// exchange, else NSE (web `_default_quote_exchange`).
pub(crate) fn quote_exchange(symbol: &str, named: &str) -> String {
    if !named.is_empty() {
        return named.to_uppercase();
    }
    let s = symbol.to_uppercase();
    if is_bse_index(&s) {
        "BSE_INDEX".into()
    } else if NSE_INDEX_SYMBOLS.contains(&s.as_str()) {
        "NSE_INDEX".into()
    } else {
        "NSE".into()
    }
}

/// The exchange `get_expiry_dates` asks for: the caller's, else BFO for
/// the BSE indices (SENSEX, BANKEX and SENSEX50 trade there) and NFO for
/// any other underlying (web `_default_derivatives_exchange`).
pub(crate) fn derivatives_exchange(symbol: &str, named: &str) -> String {
    if !named.is_empty() {
        return named.to_uppercase();
    }
    if is_bse_index(symbol) { "BFO" } else { "NFO" }.into()
}

#[cfg(test)]
mod tests;
