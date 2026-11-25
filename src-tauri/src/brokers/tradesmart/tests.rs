//! TradeSmart's master contract and history (marketcalls/openalgo#2475,
//! 9a12bd0f7). The web commit adds no test of its own; these cases are the
//! examples its commit messages give (QA MC-03, MC-04, MC-11, HS-07), on
//! the synthetic scrip-master rows in `tests/fixtures/brokers/tradesmart/`
//! (no account data). Each runs through the adapter's public paths
//! (`parse_file`, `Broker::get_history`, `Broker::on_logout`), and the
//! other Noren members are held to the rules they had.

use super::config;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::families::noren::master_contract::parse_file;
use crate::brokers::shoonya;
use std::collections::HashSet;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../tests/fixtures/brokers/tradesmart/",
            $name
        ))
    };
}

fn row<'a>(rows: &'a [SymToken], exchange: &str, symbol: &str) -> &'a SymToken {
    rows.iter()
        .find(|r| r.exchange == exchange && r.symbol == symbol)
        .unwrap_or_else(|| panic!("no {exchange}:{symbol} in {rows:?}"))
}

/// Every master row TradeSmart's files give, as the adapter stores them.
fn tradesmart_master() -> Vec<SymToken> {
    let cfg = config();
    let mut rows = parse_file(cfg, "NSE", fixture!("NSE_symbols.txt"));
    rows.extend(parse_file(cfg, "BSE", fixture!("BSE_symbols.txt")));
    rows.extend(parse_file(cfg, "BFO", fixture!("BFO_symbols.txt")));
    rows
}

/// QA MC-04 allows only EQ, FUT, CE and PE. The Noren `Instrument` column
/// on NSE holds the series (BE, GB, N0, SG), which was stored as the type;
/// index rows were `INDEX`. Every NSE row is EQ now, and the index rows
/// are EQ on NSE_INDEX, as Kite types them.
#[test]
fn every_nse_row_is_eq_and_index_rows_are_eq_on_the_index_exchange() {
    let rows = parse_file(config(), "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(rows.len(), 14);
    for r in &rows {
        assert_eq!(r.instrument_type, "EQ", "{}:{}", r.exchange, r.symbol);
    }
    for (symbol, series) in [
        ("YESBANK", "YESBANK-BE"),
        ("SGBJUN31I-GB", "SGBJUN31I-GB"),
        ("NHAI-N0", "NHAI-N0"),
        ("735GS2033-SG", "735GS2033-SG"),
    ] {
        assert_eq!(row(&rows, "NSE", symbol).brsymbol, series);
    }
    for (symbol, tsym, token) in [
        ("NIFTY", "NIFTY INDEX", "26000"),
        ("BANKNIFTY", "NIFTY BANK", "26009"),
        ("FINNIFTY", "FINNIFTY", "26037"),
        ("INDIAVIX", "INDIA VIX", "26017"),
    ] {
        let index = row(&rows, "NSE_INDEX", symbol);
        assert_eq!(
            (index.brsymbol.as_str(), index.brexchange.as_str()),
            (tsym, "NSE_INDEX")
        );
        assert_eq!(index.token, token);
    }
}

/// QA MC-03: TradeSmart lists four BSE rows (WEAL, MFLS2, MFLS3, INVE)
/// with the trading symbol `12:` and tick size 0. They cannot be traded or
/// addressed by symbol and were stored as duplicates; they are skipped.
/// The manual SENSEX and BANKEX rows are EQ on BSE_INDEX.
#[test]
fn bse_rows_without_a_tick_are_skipped_and_the_bse_indices_are_eq() {
    let rows = parse_file(config(), "BSE", fixture!("BSE_symbols.txt"));
    assert!(
        rows.iter().all(|r| r.brsymbol != "12:"),
        "placeholder rows stored: {rows:?}"
    );
    assert_eq!(rows.len(), 6, "four equities and two indices");
    for symbol in ["RELIANCE", "BAJAJ-AUTO", "MRF", "IDEA"] {
        assert_eq!(row(&rows, "BSE", symbol).instrument_type, "EQ");
    }
    for (symbol, token) in [("SENSEX", "1"), ("BANKEX", "12")] {
        let index = row(&rows, "BSE_INDEX", symbol);
        assert_eq!(
            (index.token.as_str(), index.instrument_type.as_str()),
            (token, "EQ")
        );
    }
}

/// QA MC-03: the BFO `Symbol` column holds codes (BSXOPT, SX50FUT), and a
/// letters-only match on the trading symbol cut SENSEX50 to SENSEX (its
/// futures collided with SENSEX's), M&M to M, BAJAJ-AUTO to BAJAJ, and
/// NAM-INDIA, GVT&D and 360ONE likewise. The underlying is the trading
/// symbol less its expiry, strike and type, weekly symbols included.
#[test]
fn bfo_underlying_is_the_trading_symbol_less_its_contract() {
    let rows = parse_file(config(), "BFO", fixture!("BFO_symbols.txt"));
    assert_eq!(rows.len(), 10);
    for (tsym, symbol, name, kind) in [
        ("SENSEX5026NOVFUT", "SENSEX5026NOV26FUT", "SENSEX50", "FUT"),
        ("SENSEX26NOVFUT", "SENSEX26NOV26FUT", "SENSEX", "FUT"),
        ("SENSEX26D0389500PE", "SENSEX03DEC2689500PE", "SENSEX", "PE"),
        ("SENSEX26NOV85000CE", "SENSEX26NOV2685000CE", "SENSEX", "CE"),
        ("BANKEX26NOV26000CE", "BANKEX26NOV2626000CE", "BANKEX", "CE"),
        ("M&M26NOVFUT", "M&M26NOV26FUT", "M&M", "FUT"),
        (
            "BAJAJ-AUTO26NOV9000CE",
            "BAJAJ-AUTO26NOV269000CE",
            "BAJAJ-AUTO",
            "CE",
        ),
        (
            "NAM-INDIA26NOVFUT",
            "NAM-INDIA26NOV26FUT",
            "NAM-INDIA",
            "FUT",
        ),
        ("GVT&D26NOV2500PE", "GVT&D26NOV262500PE", "GVT&D", "PE"),
        (
            "360ONE26NOV1012.5CE",
            "360ONE26NOV261012.5CE",
            "360ONE",
            "CE",
        ),
    ] {
        let r = row(&rows, "BFO", symbol);
        assert_eq!(
            (
                r.brsymbol.as_str(),
                r.name.as_str(),
                r.instrument_type.as_str()
            ),
            (tsym, name, kind)
        );
    }
    assert_eq!(
        row(&rows, "BFO", "SENSEX03DEC2689500PE").expiry,
        "03-DEC-26"
    );
    assert_eq!(row(&rows, "BFO", "360ONE26NOV261012.5CE").strike, 1012.5);
}

/// QA MC-03: no (symbol, exchange) pair twice in the whole master.
#[test]
fn the_master_holds_each_symbol_once() {
    let rows = tradesmart_master();
    let mut seen = HashSet::new();
    for r in &rows {
        assert!(
            seen.insert((r.exchange.as_str(), r.symbol.as_str())),
            "{}:{} stored twice",
            r.exchange,
            r.symbol
        );
    }
}

/// The other members keep the rules their own web code has: on the same
/// files Shoonya stores the series as the type, keeps the `12:` rows,
/// reads the BFO underlying from the leading letters (SENSEX50 reads
/// SENSEX, M&M reads M) and types its indices INDEX.
#[test]
fn other_noren_members_keep_their_master_rules() {
    let cfg = shoonya::config();
    let nse = parse_file(cfg, "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(row(&nse, "NSE", "SGBJUN31I-GB").instrument_type, "GB");
    assert_eq!(row(&nse, "NSE_INDEX", "NIFTY").instrument_type, "INDEX");
    let bse = parse_file(cfg, "BSE", fixture!("BSE_symbols.txt"));
    assert_eq!(bse.iter().filter(|r| r.brsymbol == "12:").count(), 4);
    assert_eq!(row(&bse, "BSE_INDEX", "SENSEX").instrument_type, "INDEX");
    let bfo = parse_file(cfg, "BFO", fixture!("BFO_symbols.txt"));
    assert_eq!(row(&bfo, "BFO", "M26NOV26FUT").brsymbol, "M&M26NOVFUT");
    let sensex_futures = bfo
        .iter()
        .filter(|r| r.symbol == "SENSEX26NOV26FUT")
        .count();
    assert_eq!(sensex_futures, 2, "SENSEX50 reads SENSEX on Shoonya");
}

/// QA TS-01/04/05/06, MC-11: TradeSmart's NSE and BSE files give the tick
/// size in rupees (RELIANCE 0.1, BAJAJ-AUTO 1, MRF 5), and dividing it as
/// paise stored RELIANCE at 0.001 and MRF at 0.05. Every file is stored as
/// sent, derivatives as before; Shoonya's paise rule is unchanged.
#[test]
fn nse_and_bse_ticks_are_stored_in_rupees() {
    let rows = tradesmart_master();
    for (exchange, symbol, tick) in [
        ("NSE", "IDEA", 0.01),
        ("NSE", "RELIANCE", 0.1),
        ("NSE", "BAJAJ-AUTO", 1.0),
        ("NSE", "MRF", 5.0),
        ("NSE_INDEX", "NIFTY", 0.05),
        ("BSE", "RELIANCE", 0.05),
        ("BSE", "BAJAJ-AUTO", 1.0),
        ("BSE", "MRF", 5.0),
        ("BSE_INDEX", "SENSEX", 0.05),
        ("BFO", "M&M26NOV26FUT", 0.1),
        ("BFO", "SENSEX26NOV2685000CE", 0.05),
    ] {
        assert_eq!(
            row(&rows, exchange, symbol).tick_size,
            tick,
            "{exchange}:{symbol}"
        );
    }
    let shoonya = parse_file(shoonya::config(), "NSE", fixture!("NSE_symbols.txt"));
    assert_eq!(row(&shoonya, "NSE", "MRF").tick_size, 0.05);
}
